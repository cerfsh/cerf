use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "job.kill",
    description: "Send a signal to a job.",
    usage: "job.kill [-s sigspec | -n signum | -sigspec] pid | jobspec ... or job.kill -l\n\nSend the processes identified by PID or JOBSPEC the signal named by SIGSPEC or SIGNUM. If no signal is specified, SIGTERM is assumed.",
    run: kill_runner,
};

pub fn kill_runner(args: &[String], state: &mut ShellState) -> (ExecutionResult, i32) {
    let code = run(args, state);
    (ExecutionResult::KeepRunning, code)
}

const SIGNAL_NAMES: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("STKFLT", 16),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
    ("URG", 23),
    ("XCPU", 24),
    ("XFSZ", 25),
    ("VTALRM", 26),
    ("PROF", 27),
    ("WINCH", 28),
    ("IO", 29),
    ("PWR", 30),
    ("SYS", 31),
];

/// Parse a signal name (`TERM`, `SIGTERM`, `term`) or number (`15`).
fn parse_signal(spec: &str) -> Option<i32> {
    let name = spec
        .strip_prefix("SIG")
        .or_else(|| spec.strip_prefix("sig"))
        .unwrap_or(spec);
    if let Ok(number) = name.parse::<i32>() {
        // 0 is the "check only" signal; 1..=64 covers the usual range.
        return (0..=64).contains(&number).then_some(number);
    }
    let upper = name.to_ascii_uppercase();
    SIGNAL_NAMES
        .iter()
        .find(|(signal, _)| *signal == upper)
        .map(|(_, number)| *number)
}

fn signal_name(number: i32) -> Option<&'static str> {
    SIGNAL_NAMES
        .iter()
        .find(|(_, value)| *value == number)
        .map(|(name, _)| *name)
}

fn list_signals(args: &[String]) -> i32 {
    if args.is_empty() {
        let names = SIGNAL_NAMES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(" ");
        println!("{}", names);
        return 0;
    }

    let mut code = 0;
    for arg in args {
        match parse_signal(arg) {
            Some(number) => match signal_name(number) {
                Some(name) => println!("{}", name),
                None => println!("{}", number),
            },
            None => {
                eprintln!("cerf: kill: {}: invalid signal specification", arg);
                code = 1;
            }
        }
    }
    code
}

/// Parse the optional signal option, returning the signal to send and the
/// first operand index.
fn parse_options(args: &[String]) -> Result<(i32, usize), String> {
    let mut signal = 15;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-s" || arg == "-n" {
            let spec = args
                .get(index + 1)
                .ok_or_else(|| format!("option requires an argument -- '{}'", arg))?;
            signal = parse_signal(spec)
                .ok_or_else(|| format!("{}: invalid signal specification", spec))?;
            index += 2;
            continue;
        }
        if let Some(spec) = arg.strip_prefix('-')
            && !spec.is_empty()
        {
            signal = parse_signal(spec)
                .ok_or_else(|| format!("{}: invalid signal specification", spec))?;
            index += 1;
            continue;
        }
        break;
    }
    Ok((signal, index))
}

pub fn run(args: &[String], state: &mut ShellState) -> i32 {
    if args.is_empty() {
        eprintln!("cerf: kill: usage: kill [-s sigspec] pid | jobspec ...");
        return 1;
    }

    if args[0] == "-l" || args[0] == "--list" {
        return list_signals(&args[1..]);
    }

    let (sig, first_operand) = match parse_options(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("cerf: kill: {}", error);
            return 1;
        }
    };
    let targets = &args[first_operand..];
    if targets.is_empty() {
        eprintln!("cerf: kill: usage: kill [-s sigspec] pid | jobspec ...");
        return 1;
    }

    let mut code = 0;

    #[cfg(unix)]
    {
        for target in targets {
            let mut pids_to_kill = Vec::new();

            if target.starts_with('%') {
                match crate::engine::job_control::resolve_job_specifier(target, state) {
                    Ok(id) => match state.jobs.get(&id) {
                        Some(job) if job.pgid != 0 => {
                            pids_to_kill.push(-(job.pgid as i32));
                        }
                        Some(_) => {
                            eprintln!("cerf: kill: {}: invalid process group", target);
                            code = 1;
                            continue;
                        }
                        None => {
                            eprintln!("cerf: kill: {}: no such job", target);
                            code = 1;
                            continue;
                        }
                    },
                    Err(error) => {
                        eprintln!("cerf: kill: {}", error);
                        code = 1;
                        continue;
                    }
                }
            } else if let Ok(pid) = target.parse::<i32>() {
                pids_to_kill.push(pid);
            } else {
                eprintln!(
                    "cerf: kill: {}: arguments must be process or job IDs",
                    target
                );
                code = 1;
                continue;
            }

            for pid in pids_to_kill {
                // SAFETY: plain libc call with a validated signal number.
                if unsafe { nix::libc::kill(pid, sig) } != 0 {
                    eprintln!(
                        "cerf: kill: ({}) - {}",
                        pid,
                        std::io::Error::last_os_error()
                    );
                    code = 1;
                }
            }
        }
    }

    #[cfg(windows)]
    {
        for target in targets {
            let mut job_handle_to_kill = None;
            let mut pids_to_kill = Vec::new();

            if target.starts_with('%') {
                match crate::engine::job_control::resolve_job_specifier(target, state) {
                    Ok(id) => match state.jobs.get(&id) {
                        Some(job) => {
                            job_handle_to_kill = Some(job.job_handle);
                            pids_to_kill.extend(job.processes.iter().map(|p| p.pid));
                        }
                        None => {
                            eprintln!("cerf: kill: {}: no such job", target);
                            code = 1;
                            continue;
                        }
                    },
                    Err(error) => {
                        eprintln!("cerf: kill: {}", error);
                        code = 1;
                        continue;
                    }
                }
            } else if let Ok(pid) = target.parse::<u32>() {
                pids_to_kill.push(pid);
            } else {
                eprintln!(
                    "cerf: kill: {}: arguments must be process or job IDs",
                    target
                );
                code = 1;
                continue;
            }

            if sig == 9 || sig == 15 || sig == 2 {
                if let Some(jh) = job_handle_to_kill {
                    unsafe {
                        windows_sys::Win32::System::JobObjects::TerminateJobObject(jh as _, 1);
                    }
                } else {
                    for pid in pids_to_kill {
                        unsafe {
                            let handle = windows_sys::Win32::System::Threading::OpenProcess(
                                windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                                0,
                                pid,
                            );
                            if !handle.is_null() {
                                windows_sys::Win32::System::Threading::TerminateProcess(handle, 1);
                                windows_sys::Win32::Foundation::CloseHandle(handle);
                            } else {
                                code = 1;
                            }
                        }
                    }
                }
            } else if sig == 18 || sig == 19 {
                let suspend = sig == 19;
                for pid in pids_to_kill {
                    crate::builtins::kill_cmd::suspend_or_resume_process_win(pid, suspend);
                }
            } else if sig != 0 {
                eprintln!(
                    "cerf: kill: signal {} is not supported on this platform",
                    sig
                );
                code = 1;
            }
        }
    }

    code
}

#[cfg(windows)]
pub fn suspend_or_resume_process_win(pid: u32, suspend: bool) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            let mut te32: THREADENTRY32 = std::mem::zeroed();
            te32.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

            if Thread32First(snapshot, &mut te32) != 0 {
                loop {
                    if te32.th32OwnerProcessID == pid {
                        let thread_handle = OpenThread(THREAD_SUSPEND_RESUME, 0, te32.th32ThreadID);
                        if !thread_handle.is_null() {
                            if suspend {
                                SuspendThread(thread_handle);
                            } else {
                                ResumeThread(thread_handle);
                            }
                            CloseHandle(thread_handle);
                        }
                    }
                    if Thread32Next(snapshot, &mut te32) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
        }
    }
}

#[cfg(windows)]
pub fn get_job_pids(job_handle: isize) -> Vec<u32> {
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
    };

    let mut pids = Vec::new();
    unsafe {
        // Initial buffer size, say 32 processes
        let mut buffer_size = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
            + std::mem::size_of::<usize>() * 32;
        let mut buffer: Vec<u8> = vec![0; buffer_size];

        let mut return_length = 0;
        let mut success = QueryInformationJobObject(
            job_handle as _,
            JobObjectBasicProcessIdList,
            buffer.as_mut_ptr() as *mut _,
            buffer_size as u32,
            &mut return_length,
        );

        let mut list_ptr = buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST;

        // If buffer was too small, reallocate and try again
        if success == 0
            && (*list_ptr).NumberOfAssignedProcesses > (*list_ptr).NumberOfProcessIdsInList
        {
            buffer_size = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + std::mem::size_of::<usize>() * (*list_ptr).NumberOfAssignedProcesses as usize;
            buffer = vec![0; buffer_size];
            success = QueryInformationJobObject(
                job_handle as _,
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr() as *mut _,
                buffer_size as u32,
                &mut return_length,
            );
            list_ptr = buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST;
        }

        if success != 0 {
            let num_pids = (*list_ptr).NumberOfProcessIdsInList as usize;
            let pid_array_ptr = (*list_ptr).ProcessIdList.as_ptr();
            for i in 0..num_pids {
                pids.push(*pid_array_ptr.add(i) as u32);
            }
        }
    }
    pids
}
