/// Expand variable references in `input` before parsing.
///
/// Substitution rules (mirrors POSIX sh behaviour):
/// - `$$`        → a literal `$`
/// - `$VAR`      → the value of the variable `VAR`
///   (identifier chars: ASCII alphanumeric + `_`)
/// - `${VAR}`    → same, with brace delimiters
/// - Bare `$` with no following identifier or `{` → kept as-is
pub fn expand_vars(
    input: &str,
    shell_vars: &std::collections::HashMap<String, crate::engine::state::Variable>,
) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            result.push(ch);
            continue;
        }

        match chars.peek() {
            // $$ → literal $
            Some('$') => {
                chars.next();
                result.push('$');
            }
            // ${VAR} style
            Some('{') => {
                chars.next(); // consume '{'
                let var_name: String = chars.by_ref().take_while(|&c| c != '}').collect();
                let value = shell_vars
                    .get(&var_name)
                    .map(|v| v.value.as_string())
                    .unwrap_or_default();
                result.push_str(&value);
            }
            // Positional and special parameters: `$1`, `$#`, `$@`, `$*`.
            Some(&c) if c.is_ascii_digit() || matches!(c, '#' | '@' | '*') => {
                chars.next();
                let value = shell_vars
                    .get(&c.to_string())
                    .map(|v| v.value.as_string())
                    .unwrap_or_else(|| {
                        // `$#` is 0 when no positional parameters exist.
                        if c == '#' {
                            "0".to_string()
                        } else {
                            String::new()
                        }
                    });
                result.push_str(&value);
            }
            // $VAR style — identifier starts with alpha or '_'
            Some(&c) if c.is_ascii_alphabetic() || c == '_' => {
                let var_name: String = std::iter::once(chars.next().unwrap())
                    .chain(std::iter::from_fn(|| {
                        chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '_')
                    }))
                    .collect();
                let value = shell_vars
                    .get(&var_name)
                    .map(|v| v.value.as_string())
                    .unwrap_or_default();
                result.push_str(&value);
            }
            // Bare $ with no following identifier → keep as-is
            _ => {
                result.push('$');
            }
        }
    }

    result
}

/// Expand the expandable parts of a parsed argument exactly once.
///
/// Text protected by single quotes or a backslash is copied verbatim, so the
/// resulting value is data and never re-parsed as shell syntax.
pub fn expand_arg(
    arg: &crate::parser::Arg,
    shell_vars: &std::collections::HashMap<String, crate::engine::state::Variable>,
) -> String {
    if arg.parts.is_empty() {
        return arg.value.clone();
    }
    let mut result = String::with_capacity(arg.value.len());
    for part in &arg.parts {
        if part.expand {
            result.push_str(&expand_vars(&part.text, shell_vars));
        } else {
            result.push_str(&part.text);
        }
    }
    result
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Arg;
    use crate::parser::ast::WordPart;

    fn var(value: &str) -> crate::engine::state::Variable {
        crate::engine::state::Variable::new_string(value.to_string())
    }

    fn parts(items: &[(&str, bool)]) -> Vec<WordPart> {
        items
            .iter()
            .map(|(text, expand)| WordPart {
                text: text.to_string(),
                expand: *expand,
                glob: *expand,
            })
            .collect()
    }

    #[test]
    fn expand_arg_respects_protected_parts() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("Q".to_string(), var("abc"));
        let arg = Arg::from_parts(parts(&[("$Q", true)]), false);
        assert_eq!(expand_arg(&arg, &vars), "abc");
        let literal = Arg::from_parts(parts(&[("$Q", false)]), true);
        assert_eq!(expand_arg(&literal, &vars), "$Q");
        let mixed = Arg::from_parts(parts(&[("x", true), ("$Q", false), ("$Q", true)]), false);
        assert_eq!(expand_arg(&mixed, &vars), "x$Qabc");
    }

    #[test]
    fn expand_arg_is_not_recursive() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("OUTER".to_string(), var("$INNER"));
        vars.insert("INNER".to_string(), var("secret"));
        let arg = Arg::from_parts(parts(&[("$OUTER", true)]), false);
        assert_eq!(expand_arg(&arg, &vars), "$INNER");
    }

    #[test]
    fn test_expand_known_var() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "CERF_TEST_VAR".to_string(),
            crate::engine::state::Variable::new_string("hello".to_string()),
        );
        assert_eq!(expand_vars("$CERF_TEST_VAR", &vars), "hello");
        assert_eq!(expand_vars("${CERF_TEST_VAR}", &vars), "hello");
    }

    #[test]
    fn test_expand_missing_var_is_empty() {
        let vars = std::collections::HashMap::new();
        assert_eq!(expand_vars("$CERF_UNDEFINED_XYZ", &vars), "");
        assert_eq!(expand_vars("${CERF_UNDEFINED_XYZ}", &vars), "");
    }

    #[test]
    fn test_expand_dollar_dollar_escape() {
        let vars = std::collections::HashMap::new();
        assert_eq!(expand_vars("$$", &vars), "$");
        assert_eq!(expand_vars("$$$", &vars), "$$");
        assert_eq!(expand_vars("cost: $$5", &vars), "cost: $5");
    }

    #[test]
    fn test_expand_bare_dollar_kept() {
        let vars = std::collections::HashMap::new();
        assert_eq!(expand_vars("$ ", &vars), "$ ");
        assert_eq!(expand_vars("$", &vars), "$");
    }

    #[test]
    fn test_expand_inline() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "CERF_GREET".to_string(),
            crate::engine::state::Variable::new_string("world".to_string()),
        );
        assert_eq!(expand_vars("hello $CERF_GREET!", &vars), "hello world!");
    }

    #[test]
    fn test_expand_multiple_vars() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "CERF_A".to_string(),
            crate::engine::state::Variable::new_string("foo".to_string()),
        );
        vars.insert(
            "CERF_B".to_string(),
            crate::engine::state::Variable::new_string("bar".to_string()),
        );
        assert_eq!(expand_vars("$CERF_A/$CERF_B", &vars), "foo/bar");
    }

    #[test]
    fn test_expand_no_dollar_unchanged() {
        let vars = std::collections::HashMap::new();
        assert_eq!(expand_vars("ls -la", &vars), "ls -la");
        assert_eq!(expand_vars("", &vars), "");
    }
}
