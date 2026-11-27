use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};
use std::io::Write;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "io.echo",
    description: "Write arguments to the standard output.",
    usage: "io.echo [arg ...]",
    run,
};

pub fn run(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{}", args.join(" ")) {
        // `println!` panics on EPIPE; a builtin running in-process must
        // report a status instead of taking the shell down.
        eprintln!("cerf: io.echo: write error: {}", error);
        return (ExecutionResult::KeepRunning, 1);
    }
    (ExecutionResult::KeepRunning, 0)
}
