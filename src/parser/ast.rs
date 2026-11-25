// ── AST types ──────────────────────────────────────────────────────────────

/// One run of characters inside a word.
///
/// `expand` is false for text protected from variable expansion by single
/// quotes or a backslash. Expansion happens at execution time, not parse
/// time, so variable contents are always treated as data.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct WordPart {
    pub text: String,
    pub expand: bool,
}

/// A single shell argument with quoting metadata.
///
/// `quoted == true` means the argument was entirely wrapped in quotes
/// (`'…'` or `"…"`), and should **not** undergo glob expansion.
///
/// `value` is the unexpanded text with quote characters removed. `parts`
/// records which runs may be expanded; programmatically built arguments
/// leave it empty and are used verbatim.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Arg {
    pub value: String,
    pub quoted: bool,
    pub parts: Vec<WordPart>,
}

impl Arg {
    pub fn new(value: impl Into<String>, quoted: bool) -> Self {
        Self {
            value: value.into(),
            quoted,
            parts: Vec::new(),
        }
    }

    pub fn plain(value: impl Into<String>) -> Self {
        Self::new(value, false)
    }

    /// Build an argument from parsed word parts, deriving `value` from them.
    pub fn from_parts(parts: Vec<WordPart>, quoted: bool) -> Self {
        let value = parts.iter().map(|part| part.text.as_str()).collect();
        Self {
            value,
            quoted,
            parts,
        }
    }
}

/// Helper: extract just the string values from a slice of `Arg`s.
#[cfg(test)]
pub fn arg_values(args: &[Arg]) -> Vec<&str> {
    args.iter().map(|a| a.value.as_str()).collect()
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SimpleCommand {
    pub assignments: Vec<(String, Arg)>,
    pub name: Option<String>,
    pub args: Vec<Arg>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CommandNode {
    Simple(SimpleCommand),
    If {
        branches: Vec<(Vec<CommandEntry>, Vec<CommandEntry>)>,
        else_branch: Option<Vec<CommandEntry>>,
        redirects: Vec<Redirect>,
    },
    FuncDecl {
        name: String,
        body: Vec<CommandEntry>,
    },
    For {
        var: String,
        items: Vec<Arg>,
        body: Vec<CommandEntry>,
        redirects: Vec<Redirect>,
    },
    While {
        cond: Vec<CommandEntry>,
        body: Vec<CommandEntry>,
        redirects: Vec<Redirect>,
    },
    Loop {
        body: Vec<CommandEntry>,
        redirects: Vec<Redirect>,
    },
    Break,
    Continue,
}

impl CommandNode {
    #[cfg(test)]
    pub fn name(&self) -> Option<&String> {
        match self {
            Self::Simple(s) => s.name.as_ref(),
            _ => None,
        }
    }

    pub fn args(&self) -> &[Arg] {
        match self {
            Self::Simple(s) => &s.args,
            _ => &[],
        }
    }

    pub fn redirects(&self) -> &[Redirect] {
        match self {
            Self::Simple(s) => &s.redirects,
            Self::If { redirects, .. } => redirects,
            Self::For { redirects, .. } => redirects,
            Self::While { redirects, .. } => redirects,
            Self::Loop { redirects, .. } => redirects,
            _ => &[],
        }
    }

    pub fn assignments(&self) -> &[(String, Arg)] {
        match self {
            Self::Simple(s) => &s.assignments,
            _ => &[],
        }
    }
}

/// I/O redirection attached to a single command.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum RedirectKind {
    /// `>  file` — truncate-write stdout to file
    StdoutOverwrite,
    /// `>> file` — append stdout to file
    StdoutAppend,
    /// `<  file` — read stdin from file
    StdinFrom,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub file: String,
}

/// A pipeline is one or more commands connected by `|`.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Pipeline {
    pub commands: Vec<CommandNode>, // length ≥ 1
    pub negated: bool,
    pub background: bool,
}

/// How consecutive commands are joined.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Connector {
    /// `;`  — always run the next command
    Semi,
    /// `&&` — run next only if previous succeeded (exit code 0)
    And,
    /// `||` — run next only if previous failed  (exit code ≠ 0)
    Or,
    /// `&` — run previous command in the background
    Amp,
}

/// A single entry in a command list:
/// - `connector` is `None` for the very first command, `Some(…)` for every
///   subsequent command and describes the operator that precedes it.
/// - `pipeline` is the pipeline to execute.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct CommandEntry {
    pub connector: Option<Connector>,
    pub pipeline: Pipeline,
}
