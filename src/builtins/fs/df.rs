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

/// Return `(total, used, available)` in bytes.
///
/// GNU `df` computes Used from the total and *free* block counts, while Avail
/// comes from the unprivileged available count. Using `available` for both
/// counts reserved blocks as used.
#[cfg(unix)]
fn disk_space(disk: &sysinfo::Disk) -> Option<(u64, u64, u64)> {
    let stats = nix::sys::statvfs::statvfs(disk.mount_point()).ok()?;
    let fragment = stats.fragment_size() as u64;
    let total = (stats.blocks() as u64).saturating_mul(fragment);
    let free = (stats.blocks_free() as u64).saturating_mul(fragment);
    let available = (stats.blocks_available() as u64).saturating_mul(fragment);
    Some((total, total.saturating_sub(free), available))
}

#[cfg(not(unix))]
fn disk_space(disk: &sysinfo::Disk) -> Option<(u64, u64, u64)> {
    let total = disk.total_space();
    let available = disk.available_space();
    Some((total, total.saturating_sub(available), available))
}

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
        let (total, used, available) = disk_space(disk).unwrap_or_else(|| {
            let total = disk.total_space();
            let available = disk.available_space();
            (total, total.saturating_sub(available), available)
        });
        let use_pct = if total > 0 {
            // GNU df rounds the capacity up, so a nearly-empty filesystem
            // still shows 1% rather than 0%.
            ((used as f64 / total as f64) * 100.0).ceil() as u64
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
