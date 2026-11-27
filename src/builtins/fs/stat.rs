use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use chrono::{DateTime, Local};
use std::fmt::Write as _;
use std::fs;

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.stat",
    description: "Display file or file system status.",
    usage: "fs.stat [-L] [file ...]\n\nDisplay file or file system status. Symbolic links are\nreported as links unless -L (dereference) is given.",
    run: runner,
};

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let mut dereference = false;
    let mut files = Vec::new();
    let mut parse_options = true;
    for arg in args {
        if parse_options && arg == "--" {
            parse_options = false;
        } else if parse_options && arg == "-L" {
            dereference = true;
        } else if parse_options && arg.starts_with('-') && arg.len() > 1 {
            eprintln!("cerf: fs.stat: unknown option: {}", arg);
            return (ExecutionResult::KeepRunning, 1);
        } else {
            files.push(arg);
        }
    }

    if files.is_empty() {
        eprintln!("cerf: fs.stat: missing operand");
        return (ExecutionResult::KeepRunning, 1);
    }

    let mut exit_code = 0;
    let mut output = String::new();
    for arg in files {
        let path = expand_home(arg);
        let metadata = if dereference {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        match metadata {
            Ok(meta) => {
                let file_type = meta.file_type();
                let kind = if file_type.is_symlink() {
                    "symbolic link"
                } else if meta.is_dir() {
                    "directory"
                } else if meta.is_file() {
                    "regular file"
                } else {
                    "special file"
                };

                match (!dereference)
                    .then(|| fs::read_link(&path).ok())
                    .flatten()
                {
                    Some(target) => {
                        let _ = writeln!(
                            output,
                            "  File: {} -> {}",
                            arg,
                            target.to_string_lossy()
                        );
                    }
                    None => {
                        let _ = writeln!(output, "  File: {}", arg);
                    }
                }

                let _ = writeln!(
                    output,
                    "  Size: {:<15} Blocks: {:<10} IO Block: {:<10} {}",
                    meta.len(),
                    meta.len().div_ceil(512), // Rough estimation of 512-byte blocks
                    4096,                     // IO block size (typical)
                    kind
                );

                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    let file_attributes = meta.file_attributes();
                    let _ = writeln!(
                        output,
                        "Device: unknown         Inode: unknown         Links: unknown"
                    );
                    let _ = writeln!(
                        output,
                        "Access: ({:o})  Uid: unknown   Gid: unknown",
                        file_attributes & 0o777
                    );
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let _ = writeln!(
                        output,
                        "Device: {:<15x} Inode: {:<15} Links: {}",
                        meta.dev(),
                        meta.ino(),
                        meta.nlink()
                    );
                    let _ = writeln!(
                        output,
                        "Access: ({:04o})  Uid: ({:5})   Gid: ({:5})",
                        meta.mode() & 0o7777,
                        meta.uid(),
                        meta.gid()
                    );
                }

                #[cfg(not(any(windows, unix)))]
                {
                    let _ = writeln!(
                        output,
                        "Device: unknown         Inode: unknown         Links: unknown"
                    );
                    let _ = writeln!(
                        output,
                        "Access: unknown         Uid: unknown           Gid: unknown"
                    );
                }

                if let Ok(atime) = meta.accessed() {
                    let dt: DateTime<Local> = atime.into();
                    let _ = writeln!(output, "Access: {}", dt.format("%Y-%m-%d %H:%M:%S.%f %z"));
                }
                if let Ok(mtime) = meta.modified() {
                    let dt: DateTime<Local> = mtime.into();
                    let _ = writeln!(output, "Modify: {}", dt.format("%Y-%m-%d %H:%M:%S.%f %z"));
                }
                if let Ok(ctime) = meta.created() {
                    let dt: DateTime<Local> = ctime.into();
                    let _ = writeln!(output, "Birth:  {}", dt.format("%Y-%m-%d %H:%M:%S.%f %z"));
                }
            }
            Err(e) => {
                eprintln!("cerf: fs.stat: cannot stat '{}': {}", arg, e);
                exit_code = 1;
            }
        }
    }

    let mut stdout = std::io::stdout().lock();
    if let Err(error) = std::io::Write::write_all(&mut stdout, output.as_bytes()) {
        eprintln!("cerf: fs.stat: write error: {}", error);
        return (ExecutionResult::KeepRunning, 1);
    }

    (ExecutionResult::KeepRunning, exit_code)
}
