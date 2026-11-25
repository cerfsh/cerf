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
