use crate::parser::Arg;

/// Expand glob patterns in a list of arguments.
///
/// For each argument:
/// - If no unquoted glob metacharacter remains → push the raw value unchanged.
/// - Otherwise build a pattern where quoted or backslash-escaped `*?[]` are
///   escaped literally, then call `glob::glob()`.
///   - If matches are found → push all matches (sorted lexicographically).
///   - If no matches → push the original value unchanged (bash default).
pub fn expand_globs(args: &[Arg]) -> Vec<String> {
    let mut expanded: Vec<String> = Vec::new();

    for arg in args {
        let Some(pattern) = glob_pattern(arg) else {
            expanded.push(arg.value.clone());
            continue;
        };

        // Attempt glob expansion. The glob crate matches dotfiles by default,
        // so filter them out unless the pattern component itself starts with a
        // dot (POSIX shell behaviour).
        match glob::glob(&pattern) {
            Ok(paths) => {
                let mut matches: Vec<String> = paths
                    .filter_map(|entry| entry.ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .filter(|matched| dotfile_rule_allows(&pattern, matched))
                    .collect();

                if matches.is_empty() {
                    // No matches — keep the original pattern (bash behaviour).
                    expanded.push(arg.value.clone());
                } else {
                    matches.sort();
                    expanded.append(&mut matches);
                }
            }
            Err(_) => {
                // Invalid pattern — keep as-is.
                expanded.push(arg.value.clone());
            }
        }
    }

    expanded
}

/// Build the glob pattern for an argument, or `None` when it must not be
/// expanded.
///
/// Parts retain the quoting information from parsing. A metacharacter in a
/// part with `glob == false` is replaced by `glob::Pattern::escape`, so it
/// matches itself instead of acting as a wildcard. This is what makes
/// `"file[1]".txt` name the literal file rather than `file1.txt`.
fn glob_pattern(arg: &Arg) -> Option<String> {
    if arg.parts.is_empty() {
        // Programmatically built arguments carry no part metadata; `quoted`
        // still suppresses expansion for them.
        if arg.quoted || !contains_glob_chars(&arg.value) {
            return None;
        }
        return Some(arg.value.clone());
    }

    let mut pattern = String::new();
    let mut has_active_glob = false;
    for part in &arg.parts {
        for ch in part.text.chars() {
            if matches!(ch, '*' | '?' | '[' | ']') {
                if part.glob {
                    pattern.push(ch);
                    has_active_glob = true;
                } else {
                    pattern.push_str(&glob::Pattern::escape(&ch.to_string()));
                }
            } else {
                pattern.push(ch);
            }
        }
    }

    if has_active_glob { Some(pattern) } else { None }
}

/// Check the POSIX rule that a leading dot must be matched literally.
///
/// The glob crate reports relative matches with a `./` prefix (`.\\` on
/// Windows) and includes the `.` / `..` entries, so those are normalized here.
fn dotfile_rule_allows(pattern: &str, matched: &str) -> bool {
    // Compare components with one separator; Windows reports `.\.` etc.
    let normalize = |value: &str| {
        let value = value.replace('\\', "/");
        value
            .strip_prefix("./")
            .unwrap_or(&value)
            .trim_end_matches('/')
            .to_string()
    };
    let pattern = normalize(pattern);
    let matched = normalize(matched);

    if matched == "." || matched == ".." || matched.ends_with("/.") || matched.ends_with("/..") {
        return false;
    }

    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let matched_parts: Vec<&str> = matched.split('/').collect();
    if pattern_parts.len() == matched_parts.len() {
        return pattern_parts
            .iter()
            .zip(matched_parts.iter())
            .all(|(pat, part)| !(part.starts_with('.') && !pat.starts_with('.')));
    }

    // Fall back to the final component when the shapes differ (e.g. `**`).
    match (pattern_parts.last(), matched_parts.last()) {
        (Some(pat), Some(part)) => !(part.starts_with('.') && !pat.starts_with('.')),
        _ => true,
    }
}

/// Does `s` contain any glob meta-characters?
fn contains_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::WordPart;

    fn part(text: &str, glob: bool) -> WordPart {
        WordPart {
            text: text.to_string(),
            expand: true,
            glob,
        }
    }

    #[test]
    fn test_no_glob_chars_passes_through() {
        let args = vec![Arg::plain("hello"), Arg::plain("-la")];
        let result = expand_globs(&args);
        assert_eq!(result, vec!["hello", "-la"]);
    }

    #[test]
    fn test_quoted_arg_not_expanded() {
        let args = vec![Arg::new("*.rs", true)];
        let result = expand_globs(&args);
        assert_eq!(result, vec!["*.rs"]);
    }

    #[test]
    fn test_partially_quoted_metacharacters_are_literal() {
        let literal = Arg::from_parts(vec![part("file[1]", false), part(".txt", true)], false);
        let wildcard = Arg::from_parts(vec![part("*", false), part(".txt", true)], false);
        assert_eq!(glob_pattern(&literal), None);
        assert_eq!(glob_pattern(&wildcard), None);
        assert_eq!(expand_globs(&[literal]), vec!["file[1].txt"]);
        assert_eq!(expand_globs(&[wildcard]), vec!["*.txt"]);
    }

    #[test]
    fn test_mixed_quoted_and_unquoted_glob() {
        // `"pre*"` is literal but the trailing `*` is active.
        let arg = Arg::from_parts(vec![part("pre*", false), part("*", true)], false);
        assert_eq!(glob_pattern(&arg), Some("pre[*]*".to_string()));
    }

    #[test]
    fn test_glob_no_matches_kept_as_is() {
        let args = vec![Arg::plain("*.this_extension_should_not_exist_xyzzy")];
        let result = expand_globs(&args);
        assert_eq!(result, vec!["*.this_extension_should_not_exist_xyzzy"]);
    }

    #[test]
    fn test_windows_paths_normalize_before_the_dotfile_rule() {
        // The glob crate reports these as `.\` / `.\..` / `.\a.txt` on Windows.
        assert!(!dotfile_rule_allows(".*", ".\\."));
        assert!(!dotfile_rule_allows(".*", ".\\.."));
        assert!(dotfile_rule_allows(".*", ".\\.env"));
        assert!(dotfile_rule_allows("*", ".\\a.txt"));
    }
}
