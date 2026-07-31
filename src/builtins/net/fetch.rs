use crate::engine::{ExecutionResult, ShellState};
use reqwest::blocking::get;
use std::io::{self, Write};

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo = crate::builtins::registry::CommandInfo {
    name: "net.fetch",
    description: "Fetches the contents of a URL.",
    usage: "net.fetch <url>",
    run: run_fetch,
};

fn run_fetch(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.len() != 1 {
        eprintln!("Usage: net.fetch <url>");
        return (ExecutionResult::Failure, 1);
    }

    let url = &args[0];
    match get(url) {
        Ok(response) => {
            if response.status().is_success() {
                match response.text() {
                    Ok(text) => {
                        io::stdout().write_all(text.as_bytes()).unwrap();
                        (ExecutionResult::Success, 0)
                    }
                    Err(e) => {
                        eprintln!("Failed to read response: {}", e);
                        (ExecutionResult::Failure, 1)
                    }
                }
            } else {
                eprintln!("Request failed with status: {}", response.status());
                (ExecutionResult::Failure, 1)
            }
        }
        Err(e) => {
            eprintln!("Failed to fetch URL: {}", e);
            (ExecutionResult::Failure, 1)
        }
    }
}
