use crate::builtins::registry::CommandInfo;
use crate::engine::job_control::{resolve_job_specifier, wait_for_job, wait_for_pid};
use crate::engine::state::{ExecutionResult, ShellState};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "job.wait",
    description: "Wait for job completion and return exit status.",
    usage: "job.wait [pid | jobspec ...]\n\nWait for the specified process or job and return its termination status.",
    run: wait_runner,
};

pub fn wait_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = run(args, state);
    (ExecutionResult::KeepRunning, code)
}

pub fn run(args: &[String], state: &mut ShellState) -> i32 {
    if args.is_empty() {
        // Wait for every job, then return success (POSIX).
        let mut job_ids: Vec<usize> = state.jobs.keys().copied().collect();
        job_ids.sort_unstable();
        for id in job_ids {
            wait_for_job(id, state, false);
        }
        return 0;
    }

    let mut last_code = 0;
    for arg in args {
        last_code = wait_for_operand(arg, state);
    }
    last_code
}

fn wait_for_operand(arg: &str, state: &mut ShellState) -> i32 {
    if arg.starts_with('%') {
        match resolve_job_specifier(arg, state) {
            Ok(id) if state.jobs.contains_key(&id) => wait_for_job(id, state, false),
            Ok(id) => {
                eprintln!("cerf: wait: %{}: no such job", id);
                127
            }
            Err(error) => {
                eprintln!("cerf: wait: {}", error);
                1
            }
        }
    } else if let Ok(pid) = arg.parse::<i32>() {
        // A bare number is a process ID, not a job number.
        wait_for_pid(pid, state)
    } else {
        eprintln!("cerf: wait: '{}': not a pid or valid job spec", arg);
        1
    }
}
