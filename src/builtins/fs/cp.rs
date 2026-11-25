use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use same_file::is_same_file;
use std::fs;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.cp",
    description: "Copy files and directories.",
    usage: "fs.cp source destination\n\nCopy SOURCE to DEST.",
    run: runner,
};

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.len() < 2 {
        eprintln!("cerf: fs.cp: missing file operand");
        return (ExecutionResult::KeepRunning, 1);
    }

    let src = expand_home(&args[0]);
    let dst = expand_home(&args[1]);

    // `fs::copy` truncates the destination before reading the source, so a
    // same-file copy (direct, hard link, or symlink) would destroy the source.
    if is_same_file(&src, &dst).unwrap_or(false) {
        eprintln!(
            "cerf: fs.cp: '{}' and '{}' are the same file",
            args[0], args[1]
        );
        return (ExecutionResult::KeepRunning, 1);
    }

    if let Err(e) = fs::copy(&src, &dst) {
        eprintln!(
            "cerf: fs.cp: cannot copy '{}' to '{}': {}",
            args[0], args[1], e
        );
        return (ExecutionResult::KeepRunning, 1);
    }

    (ExecutionResult::KeepRunning, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> i32 {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        runner(&args, &mut ShellState::new()).1
    }

    #[test]
    fn refuses_to_copy_a_file_onto_itself() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("f");
        std::fs::write(&file, b"important").expect("write file");
        let path = file.to_string_lossy().to_string();

        assert_eq!(run(&[&path, &path]), 1);
        assert_eq!(std::fs::read(&file).expect("read file"), b"important");
    }

    #[test]
    fn refuses_to_copy_over_a_hard_link() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let src = directory.path().join("f");
        let dst = directory.path().join("g");
        std::fs::write(&src, b"important").expect("write file");
        std::fs::hard_link(&src, &dst).expect("create hard link");
        let src = src.to_string_lossy().to_string();
        let dst = dst.to_string_lossy().to_string();

        assert_eq!(run(&[&src, &dst]), 1);
        assert_eq!(std::fs::read(&src).expect("read source"), b"important");
        assert_eq!(std::fs::read(&dst).expect("read link"), b"important");
    }
}
