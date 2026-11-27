use super::utils::format_size;
use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};
use std::fmt::Write as _;
use sysinfo::Disks;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.df",
    description: "Report file system disk space usage.",
    usage: "fs.df\n\nShow information about the file system on which each FILE resides.",
    run: runner,
};

pub fn runner(_args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let disks = match Disks::new_with_refreshed_list() {
        Ok(disks) => disks,
        Err(error) => {
            eprintln!("fs.df: {}", error);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    let mut output = String::new();
    let _ = writeln!(
        output,
        "{:<20} {:<10} {:<10} {:<10} {:<5} Mounted on",
        "Filesystem", "Size", "Used", "Avail", "Use%"
    );

    for disk in &disks {
        let total = disk.total_space();
        let available = disk.available_space();
        let used = total - available;
        let use_pct = if total > 0 {
            (used as f64 / total as f64 * 100.0) as u64
        } else {
            0
        };

        let _ = writeln!(
            output,
            "{:<20} {:<10} {:<10} {:<10} {:>3}% {}",
            disk.name().to_string_lossy(),
            format_size(total),
            format_size(used),
            format_size(available),
            use_pct,
            disk.mount_point().display()
        );
    }

    let mut stdout = std::io::stdout().lock();
    if let Err(error) = std::io::Write::write_all(&mut stdout, output.as_bytes()) {
        eprintln!("cerf: fs.df: write error: {}", error);
        return (ExecutionResult::KeepRunning, 1);
    }

    (ExecutionResult::KeepRunning, 0)
}
