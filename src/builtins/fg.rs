use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "job.fg",
    description: "Move job to the foreground.",
    usage: "job.fg [job_spec]\n\nPlace the job identified by JOB_SPEC in the foreground, making it the current job.",
    run: fg_runner,
};

pub fn fg_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = run(args, state);
    (ExecutionResult::KeepRunning, code)
}

pub fn run(args: &[String], state: &mut ShellState) -> i32 {
    let mut job_id = None;
    if args.is_empty() {
        if let Ok(id) = crate::engine::job_control::resolve_job_specifier("%+", state) {
            job_id = Some(id);
        } else if let Some(&id) = state.jobs.keys().max() {
            job_id = Some(id);
        }
    } else if let Ok(id) = crate::engine::job_control::resolve_job_specifier(&args[0], state) {
        job_id = Some(id);
    } else {
        eprintln!(
            "cerf: fg: {}",
            crate::engine::job_control::resolve_job_specifier(&args[0], state).unwrap_err()
        );
        return 1;
    }

    let Some(id) = job_id else {
        eprintln!("cerf: fg: current: no such job");
        return 1;
    };

    {
        let Some(job) = state.jobs.get_mut(&id) else {
            eprintln!("cerf: fg: %{}: no such job", id);
            return 1;
        };
        println!("{}", job.command);
        // The job is about to run again: clear Stopped before waiting, or
        // wait_for_job returns immediately and leaves it untracked.
        crate::engine::job_control::resume_job(job);

        #[cfg(unix)]
        {
            let pgid = job.pgid;
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-(pgid as i32)),
                nix::sys::signal::Signal::SIGCONT,
            );
        }

        #[cfg(windows)]
        {
            let pids = crate::builtins::kill_cmd::get_job_pids(job.job_handle);
            for pid in pids {
                crate::builtins::kill_cmd::suspend_or_resume_process_win(pid, false);
            }
        }
    }

    crate::engine::job_control::set_current_job(state, id);
    crate::engine::job_control::wait_for_job(id, state, true)
}
