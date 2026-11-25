#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::builtins;
use crate::parser::{Arg, CommandEntry, Connector, Pipeline, expand_arg, expand_vars};
#[cfg(unix)]
use crate::signals;

use super::alias::expand_alias;
use super::glob::expand_globs;
use super::path::{expand_home, find_executable};
use super::redirect::{open_stdin_redirect, open_stdout_redirect, resolve_redirects};
use super::state::{ExecutionResult, ShellState, Variable};

/// Expand every argument once against the current variables. The result is
/// plain data: no parsing, no recursive expansion.
fn expand_args(args: &[Arg], state: &ShellState) -> Vec<Arg> {
    args.iter()
        .map(|arg| Arg {
            value: expand_arg(arg, &state.variables),
            quoted: arg.quoted,
            parts: Vec::new(),
        })
        .collect()
}

/// Expand a command's prefix assignments (`VAR=value cmd ...`).
fn expanded_assignments(
    assignments: &[(String, Arg)],
    state: &ShellState,
) -> Vec<(String, String)> {
    assignments
        .iter()
        .map(|(key, value)| (key.clone(), expand_arg(value, &state.variables)))
        .collect()
}

fn internal_command(name: &str, args: &[String]) -> Command {
    let executable = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cerf"));
    let mut command = Command::new(executable);
    command.arg("--internal-builtin").arg(name).args(args);
    command
}

pub(crate) fn execute_isolated(
    name: &str,
    args: &[String],
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    let command = internal_command(name, args);
    let cmd = crate::parser::SimpleCommand {
        assignments: Vec::new(),
        name: Some(name.to_string()),
        args: args.iter().cloned().map(Arg::plain).collect(),
        redirects: Vec::new(),
    };
    let pipeline = Pipeline {
        commands: vec![crate::parser::CommandNode::Simple(cmd)],
        negated: false,
        background: false,
    };
    execute_process(command, name, &pipeline, state, None)
}

fn apply_command_io(
    command: &mut Command,
    cmd: &crate::parser::SimpleCommand,
    pipeline: &Pipeline,
    state: &ShellState,
) -> Result<(), String> {
    let (stdin_redir, stdout_redir) = resolve_redirects(&cmd.redirects);

    if let Some(redir) = stdin_redir {
        let mut expanded_redir = redir.clone();
        expanded_redir.file = expand_vars(&redir.file, &state.variables);
        command.stdin(Stdio::from(open_stdin_redirect(&expanded_redir)?));
    } else if pipeline.background {
        command.stdin(Stdio::null());
    }

    if let Some(redir) = stdout_redir {
        let mut expanded_redir = redir.clone();
        expanded_redir.file = expand_vars(&redir.file, &state.variables);
        command.stdout(Stdio::from(open_stdout_redirect(&expanded_redir)?));
    }

    Ok(())
}

/// True when the command will be run inside this process, so redirects must
/// be applied by swapping descriptors instead of handing them to a child.
fn simple_is_in_process_builtin(cmd: &crate::parser::SimpleCommand, state: &ShellState) -> bool {
    let Some(raw_name) = cmd.name.as_deref() else {
        return false;
    };
    let name = expand_vars(raw_name, &state.variables);
    if state.functions.contains_key(&name) {
        return false;
    }
    if crate::builtins::registry::is_process_isolated(&name) {
        return false;
    }
    crate::builtins::registry::find_command(&name).is_some()
}

/// Temporarily point stdin/stdout at redirection files for an in-process
/// builtin or control-flow block, restoring the originals on drop.
///
/// This is what makes `echo hi > out.txt`, `cat < in.txt`, and `read` honor
/// redirects without forking a child (which would lose shell state).
#[cfg(unix)]
struct StdioRedirect {
    saved: Vec<(i32, i32)>,
}

#[cfg(unix)]
impl StdioRedirect {
    fn apply(redirects: &[crate::parser::Redirect], state: &ShellState) -> Result<Self, String> {
        use std::io::Write as _;
        use std::os::fd::IntoRawFd as _;

        let (stdin_redir, stdout_redir) = resolve_redirects(redirects);
        let mut files: Vec<(std::fs::File, i32)> = Vec::new();
        if let Some(redir) = stdin_redir {
            let mut expanded = redir.clone();
            expanded.file = expand_vars(&redir.file, &state.variables);
            files.push((open_stdin_redirect(&expanded)?, nix::libc::STDIN_FILENO));
        }
        if let Some(redir) = stdout_redir {
            let mut expanded = redir.clone();
            expanded.file = expand_vars(&redir.file, &state.variables);
            files.push((open_stdout_redirect(&expanded)?, nix::libc::STDOUT_FILENO));
        }

        let _ = std::io::stdout().flush();
        let mut guard = StdioRedirect { saved: Vec::new() };
        for (file, target) in files {
            let raw = file.into_raw_fd();
            // SAFETY: `raw` and `target` are valid descriptors; failures are
            // reported and every successful swap is restored by `Drop`.
            let saved = unsafe { nix::libc::dup(target) };
            if saved < 0 {
                unsafe { nix::libc::close(raw) };
                return Err(format!(
                    "cerf: redirect: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if unsafe { nix::libc::dup2(raw, target) } < 0 {
                let error = std::io::Error::last_os_error();
                unsafe {
                    nix::libc::close(saved);
                    nix::libc::close(raw);
                }
                return Err(format!("cerf: redirect: {}", error));
            }
            unsafe { nix::libc::close(raw) };
            guard.saved.push((saved, target));
        }
        Ok(guard)
    }
}

#[cfg(unix)]
impl Drop for StdioRedirect {
    fn drop(&mut self) {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        for &(saved, target) in &self.saved {
            unsafe {
                nix::libc::dup2(saved, target);
                nix::libc::close(saved);
            }
        }
    }
}

/// Windows equivalent of `StdioRedirect`: `SetStdHandle` is process-global and
/// Rust's std looks up the handle on every read/write, so an in-process builtin
/// can be redirected without forking (which would lose shell state).
#[cfg(windows)]
struct StdioRedirect {
    saved: Vec<(
        windows_sys::Win32::System::Console::STD_HANDLE,
        windows_sys::Win32::Foundation::HANDLE,
    )>,
    files: Vec<std::fs::File>,
}

#[cfg(windows)]
impl StdioRedirect {
    fn apply(redirects: &[crate::parser::Redirect], state: &ShellState) -> Result<Self, String> {
        use std::io::Write as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        };

        let (stdin_redir, stdout_redir) = resolve_redirects(redirects);
        let mut files = Vec::new();
        let mut targets = Vec::new();
        if let Some(redir) = stdin_redir {
            let mut expanded = redir.clone();
            expanded.file = expand_vars(&redir.file, &state.variables);
            files.push(open_stdin_redirect(&expanded)?);
            targets.push(STD_INPUT_HANDLE);
        }
        if let Some(redir) = stdout_redir {
            let mut expanded = redir.clone();
            expanded.file = expand_vars(&redir.file, &state.variables);
            files.push(open_stdout_redirect(&expanded)?);
            targets.push(STD_OUTPUT_HANDLE);
        }

        let _ = std::io::stdout().flush();
        let mut guard = StdioRedirect {
            saved: Vec::new(),
            files,
        };
        for (file, target) in guard.files.iter().zip(targets) {
            use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

            let handle = file.as_raw_handle() as _;
            // Mark the file inheritable so children spawned while the redirect
            // is active inherit it, the way Unix passes fd 0/1 down.
            if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
                == 0
            {
                return Err(format!(
                    "cerf: redirect: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let saved = unsafe { GetStdHandle(target) };
            if unsafe { SetStdHandle(target, handle) } == 0 {
                return Err(format!(
                    "cerf: redirect: {}",
                    std::io::Error::last_os_error()
                ));
            }
            guard.saved.push((target, saved));
        }
        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for StdioRedirect {
    fn drop(&mut self) {
        use std::io::Write as _;

        let _ = std::io::stdout().flush();
        for &(target, saved) in self.saved.iter().rev() {
            unsafe { windows_sys::Win32::System::Console::SetStdHandle(target, saved) };
        }
    }
}

fn execute_process(
    mut command: Command,
    name: &str,
    pipeline: &Pipeline,
    state: &mut ShellState,
    snapshot: Option<tempfile::TempPath>,
) -> (ExecutionResult, i32) {
    #[cfg(unix)]
    let is_background = pipeline.background;

    // Internal-builtin children (`--internal-builtin`) may already run in a
    // background process group with default SIGTTOU handling, so they must not
    // claim a new process group or call `tcsetpgrp` (which would stop them).
    #[cfg(unix)]
    let manage_job_control = !state.internal_builtin;

    #[cfg(unix)]
    let result = unsafe {
        command
            .pre_exec(move || {
                signals::ignore_sigttou();
                let pid = nix::unistd::getpid();
                // Background jobs always need their own process group so that
                // job-control signalling (`kill %n`, `bg`, `fg`) targets a
                // valid group, including from internal-builtin children.
                // Foreground internal launches keep the ancestor's group.
                if manage_job_control || is_background {
                    let _ = nix::unistd::setpgid(pid, pid);
                }
                if manage_job_control && !is_background {
                    let stdin = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO);
                    let stderr = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDERR_FILENO);
                    let stdout = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDOUT_FILENO);
                    let _ = nix::unistd::tcsetpgrp(stdin, pid)
                        .or_else(|_| nix::unistd::tcsetpgrp(stderr, pid))
                        .or_else(|_| nix::unistd::tcsetpgrp(stdout, pid));
                }
                signals::restore_default();
                Ok(())
            })
            .spawn()
    };

    #[cfg(windows)]
    let result = command.spawn();

    let code = match result {
        Ok(child) => {
            if let Some(mut snapshot) = snapshot {
                // The child can now read and remove the snapshot.
                snapshot.disable_cleanup(true);
            }
            #[cfg(windows)]
            let mut child = child;
            let pid = child.id();

            // Job-control-less children must not touch the terminal, which
            // already belongs to an ancestor. Foreground ones are simply
            // reaped; background ones fall through to job registration below
            // so `job.wait` and `update_jobs` can still track and report them.
            #[cfg(unix)]
            if !manage_job_control && !pipeline.background {
                let mut child = child;
                let code = child.wait().map(|s| s.code().unwrap_or(0)).unwrap_or(1);
                return (ExecutionResult::KeepRunning, code);
            }

            // Mirror the child's group setup; internal foreground launches keep
            // the ancestor's group, while internal background jobs get their own.
            #[cfg(unix)]
            if state.shell_pgid.is_some() || (!manage_job_control && pipeline.background) {
                let _ = nix::unistd::setpgid(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::unistd::Pid::from_raw(pid as i32),
                );
            }

            #[cfg(windows)]
            let job_handle = unsafe {
                let handle = windows_sys::Win32::System::JobObjects::CreateJobObjectW(
                    std::ptr::null(),
                    std::ptr::null(),
                );
                let mut limit_info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                if !pipeline.background {
                    limit_info.BasicLimitInformation.LimitFlags =
                        windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                }
                windows_sys::Win32::System::JobObjects::SetInformationJobObject(
                    handle,
                    windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation,
                    &limit_info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of_val(&limit_info) as u32,
                );
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    handle,
                    std::os::windows::io::AsRawHandle::as_raw_handle(&child) as _,
                );
                windows_sys::Win32::System::IO::CreateIoCompletionPort(
                    handle,
                    state.iocp_handle as _,
                    state.next_job_id as _,
                    0,
                );
                handle as isize
            };

            let job = crate::engine::state::Job {
                id: state.next_job_id,
                #[cfg(unix)]
                pgid: pid,
                #[cfg(windows)]
                job_handle,
                command: crate::engine::job_control::format_command(pipeline),
                processes: vec![crate::engine::state::ProcessInfo {
                    pid,
                    state: crate::engine::state::JobState::Running,
                }],
                reported_done: false,
            };
            let job_id = state.next_job_id;
            state.jobs.insert(job_id, job);
            state.next_job_id += 1;

            if pipeline.background {
                println!("[{}] {}", job_id, pid);
                0
            } else {
                #[cfg(unix)]
                {
                    crate::engine::job_control::wait_for_job(job_id, state, true)
                }
                #[cfg(windows)]
                {
                    let code = child.wait().map(|s| s.code().unwrap_or(0)).unwrap_or(1);
                    if let Some(job) = state.jobs.get_mut(&job_id) {
                        for process in &mut job.processes {
                            process.state = crate::engine::state::JobState::Done(code);
                        }
                    }
                    state.jobs.remove(&job_id);
                    code
                }
            }
        }
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                eprintln!("cerf: command not found: {}", name);
            } else {
                eprintln!("cerf: error executing '{}': {}", name, error);
            }
            127
        }
    };

    (ExecutionResult::KeepRunning, code)
}

// ── Single command (no pipe) ──────────────────────────────────────────────

/// Execute one simple command with optional redirections.
/// Returns `(ExecutionResult, exit_code)`.
fn execute_simple(pipeline: &Pipeline, state: &mut ShellState) -> (ExecutionResult, i32) {
    let cmd_node = &pipeline.commands[0];
    let cmd = match cmd_node {
        crate::parser::CommandNode::Simple(s) => s,
        _ => return (ExecutionResult::KeepRunning, 0), // handled in execute()
    };
    let (stdin_redir, stdout_redir) = resolve_redirects(&cmd.redirects);

    if cmd.name.is_none() {
        // Just assignments
        for (key, val) in &cmd.assignments {
            let expanded_val = expand_arg(val, &state.variables);
            state.set_var(key, Variable::new_string(expanded_val.clone()));
            // If already in env, update it there too
            if std::env::var(key).is_ok() {
                unsafe {
                    std::env::set_var(key, &expanded_val);
                }
            }
        }
        // Handle residuals like redirects (e.g., VAR=val > file)
        if let Some(redir) = stdin_redir {
            let mut expanded_redir = redir.clone();
            expanded_redir.file = expand_vars(&redir.file, &state.variables);
            if let Err(e) = open_stdin_redirect(&expanded_redir) {
                eprintln!("{}", e);
                return (ExecutionResult::KeepRunning, 1);
            }
        }
        if let Some(redir) = stdout_redir {
            let mut expanded_redir = redir.clone();
            expanded_redir.file = expand_vars(&redir.file, &state.variables);
            if let Err(e) = open_stdout_redirect(&expanded_redir) {
                eprintln!("{}", e);
                return (ExecutionResult::KeepRunning, 1);
            }
        }
        return (ExecutionResult::KeepRunning, 0);
    }

    let raw_name = cmd.name.as_ref().unwrap();
    let name = expand_vars(raw_name, &state.variables);

    // Expand variables in the argument list BEFORE glob expansion.
    let expanded_args = expand_args(&cmd.args, state);

    let args = expand_globs(&expanded_args);

    let isolated_wrapper = if matches!(name.as_str(), "sys.builtin" | "sys.command") {
        args.first()
            .filter(|target| builtins::registry::is_process_isolated(target.as_str()))
            .map(|target| (target.as_str(), &args[1..]))
    } else {
        None
    };

    if let Some(cmd_info) = builtins::registry::find_command(name.as_str()) {
        if pipeline.background
            || builtins::registry::is_process_isolated(&name)
            || isolated_wrapper.is_some()
        {
            let (launch_name, launch_args) = match isolated_wrapper {
                Some(target) => target,
                None => (name.as_str(), args.as_slice()),
            };
            let assignments = expanded_assignments(&cmd.assignments, state);
            let mut command = internal_command(launch_name, launch_args);
            command.envs(assignments.iter().map(|(key, value)| (key, value)));
            // Background builtins run in a child process, so hand them a
            // launch-time copy of the variables and options.
            let snapshot = if pipeline.background {
                match state.write_snapshot(&assignments) {
                    Ok(path) => {
                        command.env(super::state::SNAPSHOT_ENV, path.as_os_str());
                        Some(path)
                    }
                    Err(error) => {
                        eprintln!("cerf: could not write state snapshot: {}", error);
                        return (ExecutionResult::KeepRunning, 1);
                    }
                }
            } else {
                None
            };
            if let Err(error) = apply_command_io(&mut command, cmd, pipeline, state) {
                eprintln!("{}", error);
                return (ExecutionResult::KeepRunning, 1);
            }
            return execute_process(command, launch_name, pipeline, state, snapshot);
        }

        let run_generic =
            |state: &mut ShellState| -> (ExecutionResult, i32) { (cmd_info.run)(&args, state) };

        run_generic(state)
    } else {
        let resolved = find_executable(&name).unwrap_or_else(|| expand_home(&name));

        #[cfg(windows)]
        let mut command = {
            let is_batch = resolved.extension().is_some_and(|e| {
                let e = e.to_string_lossy().to_lowercase();
                e == "cmd" || e == "bat"
            });
            if is_batch {
                let mut c = Command::new("cmd");
                c.arg("/c").arg(&resolved);
                c
            } else {
                Command::new(&resolved)
            }
        };

        #[cfg(unix)]
        let mut command = Command::new(&resolved);

        command.args(&args);
        let assignments = expanded_assignments(&cmd.assignments, state);
        command.envs(assignments.iter().map(|(key, value)| (key, value)));

        if let Err(error) = apply_command_io(&mut command, cmd, pipeline, state) {
            eprintln!("{}", error);
            return (ExecutionResult::KeepRunning, 1);
        }

        execute_process(command, &name, pipeline, state, None)
    }
}

// ── Pipeline execution ────────────────────────────────────────────────────

/// Execute a full pipeline (one or more commands connected by `|`).
/// Returns `(ExecutionResult, exit_code)`.
pub fn execute(pipeline: &Pipeline, state: &mut ShellState) -> (ExecutionResult, i32) {
    let mut pipeline = pipeline.clone();

    // Expand aliases on every command's name (only the first command of a
    // pipeline gets alias-expanded, same as bash behaviour for safety).
    for cmd in &mut pipeline.commands {
        expand_alias(cmd, &state.aliases);
    }

    let cmds = &pipeline.commands;

    // Single-command pipeline — just run the command directly (supports builtins).
    if cmds.len() == 1 {
        let node = &cmds[0];
        let redirects = node.redirects();
        let in_process = match node {
            crate::parser::CommandNode::Simple(cmd) => {
                !pipeline.background && simple_is_in_process_builtin(cmd, state)
            }
            _ => true,
        };
        if !redirects.is_empty() && in_process {
            // Windows cannot hand a redirected block's descriptors to the
            // processes it spawns, so blocks still run in a subshell there.
            #[cfg(not(unix))]
            if !matches!(node, crate::parser::CommandNode::Simple(_)) {
                return execute_block_with_redirects(&pipeline, node, redirects, state);
            }

            let _guard = match StdioRedirect::apply(redirects, state) {
                Ok(guard) => guard,
                Err(error) => {
                    eprintln!("{}", error);
                    return (ExecutionResult::KeepRunning, 1);
                }
            };
            return execute_node(&pipeline, node, state);
        }
        return execute_node(&pipeline, node, state);
    }

    // Multi-command pipeline: fork external processes connected by pipes.
    // Builtins in a multi-command pipeline are run as external commands
    // (same behaviour as bash).
    let last_idx = cmds.len() - 1;
    let mut children: Vec<std::process::Child> = Vec::with_capacity(cmds.len());
    let mut prev_stdout: Option<std::process::ChildStdout> = None;

    let mut first_pgid = 0;
    let mut processes = Vec::new();

    #[cfg(windows)]
    let job_handle = unsafe {
        let handle = windows_sys::Win32::System::JobObjects::CreateJobObjectW(
            std::ptr::null(),
            std::ptr::null(),
        );
        let mut limit_info: windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        if !pipeline.background {
            limit_info.BasicLimitInformation.LimitFlags =
                windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        windows_sys::Win32::System::JobObjects::SetInformationJobObject(
            handle,
            windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation,
            &limit_info as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&limit_info) as u32,
        );
        windows_sys::Win32::System::IO::CreateIoCompletionPort(
            handle,
            state.iocp_handle as _,
            state.next_job_id as _,
            0,
        );
        handle as isize
    };

    for (i, cmd) in cmds.iter().enumerate() {
        let name = match cmd {
            crate::parser::CommandNode::Simple(s) => s.name.as_deref().unwrap_or(""),
            crate::parser::CommandNode::Break | crate::parser::CommandNode::Continue => {
                eprintln!("cerf: control flow command in pipeline is currently unsupported");
                continue;
            }
            _ => {
                // For now, complex blocks in pipelines are unsupported in the external pipe forker.
                eprintln!("cerf: complex blocks in pipelines are currently unsupported");
                continue;
            }
        };

        if name.is_empty() {
            continue;
        }

        // If a builtin appears in a multi-command pipeline, check for exit
        if name == "exit" {
            // Kill any children we already spawned
            for mut child in children {
                let _ = child.kill();
            }
            builtins::system::exit();
            return (ExecutionResult::Exit, 0);
        }

        let is_builtin = builtins::registry::find_command(name).is_some();
        let resolved = if is_builtin {
            std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cerf"))
        } else {
            find_executable(name).unwrap_or_else(|| expand_home(name))
        };

        // Expand globs on the argument list (only used for non-builtins).
        let expanded = expand_args(cmd.args(), state);
        let args = expand_globs(&expanded);

        #[cfg(windows)]
        let mut command = if is_builtin {
            internal_command(name, &args)
        } else {
            let is_batch = resolved.extension().is_some_and(|e| {
                let e = e.to_string_lossy().to_lowercase();
                e == "cmd" || e == "bat"
            });
            let mut command = if is_batch {
                let mut command = Command::new("cmd");
                command.arg("/c").arg(&resolved);
                command
            } else {
                Command::new(&resolved)
            };
            command.args(&args);
            command
        };

        #[cfg(unix)]
        let mut command = if is_builtin {
            internal_command(name, &args)
        } else {
            let mut command = Command::new(&resolved);
            command.args(&args);
            command
        };

        let assignments = expanded_assignments(cmd.assignments(), state);
        command.envs(assignments.iter().map(|(k, v)| (k, v)));

        // Stdin: first command may have < redirect, others get previous pipe
        if i == 0 {
            let (stdin_redir, _) = resolve_redirects(cmd.redirects());
            if let Some(redir) = stdin_redir {
                match open_stdin_redirect(redir) {
                    Ok(f) => {
                        command.stdin(Stdio::from(f));
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        // Kill already started children
                        for mut child in children {
                            let _ = child.kill();
                        }
                        return (ExecutionResult::KeepRunning, 1);
                    }
                }
            } else if pipeline.background {
                command.stdin(Stdio::null());
            }
        } else if let Some(stdout) = prev_stdout.take() {
            command.stdin(Stdio::from(stdout));
        }

        // Stdout: last command may have > or >> redirect, others pipe
        if i == last_idx {
            let (_, stdout_redir) = resolve_redirects(cmd.redirects());
            if let Some(redir) = stdout_redir {
                match open_stdout_redirect(redir) {
                    Ok(f) => {
                        command.stdout(Stdio::from(f));
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        for mut child in children {
                            let _ = child.kill();
                        }
                        return (ExecutionResult::KeepRunning, 1);
                    }
                }
            }
        } else {
            command.stdout(Stdio::piped());
        }

        #[cfg(unix)]
        let target_pgid = first_pgid;

        #[cfg(unix)]
        let is_bg = pipeline.background;

        #[cfg(unix)]
        let result = unsafe {
            command
                .pre_exec(move || {
                    signals::ignore_sigttou();
                    let pid = nix::unistd::getpid();
                    let pgid = if target_pgid == 0 {
                        pid
                    } else {
                        nix::unistd::Pid::from_raw(target_pgid as i32)
                    };
                    let _ = nix::unistd::setpgid(pid, pgid);
                    if !is_bg {
                        let stdin = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO);
                        let stderr = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDERR_FILENO);
                        let stdout = std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDOUT_FILENO);
                        let _ = nix::unistd::tcsetpgrp(stdin, pgid)
                            .or_else(|_| nix::unistd::tcsetpgrp(stderr, pgid))
                            .or_else(|_| nix::unistd::tcsetpgrp(stdout, pgid));
                    }
                    signals::restore_default();
                    Ok(())
                })
                .spawn()
        };

        #[cfg(windows)]
        let result = command.spawn();

        match result {
            Ok(mut child) => {
                let pid = child.id();
                // The first *spawned* command defines the process group.
                // An assignment-only first element is skipped above, so `i`
                // is not a reliable marker here.
                if first_pgid == 0 {
                    first_pgid = pid;
                }

                #[cfg(unix)]
                if state.shell_pgid.is_some() {
                    let _ = nix::unistd::setpgid(
                        nix::unistd::Pid::from_raw(pid as i32),
                        nix::unistd::Pid::from_raw(first_pgid as i32),
                    );
                }

                #[cfg(windows)]
                unsafe {
                    windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                        job_handle as _,
                        std::os::windows::io::AsRawHandle::as_raw_handle(&child) as _,
                    );
                }

                processes.push(crate::engine::state::ProcessInfo {
                    pid,
                    state: crate::engine::state::JobState::Running,
                });

                if i != last_idx {
                    prev_stdout = child.stdout.take();
                }
                children.push(child);
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    eprintln!("cerf: command not found: {}", name);
                } else {
                    eprintln!("cerf: error executing '{}': {}", name, e);
                }
                // Kill already started children
                for mut child in children {
                    let _ = child.kill();
                }
                return (ExecutionResult::KeepRunning, 127);
            }
        }
    }

    if processes.is_empty() {
        // Every pipeline element was skipped (for example assignment-only):
        // there is no process to track and never a pgid 0 job.
        return (ExecutionResult::KeepRunning, 0);
    }

    let job = crate::engine::state::Job {
        id: state.next_job_id,
        #[cfg(unix)]
        pgid: first_pgid,
        #[cfg(windows)]
        job_handle,
        command: crate::engine::job_control::format_command(&pipeline),
        processes,
        reported_done: false,
    };
    let job_id = state.next_job_id;
    state.jobs.insert(job_id, job);
    state.next_job_id += 1;

    let last_code = if pipeline.background {
        println!("[{}] {}", job_id, first_pgid);
        0
    } else {
        #[cfg(unix)]
        {
            crate::engine::job_control::wait_for_job(job_id, state, true)
        }
        #[cfg(windows)]
        {
            let mut last = 0;
            for mut child in children {
                last = child.wait().map(|s| s.code().unwrap_or(0)).unwrap_or(1);
            }
            if let Some(job) = state.jobs.get_mut(&job_id) {
                for p in &mut job.processes {
                    p.state = crate::engine::state::JobState::Done(last);
                }
            }
            state.jobs.remove(&job_id);
            last
        }
    };

    let final_code = if pipeline.negated {
        if last_code == 0 { 1 } else { 0 }
    } else {
        last_code
    };

    (ExecutionResult::KeepRunning, final_code)
}

// ── Command list (&&, ||, ;) ───────────────────────────────────────────────

/// Execute a list of pipelines chained by `&&`, `||`, and `;`.
///
/// Semantics follow POSIX sh:
/// - **`;`**  — always run the next pipeline regardless of the previous exit code.
/// - **`&&`** — run the next pipeline only if the previous returned exit
///   code `0` (success).
/// - **`||`** — run the next pipeline only if the previous returned a
///   non-zero exit code (failure).
pub fn execute_list(entries: Vec<CommandEntry>, state: &mut ShellState) -> (ExecutionResult, i32) {
    let mut last_code: i32 = 0;

    for entry in entries {
        // Decide whether to skip this pipeline based on the connector and the
        // last exit code.
        let skip = match entry.connector {
            None => false,                          // first command: always run
            Some(Connector::Semi) => false,         // ;  → always run
            Some(Connector::And) => last_code != 0, // && → skip on failure
            Some(Connector::Or) => last_code == 0,  // || → skip on success
            Some(Connector::Amp) => false,          // &  → always run
        };

        if skip {
            continue;
        }

        let (result, code) = execute(&entry.pipeline, state);
        // Internal builtin processes have no interactive loop to poll children.
        #[cfg(unix)]
        if state.internal_builtin {
            crate::engine::job_control::update_jobs(state);
        }
        last_code = code;

        match result {
            ExecutionResult::KeepRunning => {}
            // `break` / `continue` / `exit` must reach the enclosing loop or
            // the REPL instead of being swallowed here.
            other => return (other, last_code),
        }
    }

    (ExecutionResult::KeepRunning, last_code)
}

#[cfg(not(unix))]
fn execute_block_with_redirects(
    pipeline: &Pipeline,
    node: &crate::parser::CommandNode,
    redirects: &[crate::parser::Redirect],
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    // Blocks with redirects run in a subshell.
    let mut command =
        Command::new(std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cerf")));

    // We need to format the specific command node.
    command
        .arg("-c")
        .arg(crate::engine::job_control::format_node_full(node));

    let (stdin_redir, stdout_redir) = resolve_redirects(redirects);

    if let Some(redir) = stdin_redir {
        let mut expanded_redir = redir.clone();
        expanded_redir.file = expand_vars(&redir.file, &state.variables);
        if let Ok(f) = open_stdin_redirect(&expanded_redir) {
            command.stdin(Stdio::from(f));
        }
    }
    if let Some(redir) = stdout_redir {
        let mut expanded_redir = redir.clone();
        expanded_redir.file = expand_vars(&redir.file, &state.variables);
        if let Ok(f) = open_stdout_redirect(&expanded_redir) {
            command.stdout(Stdio::from(f));
        }
    }

    if pipeline.background {
        match command.spawn() {
            Ok(child) => {
                let pid = child.id();
                let job_id = state.next_job_id;
                println!("[{}] {}", job_id, pid);

                #[cfg(windows)]
                let job_handle = unsafe {
                    let handle = windows_sys::Win32::System::JobObjects::CreateJobObjectW(
                        std::ptr::null(),
                        std::ptr::null(),
                    );
                    windows_sys::Win32::System::IO::CreateIoCompletionPort(
                        handle,
                        state.iocp_handle as _,
                        job_id as _,
                        0,
                    );
                    windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                        handle,
                        std::os::windows::io::AsRawHandle::as_raw_handle(&child) as _,
                    );
                    handle as isize
                };

                let job = crate::engine::state::Job {
                    id: job_id,
                    #[cfg(unix)]
                    pgid: pid,
                    #[cfg(windows)]
                    job_handle,
                    command: crate::engine::job_control::format_command(pipeline),
                    processes: vec![crate::engine::state::ProcessInfo {
                        pid,
                        state: crate::engine::state::JobState::Running,
                    }],
                    reported_done: false,
                };
                state.jobs.insert(job_id, job);
                state.next_job_id += 1;
                (ExecutionResult::KeepRunning, 0)
            }
            Err(e) => {
                eprintln!("cerf: error backgrounding block: {}", e);
                (ExecutionResult::KeepRunning, 1)
            }
        }
    } else {
        match command.spawn() {
            Ok(mut child) => {
                let status = child.wait().map(|s| s.code().unwrap_or(0)).unwrap_or(1);
                (ExecutionResult::KeepRunning, status)
            }
            Err(e) => {
                eprintln!("cerf: error executing block: {}", e);
                (ExecutionResult::KeepRunning, 1)
            }
        }
    }
}

/// Execute a single command node in the current shell process.
fn execute_node(
    pipeline: &Pipeline,
    node: &crate::parser::CommandNode,
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    match node {
        crate::parser::CommandNode::Break => (ExecutionResult::Break, 0),
        crate::parser::CommandNode::Continue => (ExecutionResult::Continue, 0),
        crate::parser::CommandNode::If {
            branches,
            else_branch,
            ..
        } => {
            let mut final_code = 0;
            let mut executed = false;
            for (cond, body) in branches {
                let (res, cond_code) = execute_list(cond.clone(), state);
                if !matches!(res, ExecutionResult::KeepRunning) {
                    return (res, cond_code);
                }
                if cond_code == 0 {
                    let (res, body_code) = execute_list(body.clone(), state);
                    if !matches!(res, ExecutionResult::KeepRunning) {
                        return (res, body_code);
                    }
                    final_code = body_code;
                    executed = true;
                    break;
                }
            }
            if !executed && let Some(body) = else_branch {
                let (res, body_code) = execute_list(body.clone(), state);
                if !matches!(res, ExecutionResult::KeepRunning) {
                    return (res, body_code);
                }
                final_code = body_code;
            }
            let code = if pipeline.negated {
                if final_code == 0 { 1 } else { 0 }
            } else {
                final_code
            };
            (ExecutionResult::KeepRunning, code)
        }
        crate::parser::CommandNode::FuncDecl { name, body } => {
            state.functions.insert(name.clone(), body.clone());
            (ExecutionResult::KeepRunning, 0)
        }
        crate::parser::CommandNode::For {
            var, items, body, ..
        } => {
            // Expand variables in loop items
            let expanded_items_vars = expand_args(items, state);

            let expanded_items = expand_globs(&expanded_items_vars);
            let mut final_code = 0;
            for item in expanded_items {
                state.set_var(var, Variable::new_string(item.clone()));
                let (res, code) = execute_list(body.clone(), state);
                match res {
                    ExecutionResult::Exit => return (res, code),
                    ExecutionResult::Break => break,
                    ExecutionResult::Continue => continue,
                    ExecutionResult::KeepRunning => {
                        final_code = code;
                    }
                }
            }
            let code = if pipeline.negated {
                if final_code == 0 { 1 } else { 0 }
            } else {
                final_code
            };
            (ExecutionResult::KeepRunning, code)
        }
        crate::parser::CommandNode::While { cond, body, .. } => {
            let mut final_code = 0;
            loop {
                let (res, cond_code) = execute_list(cond.clone(), state);
                if !matches!(res, ExecutionResult::KeepRunning) {
                    return (res, cond_code);
                }
                if cond_code != 0 {
                    break;
                }
                let (res, body_code) = execute_list(body.clone(), state);
                match res {
                    ExecutionResult::Exit => return (res, body_code),
                    ExecutionResult::Break => break,
                    ExecutionResult::Continue => continue,
                    ExecutionResult::KeepRunning => {
                        final_code = body_code;
                    }
                }
            }
            let code = if pipeline.negated {
                if final_code == 0 { 1 } else { 0 }
            } else {
                final_code
            };
            (ExecutionResult::KeepRunning, code)
        }
        crate::parser::CommandNode::Loop { body, .. } => {
            let mut final_code = 0;
            loop {
                let (res, body_code) = execute_list(body.clone(), state);
                match res {
                    ExecutionResult::Exit => return (res, body_code),
                    ExecutionResult::Break => break,
                    ExecutionResult::Continue => continue,
                    ExecutionResult::KeepRunning => {
                        final_code = body_code;
                    }
                }
            }
            (ExecutionResult::KeepRunning, final_code)
        }
        crate::parser::CommandNode::Simple(cmd) => {
            let name = expand_vars(cmd.name.as_deref().unwrap_or(""), &state.variables);
            if let Some(func_body) = state.functions.get(&name).cloned() {
                let (res, code) = execute_list(func_body, state);
                let final_code = if pipeline.negated {
                    if code == 0 { 1 } else { 0 }
                } else {
                    code
                };
                return (res, final_code);
            }

            let (res, code) = execute_simple(pipeline, state);
            let final_code = if pipeline.negated {
                if code == 0 { 1 } else { 0 }
            } else {
                code
            };
            (res, final_code)
        }
    }
}
