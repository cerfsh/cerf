use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use jwalk::WalkDir;
use std::fmt::Write as _;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.du",
    description: "Estimate file space usage.",
    usage: "fs.du [path ...]\n\nSummarize disk usage of each FILE, recursively for directories.",
    run: runner,
};

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let targets = if args.is_empty() {
        vec![".".to_string()]
    } else {
        args.to_vec()
    };

    let mut exit_code = 0;
    let mut output = String::new();
    for target in targets {
        let path = expand_home(&target);
        if !path.exists() {
            eprintln!(
                "cerf: fs.du: cannot access '{}': No such file or directory",
                target
            );
            exit_code = 1;
            continue;
        }

        let mut total_size = 0;
        for entry in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_file()
                && let Ok(meta) = entry.metadata()
            {
                let size = meta.len();
                total_size += size;
                // Streaming: print each file size
                let _ = writeln!(
                    output,
                    "{}\t{}",
                    size.div_ceil(1024),
                    entry.path().display()
                );
            }
        }
        let _ = writeln!(output, "{}\ttotal", total_size.div_ceil(1024));
    }

    let mut stdout = std::io::stdout().lock();
    if let Err(error) = std::io::Write::write_all(&mut stdout, output.as_bytes()) {
        eprintln!("cerf: fs.du: write error: {}", error);
        return (ExecutionResult::KeepRunning, 1);
    }

    (ExecutionResult::KeepRunning, exit_code)
}
