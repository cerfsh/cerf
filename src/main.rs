mod builtins;
mod engine;
mod parser;
mod signals;

use engine::ShellState;
use rustyline::error::ReadlineError;
use rustyline::{Editor, history::DefaultHistory};
use std::env;
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;

/// Foreground job tracked for the Windows console control handler. On Windows
/// this is the process-group leader PID targeted by Ctrl-C forwarding; it is
/// unused on Unix, where the foreground job owns the terminal directly.
pub static FG_JOB: AtomicUsize = AtomicUsize::new(0);

/// Parsed command-line invocation.
enum Cli {
    InternalBuiltin { name: String, args: Vec<String> },
    Script {
        script: String,
        positional: Vec<String>,
    },
    Version,
    Interactive,
}

fn parse_cli(args: &[String]) -> Result<Cli, String> {
    let Some(first) = args.get(1) else {
        return Ok(Cli::Interactive);
    };

    match first.as_str() {
        "--internal-builtin" => {
            let Some(name) = args.get(2) else {
                return Err("--internal-builtin requires a builtin name".to_string());
            };
            Ok(Cli::InternalBuiltin {
                name: name.clone(),
                args: args[3..].to_vec(),
            })
        }
        "-c" => {
            let Some(script) = args.get(2) else {
                return Err("-c requires a script".to_string());
            };
            // Everything after the script becomes the positional parameters
            // ($1, $2, ...) for this invocation.
            Ok(Cli::Script {
                script: script.clone(),
                positional: args[3..].to_vec(),
            })
        }
        "--version" => {
            if args.len() != 2 {
                return Err("--version takes no arguments".to_string());
            }
            Ok(Cli::Version)
        }
        option if option.starts_with('-') => Err(format!("unknown option: {}", option)),
        operand => Err(format!("unexpected argument: {}", operand)),
    }
}

fn main() -> ExitCode {
    // Non-Unicode arguments are converted lossily instead of panicking, so
    // the shell still starts when a caller passes arbitrary bytes.
    let args: Vec<String> = env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let cli = match parse_cli(&args) {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("cerf: {}", error);
            eprintln!("Usage: cerf [-c SCRIPT [args...]] [--version]");
            return ExitCode::from(2);
        }
    };

    match cli {
        Cli::InternalBuiltin { name, args } => {
            return ExitCode::from(run_internal_builtin(&name, &args) as u8);
        }
        Cli::Version => {
            println!("v{}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Cli::Script { script, positional } => {
            // Without this, SIGTTOU keeps its default action and the child's
            // `tcsetpgrp` stops it, hanging `-c` on a controlling terminal.
            signals::init();
            let mut state = ShellState::new();
            state.set_positional_parameters(&positional);
            let code = match parser::parse_pipeline(&script, &state.variables) {
                Some(entries) => engine::execute_list(entries, &mut state).1,
                None if parser::is_blank_or_comment_only(&script) => 0,
                None => 2,
            };
            return ExitCode::from(code as u8);
        }
        Cli::Interactive => {}
    }

    signals::init();

    // Initialize job control
    #[cfg(unix)]
    {
        let pid = nix::unistd::getpid();
        let _ = nix::unistd::setpgid(pid, pid);
        let _ = nix::unistd::tcsetpgrp(
            unsafe { std::os::fd::BorrowedFd::borrow_raw(nix::libc::STDIN_FILENO) },
            pid,
        );
    }

    let mut state = ShellState::new();

    #[cfg(unix)]
    {
        state.shell_pgid = Some(nix::unistd::Pid::from_raw(nix::unistd::getpid().as_raw()));
    }

    // Source the user profile (~/.cerfrc) for interactive sessions.
    source_profile(&mut state);

    let config = rustyline::Config::builder().bracketed_paste(true).build();
    let mut rl = match Editor::<(), DefaultHistory>::with_config(config) {
        Ok(editor) => editor,
        Err(error) => {
            eprintln!("cerf: {}", error);
            return ExitCode::FAILURE;
        }
    };

    #[cfg(windows)]
    {
        let (tx, rx) = std::sync::mpsc::channel::<engine::job_control::IocpMessage>();
        state.iocp_receiver = Some(rx);
        let handle = state.iocp_handle;

        std::thread::spawn(move || {
            use windows_sys::Win32::System::IO::GetQueuedCompletionStatus;

            loop {
                let mut num_bytes = 0;
                let mut comp_key = 0;
                let mut overlapped = std::ptr::null_mut();

                let res = unsafe {
                    GetQueuedCompletionStatus(
                        handle as _,
                        &mut num_bytes,
                        &mut comp_key,
                        &mut overlapped,
                        windows_sys::Win32::System::Threading::INFINITE,
                    )
                };

                if res != 0 {
                    let msg = num_bytes;
                    let event_job_id = comp_key;
                    let pid = overlapped as usize as u32;

                    let _ = tx.send(engine::job_control::IocpMessage {
                        msg,
                        job_id: event_job_id,
                        pid,
                    });
                }
            }
        });
    }

    let mut input_buffer = String::new();
    let mut shell_status = 0u8;
    loop {
        // Poll for any background jobs that have finished.
        engine::job_control::update_jobs(&mut state);

        // Ensure shell owns the terminal
        #[cfg(unix)]
        engine::job_control::restore_terminal(&state);

        let prompt = if input_buffer.is_empty() {
            expand_prompt(
                &state
                    .get_var_string("PS1")
                    .unwrap_or_else(|| "\\u@\\h:\\w\\$ ".to_string()),
            )
        } else {
            expand_prompt(
                &state
                    .get_var_string("PS2")
                    .unwrap_or_else(|| "> ".to_string()),
            )
        };

        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let trimmed = line.trim_end();

                // Explicit comma continuation (kept for backwards compat).
                if let Some(stripped) = trimmed.strip_suffix(',') {
                    input_buffer.push_str(stripped);
                    continue;
                }

                if input_buffer.is_empty() {
                    input_buffer.push_str(&line);
                } else {
                    // Separate accumulated lines with a real newline so
                    // the parser sees them as distinct commands.
                    input_buffer.push('\n');
                    input_buffer.push_str(&line);
                }

                // If the input looks incomplete (unbalanced braces, trailing
                // operator, etc.), keep reading the next line.
                if parser::is_incomplete(&input_buffer) {
                    continue;
                }

                let input = input_buffer.trim().to_string();
                input_buffer.clear();

                if input.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&input);
                state.add_history(&input);

                if let Some(entries) = parser::parse_pipeline(&input, &state.variables) {
                    let (result, code) = engine::execute_list(entries, &mut state);
                    if let engine::ExecutionResult::Exit = result {
                        shell_status = code as u8;
                        break;
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                input_buffer.clear();
                continue;
            }
            Err(ReadlineError::Eof) => {
                if !input_buffer.is_empty() {
                    let input = input_buffer.trim().to_string();
                    input_buffer.clear();
                    if let Some(entries) = parser::parse_pipeline(&input, &state.variables) {
                        let _ = engine::execute_list(entries, &mut state);
                    }
                }
                println!("exit");
                break;
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                break;
            }
        }
    }
    ExitCode::from(shell_status)
}

fn run_internal_builtin(name: &str, args: &[String]) -> i32 {
    let Some(command) = builtins::registry::find_command(name) else {
        eprintln!("cerf: {}: not a shell builtin", name);
        return 127;
    };
    let mut state = ShellState::new();
    state.internal_builtin = true;
    if let Some(path) = env::var_os(engine::state::SNAPSHOT_ENV) {
        // SAFETY: called before this process starts any threads.
        unsafe { env::remove_var(engine::state::SNAPSHOT_ENV) };
        let mut applied = false;
        match std::fs::read(&path) {
            Ok(bytes) => {
                if let Some(assignments) = state.apply_snapshot(&bytes) {
                    applied = true;
                    // Preserve the snapshot values of unexported variables
                    // when merging the inherited environment.
                    for (key, value) in env::vars_os() {
                        let key = key.to_string_lossy().into_owned();
                        let value = value.to_string_lossy().into_owned();
                        match state.variables.get_mut(&key) {
                            Some(var) if var.exported => {
                                var.value = engine::state::VarValue::String(value);
                            }
                            Some(_) => {}
                            None => {
                                let mut var = engine::state::Variable::new_string(value);
                                var.exported = true;
                                state.variables.insert(key, var);
                            }
                        }
                    }
                    for (key, value) in assignments {
                        let var = state
                            .variables
                            .entry(key)
                            .or_insert_with(|| engine::state::Variable::new_string(String::new()));
                        var.value = engine::state::VarValue::String(value);
                        var.exported = true;
                    }
                } else {
                    eprintln!(
                        "cerf: warning: ignoring state snapshot not created by cerf: {}",
                        path.display()
                    );
                }
            }
            Err(error) => eprintln!("cerf: warning: could not read state snapshot: {}", error),
        }
        // Only remove a file whose contents proved it was our snapshot; an
        // inherited or stale variable value must be left untouched.
        if applied {
            let _ = std::fs::remove_file(&path);
        }
    }
    (command.run)(args, &mut state).1
}

/// Source `~/.cerfrc` if it exists.
fn source_profile(state: &mut ShellState) {
    if let Some(home) = dirs::home_dir() {
        let rc_path = home.join(".cerfrc");
        if rc_path.exists() {
            let path_str = rc_path.to_string_lossy().to_string();
            builtins::source::run(&[path_str], state);
        }
    }
}

fn expand_prompt(prompt: &str) -> String {
    let mut result = prompt.to_string();

    if result.contains("\\u") {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".to_string());
        result = result.replace("\\u", &user);
    }

    if result.contains("\\h") {
        let host = sysinfo::System::host_name().unwrap_or_else(|_| "localhost".to_string());
        let short_host = host.split('.').next().unwrap_or("localhost");
        result = result.replace("\\h", short_host);
    }

    if result.contains("\\w") {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut cwd_str = cwd.display().to_string();
        if let Some(home) = dirs::home_dir()
            && cwd.starts_with(&home)
            && let Ok(relative) = cwd.strip_prefix(&home)
        {
            cwd_str = if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
            };
        }
        result = result.replace("\\w", &cwd_str);
    }

    if result.contains("\\$") {
        let is_root = std::env::var("UID").unwrap_or_default() == "0";
        result = result.replace("\\$", if is_root { "#" } else { "$" });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, String> {
        let mut argv = vec!["cerf".to_string()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        parse_cli(&argv)
    }

    #[test]
    fn parses_script_invocations_and_positional_arguments() {
        match parse(&["-c", "io.echo $1", "one", "two"]).expect("parse") {
            Cli::Script { script, positional } => {
                assert_eq!(script, "io.echo $1");
                assert_eq!(positional, vec!["one".to_string(), "two".to_string()]);
            }
            _ => panic!("expected script invocation"),
        }
    }

    #[test]
    fn rejects_malformed_invocations() {
        assert!(parse(&["-c"]).is_err());
        assert!(parse(&["--version", "extra"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["script.cerf"]).is_err());
        assert!(parse(&["--internal-builtin"]).is_err());
        assert!(matches!(parse(&["--version"]), Ok(Cli::Version)));
        assert!(matches!(parse(&[]), Ok(Cli::Interactive)));
    }
}
