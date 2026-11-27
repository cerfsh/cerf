use std::process::Command;

fn cerf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cerf"))
}

#[cfg(target_os = "linux")]
#[test]
fn script_mode_runs_external_commands_on_a_terminal() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // util-linux `script` gives the child a controlling terminal so the job
    // control path is exercised. Skip where it is unavailable or incompatible.
    let version = match Command::new("script").arg("--version").output() {
        Ok(output) => output,
        Err(_) => return,
    };
    if !String::from_utf8_lossy(&version.stdout).contains("util-linux") {
        return;
    }

    let mut child = Command::new("script")
        .args(["-qec", "$CERF -c '/bin/echo hi | /bin/cat'", "/dev/null"])
        .env("CERF", env!("CARGO_BIN_EXE_cerf"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn script");

    // Watchdog: a regression stops the child with SIGTTOU and blocks forever.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("wait for script") {
            assert!(status.success(), "script exited with {status:?}");
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cerf -c hung on an external command with a controlling terminal");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn script_mode_propagates_builtin_status() {
    let output = cerf()
        .args(["-c", "net.fetch file:///etc/passwd"])
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("http"),
        "stderr was {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolated_builtin_receives_stdout_redirect() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("fetch.out");
    let command = format!("net.fetch file:///etc/passwd > '{}'", path.display());

    let output = cerf().args(["-c", &command]).output().expect("run cerf");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        path.exists(),
        "the engine should open the redirect before launching the isolated command"
    );
    assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
}

#[test]
fn wrapped_isolated_builtin_receives_stdout_redirect() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("wrapped.out");
    let command = format!(
        "sys.builtin net.fetch file:///etc/passwd > '{}'",
        path.display()
    );

    let output = cerf().args(["-c", &command]).output().expect("run cerf");

    assert_eq!(output.status.code(), Some(1));
    assert!(path.exists(), "wrapped command should receive the redirect");
    assert_eq!(std::fs::metadata(&path).expect("metadata").len(), 0);
}

#[test]
fn empty_and_comment_scripts_succeed_but_syntax_errors_fail() {
    for script in ["", "   ", "# comment", "# one\n  # two\n"] {
        let output = cerf().args(["-c", script]).output().expect("run cerf");
        assert_eq!(output.status.code(), Some(0), "script was {script:?}");
    }

    // A leading comment must not swallow the command lines after it.
    let output = cerf()
        .args(["-c", "# comment\necho hi"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");

    let output = cerf()
        .args(["-c", "# comment\n|"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(2));

    let output = cerf().args(["-c", "|"]).output().expect("run cerf");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn pipeline_status_uses_last_builtin() {
    let output = cerf()
        .args(["-c", "io.echo hello | test.false"])
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn all_network_commands_are_registered() {
    for name in [
        "net.fetch",
        "net.wfp",
        "net.http",
        "net.listen",
        "net.whoami",
        "net.ip",
        "net.dns",
        "net.tunnel",
        "net.socket",
    ] {
        let output = cerf()
            .args(["-c", &format!("sys.type {name}")])
            .output()
            .expect("run cerf");
        assert_eq!(output.status.code(), Some(0), "{name}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("is a shell builtin"),
            "{name}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn background_builtin_starts_one_tracked_process() {
    let output = cerf()
        .args(["-c", "test.true &"])
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("[1] "))
            .count(),
        1,
        "stdout was {stdout:?}"
    );
}

#[test]
fn background_builtin_restores_explicit_prefix_assignments() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = directory.path().join("variables.cerf");
    std::fs::write(
        &script,
        "io.echo \"$CERF_TEST_OVERRIDE:$CERF_TEST_UNRELATED:$CERF_TEST_NEW\"",
    )
    .expect("write script");

    let command = format!(
        "CERF_TEST_OVERRIDE=original; CERF_TEST_UNRELATED=local; \
         CERF_TEST_OVERRIDE=prefix CERF_TEST_NEW=new env.source '{}' &",
        script.display()
    );
    let output = cerf()
        .env("CERF_TEST_UNRELATED", "inherited")
        .args(["-c", &command])
        .output()
        .expect("run cerf");

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "prefix:local:new"),
        "{output:?}"
    );
}

#[cfg(unix)]
#[test]
fn background_builtin_requires_a_snapshot_before_launch() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = cerf()
        .env("TMPDIR", directory.path().join("missing"))
        .args(["-c", "io.echo should-not-run &"])
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not write state snapshot"));
}

#[cfg(unix)]
#[test]
fn background_builtin_cleans_snapshot_after_launch_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let snapshots = directory.path().join("snapshots");
    std::fs::create_dir(&snapshots).expect("create snapshot directory");

    let output = cerf()
        .env("TMPDIR", &snapshots)
        .args(["-c", "test.true < missing-input &"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read_dir(&snapshots).unwrap().count(), 0);

    // A NUL in a prefix value makes Command::spawn fail. Source the script
    // from disk because process arguments cannot contain NULs either.
    let script = directory.path().join("spawn-failure.cerf");
    std::fs::write(&script, "CERF_TEST_VALUE='invalid\0value' test.true &").expect("write script");
    let output = cerf()
        .env("TMPDIR", &snapshots)
        .args(["--internal-builtin", "env.source"])
        .arg(script)
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(127), "{output:?}");
    assert_eq!(std::fs::read_dir(&snapshots).unwrap().count(), 0);

    let output = cerf()
        .env("TMPDIR", &snapshots)
        .args(["-c", "test.true &"])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(std::fs::read_dir(&snapshots).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn internal_builtin_waits_only_for_foreground_processes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let script = directory.path().join("jobs.cerf");
    std::fs::write(
        &script,
        "/bin/sh -c 'for i in 1 2 3 4 5 6 7 8 9 10; do \
         if test -f ready; then echo background; exit; fi; sleep 0.1; done; echo blocked' &\n\
         /bin/sh -c 'echo foreground; touch ready; exit 7'",
    )
    .expect("write script");
    let output = cerf()
        .args(["--internal-builtin", "env.source"])
        .arg(script)
        .current_dir(directory.path())
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(7));
    // Background jobs are registered in job-control-less children now, so
    // ignore their `[1] ...` job lines when checking the command output.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let messages: Vec<_> = stdout
        .lines()
        .filter(|line| !line.starts_with("[1] "))
        .collect();
    assert_eq!(
        messages,
        ["foreground", "background"],
        "stdout was {stdout:?}"
    );
}

#[test]
#[cfg(unix)]
fn background_command_inside_isolated_builtin_is_registered() {
    // `sys.eval` runs isolated; the `test.true &` it evaluates must be
    // registered as a job in the isolated child (printing `[1] pid`) and
    // return asynchronously, not be silently reaped in the foreground.
    let output = cerf()
        .args(["-c", "sys.eval 'test.true &' &"])
        .output()
        .expect("run cerf");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let registrations = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("[1] "))
        .filter(|rest| {
            rest.split_whitespace()
                .next()
                .is_some_and(|pid| pid.parse::<u32>().is_ok())
        })
        .count();
    assert_eq!(
        registrations, 2,
        "both the outer shell and the isolated builtin should register a background job; stdout was {stdout:?}"
    );
}

#[test]
fn builtin_stdout_redirect_writes_the_file() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("echo.out");
    let command = format!("echo hi > '{}'", path.display());

    let output = cerf().args(["-c", &command]).output().expect("run cerf");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "output leaked: {output:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi\n");
}

#[test]
fn builtin_stdin_redirect_is_read() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("input.txt");
    std::fs::write(&path, "fileline\n").expect("write input");
    let command = format!("read x < '{}'; io.echo \"x=$x\"", path.display());

    let output = cerf().args(["-c", &command]).output().expect("run cerf");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "x=fileline\n");
}

#[test]
fn redirected_block_keeps_each_command_separate() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("block.out");
    let command = format!(
        "if true {{ io.echo one; io.echo two }} > '{}'",
        path.display()
    );

    let output = cerf().args(["-c", &command]).output().expect("run cerf");

    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
}

#[test]
fn break_and_continue_control_their_loop() {
    let output = cerf()
        .args(["-c", "for x in a b c { io.echo $x; break }"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a\n");

    let output = cerf()
        .args(["-c", "for x in a b c { continue; io.echo $x }"])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");

    let output = cerf()
        .args(["-c", "while true { break }"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
}

#[cfg(unix)]
#[test]
fn wait_blocks_for_jobs_and_reports_status() {
    use std::time::{Duration, Instant};

    let started = Instant::now();
    let output = cerf()
        .args(["-c", "/bin/sh -c 'sleep 0.3' & wait"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "wait returned before the job finished"
    );

    let output = cerf()
        .args(["-c", "/bin/sh -c 'exit 7' & wait %1"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(7), "{output:?}");

    let output = cerf()
        .args(["-c", "wait 999999"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(127), "{output:?}");
}

#[test]
fn bracket_and_one_argument_test_forms_follow_posix() {
    for (script, expected) in [
        ("[ 1 = 1 ]", Some(0)),
        ("[ ]", Some(1)),
        ("test !", Some(0)),
        ("test -n", Some(0)),
        ("test -f = -f", Some(0)),
        ("test -n value", Some(0)),
        ("test -z value", Some(1)),
    ] {
        let output = cerf().args(["-c", script]).output().expect("run cerf");
        assert_eq!(output.status.code(), expected, "script was {script:?}");
    }

    // `/etc/passwd` exists only on Unix, so use an absolute path this test creates.
    let directory = tempfile::tempdir().expect("temporary directory");
    let existing = directory.path().join("existing.txt");
    std::fs::write(&existing, "").expect("write file");
    let output = cerf()
        .args(["-c", &format!("[ -f '{}' ]", existing.display())])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(0), "-f on an existing file");

    let output = cerf()
        .args(["-c", "if [ 1 = 1 ] { io.echo yes }"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "yes\n");
}

#[cfg(unix)]
#[test]
fn kill_zero_probes_without_signalling() {
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id().to_string();

    let output = cerf()
        .args(["-c", &format!("kill -0 {}", pid)])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert!(
        child.try_wait().expect("poll child").is_none(),
        "kill -0 terminated the target"
    );

    let output = cerf()
        .args(["-c", &format!("kill -s HUP {}", pid)])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    let status = child.wait().expect("wait for child");
    assert!(
        status.code().is_none(),
        "SIGHUP should terminate the child by signal: {status:?}"
    );

    let output = cerf().args(["-c", "kill %99"]).output().expect("run cerf");
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let output = cerf().args(["-c", "kill -l"]).output().expect("run cerf");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("TERM"));
}

#[cfg(unix)]
#[test]
fn background_pipeline_uses_first_spawned_pid_as_pgid() {
    // The first pipeline element is assignment-only and produces no process;
    // the job must still record the real child's pid, never pgid 0.
    let output = cerf()
        .args(["-c", "X=1 >/dev/null | /bin/sleep 20 & kill %1"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .lines()
        .find_map(|line| line.strip_prefix("[1] "))
        .expect("job registration");
    assert_ne!(pid.trim(), "0", "job registered with pgid 0: {stdout:?}");

    // Killing that job must not signal the shell's own process group.
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{} -c 'X=1 >/dev/null | /bin/sleep 20 & kill %1'",
            env!("CARGO_BIN_EXE_cerf")
        ))
        .output()
        .expect("run sh");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn cat_reads_stdin_without_operands_and_for_dash() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = cerf()
        .args(["-c", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn cerf");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"from-stdin\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "from-stdin\n");

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("input.txt");
    std::fs::write(&path, "file\n").expect("write input");
    let command = format!("cat - < '{}'", path.display());
    let output = cerf().args(["-c", &command]).output().expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "file\n");
}

#[test]
fn glob_does_not_match_dotfiles_unless_asked() {
    let directory = tempfile::tempdir().expect("temporary directory");
    std::fs::write(directory.path().join("a.txt"), "").expect("write a.txt");
    std::fs::write(directory.path().join(".env"), "").expect("write .env");

    let output = cerf()
        .args(["-c", "io.echo *"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "a.txt", "stdout was {stdout:?}");

    let output = cerf()
        .args(["-c", "io.echo .*"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), ".env", "stdout was {stdout:?}");
}

// ── Regression tests for the medium-severity issue batch ───────────────────

#[test]
fn inline_comments_are_not_arguments() {
    let output = cerf()
        .args(["-c", "io.echo hi # comment"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");

    // `/etc/passwd` exists only on Unix, so use an absolute path this test creates.
    let directory = tempfile::tempdir().expect("temporary directory");
    let existing = directory.path().join("existing.txt");
    std::fs::write(&existing, "").expect("write file");
    let output = cerf()
        .args(["-c", &format!("test -f '{}' # check", existing.display())])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(0), "{output:?}");

    // A `#` inside a word or quotes is literal.
    let output = cerf()
        .args(["-c", "io.echo hi#there"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi#there\n");

    let output = cerf()
        .args(["-c", "io.echo \"hi # there\""])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi # there\n");

    let output = cerf()
        .args(["-c", "io.echo one # comment\nio.echo two"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "one\ntwo\n");
}

#[test]
fn read_handles_a_multibyte_ifs_delimiter() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = cerf()
        .args(["-c", "IFS=é\nread a b\nio.echo \"$a|$b\""])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn cerf");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all("xéy\n".as_bytes())
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "x|y\n");
}

#[cfg(unix)]
#[test]
fn touch_updates_timestamps_and_supports_special_files() {
    use std::time::{Duration, UNIX_EPOCH};

    let directory = tempfile::tempdir().expect("temporary directory");
    let file = directory.path().join("f");
    std::fs::write(&file, "x").expect("write file");

    let old = UNIX_EPOCH + Duration::from_secs(946_659_600);
    std::fs::File::open(&file)
        .expect("open file")
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(old)
                .set_modified(old),
        )
        .expect("set old timestamps");

    let output = cerf()
        .args(["-c", &format!("touch '{}'", file.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    let modified = std::fs::metadata(&file)
        .expect("metadata")
        .modified()
        .expect("mtime");
    assert!(modified > old, "touch did not update the modification time");

    // Directories must work without opening them for writing.
    let dir = directory.path().join("d");
    std::fs::create_dir(&dir).expect("create dir");
    let output = cerf()
        .args(["-c", &format!("touch '{}'", dir.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");

    // A FIFO must not block waiting for a reader.
    let fifo = directory.path().join("p");
    if Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|status| status.success())
    {
        let output = Command::new("timeout")
            .arg("5")
            .arg(env!("CARGO_BIN_EXE_cerf"))
            .args(["-c", &format!("touch '{}'", fifo.display())])
            .output();
        if let Ok(output) = output
            && output.status.code() != Some(127)
        {
            assert!(output.status.success(), "{output:?}");
        }
    }
}

#[cfg(unix)]
#[test]
fn rm_removes_dangling_and_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory");
    let dangling = directory.path().join("dangling");
    let real = directory.path().join("realdir");
    let link = directory.path().join("link");
    std::fs::create_dir(&real).expect("create real directory");
    symlink("/nonexistent-cerf-target", &dangling).expect("create dangling link");
    symlink(&real, &link).expect("create directory link");

    let command = format!("rm '{}' '{}'", dangling.display(), link.display());
    let output = cerf().args(["-c", &command]).output().expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert!(std::fs::symlink_metadata(&dangling).is_err());
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert!(real.exists(), "the symlink target must survive");
}

#[cfg(unix)]
#[test]
fn non_utf8_environment_and_arguments_do_not_panic() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let output = cerf()
        .env(
            OsString::from_vec(b"CERF_BAD_VALUE".to_vec()),
            OsString::from_vec(b"\xff\xfe".to_vec()),
        )
        .args(["-c", "io.echo ok"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");

    let output = cerf()
        .arg("-c")
        .arg(OsString::from_vec(b"io.echo \xff".to_vec()))
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert!(output.status.code() != Some(101));
}

#[test]
fn partially_quoted_words_are_not_glob_expanded() {
    let directory = tempfile::tempdir().expect("temporary directory");
    std::fs::write(directory.path().join("file[1].txt"), "").expect("write bracket file");
    std::fs::write(directory.path().join("file1.txt"), "").expect("write plain file");

    let output = cerf()
        .args(["-c", "io.echo \"file[1]\".txt"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "file[1].txt"
    );

    let output = cerf()
        .args(["-c", "io.echo \"*\".txt"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "*.txt");
}

#[cfg(unix)]
#[test]
fn descriptor_redirects_and_duplication_work() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let two = directory.path().join("two.txt");
    let output = cerf()
        .args(["-c", &format!("/bin/echo 2>'{}'", two.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&two).unwrap(), "");

    let all = directory.path().join("all.txt");
    let output = cerf()
        .args([
            "-c",
            &format!(
                "/bin/sh -c 'echo out; echo err >&2' >'{}' 2>&1",
                all.display()
            ),
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&all).unwrap(), "out\nerr\n");

    let first = directory.path().join("first.txt");
    let second = directory.path().join("second.txt");
    let output = cerf()
        .args([
            "-c",
            &format!(
                "/bin/echo hi >'{}' >'{}'",
                first.display(),
                second.display()
            ),
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "hi\n");
}

#[cfg(unix)]
#[test]
fn home_expansion_does_not_collapse_parent_through_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir_all(directory.path().join("real/sub")).expect("create directories");
    symlink("real/sub", directory.path().join("link")).expect("create symlink");

    let output = cerf()
        .args(["-c", "/bin/echo hi > link/../cerf.txt"])
        .current_dir(directory.path())
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("real/cerf.txt").exists());
    assert!(!directory.path().join("cerf.txt").exists());
}

#[test]
fn positional_parameters_and_shift_work() {
    let output = cerf()
        .args(["-c", "set -- a b; shift; io.echo \"$1:$#:$*\""])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "b:1:b\n");

    let output = cerf()
        .args(["-c", "set -- a b; io.echo \"$1\""])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a\n");

    let output = cerf()
        .args(["-c", "set -- a b; shift 0; io.echo \"$1:$#\""])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a:2\n");
}

#[test]
fn function_calls_keep_arguments_redirects_and_pipelines() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let out = directory.path().join("out.txt");
    let output = cerf()
        .args([
            "-c",
            &format!("func f {{ io.echo hi }}; f >'{}'", out.display()),
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "hi\n");

    let output = cerf()
        .args(["-c", "func g { io.echo \"$1:$#\" }; g one two"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "one:2\n");

    let output = cerf()
        .args(["-c", "func h { io.echo hi }; h | cat"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hi\n");
}

#[test]
fn prefix_assignments_apply_to_in_process_builtins() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = cerf()
        .args(["-c", "IFS=: read X Y; io.echo \"$X|$Y\""])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn cerf");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"a:b\n")
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a|b\n");
}

#[cfg(unix)]
#[test]
fn background_control_flow_blocks_return_immediately() {
    use std::time::{Duration, Instant};

    for script in [
        "if true { /bin/sleep 2 } &",
        "for x in a b { /bin/sleep 2 } &",
        "while true { /bin/sleep 2 } &",
        "loop { /bin/sleep 2 } &",
        "func f { /bin/sleep 2 }; f &",
    ] {
        // A background child inherits stdout and keeps a pipe open until it
        // exits, so `Command::output()` would wait for it. Redirect to a file
        // and watch the shell process itself.
        let directory = tempfile::tempdir().expect("temporary directory");
        let stdout_path = directory.path().join("stdout.txt");
        let stdout = std::fs::File::create(&stdout_path).expect("create stdout file");
        let started = Instant::now();
        let status = cerf()
            .args(["-c", script])
            .stdout(std::process::Stdio::from(stdout))
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run cerf");
        assert!(status.success(), "{script}: {status:?}");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{script}: ran synchronously ({:?})",
            started.elapsed()
        );
        let stdout = std::fs::read_to_string(&stdout_path).expect("read stdout");
        assert!(
            stdout.contains("[1] "),
            "{script}: no job registration: {stdout:?}"
        );
    }
}

#[test]
fn readonly_and_export_attributes_are_consistent() {
    let output = cerf()
        .args(["-c", "declare -r R=1"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");

    let output = cerf()
        .args(["-c", "declare -r R=1; R=2; io.echo $R"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("readonly"));

    let output = cerf()
        .args(["-c", "declare -r R=1; unset R; io.echo $R"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("readonly"));

    // A lone readonly assignment is a failing command.
    let output = cerf()
        .args(["-c", "declare -r R=1; R=2"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let output = cerf()
        .args(["-c", "declare -r R=1; unset R"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(1), "{output:?}");

    let output = cerf()
        .args(["-c", "export FOO=one; declare FOO=two; printenv FOO"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "two\n");

    let output = cerf()
        .args(["-c", "export FOO=one; FOO=two; printenv FOO"])
        .output()
        .expect("run cerf");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "two\n");
}

#[test]
fn invalid_environment_names_report_errors_instead_of_panicking() {
    for script in ["declare -x =foo", "unset a=b", "export =foo"] {
        let output = cerf().args(["-c", script]).output().expect("run cerf");
        assert_eq!(output.status.code(), Some(1), "{script}: {output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "{script}: {output:?}"
        );
    }
}

#[test]
fn exit_uses_its_numeric_argument_and_last_status() {
    let output = cerf().args(["-c", "exit 5"]).output().expect("run cerf");
    assert_eq!(output.status.code(), Some(5));

    let output = cerf()
        .args(["-c", "false; exit"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(1));

    let output = cerf()
        .args(["-c", "exec; io.echo after"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "after\n");
}

#[cfg(unix)]
#[test]
fn stale_current_job_does_not_break_the_next_bare_fg() {
    let output = cerf()
        .args(["-c", "/bin/sleep 0.1 & fg; /bin/sleep 0.1 & fg"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[1] "), "{stdout:?}");
    assert!(stdout.contains("[2] "), "{stdout:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn builtins_report_write_errors_instead_of_panicking() {
    use std::fs::File;
    use std::process::Stdio;

    if !std::path::Path::new("/dev/full").exists() {
        return;
    }
    for script in ["io.echo hi", "fs.df", "fs.stat /etc/passwd"] {
        let sink = File::create("/dev/full").expect("open /dev/full");
        let output = cerf()
            .args(["-c", script])
            .stdout(Stdio::from(sink))
            .output()
            .expect("run cerf");
        assert_eq!(output.status.code(), Some(1), "{script}: {output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "{script}: {output:?}"
        );
    }
}

#[test]
fn internal_builtin_snapshot_does_not_delete_an_unrelated_file() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let victim = directory.path().join("victim.txt");
    std::fs::write(&victim, "keep\n").expect("write victim");

    let output = cerf()
        .env("CERF_INTERNAL_STATE", &victim)
        .args(["--internal-builtin", "io.echo", "hi"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(&victim).expect("victim should survive"),
        "keep\n"
    );
}

#[cfg(unix)]
#[test]
fn sys_command_runs_external_commands_with_their_status() {
    let output = cerf()
        .args(["-c", "sys.command /bin/sh -c 'exit 7'"])
        .output()
        .expect("run cerf");
    assert_eq!(output.status.code(), Some(7), "{output:?}");
}

#[test]
fn script_mode_passes_extra_operands_as_positional_parameters() {
    let output = cerf()
        .args(["-c", "io.echo \"$1:$2:$#\"", "one", "two"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "status: {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "one:two:2\n");
}

#[test]
fn malformed_invocations_report_usage_errors() {
    let missing_script = cerf().arg("-c").output().expect("run cerf");
    assert_eq!(missing_script.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&missing_script.stderr).contains("-c requires a script"),
        "stderr: {:?}",
        String::from_utf8_lossy(&missing_script.stderr)
    );

    let extra_version = cerf()
        .args(["--version", "extra"])
        .output()
        .expect("run cerf");
    assert_eq!(extra_version.status.code(), Some(2));

    let unknown = cerf().arg("--bogus").output().expect("run cerf");
    assert_eq!(unknown.status.code(), Some(2));

    let version = cerf().arg("--version").output().expect("run cerf");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with('v'));
}

#[test]
fn local_variables_stay_inside_functions() {
    let output = cerf()
        .args([
            "-c",
            "func f { local x=1; io.echo \"in:$x\" }; f; io.echo \"out:${x}\"",
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "status: {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "in:1\nout:\n");
}

#[test]
fn local_outside_a_function_is_an_error() {
    let output = cerf().args(["-c", "local x=1"]).output().expect("run cerf");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("can only be used in a function"),
        "stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn source_and_eval_report_syntax_errors() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let bad = directory.path().join("bad.cerf");
    std::fs::write(&bad, "bad |\n").expect("write script");

    let sourced = cerf()
        .args(["-c", &format!("source '{}'", bad.display())])
        .output()
        .expect("run cerf");
    assert_eq!(sourced.status.code(), Some(2));

    let evaluated = cerf()
        .args(["-c", "eval 'if {'"])
        .output()
        .expect("run cerf");
    assert_eq!(evaluated.status.code(), Some(2));

    // Blank/comment-only input remains a successful no-op.
    let blank = cerf()
        .args(["-c", "eval '# just a comment'"])
        .output()
        .expect("run cerf");
    assert!(blank.status.success());
}

#[test]
fn alias_values_understand_double_quotes() {
    let output = cerf()
        .args(["-c", "alias q='io.echo \"a b\"'; q"])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "status: {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a b\n");
}

#[test]
fn printf_reuses_the_format_and_expands_b() {
    let output = cerf()
        .args(["-c", "printf '[%s]\\n' a b c"])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "[a]\n[b]\n[c]\n");

    let output = cerf()
        .args(["-c", "printf '%b\\n' 'a\\tb'"])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a\tb\n");

    let output = cerf()
        .args(["-c", "printf '%c %.2f %5s\\n' hello 1.5 ab"])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "h 1.50    ab\n");
}

#[test]
fn type_exits_nonzero_for_missing_names() {
    let missing = cerf()
        .args(["-c", "type cerf-definitely-missing-command"])
        .output()
        .expect("run cerf");
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("not found"),
        "stderr: {:?}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let found = cerf()
        .args(["-c", "type io.echo"])
        .output()
        .expect("run cerf");
    assert!(found.status.success());

    // One missing name makes the whole invocation fail, as in POSIX.
    let mixed = cerf()
        .args(["-c", "type io.echo cerf-definitely-missing-command"])
        .output()
        .expect("run cerf");
    assert_eq!(mixed.status.code(), Some(1));
}

#[test]
fn help_exits_zero_when_help_was_printed() {
    let builtin = cerf()
        .args(["-c", "help io.echo"])
        .output()
        .expect("run cerf");
    assert!(builtin.status.success(), "status: {:?}", builtin.status);

    let missing = cerf()
        .args(["-c", "help cerf-definitely-missing-command"])
        .output()
        .expect("run cerf");
    assert_eq!(missing.status.code(), Some(127));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no help topics match"),
        "stderr: {:?}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[test]
fn pushd_and_popd_refresh_pwd() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir(&first).expect("first directory");
    std::fs::create_dir(&second).expect("second directory");
    let first = std::fs::canonicalize(first).expect("canonical first");
    let second = std::fs::canonicalize(second).expect("canonical second");

    let output = cerf()
        .args([
            "-c",
            &format!(
                "cd '{}'; pushd '{}'; io.echo \"$PWD\"; popd; io.echo \"$PWD\"",
                first.display(),
                second.display()
            ),
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "status: {:?}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.contains(&second.to_string_lossy().as_ref()),
        "missing new PWD in {stdout:?}"
    );
    assert_eq!(
        lines.last().copied(),
        Some(first.to_string_lossy().as_ref()),
        "popd did not restore PWD: {stdout:?}"
    );
}

#[cfg(unix)]
#[test]
fn du_reports_allocated_blocks_and_counts_hard_links_once() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let sparse = directory.path().join("sparse.bin");
    let sparse_file = std::fs::File::create(&sparse).expect("create sparse file");
    sparse_file.set_len(1024 * 1024 * 1024).expect("size file");
    drop(sparse_file);

    let output = cerf()
        .args(["-c", &format!("fs.du '{}'", sparse.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_size: u64 = stdout
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .and_then(|size| size.parse().ok())
        .expect("file size");
    assert!(
        first_size < 1024,
        "sparse file should use allocated blocks, got {stdout:?}"
    );

    let first = directory.path().join("a");
    let second = directory.path().join("b");
    std::fs::write(&first, vec![b'x'; 4096]).expect("write first");
    std::fs::hard_link(&first, &second).expect("hard link");

    let output = cerf()
        .args([
            "-c",
            &format!("fs.du '{}' '{}'", first.display(), second.display()),
        ])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let file_lines = stdout
        .lines()
        .filter(|line| !line.ends_with("\ttotal"))
        .count();
    assert_eq!(file_lines, 1, "hard link counted twice: {stdout:?}");
}

#[cfg(unix)]
#[test]
fn stat_reports_symlinks_without_following_them() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let real = directory.path().join("real.txt");
    let link = directory.path().join("slink");
    let dangling = directory.path().join("dangling");
    std::fs::write(&real, b"hi").expect("write real file");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    std::os::unix::fs::symlink("/cerf-definitely-missing", &dangling).expect("dangling symlink");

    let output = cerf()
        .args(["-c", &format!("stat '{}'", link.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("symbolic link"), "stdout: {stdout:?}");
    assert!(stdout.contains("slink ->"), "stdout: {stdout:?}");

    let output = cerf()
        .args(["-c", &format!("stat '{}'", dangling.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "dangling link should be stat-able");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("symbolic link"),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let output = cerf()
        .args(["-c", &format!("stat -L '{}'", link.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("regular file"),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[cfg(unix)]
#[test]
fn ls_handles_symlinks_without_following_them() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let real = directory.path().join("realdir");
    std::fs::create_dir(&real).expect("real directory");
    std::fs::write(real.join("x.txt"), b"x").expect("file in real directory");
    let link = directory.path().join("link");
    let dangling = directory.path().join("dangling");
    std::os::unix::fs::symlink(&real, &link).expect("symlink to directory");
    std::os::unix::fs::symlink("/cerf-definitely-missing", &dangling).expect("dangling symlink");

    let output = cerf()
        .args(["-c", &format!("ls '{}'", dangling.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("dangling"));

    let output = cerf()
        .args(["-c", &format!("ls -l '{}'", link.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(" -> "), "stdout: {stdout:?}");
    assert!(!stdout.contains("x.txt"), "stdout: {stdout:?}");

    let output = cerf()
        .args(["-c", &format!("ls -F '{}'", link.display())])
        .output()
        .expect("run cerf");
    assert!(String::from_utf8_lossy(&output.stdout).contains("link@"));

    // A symlink loop must not be followed by -R.
    let top = directory.path().join("up");
    let nested = top.join("a");
    std::fs::create_dir_all(&nested).expect("nested directory");
    std::fs::write(nested.join("x.txt"), b"x").expect("nested file");
    std::os::unix::fs::symlink(&top, nested.join("up")).expect("loop symlink");
    let output = cerf()
        .args(["-c", &format!("ls -R '{}'", top.display())])
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("up/up"),
        "recursion followed a symlink: {stdout:?}"
    );
}

#[cfg(unix)]
#[test]
fn path_search_skips_non_executable_files() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let bad = directory.path().join("bad");
    let good = directory.path().join("good");
    std::fs::create_dir(&bad).expect("bad directory");
    std::fs::create_dir(&good).expect("good directory");
    std::fs::write(bad.join("prog"), "not executable\n").expect("bad program");
    let program = good.join("prog");
    std::fs::write(&program, "#!/bin/sh\necho GOOD\n").expect("good program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make executable");

    let path = format!("{}:{}", bad.display(), good.display());
    let output = cerf()
        .args(["-c", "prog"])
        .env("PATH", path)
        .output()
        .expect("run cerf");
    assert!(output.status.success(), "status: {:?}", output.status);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "GOOD\n");
}

#[cfg(unix)]
#[test]
fn tilde_expands_for_arguments_assignments_and_globs() {
    let home = tempfile::tempdir().expect("temporary home");
    std::fs::write(home.path().join("visible.txt"), b"x").expect("visible file");
    let visible = std::fs::canonicalize(home.path().join("visible.txt")).expect("canonical file");

    let output = cerf()
        .args(["-c", "/bin/echo ~"])
        .env("HOME", home.path())
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        home.path().canonicalize().unwrap().to_string_lossy()
    );

    // Globbing happens after tilde expansion.
    let output = cerf()
        .args(["-c", "io.echo ~/visible*"])
        .env("HOME", home.path())
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        visible.to_string_lossy()
    );

    // Assignments expand a leading tilde too.
    let output = cerf()
        .args(["-c", "X=~/visible.txt; io.echo \"$X\""])
        .env("HOME", home.path())
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        visible.to_string_lossy()
    );

    // Quoted tildes stay literal.
    let output = cerf()
        .args(["-c", "io.echo \"~\" '~'"])
        .env("HOME", home.path())
        .output()
        .expect("run cerf");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "~ ~\n");
}

#[test]
fn huge_timeouts_are_rejected_without_panicking() {
    let huge = "18446744073709551615";
    for command in [
        format!("net.wfp 127.0.0.1 80 --timeout {huge}"),
        format!("net.socket 127.0.0.1 80 --connect-timeout {huge}"),
        format!("net.socket --udp 127.0.0.1 80 --idle-timeout {huge}"),
    ] {
        let output = cerf().args(["-c", &command]).output().expect("run cerf");
        assert_eq!(
            output.status.code(),
            Some(1),
            "{command} exited with {:?}",
            output.status
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("too large"),
            "{command} stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
