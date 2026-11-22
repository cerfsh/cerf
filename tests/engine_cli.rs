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
    for script in ["", "   ", "# comment"] {
        let output = cerf().args(["-c", script]).output().expect("run cerf");
        assert_eq!(output.status.code(), Some(0), "script was {script:?}");
    }

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
