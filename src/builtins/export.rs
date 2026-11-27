use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState, Variable, is_valid_name};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "env.export",
    description: "Set export attribute for shell variables.",
    usage: "env.export [name[=value] ...]\n\nMarks each NAME for automatic export to the environment of subsequently executed commands. If VALUE is supplied, assign VALUE before exporting.",
    run: export_runner,
};

pub fn export_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = run(args, state);
    (ExecutionResult::KeepRunning, code)
}

/// Run the `export` builtin.
///
/// Behaviour:
/// - `export`             → print all environment variables
/// - `export name=value`  → set variable in both shell and environment
/// - `export name`        → promote existing shell variable to environment
pub fn run(args: &[String], state: &mut ShellState) -> i32 {
    if args.is_empty() {
        let mut pairs: Vec<(String, String)> = std::env::vars_os()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect();
        pairs.sort_by_key(|(k, _)| k.clone());
        for (name, value) in pairs {
            println!("export {}='{}'", name, value);
        }
        return 0;
    }

    let mut exit_code = 0;
    for arg in args {
        if let Some(eq_pos) = arg.find('=') {
            // Assignment: name=value
            let name = &arg[..eq_pos];
            let value = &arg[eq_pos + 1..];
            if !is_valid_name(name) {
                eprintln!("cerf: export: '{}': not a valid identifier", arg);
                exit_code = 1;
                continue;
            }

            if state.get_var(name).is_some_and(|var| var.readonly) {
                eprintln!("cerf: export: {}: readonly variable", name);
                exit_code = 1;
                continue;
            }

            // Exporting creates a shell variable carrying the export
            // attribute. Previously `export FOO=one` inserted a variable with
            // `exported = false`, so later assignments only updated the shell
            // copy and `printenv FOO` stayed stale.
            let mut var = state
                .get_var(name)
                .cloned()
                .unwrap_or_else(|| Variable::new_string(String::new()));
            var.value = crate::engine::state::VarValue::String(value.to_string());
            var.exported = true;
            state.set_var(name, var);
            unsafe {
                std::env::set_var(name, value);
            }
        } else {
            if !is_valid_name(arg) {
                eprintln!("cerf: export: '{}': not a valid identifier", arg);
                exit_code = 1;
                continue;
            }

            // Export existing: promote existing shell variable to env.
            match state.get_var_mut(arg) {
                Some(var) => {
                    var.exported = true;
                    let value = var.value.as_string();
                    unsafe {
                        std::env::set_var(arg, value);
                    }
                }
                None => {
                    // Bash allows `export FOO` before assigning FOO. Keep an
                    // empty exported shell variable so a later assignment
                    // updates the environment.
                    let mut var = Variable::new_string(String::new());
                    var.exported = true;
                    state.set_var(arg, var);
                }
            }
        }
    }

    exit_code
}
