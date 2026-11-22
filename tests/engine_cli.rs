use std::process::Command;

fn cerf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cerf"))
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
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "foreground\nbackground\n"
    );
}
