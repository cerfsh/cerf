use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Running,
    Stopped,
    Done(i32),
}

impl std::fmt::Display for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobState::Running => write!(f, "Running"),
            JobState::Stopped => write!(f, "Stopped"),
            JobState::Done(code) => write!(f, "Done({})", code),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub state: JobState,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    #[cfg(unix)]
    pub pgid: u32,
    #[cfg(windows)]
    pub job_handle: isize,
    pub command: String,
    pub processes: Vec<ProcessInfo>,
    pub reported_done: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VarValue {
    String(String),
    Array(Vec<String>),
}

impl VarValue {
    pub fn as_string(&self) -> String {
        match self {
            VarValue::String(s) => s.clone(),
            VarValue::Array(a) => a.join(" "),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Variable {
    pub value: VarValue,
    pub readonly: bool,
    pub integer: bool,
    pub exported: bool,
}

impl Variable {
    pub fn new_string(val: String) -> Self {
        Self {
            value: VarValue::String(val),
            readonly: false,
            integer: false,
            exported: false,
        }
    }

    pub fn new_array(val: Vec<String>) -> Self {
        Self {
            value: VarValue::Array(val),
            readonly: false,
            integer: false,
            exported: false,
        }
    }
}

impl Job {
    pub fn is_stopped(&self) -> bool {
        let all_suspended = self
            .processes
            .iter()
            .all(|p| matches!(p.state, JobState::Stopped | JobState::Done(_)));
        let any_stopped = self
            .processes
            .iter()
            .any(|p| matches!(p.state, JobState::Stopped));
        all_suspended && any_stopped
    }

    pub fn is_done(&self) -> bool {
        self.processes
            .iter()
            .all(|p| matches!(p.state, JobState::Done(_)))
    }

    pub fn state(&self) -> JobState {
        if self.is_done() {
            // Find last process exit code
            let code = self
                .processes
                .last()
                .map(|p| match p.state {
                    JobState::Done(c) => c,
                    _ => 0,
                })
                .unwrap_or(0);
            JobState::Done(code)
        } else if self.is_stopped() {
            JobState::Stopped
        } else {
            JobState::Running
        }
    }
}

/// Environment variable holding the path of a launch-time state snapshot
/// handed to an internal-builtin child process.
pub const SNAPSHOT_ENV: &str = "CERF_INTERNAL_STATE";

pub struct ShellState {
    pub previous_dir: Option<PathBuf>,
    pub dir_stack: Vec<PathBuf>,
    /// All currently-defined aliases. Maps alias name → replacement string.
    pub aliases: HashMap<String, String>,
    /// Global shell variables.
    pub variables: HashMap<String, Variable>,
    /// Local variable scopes.
    pub scopes: Vec<HashMap<String, Variable>>,
    /// Defined shell functions.
    pub functions: HashMap<String, Vec<crate::parser::CommandEntry>>,
    /// Positional arguments ($1, $2, etc.). First element is $1.
    pub positional_args: Vec<String>,
    /// Shell options enabled via `set -o` / `set -e` etc.
    pub set_options: HashSet<String>,
    /// Command history (persisted to `~/.cerf_history`).
    pub history: Vec<String>,
    /// True when this process was launched as an `--internal-builtin` child.
    /// Such children must not take ownership of a job-control group or the
    /// terminal, since they may already be a background process group.
    pub internal_builtin: bool,

    // Job control
    pub jobs: HashMap<usize, Job>,
    pub next_job_id: usize,
    pub current_job: Option<usize>,
    pub previous_job: Option<usize>,
    #[cfg(unix)]
    pub shell_pgid: Option<nix::unistd::Pid>,
    #[cfg(unix)]
    pub shell_term: Option<std::os::fd::RawFd>,
    #[cfg(windows)]
    pub iocp_handle: isize,
    #[cfg(windows)]
    pub iocp_receiver: Option<std::sync::mpsc::Receiver<crate::engine::job_control::IocpMessage>>,
}

impl ShellState {
    pub fn new() -> Self {
        let mut variables = HashMap::new();
        for (k, v) in init_env_vars() {
            let mut var = Variable::new_string(v);
            var.exported = true;
            variables.insert(k, var);
        }

        let mut state = ShellState {
            previous_dir: None,
            dir_stack: Vec::new(),
            aliases: init_default_aliases(),
            variables,
            scopes: Vec::new(),
            functions: HashMap::new(),
            positional_args: Vec::new(),
            set_options: HashSet::new(),
            history: Vec::new(),
            internal_builtin: false,
            jobs: HashMap::new(),
            next_job_id: 1,
            current_job: None,
            previous_job: None,
            #[cfg(unix)]
            shell_pgid: None,
            #[cfg(unix)]
            shell_term: Some(nix::libc::STDIN_FILENO),
            #[cfg(windows)]
            iocp_handle: unsafe {
                windows_sys::Win32::System::IO::CreateIoCompletionPort(
                    windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE,
                    std::ptr::null_mut(),
                    0,
                    1,
                ) as isize
            },
            #[cfg(windows)]
            iocp_receiver: None,
        };
        state.load_history();
        state
    }

    /// Set a variable in the current scope.
    pub fn set_var(&mut self, name: &str, value: Variable) {
        if self.scopes.is_empty() {
            self.variables.insert(name.to_string(), value);
        } else {
            let last_idx = self.scopes.len() - 1;
            // Only insert into global if it was already global and not local,
            // but typical bash behavior is to create local if requested by `local`,
            // otherwise assignment modifies existing or creates global.
            // For now, simple approach: set in current scope.
            // If we are setting a variable and it's not local, usually
            // we'd walk up scopes. But standard simple approach:

            // Check if it exists in local scope already
            if self.scopes[last_idx].contains_key(name) {
                self.scopes[last_idx].insert(name.to_string(), value);
            } else {
                // Bash normally sets variables globally if they aren't declared local.
                // We will need a more complex `set_var` to replicate that strictly,
                // but for now, let's just insert globally unless we're using `local`.
                // Actually, standard behavior: modify where found, else create global.
                let mut found_scope = None;
                for (i, scope) in self.scopes.iter().enumerate().rev() {
                    if scope.contains_key(name) {
                        found_scope = Some(i);
                        break;
                    }
                }
                if let Some(i) = found_scope {
                    self.scopes[i].insert(name.to_string(), value);
                } else {
                    self.variables.insert(name.to_string(), value);
                }
            }
        }
    }

    /// Set a variable strictly in the current local scope (for `env.local`).
    pub fn set_local_var(&mut self, name: &str, value: Variable) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), value);
        } else {
            self.variables.insert(name.to_string(), value);
        }
    }

    /// Get a variable looking up through scopes.
    pub fn get_var(&self, name: &str) -> Option<&Variable> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        self.variables.get(name)
    }

    /// Get a variable's string value.
    pub fn get_var_string(&self, name: &str) -> Option<String> {
        self.get_var(name).map(|v| v.value.as_string())
    }

    /// Load history entries from `~/.cerf_history` (if it exists).
    pub fn load_history(&mut self) {
        if let Some(path) = Self::history_path()
            && path.exists()
            && let Ok(contents) = std::fs::read_to_string(&path)
        {
            self.history = contents
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
        }
    }

    /// Append a single line to the in-memory history and to `~/.cerf_history`.
    pub fn add_history(&mut self, line: &str) {
        self.history.push(line.to_string());
        if let Some(path) = Self::history_path()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(f, "{}", line);
        }
    }

    /// Return the path to `~/.cerf_history`.
    fn history_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".cerf_history"))
    }

    /// Serialize variables and `set` options so an internal-builtin child can
    /// start from the parent's launch-time state.
    fn snapshot(&self, assignments: &[(String, String)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.variables.len() as u32).to_le_bytes());
        for (name, var) in &self.variables {
            let (values, is_array): (Vec<&String>, bool) = match &var.value {
                VarValue::String(value) => (vec![value], false),
                VarValue::Array(values) => (values.iter().collect(), true),
            };
            let flags = (is_array as u8)
                | ((var.readonly as u8) << 1)
                | ((var.integer as u8) << 2)
                | ((var.exported as u8) << 3);
            out.push(flags);
            push_bytes(&mut out, name.as_bytes());
            out.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                push_bytes(&mut out, value.as_bytes());
            }
        }
        out.extend_from_slice(&(self.set_options.len() as u32).to_le_bytes());
        for option in &self.set_options {
            push_bytes(&mut out, option.as_bytes());
        }
        out.extend_from_slice(&(assignments.len() as u32).to_le_bytes());
        for (key, value) in assignments {
            push_bytes(&mut out, key.as_bytes());
            push_bytes(&mut out, value.as_bytes());
        }
        out
    }

    /// Write a launch snapshot. The caller owns cleanup until the child starts.
    pub fn write_snapshot(
        &self,
        assignments: &[(String, String)],
    ) -> std::io::Result<tempfile::TempPath> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&self.snapshot(assignments))?;
        file.flush()?;
        Ok(file.into_temp_path())
    }

    /// Restore variables and options and return the separate prefix assignments.
    /// Returns `None` for a malformed snapshot.
    pub fn apply_snapshot(&mut self, bytes: &[u8]) -> Option<Vec<(String, String)>> {
        let mut pos = 0;
        let var_count = take_u32(bytes, &mut pos)?;
        let mut variables = HashMap::new();
        for _ in 0..var_count {
            let &flags = bytes.get(pos)?;
            pos += 1;
            let name = take_string(bytes, &mut pos)?;
            let value_count = take_u32(bytes, &mut pos)?;
            let mut values = Vec::new();
            for _ in 0..value_count {
                values.push(take_string(bytes, &mut pos)?);
            }
            let value = if flags & 1 == 1 {
                VarValue::Array(values)
            } else {
                VarValue::String(values.into_iter().next().unwrap_or_default())
            };
            variables.insert(
                name,
                Variable {
                    value,
                    readonly: flags & 2 != 0,
                    integer: flags & 4 != 0,
                    exported: flags & 8 != 0,
                },
            );
        }
        let option_count = take_u32(bytes, &mut pos)?;
        let mut options = HashSet::new();
        for _ in 0..option_count {
            options.insert(take_string(bytes, &mut pos)?);
        }
        let assignment_count = take_u32(bytes, &mut pos)?;
        let mut assignments = Vec::new();
        for _ in 0..assignment_count {
            assignments.push((take_string(bytes, &mut pos)?, take_string(bytes, &mut pos)?));
        }
        if pos != bytes.len() {
            return None;
        }
        self.variables = variables;
        self.set_options = options;
        Some(assignments)
    }
}

pub enum ExecutionResult {
    KeepRunning,
    Exit,
    Break,
    Continue,
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn take_u32(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let end = pos.checked_add(4)?;
    let slice = bytes.get(*pos..end)?;
    *pos = end;
    Some(u32::from_le_bytes(slice.try_into().ok()?))
}

fn take_bytes<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let len = take_u32(bytes, pos)? as usize;
    let end = pos.checked_add(len)?;
    let slice = bytes.get(*pos..end)?;
    *pos = end;
    Some(slice)
}

fn take_string(bytes: &[u8], pos: &mut usize) -> Option<String> {
    let bytes = take_bytes(bytes, pos)?;
    Some(std::str::from_utf8(bytes).ok()?.to_owned())
}

/// Initialize shell variables from the OS environment and set defaults for missing ones.
fn init_env_vars() -> HashMap<String, String> {
    let mut vars: HashMap<String, String> = std::env::vars().collect();

    // Helper to get temp dir
    fn temp_dir(_vars: &HashMap<String, String>) -> String {
        std::env::temp_dir().to_string_lossy().to_string()
    }

    // Helper to get user name
    fn user_name(_vars: &HashMap<String, String>) -> String {
        #[cfg(windows)]
        {
            std::env::var("USERNAME").unwrap_or_else(|_| "unknown".to_string())
        }
        #[cfg(not(windows))]
        {
            std::env::var("USER").unwrap_or_else(|_| "unknown".to_string())
        }
    }
    // 1. Ensure HOME is set
    if !vars.contains_key("HOME") {
        #[cfg(windows)]
        {
            if let Ok(profile) = std::env::var("USERPROFILE") {
                vars.insert("HOME".to_string(), profile);
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(home) = std::env::var("HOME") {
                vars.insert("HOME".to_string(), home);
            } else if let Some(home_path) = dirs::home_dir() {
                vars.insert("HOME".to_string(), home_path.to_string_lossy().to_string());
            }
        }
    }

    // 2. Ensure PATH is set
    if !vars.contains_key("PATH") {
        #[cfg(windows)]
        vars.insert(
            "PATH".to_string(),
            "C:\\Windows\\system32;C:\\Windows".to_string(),
        );
        #[cfg(not(windows))]
        vars.insert(
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        );
    }

    // 3. Ensure EDITOR is set
    if !vars.contains_key("EDITOR") {
        #[cfg(windows)]
        vars.insert("EDITOR".to_string(), "notepad".to_string());
        #[cfg(not(windows))]
        vars.insert("EDITOR".to_string(), "vi".to_string());
    }

    // 4. Ensure SHELL is set
    if !vars.contains_key("SHELL") {
        let path = std::env::current_exe().unwrap();
        vars.insert("SHELL".to_string(), path.to_string_lossy().to_string());
    }

    // 5. Ensure PWD is set
    if !vars.contains_key("PWD")
        && let Ok(cwd) = std::env::current_dir()
    {
        vars.insert("PWD".to_string(), cwd.to_string_lossy().to_string());
    }

    // 6. Ensure XDG_CONFIG_HOME is set
    if !vars.contains_key("XDG_CONFIG_HOME") {
        #[cfg(windows)]
        {
            if let Ok(appdata) = std::env::var("LOCALAPPDATA") {
                vars.insert("XDG_CONFIG_HOME".to_string(), appdata);
            } else if let Some(home) = vars.get("HOME") {
                vars.insert(
                    "XDG_CONFIG_HOME".to_string(),
                    format!("{}\\AppData\\Local", home),
                );
            }
        }
        #[cfg(not(windows))]
        {
            if let Some(home) = vars.get("HOME") {
                vars.insert("XDG_CONFIG_HOME".to_string(), format!("{}/.config", home));
            }
        }
    }

    // 7. Ensure TERM is set
    if !vars.contains_key("TERM") {
        vars.insert("TERM".to_string(), "xterm-256color".to_string());
    }

    // 8. Ensure PS1 and PS2 are set
    if !vars.contains_key("PS1") {
        vars.insert("PS1".to_string(), "\\u@\\h:\\w\\$ ".to_string());
    }
    if !vars.contains_key("PS2") {
        vars.insert("PS2".to_string(), "> ".to_string());
    }

    // 9. Ensure SHLVL is set
    if let Some(shlvl_str) = vars.get("SHLVL") {
        let new_shlvl = shlvl_str.parse::<i32>().unwrap_or(0) + 1;
        vars.insert("SHLVL".to_string(), new_shlvl.to_string());
    } else {
        vars.insert("SHLVL".to_string(), "1".to_string());
    }

    // 10. Ensure LANG is set
    if !vars.contains_key("LANG") {
        vars.insert("LANG".to_string(), "en_US.UTF-8".to_string());
    }
    // 11. Ensure HISTFILE is set
    if !vars.contains_key("HISTFILE")
        && let Some(home) = vars.get("HOME")
    {
        vars.insert("HISTFILE".to_string(), format!("{}/.cerf_history", home));
    }
    // 12. Ensure HISTFILESIZE is set
    if !vars.contains_key("HISTFILESIZE") {
        vars.insert("HISTFILESIZE".to_string(), "10000".to_string());
    }
    // 13. Ensure HISTSIZE is set
    if !vars.contains_key("HISTSIZE") {
        vars.insert("HISTSIZE".to_string(), "10000".to_string());
    }
    // 14. Ensure TMPDIR is set
    if !vars.contains_key("TMPDIR") {
        vars.insert("TMPDIR".to_string(), temp_dir(&vars));
    }
    // 15. Ensure TMP is set
    if !vars.contains_key("TMP") {
        vars.insert("TMP".to_string(), temp_dir(&vars));
    }
    // 16. Ensure TEMP is set
    if !vars.contains_key("TEMP") {
        vars.insert("TEMP".to_string(), temp_dir(&vars));
    }
    // 17. Ensure USER is set
    if !vars.contains_key("USER") {
        vars.insert("USER".to_string(), user_name(&vars));
    }
    // 18. Ensure USERNAME is set
    if !vars.contains_key("USERNAME") {
        vars.insert("USERNAME".to_string(), user_name(&vars));
    }
    // 19. Ensure USERID is set
    if !vars.contains_key("UID") {
        // Windows doesn't have UID, we print nothing
        #[cfg(windows)]
        {
            vars.insert("UID".to_string(), "".to_string());
        }
        #[cfg(not(windows))]
        {
            let uid = std::env::var("UID").unwrap_or_else(|_| "unknown".to_string());
            vars.insert("UID".to_string(), uid);
        }
    }
    // 20. Ensure CERF_VERSION is set
    if !vars.contains_key("CERF_VERSION") {
        vars.insert(
            "CERF_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
    }
    // Sync environment variables that we just added defaults for
    for (key, val) in &vars {
        if std::env::var(key).is_err() {
            unsafe {
                std::env::set_var(key, val);
            }
        }
    }

    vars
}

/// Initialize the default aliases for backward POSIX compatibility with renamed `<type>.<action>` builtins.
fn init_default_aliases() -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let mappings = [
        ("cd", "dir.cd"),
        ("pwd", "dir.pwd"),
        ("pushd", "dir.pushd"),
        ("popd", "dir.popd"),
        ("dirs", "dir.dirs"),
        ("jobs", "job.list"),
        ("fg", "job.fg"),
        ("bg", "job.bg"),
        ("wait", "job.wait"),
        ("kill", "job.kill"),
        ("tether", "job.tether"),
        ("untether", "job.untether"),
        ("export", "env.export"),
        ("unset", "env.unset"),
        ("set", "env.set"),
        ("source", "env.source"),
        (".", "env.source"),
        ("alias", "alias.set"),
        ("unalias", "alias.unset"),
        ("exit", "sys.exit"),
        ("clear", "sys.clear"),
        ("exec", "sys.exec"),
        ("history", "sys.history"),
        ("help", "sys.help"),
        ("type", "sys.type"),
        ("echo", "io.echo"),
        ("read", "io.read"),
        ("printf", "io.printf"),
        ("mapfile", "io.mapfile"),
        ("readarray", "io.mapfile"),
        ("eval", "sys.eval"),
        ("command", "sys.command"),
        ("builtin", "sys.builtin"),
        ("ulimit", "sys.ulimit"),
        ("umask", "sys.umask"),
        ("declare", "env.declare"),
        ("typeset", "env.declare"),
        ("local", "env.local"),
        ("shift", "env.shift"),
        ("true", "test.true"),
        ("false", "test.false"),
        ("test", "test.check"),
        ("[", "test.bracket"),
        ("cat", "fs.cat"),
        ("less", "fs.less"),
        ("more", "fs.more"),
        ("ls", "fs.ls"),
        ("dir", "fs.ls"),
        ("mkdir", "fs.mkdir"),
        ("rm", "fs.rm"),
        ("cp", "fs.cp"),
        ("mv", "fs.mv"),
        ("touch", "fs.touch"),
        ("stat", "fs.stat"),
        ("du", "fs.du"),
        ("df", "fs.df"),
    ];
    for (name, target) in mappings {
        aliases.insert(name.to_string(), target.to_string());
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips_variables_and_options() {
        let mut parent = ShellState::new();
        let mut flag = Variable::new_string("value".to_string());
        flag.integer = true;
        parent.set_var("CERF_SNAPSHOT_TEST", flag);
        parent.set_var(
            "CERF_SNAPSHOT_ARRAY",
            Variable::new_array(vec!["a".into(), "b".into()]),
        );
        parent.set_options.insert("errexit".to_string());

        let mut child = ShellState::new();
        let assignments = vec![("CERF_SNAPSHOT_TEST".into(), "override".into())];
        assert_eq!(
            child.apply_snapshot(&parent.snapshot(&assignments)),
            Some(assignments)
        );
        let var = child.get_var("CERF_SNAPSHOT_TEST").expect("string var");
        assert_eq!(var.value, VarValue::String("value".to_string()));
        assert!(var.integer);
        let array = child.get_var("CERF_SNAPSHOT_ARRAY").expect("array var");
        assert_eq!(
            array.value,
            VarValue::Array(vec!["a".to_string(), "b".to_string()])
        );
        assert!(child.set_options.contains("errexit"));
        assert!(child.apply_snapshot(b"not a snapshot").is_none());
    }
}
