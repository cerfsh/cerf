use std::collections::HashMap;
use std::fs::{File, OpenOptions};

use super::path::expand_home;
use crate::engine::state::Variable;
use crate::parser::{Redirect, RedirectKind, expand_vars};

/// Open a file for an output redirect.
pub fn open_stdout_redirect(redirect: &Redirect) -> Result<File, String> {
    match redirect.kind {
        RedirectKind::StdoutOverwrite => {
            let path = expand_home(&redirect.file);
            File::create(&path).map_err(|e| format!("cerf: {}: {}", path.display(), e))
        }
        RedirectKind::StdoutAppend => {
            let path = expand_home(&redirect.file);
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("cerf: {}: {}", path.display(), e))
        }
        _ => Err("not a stdout redirect".to_string()),
    }
}

/// Open a file for an input redirect.
pub fn open_stdin_redirect(redirect: &Redirect) -> Result<File, String> {
    let path = expand_home(&redirect.file);
    File::open(&path).map_err(|e| format!("cerf: {}: {}", path.display(), e))
}

/// One already-opened redirection, in source order.
pub struct PreparedRedirect {
    /// Descriptor being replaced.
    pub fd: i32,
    pub source: RedirectSource,
}

pub enum RedirectSource {
    File(File),
    /// `dup2(target, fd)`.
    Dup(i32),
}

/// Expand and open every redirect from left to right.
///
/// All files are opened even when the same descriptor is redirected more than
/// once (`cmd >a >b` truncates both, then leaves stdout on `b`), matching
/// POSIX redirection order. The caller applies the returned list in order.
pub fn prepare_redirects(
    redirects: &[Redirect],
    variables: &HashMap<String, Variable>,
) -> Result<Vec<PreparedRedirect>, String> {
    let mut prepared = Vec::with_capacity(redirects.len());
    for redirect in redirects {
        match redirect.kind {
            RedirectKind::FdDup => {
                let target = redirect.file.parse::<i32>().map_err(|_| {
                    format!(
                        "cerf: redirect: invalid file descriptor '{}'",
                        redirect.file
                    )
                })?;
                prepared.push(PreparedRedirect {
                    fd: redirect.fd,
                    source: RedirectSource::Dup(target),
                });
            }
            RedirectKind::StdoutOverwrite | RedirectKind::StdoutAppend => {
                let mut expanded = redirect.clone();
                expanded.file = expand_vars(&redirect.file, variables);
                prepared.push(PreparedRedirect {
                    fd: redirect.fd,
                    source: RedirectSource::File(open_stdout_redirect(&expanded)?),
                });
            }
            RedirectKind::StdinFrom => {
                let mut expanded = redirect.clone();
                expanded.file = expand_vars(&redirect.file, variables);
                prepared.push(PreparedRedirect {
                    fd: redirect.fd,
                    source: RedirectSource::File(open_stdin_redirect(&expanded)?),
                });
            }
        }
    }
    Ok(prepared)
}

/// Open and close redirect files without changing any descriptor.
///
/// Used for commands that consist only of assignments: `VAR=x > file` still
/// creates/truncates `file` in a real shell.
pub fn touch_redirect_files(
    redirects: &[Redirect],
    variables: &HashMap<String, Variable>,
) -> Result<(), String> {
    drop(prepare_redirects(redirects, variables)?);
    Ok(())
}

/// Describe a redirect for display (`2>`, `2>&1`, …).
pub fn format_redirect(redirect: &Redirect) -> String {
    match redirect.kind {
        RedirectKind::FdDup => format!("{}>&{}", redirect.fd, redirect.file),
        RedirectKind::StdoutOverwrite => {
            let operator = if redirect.fd == 1 {
                ">".to_string()
            } else {
                format!("{}>", redirect.fd)
            };
            format!("{} {}", operator, redirect.file)
        }
        RedirectKind::StdoutAppend => {
            let operator = if redirect.fd == 1 {
                ">>".to_string()
            } else {
                format!("{}>>", redirect.fd)
            };
            format!("{} {}", operator, redirect.file)
        }
        RedirectKind::StdinFrom => {
            let operator = if redirect.fd == 0 {
                "<".to_string()
            } else {
                format!("{}<", redirect.fd)
            };
            format!("{} {}", operator, redirect.file)
        }
    }
}
