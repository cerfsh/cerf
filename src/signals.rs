#[cfg(unix)]
use nix::sys::signal::{SigHandler, Signal, signal};

/// Initialize shell signal handlers
#[cfg(unix)]
pub fn init() {
    unsafe {
        // We ignore SIGINT, SIGQUIT, etc. so the shell doesn't exit when these signals are sent
        // to the process group (e.g. via Ctrl+C, Ctrl+\).
        // Note: Rustyline will override SIGINT during readline() calls, which is fine.
        signal(Signal::SIGINT, SigHandler::SigIgn).expect("Failed to ignore SIGINT");
        signal(Signal::SIGQUIT, SigHandler::SigIgn).expect("Failed to ignore SIGQUIT");
        signal(Signal::SIGTSTP, SigHandler::SigIgn).expect("Failed to ignore SIGTSTP");
        signal(Signal::SIGTTIN, SigHandler::SigIgn).expect("Failed to ignore SIGTTIN");
        signal(Signal::SIGTTOU, SigHandler::SigIgn).expect("Failed to ignore SIGTTOU");
    }
}

/// Ignore SIGTTOU so a child can change the terminal's foreground process
/// group without being stopped, even when the parent shell did not install its
/// handlers (for example `cerf -c`). Restored by [`restore_default`] before
/// `exec`.
#[cfg(unix)]
pub fn ignore_sigttou() {
    unsafe {
        signal(Signal::SIGTTOU, SigHandler::SigIgn).expect("Failed to ignore SIGTTOU");
    }
}

/// Restore default signal handlers (for child processes)
#[cfg(unix)]
pub fn restore_default() {
    unsafe {
        signal(Signal::SIGINT, SigHandler::SigDfl).expect("Failed to restore SIGINT");
        signal(Signal::SIGQUIT, SigHandler::SigDfl).expect("Failed to restore SIGQUIT");
        signal(Signal::SIGTSTP, SigHandler::SigDfl).expect("Failed to restore SIGTSTP");
        signal(Signal::SIGTTIN, SigHandler::SigDfl).expect("Failed to restore SIGTTIN");
        signal(Signal::SIGTTOU, SigHandler::SigDfl).expect("Failed to restore SIGTTOU");
    }
}

#[cfg(windows)]
use std::sync::atomic::Ordering;
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
};

/// Forward Ctrl-C/Ctrl-Break to the foreground job's process group instead of
/// letting the default console handler terminate the shell itself.
///
/// Windows has no `SIGINT` to forward to an arbitrary group: a `CTRL_C_EVENT`
/// cannot target another process group, so the interrupt is delivered as
/// `CTRL_BREAK_EVENT`, which every console application treats as "terminate".
#[cfg(windows)]
unsafe extern "system" fn console_ctrl_handler(event: u32) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            let group = crate::FG_JOB.load(Ordering::Relaxed);
            if group != 0 {
                unsafe {
                    GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group as u32);
                }
            }
            // The shell itself must survive; rustyline handles Ctrl-C at the
            // prompt and the foreground job received its own event.
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => 0,
        _ => 0,
    }
}

#[cfg(windows)]
pub fn init() {
    unsafe {
        SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
    }
}

#[cfg(windows)]
#[allow(dead_code)]
pub fn restore_default() {
    // No-op on Windows
}
