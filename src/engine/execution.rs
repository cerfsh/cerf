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
use super::redirect::{RedirectSource, prepare_redirects, touch_redirect_files};
use super::state::{ExecutionResult, ShellState, Variable};

/// Expand every argument once against the current variables. The result is
/// plain data: no parsing, no recursive expansion. Quoting metadata (and
/// therefore the per-run glob protection) is preserved.
fn expand_args(args: &[Arg], state: &ShellState) -> Vec<Arg> {
    args.iter()
        .map(|arg| {
            if arg.parts.is_empty() {
                return Arg {
                    value: arg.value.clone(),
                    quoted: arg.quoted,
                    parts: Vec::new(),
                };
            }

            let mut parts: Vec<crate::parser::WordPart> = Vec::new();
            for part in &arg.parts {
                let text = if part.expand {
                    expand_vars(&part.text, &state.variables)
                } else {
                    part.text.clone()
                };
                if text.is_empty() {
                    continue;
                }
                match parts.last_mut() {
                    Some(last) if last.expand == part.expand && last.glob == part.glob => {
                        last.text.push_str(&text);
                    }
                    _ => parts.push(crate::parser::WordPart {
                        text,
                        expand: part.expand,
                        glob: part.glob,
                    }),
                }
            }

            let value = parts.iter().map(|part| part.text.as_str()).collect();
            Arg {
                value,
                quoted: arg.quoted,
                parts,
            }
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

/// State saved while prefix assignments (`VAR=value cmd`) are active.
struct SavedAssignment {
    name: String,
    variable: Option<Variable>,
    env: Option<std::ffi::OsString>,
}

/// Apply `VAR=value` assignments for the duration of an in-process command.
///
/// They become visible to the shell's variable lookup (so `IFS=: read x`
/// splits on `:`) and to the process environment (so child processes see
/// them), then are restored by [`restore_scoped_assignments`].
fn apply_scoped_assignments(
    state: &mut ShellState,
    assignments: &[(String, String)],
) -> Result<Vec<SavedAssignment>, String> {
    let mut saved = Vec::with_capacity(assignments.len());
    for (name, value) in assignments {
        if !crate::engine::state::is_valid_name(name) {
            restore_scoped_assignments(state, saved);
            return Err(format!("cerf: '{}': not a valid identifier", name));
        }
        if state.get_var(name).is_some_and(|var| var.readonly) {
            restore_scoped_assignments(state, saved);
            return Err(format!("cerf: {}: readonly variable", name));
        }

        saved.push(SavedAssignment {
            name: name.clone(),
            variable: state.get_var(name).cloned(),
            env: std::env::var_os(name),
        });
        state.set_var(name, Variable::new_string(value.clone()));
        unsafe {
            std::env::set_var(name, value);
        }
    }
    Ok(saved)
}

fn restore_scoped_assignments(state: &mut ShellState, saved: Vec<SavedAssignment>) {
    for saved in saved.into_iter().rev() {
        match saved.variable {
            Some(variable) => state.set_var(&saved.name, variable),
            None => {
                state.variables.remove(&saved.name);
                for scope in &mut state.scopes {
                    scope.remove(&saved.name);
                }
            }
        }
        unsafe {
            match saved.env {
                Some(value) => std::env::set_var(&saved.name, value),
                None => std::env::remove_var(&saved.name),
            }
        }
    }
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

/// Run an external command through the normal spawn path so it receives
/// signal defaults, its own process group, and terminal hand-off.
///
/// Used by `sys.command`, which previously called `Command::status()`
/// directly and therefore left SIGINT/SIGQUIT/SIGTSTP ignored in the child.
pub(crate) fn execute_external(
    name: &str,
    args: &[String],
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    let resolved = find_executable(name).unwrap_or_else(|| expand_home(name));

    #[cfg(windows)]
    let mut command = {
        let is_batch = resolved.extension().is_some_and(|e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "cmd" || e == "bat"
        });
        if is_batch {
            let mut command = Command::new("cmd");
            command.arg("/c").arg(&resolved);
            command
        } else {
            Command::new(&resolved)
        }
    };

    #[cfg(unix)]
    let mut command = Command::new(&resolved);

    command.args(args);

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

/// Apply redirects to a child process.
///
/// On Unix the descriptors are installed in the child through `pre_exec`, in
/// source order, so `2>`, arbitrary descriptor numbers, and `2>&1` all work.
#[cfg(unix)]
fn apply_redirects_to_command(
    command: &mut Command,
    redirects: &[crate::parser::Redirect],
    state: &ShellState,
) -> Result<(), String> {
    use std::os::fd::AsRawFd as _;

    if redirects.is_empty() {
        return Ok(());
    }
    let prepared = prepare_redirects(redirects, &state.variables)?;
    unsafe {
        command.pre_exec(move || {
            for redirect in &prepared {
                let source = match &redirect.source {
                    RedirectSource::File(file) => file.as_raw_fd(),
                    RedirectSource::Dup(target) => *target,
                };
                if nix::libc::dup2(source, redirect.fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(windows)]
fn apply_redirects_to_command(
    command: &mut Command,
    redirects: &[crate::parser::Redirect],
    state: &ShellState,
) -> Result<(), String> {
    use std::fs::File;

    #[derive(Clone)]
    enum WinSource {
        File(File),
        Inherit,
    }
    impl WinSource {
        fn try_clone(&self) -> std::io::Result<Self> {
            match self {
                Self::File(file) => Ok(Self::File(file.try_clone()?)),
                Self::Inherit => Ok(Self::Inherit),
            }
        }
    }

    // Std support only exposes 0/1/2; resolve duplications against the
    // final source for those descriptors.
    let mut slots: [Option<WinSource>; 3] = [None, None, None];
    for redirect in prepare_redirects(redirects, &state.variables)? {
        let Ok(fd) = usize::try_from(redirect.fd) else {
            continue;
        };
        if fd >= slots.len() {
            continue;
        }
        match redirect.source {
            RedirectSource::File(file) => slots[fd] = Some(WinSource::File(file)),
            RedirectSource::Dup(target) => {
                let source = usize::try_from(target).ok().and_then(|target| {
                    if target < slots.len() {
                        slots[target].clone()
                    } else {
                        Some(WinSource::Inherit)
                    }
                });
                slots[fd] = source.or(Some(WinSource::Inherit));
            }
        }
    }

    for (fd, slot) in slots.into_iter().enumerate() {
        let Some(source) = slot else {
            continue;
        };
        let stdio = match source {
            WinSource::File(file) => Stdio::from(file),
            WinSource::Inherit => Stdio::inherit(),
        };
        match fd {
            0 => command.stdin(stdio),
            1 => command.stdout(stdio),
            _ => command.stderr(stdio),
        };
    }

    Ok(())
}

fn apply_command_io(
    command: &mut Command,
    redirects: &[crate::parser::Redirect],
    pipeline: &Pipeline,
    state: &ShellState,
) -> Result<(), String> {
    apply_redirects_to_command(command, redirects, state)?;

    if pipeline.background {
        command.stdin(Stdio::null());
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
    // Functions run in this process (so state changes survive) and therefore
    // need their redirects applied by swapping descriptors here.
    if state.functions.contains_key(&name) {
        return true;
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

        let prepared = prepare_redirects(redirects, &state.variables)?;

        let _ = std::io::stdout().flush();
        let mut guard = StdioRedirect { saved: Vec::new() };
        for redirect in prepared {
            let target = redirect.fd;
            // Save the original descriptor only once, even when it is
            // redirected repeatedly (`>a >b`, `2>&1`).
            if !guard
                .saved
                .iter()
                .any(|&(_, saved_target)| saved_target == target)
            {
                let saved = unsafe { nix::libc::dup(target) };
                if saved < 0 {
                    return Err(format!(
                        "cerf: redirect: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                guard.saved.push((saved, target));
            }

            let result = match redirect.source {
                RedirectSource::File(file) => {
                    let raw = file.into_raw_fd();
                    let result = unsafe { nix::libc::dup2(raw, target) };
                    unsafe { nix::libc::close(raw) };
                    result
                }
                RedirectSource::Dup(source) => unsafe { nix::libc::dup2(source, target) },
            };
            if result < 0 {
                return Err(format!(
                    "cerf: redirect: {}",
                    std::io::Error::last_os_error()
                ));
            }
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
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        };

        fn std_handle(fd: i32) -> windows_sys::Win32::System::Console::STD_HANDLE {
            match fd {
                0 => STD_INPUT_HANDLE,
                1 => STD_OUTPUT_HANDLE,
                _ => STD_ERROR_HANDLE,
            }
        }

        let prepared = prepare_redirects(redirects, &state.variables)?;
        let _ = std::io::stdout().flush();
        let mut guard = StdioRedirect {
            saved: Vec::new(),
            files: Vec::new(),
        };

        // Track the current handle for 0/1/2 so `2>&1` duplicates the handle
        // stdout was last pointed at.
        let mut current: [Option<HANDLE>; 3] = [
            Some(unsafe { GetStdHandle(STD_INPUT_HANDLE) }),
            Some(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }),
            Some(unsafe { GetStdHandle(STD_ERROR_HANDLE) }),
        ];

        for redirect in prepared {
            let Ok(fd) = usize::try_from(redirect.fd) else {
                continue;
            };
            if fd >= current.len() {
                continue;
            }
            let handle = match redirect.source {
                RedirectSource::File(file) => {
                    use windows_sys::Win32::Foundation::{
                        HANDLE_FLAG_INHERIT, SetHandleInformation,
                    };
                    let handle = file.as_raw_handle() as HANDLE;
                    // Mark the file inheritable so children spawned while the
                    // redirect is active inherit it, like fd 0/1 on Unix.
                    if unsafe {
                        SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
                    } == 0
                    {
                        return Err(format!(
                            "cerf: redirect: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    guard.files.push(file);
                    handle
                }
                RedirectSource::Dup(source) => usize::try_from(source)
                    .ok()
                    .filter(|source| *source < current.len())
                    .and_then(|source| current[source])
                    .unwrap_or(std::ptr::null_mut()),
            };

            if !guard
                .saved
                .iter()
                .any(|&(saved, _)| saved == std_handle(fd as i32))
            {
                let saved = unsafe { GetStdHandle(std_handle(fd as i32)) };
                guard.saved.push((std_handle(fd as i32), saved));
            }
            if unsafe { SetStdHandle(std_handle(fd as i32), handle) } == 0 {
                return Err(format!(
                    "cerf: redirect: {}",
                    std::io::Error::last_os_error()
                ));
            }
            current[fd] = Some(handle);
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

/// Build a self-contained script that declares a function and calls it with
/// the given (already expanded) arguments. Used to run a function as a
/// pipeline element or subshell.
fn function_call_script(name: &str, body: &[CommandEntry], args: &[String]) -> String {
    let decl =
        crate::engine::job_control::format_node_full(&crate::parser::CommandNode::FuncDecl {
            name: name.to_string(),
            body: body.to_vec(),
        });
    let mut call = crate::engine::job_control::quote_word(name);
    for arg in args {
        call.push(' ');
        call.push_str(&crate::engine::job_control::quote_word(arg));
    }
    format!("{}; {}", decl, call)
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
    if cmd.name.is_none() {
        // Just assignments
        for (key, val) in &cmd.assignments {
            let expanded_val = expand_arg(val, &state.variables);
            if state.get_var(key).is_some_and(|var| var.readonly) {
                eprintln!("cerf: {}: readonly variable", key);
                return (ExecutionResult::KeepRunning, 1);
            }
            // Exported variables stay in sync with the process environment;
            // merely being present there is not enough (unexported variables
            // inherited by the shell must not be overwritten).
            let exported = state.get_var(key).is_some_and(|var| var.exported);
            state.set_var(key, Variable::new_string(expanded_val.clone()));
            if exported {
                unsafe {
                    std::env::set_var(key, &expanded_val);
                }
            }
        }
        // Handle residuals like redirects (e.g., VAR=val > file): a shell
        // still creates/truncates the target even with no command.
        if let Err(e) = touch_redirect_files(&cmd.redirects, &state.variables) {
            eprintln!("{}", e);
            return (ExecutionResult::KeepRunning, 1);
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
            if let Err(error) = apply_command_io(&mut command, &cmd.redirects, pipeline, state) {
                eprintln!("{}", error);
                return (ExecutionResult::KeepRunning, 1);
            }
            return execute_process(command, launch_name, pipeline, state, snapshot);
        }

        // In-process builtins run with their prefix assignments applied to
        // both shell state and the environment, then everything is restored.
        let assignments = expanded_assignments(&cmd.assignments, state);
        let saved = match apply_scoped_assignments(state, &assignments) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("{}", error);
                return (ExecutionResult::KeepRunning, 1);
            }
        };
        let result = (cmd_info.run)(&args, state);
        restore_scoped_assignments(state, saved);
        result
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

        if let Err(error) = apply_command_io(&mut command, &cmd.redirects, pipeline, state) {
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

        // `if … { … } &`, `for … &`, and `func f …; f &` must not run in the
        // foreground: a subshell performs the work and the job is registered
        // immediately.
        if pipeline.background {
            if !matches!(node, crate::parser::CommandNode::Simple(_)) {
                return execute_block_with_redirects(&pipeline, node, redirects, state);
            }
            if let crate::parser::CommandNode::Simple(cmd) = node {
                let name = expand_vars(cmd.name.as_deref().unwrap_or(""), &state.variables);
                if let Some(body) = state.functions.get(&name).cloned() {
                    let args = expand_globs(&expand_args(&cmd.args, state));
                    let script = function_call_script(&name, &body, &args);
                    return execute_script_with_redirects(&pipeline, &script, redirects, state);
                }
            }
        }

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
        let is_block = matches!(
            cmd,
            crate::parser::CommandNode::If { .. }
                | crate::parser::CommandNode::For { .. }
                | crate::parser::CommandNode::While { .. }
                | crate::parser::CommandNode::Loop { .. }
        );

        let (name, func_body) = match cmd {
            crate::parser::CommandNode::Simple(s) => {
                let name = expand_vars(s.name.as_deref().unwrap_or(""), &state.variables);
                let body = state.functions.get(&name).cloned();
                (name, body)
            }
            crate::parser::CommandNode::Break | crate::parser::CommandNode::Continue => {
                eprintln!("cerf: control flow command in pipeline is currently unsupported");
                continue;
            }
            _ => (String::new(), None),
        };

        if name.is_empty() && !is_block {
            continue;
        }

        // Expand globs on the argument list (only used for non-builtins).
        let expanded = expand_args(cmd.args(), state);
        let args = expand_globs(&expanded);

        // A function or control-flow block in a pipeline cannot run in the
        // parent shell (it needs its own stdin/stdout), so run it as a
        // subshell script. Functions are serialized with their declaration.
        let script = if let Some(body) = &func_body {
            Some(function_call_script(&name, body, &args))
        } else if is_block {
            Some(crate::engine::job_control::format_node_full(cmd))
        } else {
            None
        };

        // If a builtin appears in a multi-command pipeline, check for exit
        if name == "exit" || name == "sys.exit" {
            // Kill any children we already spawned
            for mut child in children {
                let _ = child.kill();
            }
            let code = match args.first() {
                Some(arg) => arg
                    .parse::<i64>()
                    .map(|code| (code & 0xff) as i32)
                    .unwrap_or(2),
                None => state.last_status,
            };
            return (ExecutionResult::Exit, code);
        }

        let is_builtin = func_body.is_none() && builtins::registry::find_command(&name).is_some();
        let resolved = if script.is_some() || is_builtin {
            std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cerf"))
        } else {
            find_executable(&name).unwrap_or_else(|| expand_home(&name))
        };

        #[cfg(windows)]
        let mut command = if let Some(script) = &script {
            let mut command = Command::new(&resolved);
            command.arg("-c").arg(script);
            command
        } else if is_builtin {
            internal_command(&name, &args)
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
        let mut command = if let Some(script) = &script {
            let mut command = Command::new(&resolved);
            command.arg("-c").arg(script);
            command
        } else if is_builtin {
            internal_command(&name, &args)
        } else {
            let mut command = Command::new(&resolved);
            command.args(&args);
            command
        };

        let assignments = expanded_assignments(cmd.assignments(), state);
        command.envs(assignments.iter().map(|(k, v)| (k, v)));

        // Redirects apply to every pipeline element, not just the first/last
        // (`cmd 2>err | cat`), and any descriptor may be redirected.
        if let Err(error) = apply_command_io(&mut command, cmd.redirects(), &pipeline, state) {
            eprintln!("{}", error);
            for mut child in children {
                let _ = child.kill();
            }
            return (ExecutionResult::KeepRunning, 1);
        }

        // Stdin: the first command inherits/null (redirects override in the
        // child); later commands read the preceding pipe.
        if i == 0 {
            if pipeline.background {
                command.stdin(Stdio::null());
            }
        } else if let Some(stdout) = prev_stdout.take() {
            command.stdin(Stdio::from(stdout));
        }

        // Stdout: everything except the last command feeds a pipe. An
        // explicit redirect is installed later in the child and wins.
        if i != last_idx {
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
        state.last_status = code;
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

fn execute_block_with_redirects(
    pipeline: &Pipeline,
    node: &crate::parser::CommandNode,
    redirects: &[crate::parser::Redirect],
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    // A block that must not mutate the parent shell (because it is
    // backgrounded, or because Windows cannot swap the parent's descriptors)
    // runs as a subshell through the normal process path so job registration,
    // process groups, and `[1] pid` reporting are identical to an external
    // command.
    execute_script_with_redirects(
        pipeline,
        &crate::engine::job_control::format_node_full(node),
        redirects,
        state,
    )
}

fn execute_script_with_redirects(
    pipeline: &Pipeline,
    script: &str,
    redirects: &[crate::parser::Redirect],
    state: &mut ShellState,
) -> (ExecutionResult, i32) {
    let mut command =
        Command::new(std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("cerf")));

    command.arg("-c").arg(script);

    if let Err(error) = apply_redirects_to_command(&mut command, redirects, state) {
        eprintln!("{}", error);
        return (ExecutionResult::KeepRunning, 1);
    }

    execute_process(command, "<block>", pipeline, state, None)
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
                // Function arguments become the positional parameters for
                // the duration of the call, and prefix assignments apply.
                let expanded = expand_args(&cmd.args, state);
                let args = expand_globs(&expanded);
                let assignments = expanded_assignments(&cmd.assignments, state);
                let saved_assignments = match apply_scoped_assignments(state, &assignments) {
                    Ok(saved) => saved,
                    Err(error) => {
                        eprintln!("{}", error);
                        return (ExecutionResult::KeepRunning, 1);
                    }
                };

                let saved_positional = state.positional_args.clone();
                state.set_positional_parameters(&args);
                let (res, code) = execute_list(func_body, state);
                state.set_positional_parameters(&saved_positional);
                restore_scoped_assignments(state, saved_assignments);

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
