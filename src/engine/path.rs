use std::path::{Path, PathBuf};

/// Expand a leading `~` to the home directory.
///
/// The rest of the path is deliberately left alone: collapsing `..` here
/// would resolve it lexically, while the kernel resolves it after following
/// symlinks (`link/../file` can name a different file than `file`). Redirect
/// targets and command paths must let the OS do the resolution.
pub fn expand_home(path_str: &str) -> PathBuf {
    if path_str == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if (path_str.starts_with("~/") || path_str.starts_with("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(&path_str[2..]);
    }
    Path::new(path_str).to_path_buf()
}

pub fn find_executable(cmd: &str) -> Option<PathBuf> {
    let cmd_path = expand_home(cmd);

    // 1. If it has a separator, check it directly
    if cmd.contains('/') || (cfg!(windows) && cmd.contains('\\')) {
        return check_path(cmd_path);
    }

    // 2. Search PATH
    if let Ok(paths) = std::env::var("PATH") {
        for path in std::env::split_paths(&paths) {
            if let Some(found) = check_path(path.join(cmd)) {
                return Some(found);
            }
        }
    }

    // 3. Search current directory on Windows (traditional behavior)
    #[cfg(windows)]
    {
        if let Ok(cwd) = std::env::current_dir()
            && let Some(found) = check_path(cwd.join(cmd))
        {
            return Some(found);
        }
    }

    None
}

fn check_path(p: PathBuf) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // A non-executable file earlier in PATH must not shadow a real
        // executable later in the list.
        if let Ok(metadata) = p.metadata()
            && metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
        {
            return Some(p);
        }
    }

    #[cfg(windows)]
    {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let pathext_list: Vec<_> = pathext
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_uppercase())
            .collect();

        // 1. If it already has an extension that's in PATHEXT, try it as is first.
        if let Some(ext) = p.extension() {
            let ext_dot = format!(".{}", ext.to_string_lossy().to_uppercase());
            if pathext_list.iter().any(|e| e == &ext_dot) && p.is_file() {
                return Some(p);
            }
        }

        // 2. Try appending extensions from PATHEXT.
        for ext in &pathext_list {
            let mut os_str = p.clone().into_os_string();
            if !ext.starts_with('.') {
                os_str.push(".");
            }
            os_str.push(ext);
            let p_ext = PathBuf::from(os_str);

            if p_ext.is_file() {
                return Some(p_ext);
            }
        }

        // 3. Fallback: try as-is only if we didn't already try it in step 1.
        // This handles extensionless files (though they might fail to exec).
        if p.is_file() {
            return Some(p);
        }
    }

    None
}
