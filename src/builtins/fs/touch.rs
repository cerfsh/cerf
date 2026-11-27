use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use std::fs;
use std::path::Path;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.touch",
    description: "Update the access and modification times of each FILE to the current time.",
    usage: "fs.touch [file ...]\n\nUpdate the access and modification times of each FILE to the current time. A FILE argument that does not exist is created empty.",
    run: runner,
};

/// Set both timestamps of an existing path without opening it for writing.
///
/// Opening a file does not update its timestamps, and opening a FIFO for
/// writing blocks until a reader appears. `utimensat` changes the times
/// directly, works on directories and read-only files, and never blocks.
#[cfg(unix)]
fn touch_existing(path: &Path) -> std::io::Result<()> {
    use nix::sys::stat::{UtimensatFlags, utimensat};
    use nix::sys::time::TimeSpec;

    utimensat(
        nix::fcntl::AT_FDCWD,
        path,
        &TimeSpec::UTIME_NOW,
        &TimeSpec::UTIME_NOW,
        UtimensatFlags::FollowSymlink,
    )
    .map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn touch_existing(path: &Path) -> std::io::Result<()> {
    use std::time::SystemTime;

    let file = fs::OpenOptions::new().read(true).open(path)?;
    let now = SystemTime::now();
    file.set_times(fs::FileTimes::new().set_accessed(now).set_modified(now))
}

fn touch(path: &Path) -> std::io::Result<()> {
    match touch_existing(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // `touch new-file` creates it; the creation already sets the
            // timestamps to now.
            fs::File::create(path).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.is_empty() {
        eprintln!("cerf: fs.touch: missing file operand");
        return (ExecutionResult::KeepRunning, 1);
    }

    let mut exit_code = 0;
    for arg in args {
        let path = expand_home(arg);
        if let Err(e) = touch(&path) {
            eprintln!("cerf: fs.touch: cannot touch '{}': {}", arg, e);
            exit_code = 1;
        }
    }
    (ExecutionResult::KeepRunning, exit_code)
}
