use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState, is_valid_name};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "env.unset",
    description: "Unset values and attributes of shell variables and functions.",
    usage: "env.unset [-f] [-v] [-n] [name ...]\n\nUnset values and attributes of shell variables and functions.",
    run: unset_runner,
};

pub fn unset_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = run(args, state);
    (ExecutionResult::KeepRunning, code)
}

/// Run the `unset` builtin.
///
/// Behaviour:
/// - `unset name …` → remove each named variable from shell and environment
pub fn run(args: &[String], state: &mut ShellState) -> i32 {
    if args.is_empty() {
        return 0;
    }

    let mut exit_code = 0;
    for arg in args {
        // `std::env::remove_var` panics for empty names and names containing
        // `=`, so reject invalid identifiers before touching the environment.
        if !is_valid_name(arg) {
            eprintln!("cerf: unset: '{}': not a valid identifier", arg);
            exit_code = 1;
            continue;
        }

        if state.get_var(arg).is_some_and(|var| var.readonly) {
            eprintln!("cerf: unset: {}: readonly variable", arg);
            exit_code = 1;
            continue;
        }

        // Bash allows 'unset' to fail silently if the variable doesn't exist.
        // It also removes it from the environment and any local scope.
        state.variables.remove(arg);
        for scope in &mut state.scopes {
            scope.remove(arg);
        }
        unsafe {
            std::env::remove_var(arg);
        }
    }

    exit_code
}
