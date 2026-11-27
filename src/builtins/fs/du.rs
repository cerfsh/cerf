use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use jwalk::WalkDir;
use std::collections::HashSet;
use std::fmt::Write as _;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.du",
    description: "Estimate file space usage.",
    usage: "fs.du [path ...]\n\nSummarize disk usage of each FILE, recursively for directories.",
    run: runner,
};

/// Space actually allocated to a file, in bytes. GNU `du` reports allocated
/// blocks by default, not the logical size, so sparse files count as empty.
#[cfg(unix)]
fn allocated_size(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_size(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

/// Identity of a file for hard-link accounting.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let targets = if args.is_empty() {
        vec![".".to_string()]
    } else {
        args.to_vec()
    };

    let mut exit_code = 0;
    let mut output = String::new();
    // A hard-linked inode contributes its blocks exactly once, even when the
    // same file is reached through multiple names or operands.
    let mut seen = HashSet::new();
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
        for entry in WalkDir::new(&path).into_iter() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    eprintln!("cerf: fs.du: {}", error);
                    exit_code = 1;
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    eprintln!(
                        "cerf: fs.du: cannot read metadata for '{}': {}",
                        entry.path().display(),
                        error
                    );
                    exit_code = 1;
                    continue;
                }
            };
            if let Some(identity) = file_identity(&metadata)
                && !seen.insert(identity)
            {
                continue;
            }

            let size = allocated_size(&metadata);
            total_size += size;
            // Streaming: print each file size.
            let _ = writeln!(
                output,
                "{}\t{}",
                size.div_ceil(1024),
                entry.path().display()
            );
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
