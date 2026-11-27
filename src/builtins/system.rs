use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};
use std::path::PathBuf;

pub const COMMAND_INFO_EXIT: CommandInfo = CommandInfo {
    name: "sys.exit",
    description: "Exit the shell.",
    usage: "sys.exit [n]\n\nExit the shell with status N, or with the status of the last command.",
    run: exit_runner,
};

pub fn exit_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = match args.first() {
        None => state.last_status,
        Some(arg) => match arg.parse::<i64>() {
            Ok(code) => (code & 0xff) as i32,
            Err(_) => {
                eprintln!("cerf: exit: {}: numeric argument required", arg);
                2
            }
        },
    };
    (ExecutionResult::Exit, code)
}

pub const COMMAND_INFO_CLEAR: CommandInfo = CommandInfo {
    name: "sys.clear",
    description: "Clear the terminal screen.",
    usage: "sys.clear\n\nClear the terminal screen.",
    run: clear_runner,
};

pub fn clear_runner(_args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    clear();
    (ExecutionResult::KeepRunning, 0)
}

pub const COMMAND_INFO_EXEC: CommandInfo = CommandInfo {
    name: "sys.exec",
    description: "Replace the shell with the given command.",
    usage: "sys.exec [command [arguments ...]]\n\nReplace the shell with the given command.",
    run: exec_runner,
};

pub fn exec_runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    // POSIX: `exec` with no operands is a no-op, not a request to exit.
    if args.is_empty() {
        return (ExecutionResult::KeepRunning, 0);
    }

    match exec(args) {
        Ok(code) => (ExecutionResult::Exit, code),
        Err(e) => {
            eprintln!("{}", e);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}
use std::process::Command;

use crate::engine::path::{expand_home, find_executable};

pub fn clear() {
    use std::io::Write;
    print!("\x1B[2J\x1B[3J\x1B[H");
    let _ = std::io::stdout().flush();
}

/// Run the `exec` built-in.
///
/// Replaces the current shell process with the given command.
/// - On **Unix** this calls `execvp` and never returns on success.
/// - On **Windows** there is no real `exec`, so we spawn the process, wait for
///   it, and return its exit code; the caller should exit the shell.
///
/// If no command is given, `exec` is a no-op (returns success).
pub fn exec(args: &[String]) -> Result<i32, String> {
    if args.is_empty() {
        // No command — just succeed (POSIX: `exec` with no args is a no-op).
        return Ok(0);
    }

    let cmd_name = &args[0];
    let cmd_args = &args[1..];

    let resolved: PathBuf = find_executable(cmd_name).unwrap_or_else(|| expand_home(cmd_name));

    // ── Unix: true exec (replaces the process image) ─────────────────
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Ignored dispositions survive `exec`, so an interactive shell would
        // otherwise hand SIGINT/SIGQUIT/SIGTSTP/etc. over as ignored.
        crate::signals::restore_default();
        let err = Command::new(&resolved).args(cmd_args).exec(); // never returns on success

        // `exec` failed: restore the shell's own handlers before continuing.
        crate::signals::init();
        Err(format!("cerf: exec: {}: {}", cmd_name, err))
    }

    // ── Windows: spawn + exit (best-effort emulation) ────────────────
    #[cfg(windows)]
    {
        let is_batch = resolved.extension().is_some_and(|e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "cmd" || e == "bat"
        });

        let mut command = if is_batch {
            let mut c = Command::new("cmd");
            c.arg("/c").arg(&resolved);
            c
        } else {
            Command::new(&resolved)
        };

        command.args(cmd_args);
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
        }

        match command.spawn() {
            Ok(mut child) => {
                crate::FG_JOB.store(child.id() as usize, std::sync::atomic::Ordering::Relaxed);
                let code = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
                crate::FG_JOB.store(0, std::sync::atomic::Ordering::Relaxed);
                Ok(code)
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Err(format!("cerf: exec: {}: command not found", cmd_name))
                } else {
                    Err(format!("cerf: exec: {}: {}", cmd_name, e))
                }
            }
        }
    }
}
