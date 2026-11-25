use crate::builtins::registry::CommandInfo;
use crate::engine::path::expand_home;
use crate::engine::state::{ExecutionResult, ShellState};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};

pub const COMMAND_INFO: CommandInfo = CommandInfo {
    name: "fs.cat",
    description: "Concatenate and print files.",
    usage: "fs.cat [OPTION]... [FILE]...\n\nConcatenate FILE(s) to standard output. With no FILE, or when FILE is -, read standard input.\n\nOptions:\n  -n  Show line number\n  -b  Show line number if not blank\n  -e  Show a $ symbol in the end of line\n  -s  Squeeze consecutive empty lines",
    run: runner,
};

#[derive(Default)]
struct Options {
    show_line_num: bool,
    show_nonblank_num: bool,
    show_ends: bool,
    squeeze_blank: bool,
}

impl Options {
    fn fast_path(&self) -> bool {
        !self.show_line_num && !self.show_ends && !self.squeeze_blank
    }
}

pub fn runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let mut options = Options::default();
    let mut files = Vec::new();

    for arg in args {
        match arg.as_str() {
            "-n" => options.show_line_num = true,
            "-b" => {
                options.show_line_num = true;
                options.show_nonblank_num = true;
            }
            "-e" => options.show_ends = true,
            "-s" => options.squeeze_blank = true,
            _ => files.push(arg.as_str()),
        }
    }

    // POSIX cat: no file operands means standard input.
    if files.is_empty() {
        files.push("-");
    }

    let mut exit_code = 0;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut line_counter = 1;
    let mut prev_was_blank = false;

    for arg in files {
        let copied = if arg == "-" {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            copy_reader(
                &mut reader,
                "-",
                &mut handle,
                &options,
                &mut line_counter,
                &mut prev_was_blank,
            )
        } else {
            let path = expand_home(arg);
            match File::open(&path) {
                Ok(file) => {
                    let mut reader = BufReader::new(file);
                    copy_reader(
                        &mut reader,
                        arg,
                        &mut handle,
                        &options,
                        &mut line_counter,
                        &mut prev_was_blank,
                    )
                }
                Err(error) => {
                    eprintln!("cerf: fs.cat: {}: {}", arg, error);
                    false
                }
            }
        };
        if !copied {
            exit_code = 1;
        }
    }

    let _ = handle.flush();
    (ExecutionResult::KeepRunning, exit_code)
}

/// Copy one input to stdout, honoring line-numbering options.
fn copy_reader<R: BufRead>(
    reader: &mut R,
    label: &str,
    out: &mut impl Write,
    options: &Options,
    line_counter: &mut usize,
    prev_was_blank: &mut bool,
) -> bool {
    if options.fast_path() {
        return match io::copy(reader, out) {
            Ok(_) => true,
            Err(error) => {
                eprintln!("cerf: fs.cat: {}: {}", label, error);
                false
            }
        };
    }

    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => return true,
            Ok(_) => {
                let is_blank = buffer == b"\n" || buffer == b"\r\n";

                if options.squeeze_blank {
                    if is_blank {
                        if *prev_was_blank {
                            continue;
                        }
                        *prev_was_blank = true;
                    } else {
                        *prev_was_blank = false;
                    }
                }

                if options.show_line_num && (!options.show_nonblank_num || !is_blank) {
                    if let Err(error) = write!(out, "{:>6}\t", line_counter) {
                        eprintln!("cerf: fs.cat: {}: {}", label, error);
                        return false;
                    }
                    *line_counter += 1;
                }

                if options.show_ends {
                    if buffer.ends_with(b"\r\n") {
                        buffer.truncate(buffer.len() - 2);
                        buffer.extend_from_slice(b"$\r\n");
                    } else if buffer.ends_with(b"\n") {
                        buffer.truncate(buffer.len() - 1);
                        buffer.extend_from_slice(b"$\n");
                    } else {
                        buffer.push(b'$');
                    }
                }

                if let Err(error) = out.write_all(&buffer) {
                    eprintln!("cerf: fs.cat: {}: {}", label, error);
                    return false;
                }
            }
            Err(error) => {
                eprintln!("cerf: fs.cat: {}: {}", label, error);
                return false;
            }
        }
    }
}
