use std::collections::HashMap;

use crate::parser::{Arg, CommandNode};

/// Expand aliases on a `ParsedCommand` in-place (bash-style, one level).
///
/// If `cmd.name` matches an alias whose value is a single word, the name is
/// replaced and the replacement's trailing args are prepended to `cmd.args`.
/// If the alias value is a multi-word string, it is re-parsed: the first
/// token becomes the new command name and the rest are prepended to the
/// existing args.
///
/// Returns `true` when an expansion happened.
pub fn expand_alias(cmd: &mut CommandNode, aliases: &HashMap<String, String>) -> bool {
    // Only simple commands are aliased
    let simple = match cmd {
        CommandNode::Simple(s) => s,
        _ => return false,
    };

    let name = match simple.name.as_ref() {
        Some(n) => n,
        None => return false,
    };
    if let Some(value) = aliases.get(name) {
        let value = value.clone();
        let tokens = shell_split(&value);
        if tokens.is_empty() {
            return false;
        }
        simple.name = Some(tokens[0].clone());
        let mut new_args: Vec<Arg> = tokens[1..].iter().map(Arg::plain).collect();
        new_args.append(&mut simple.args);
        simple.args = new_args;
        return true;
    }
    false
}

/// Very small shell-word splitter that honours `'…'` and `"…"` quoting.
/// Used only for parsing alias values.
fn shell_split(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut token_started = false;
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                // Outside quotes a backslash escapes the next character. In
                // double quotes it only does so for characters that are
                // special there; other backslashes stay literal (so
                // `printf "%s\n"` keeps its escape for printf).
                match chars.peek() {
                    Some(&next) if !in_double || matches!(next, '"' | '\\' | '$' | '`') => {
                        chars.next();
                        current.push(next);
                    }
                    _ => current.push('\\'),
                }
                token_started = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                token_started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                token_started = true;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            other => {
                current.push(other);
                token_started = true;
            }
        }
    }
    if token_started {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::shell_split;

    #[test]
    fn splits_single_double_and_unquoted_words() {
        assert_eq!(shell_split("echo 'a b' \"c d\""), vec!["echo", "a b", "c d"]);
        assert_eq!(
            shell_split(r#"printf "%s\n" "x y""#),
            vec!["printf", "%s\\n", "x y"]
        );
        assert_eq!(shell_split(r#"one\ two three"#), vec!["one two", "three"]);
        assert_eq!(shell_split(r#"echo "" ''"#), vec!["echo", "", ""]);
        assert_eq!(shell_split("   "), Vec::<String>::new());
    }
}
