use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};

pub const COMMAND_INFO_COMMAND: CommandInfo = CommandInfo {
    name: "sys.command",
    description: "Run a simple command ignoring aliases.",
    usage: "sys.command [arg ...]\n\nRuns a command ignoring shell functions and aliases.",
    run: command_runner,
};

pub fn command_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.is_empty() {
        return (ExecutionResult::KeepRunning, 0);
    }

    let name = &args[0];

    if crate::builtins::registry::is_process_isolated(name) {
        return crate::builtins::registry::run_isolated(name, &args[1..], state);
    }

    if let Some(cmd_info) = crate::builtins::registry::find_command(name) {
        return (cmd_info.run)(&args[1..], state);
    }

    // External executable: use the engine's normal spawn path so it gets
    // default signal dispositions, its own process group, and terminal
    // hand-off (Ctrl-C/Ctrl-Z work like an ordinary command).
    crate::engine::execute_external(name, &args[1..], state)
}
