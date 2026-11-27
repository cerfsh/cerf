use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use std::fs;
use std::path::{Component, Path};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.rm",
    description: "Remove files or directories.",
    usage: "fs.rm [-r] [file ...]\n\nRemove (unlink) the FILE(s).",
    run: runner,
};

/// Return the refusal reason for operands that delete more than the user named.
/// `remove_dir_all` follows `.` and `..`, so both must be rejected before any
/// deletion starts, and recursive roots have no safe use.
fn refusal(arg: &str, recursive: bool) -> Option<String> {
    // Check the operand text, not the normalized path: `expand_home` resolves
    // `.` and `..`, and `Path::components` drops a trailing `.` as well.
    let trimmed = arg.trim_end_matches(std::path::is_separator);
    let last = trimmed.rsplit(std::path::is_separator).next().unwrap_or("");
    if last == "." || last == ".." {
        return Some(format!(
            "refusing to remove '.' or '..' directory: skipping '{arg}'"
        ));
    }

    if recursive && Path::new(arg).components().next_back() == Some(Component::RootDir) {
        return Some(format!("it is dangerous to operate recursively on '{arg}'"));
    }

    None
}

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.is_empty() {
        eprintln!("cerf: fs.rm: missing operand");
        return (ExecutionResult::KeepRunning, 1);
    }

    let mut recursive = false;
    let mut files = Vec::new();

    for arg in args {
        if arg == "-r" || arg == "-R" {
            recursive = true;
        } else {
            files.push(arg);
        }
    }

    if files.is_empty() {
        eprintln!("cerf: fs.rm: missing operand");
        return (ExecutionResult::KeepRunning, 1);
    }

    let mut exit_code = 0;
    for arg in files {
        let path = expand_home(arg);
        if let Some(reason) = refusal(arg, recursive) {
            eprintln!("cerf: fs.rm: {}", reason);
            exit_code = 1;
            continue;
        }
        // Inspect the link itself, not its target: `exists()` and `is_dir()`
        // follow symlinks, which made dangling links look missing and sent
        // links to directories into `remove_dir` (ENOTDIR).
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) => {
                eprintln!("cerf: fs.rm: cannot remove '{}': {}", arg, e);
                exit_code = 1;
                continue;
            }
        };

        let res = if metadata.file_type().is_dir() {
            if recursive {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_dir(&path)
            }
        } else {
            fs::remove_file(&path)
        };

        if let Err(e) = res {
            eprintln!("cerf: fs.rm: cannot remove '{}': {}", arg, e);
            exit_code = 1;
        }
    }
    (ExecutionResult::KeepRunning, exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_dot_dotdot_and_recursive_root() {
        for (path, recursive) in [
            (".", false),
            ("..", false),
            ("a/.", true),
            ("a/..", true),
            ("/", true),
        ] {
            assert!(
                refusal(path, recursive).is_some(),
                "{path:?} should be refused"
            );
        }

        assert!(refusal("a", true).is_none());
        assert!(refusal("/", false).is_none());
    }

    #[test]
    fn refuses_dot_paths_without_deleting_anything() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let parent = directory.path().join("a");
        let child = parent.join("b");
        std::fs::create_dir_all(&child).expect("create directories");
        std::fs::write(child.join("keep.txt"), b"data").expect("write file");

        let dot = parent.join(".");
        let dotdot = parent.join("..");
        let mut state = ShellState::new();
        for operand in [&dot, &dotdot] {
            let args = vec!["-r".to_string(), operand.to_string_lossy().to_string()];
            let (_, code) = runner(&args, &mut state);
            assert_eq!(code, 1);
        }

        assert!(child.join("keep.txt").exists());
        assert!(parent.exists());
    }
}
