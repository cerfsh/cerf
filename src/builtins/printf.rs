use crate::builtins::registry::CommandInfo;
use crate::engine::state::{ExecutionResult, ShellState};
use std::io::{self, Write};
use std::iter::Peekable;
use std::str::Chars;

pub const COMMAND_INFO_PRINTF: CommandInfo = CommandInfo {
    name: "io.printf",
    description: "Formats and prints arguments.",
    usage: "io.printf format [arguments]\n\nFormats and prints arguments dynamically according to the FORMAT.",
    run: printf_runner,
};

/// One piece of an already-parsed format string.
enum Piece {
    Text(String),
    Spec(FormatSpec),
    Stop,
}

struct FormatSpec {
    flags: String,
    width: Option<usize>,
    precision: Option<usize>,
    conversion: char,
}

pub fn printf_runner(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.is_empty() {
        eprintln!("cerf: printf: usage: printf format [arguments]");
        return (ExecutionResult::KeepRunning, 1);
    }

    let pieces = parse_format(&args[0]);
    let values = &args[1..];
    let mut arg_index = 0;
    let mut out = String::new();

    loop {
        let start_index = arg_index;
        let mut stopped = false;

        for piece in &pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Stop => {
                    stopped = true;
                    break;
                }
                Piece::Spec(spec) => {
                    let value = values.get(arg_index).map(String::as_str).unwrap_or("");
                    let (rendered, stop) = format_value(spec, value);
                    out.push_str(&rendered);
                    arg_index += 1;
                    if stop {
                        stopped = true;
                        break;
                    }
                }
            }
        }

        // The format is reused until every argument is consumed. A format
        // with no conversions runs exactly once.
        if stopped || arg_index >= values.len() || arg_index == start_index {
            break;
        }
    }

    let mut stdout = io::stdout().lock();
    if let Err(error) = stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
    {
        eprintln!("cerf: printf: write error: {}", error);
        return (ExecutionResult::KeepRunning, 1);
    }
    (ExecutionResult::KeepRunning, 0)
}

/// Parse escapes and `%` conversions out of the format string once.
fn parse_format(format: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut chars = format.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match parse_escape(&mut chars) {
                Escape::Char(escaped) => text.push(escaped),
                Escape::Text(escaped) => text.push_str(&escaped),
                Escape::Stop => {
                    push_text(&mut pieces, &mut text);
                    pieces.push(Piece::Stop);
                    return pieces;
                }
                Escape::None => text.push('\\'),
            },
            '%' => match parse_spec(&mut chars) {
                Some(Ok(spec)) => {
                    push_text(&mut pieces, &mut text);
                    pieces.push(Piece::Spec(spec));
                }
                Some(Err(literal)) => text.push_str(&literal),
                None => text.push('%'),
            },
            other => text.push(other),
        }
    }

    push_text(&mut pieces, &mut text);
    pieces
}

fn push_text(pieces: &mut Vec<Piece>, text: &mut String) {
    if !text.is_empty() {
        pieces.push(Piece::Text(std::mem::take(text)));
    }
}

/// Parse a conversion after `%`.
///
/// Returns `None` for `%%` (a literal percent), `Some(Ok)` for a supported
/// conversion, and `Some(Err(raw))` for an unsupported one, which is emitted
/// literally.
fn parse_spec(chars: &mut Peekable<Chars<'_>>) -> Option<Result<FormatSpec, String>> {
    if chars.peek() == Some(&'%') {
        chars.next();
        return None;
    }

    let mut raw = String::from("%");
    let mut flags = String::new();
    while let Some(&flag) = chars.peek() {
        if matches!(flag, '-' | '+' | ' ' | '0' | '#') {
            flags.push(flag);
            raw.push(flag);
            chars.next();
        } else {
            break;
        }
    }

    let mut width = None;
    while let Some(&digit) = chars.peek() {
        if let Some(value) = digit.to_digit(10) {
            raw.push(digit);
            chars.next();
            width = Some(width.unwrap_or(0) * 10 + value as usize);
        } else {
            break;
        }
    }

    let mut precision = None;
    if chars.peek() == Some(&'.') {
        raw.push('.');
        chars.next();
        let mut value = 0usize;
        while let Some(&digit) = chars.peek() {
            if let Some(parsed) = digit.to_digit(10) {
                raw.push(digit);
                chars.next();
                value = value.saturating_mul(10).saturating_add(parsed as usize);
            } else {
                break;
            }
        }
        precision = Some(value);
    }

    // Length modifiers do not change how these arguments are parsed.
    while let Some(&modifier) = chars.peek() {
        if matches!(modifier, 'h' | 'l' | 'L' | 'j' | 'z' | 't') {
            raw.push(modifier);
            chars.next();
        } else {
            break;
        }
    }

    let conversion = chars.next()?;
    raw.push(conversion);

    if matches!(
        conversion,
        's' | 'b' | 'c' | 'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G'
    ) {
        Some(Ok(FormatSpec {
            flags,
            width,
            precision,
            conversion,
        }))
    } else {
        Some(Err(raw))
    }
}

enum Escape {
    Char(char),
    Text(String),
    Stop,
    None,
}

/// Parse a backslash escape, with `chars` positioned after the backslash.
fn parse_escape(chars: &mut Peekable<Chars<'_>>) -> Escape {
    let Some(ch) = chars.next() else {
        return Escape::None;
    };

    match ch {
        'a' => Escape::Char('\x07'),
        'b' => Escape::Char('\x08'),
        'e' => Escape::Char('\x1b'),
        'f' => Escape::Char('\x0c'),
        'n' => Escape::Char('\n'),
        'r' => Escape::Char('\r'),
        't' => Escape::Char('\t'),
        'v' => Escape::Char('\x0b'),
        '\\' => Escape::Char('\\'),
        '"' => Escape::Char('"'),
        '\'' => Escape::Char('\''),
        'c' => Escape::Stop,
        '0' => Escape::Char(char::from_u32(parse_octal(chars, 3)).unwrap_or('\0')),
        '1'..='7' => {
            let mut value = ch.to_digit(8).unwrap_or(0);
            for _ in 0..2 {
                let Some(&next) = chars.peek() else {
                    break;
                };
                let Some(digit) = next.to_digit(8) else {
                    break;
                };
                chars.next();
                value = value * 8 + digit;
            }
            Escape::Char(char::from_u32(value).unwrap_or('\0'))
        }
        'x' => {
            let mut value = 0u32;
            let mut digits = 0;
            while digits < 2 {
                let Some(&next) = chars.peek() else {
                    break;
                };
                let Some(digit) = next.to_digit(16) else {
                    break;
                };
                chars.next();
                value = value * 16 + digit;
                digits += 1;
            }
            if digits == 0 {
                Escape::Text("\\x".to_string())
            } else {
                Escape::Char(char::from_u32(value).unwrap_or('\0'))
            }
        }
        other => Escape::Text(format!("\\{other}")),
    }
}

fn parse_octal(chars: &mut Peekable<Chars<'_>>, max_digits: usize) -> u32 {
    let mut value = 0u32;
    let mut digits = 0;
    while digits < max_digits {
        let Some(&next) = chars.peek() else {
            break;
        };
        let Some(digit) = next.to_digit(8) else {
            break;
        };
        chars.next();
        value = value * 8 + digit;
        digits += 1;
    }
    value
}

fn format_value(spec: &FormatSpec, argument: &str) -> (String, bool) {
    let mut stop = false;
    let body = match spec.conversion {
        's' => truncate_chars(argument, spec.precision),
        'b' => {
            let (expanded, escape_stopped) = expand_escapes(argument);
            stop = escape_stopped;
            truncate_chars(&expanded, spec.precision)
        }
        'c' => argument
            .chars()
            .next()
            .map(String::from)
            .unwrap_or_default(),
        'd' | 'i' => {
            let value = parse_signed(argument);
            signed_digits(value, 10, false, spec.precision)
        }
        'u' => unsigned_digits(parse_unsigned(argument), 10, false, spec.precision),
        'o' => unsigned_digits(parse_unsigned(argument), 8, false, spec.precision),
        'x' => unsigned_digits(parse_unsigned(argument), 16, false, spec.precision),
        'X' => unsigned_digits(parse_unsigned(argument), 16, true, spec.precision),
        'f' | 'F' => {
            let value = parse_float(argument);
            format!("{:.*}", spec.precision.unwrap_or(6), value)
        }
        'e' | 'E' => {
            let value = parse_float(argument);
            format!("{:.*e}", spec.precision.unwrap_or(6), value)
        }
        'g' | 'G' => {
            let value = parse_float(argument);
            let precision = spec.precision.unwrap_or(6);
            if precision == 0 {
                format!("{value:.0}")
            } else {
                format!("{value:.precision$}")
            }
        }
        other => format!("%{other}"),
    };

    let mut body = body;
    let numeric = matches!(
        spec.conversion,
        'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G'
    );
    if numeric {
        body = apply_sign(body, &spec.flags);
    }
    (apply_width(&body, spec), stop)
}

fn truncate_chars(value: &str, precision: Option<usize>) -> String {
    match precision {
        Some(limit) => value.chars().take(limit).collect(),
        None => value.to_string(),
    }
}

fn parse_signed(argument: &str) -> i64 {
    let trimmed = argument.trim();
    if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        i64::from_str_radix(rest, 16).unwrap_or(0)
    } else if let Some(rest) = trimmed.strip_prefix('0').filter(|rest| !rest.is_empty()) {
        i64::from_str_radix(rest, 8).unwrap_or(0)
    } else {
        trimmed.parse().unwrap_or(0)
    }
}

fn parse_unsigned(argument: &str) -> u64 {
    let trimmed = argument.trim();
    if let Some(rest) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(rest, 16).unwrap_or(0)
    } else {
        trimmed.parse().unwrap_or(0)
    }
}

fn parse_float(argument: &str) -> f64 {
    argument.trim().parse().unwrap_or(0.0)
}

fn signed_digits(value: i64, radix: u32, uppercase: bool, precision: Option<usize>) -> String {
    let negative = value < 0;
    let magnitude = value.unsigned_abs();
    let mut digits = if uppercase {
        format!("{:X}", magnitude)
    } else {
        match radix {
            8 => format!("{:o}", magnitude),
            16 => format!("{:x}", magnitude),
            _ => magnitude.to_string(),
        }
    };
    if let Some(precision) = precision {
        while digits.len() < precision {
            digits.insert(0, '0');
        }
    }
    if negative {
        format!("-{digits}")
    } else {
        digits
    }
}

fn unsigned_digits(value: u64, radix: u32, uppercase: bool, precision: Option<usize>) -> String {
    let mut digits = if uppercase {
        format!("{:X}", value)
    } else {
        match radix {
            8 => format!("{:o}", value),
            16 => format!("{:x}", value),
            _ => value.to_string(),
        }
    };
    if let Some(precision) = precision {
        while digits.len() < precision {
            digits.insert(0, '0');
        }
    }
    digits
}

fn apply_sign(body: String, flags: &str) -> String {
    if body.starts_with('-') {
        return body;
    }
    if flags.contains('+') {
        format!("+{body}")
    } else if flags.contains(' ') {
        format!(" {body}")
    } else {
        body
    }
}

fn apply_width(body: &str, spec: &FormatSpec) -> String {
    let Some(width) = spec.width else {
        return body.to_string();
    };
    let length = body.chars().count();
    if length >= width {
        return body.to_string();
    }
    let padding = width - length;
    if spec.flags.contains('-') {
        let mut value = body.to_string();
        value.extend(std::iter::repeat_n(' ', padding));
        return value;
    }
    if spec.flags.contains('0') {
        // Keep the sign (or 0x prefix) before the zero padding.
        let (prefix, rest) = if let Some(rest) = body.strip_prefix('-') {
            ("-", rest)
        } else if let Some(rest) = body.strip_prefix('+') {
            ("+", rest)
        } else if let Some(rest) = body.strip_prefix("0x") {
            ("0x", rest)
        } else if let Some(rest) = body.strip_prefix("0X") {
            ("0X", rest)
        } else {
            ("", body)
        };
        let mut value = String::with_capacity(width);
        value.push_str(prefix);
        value.extend(std::iter::repeat_n('0', padding));
        value.push_str(rest);
        return value;
    }
    let mut value = String::with_capacity(width);
    value.extend(std::iter::repeat_n(' ', padding));
    value.push_str(body);
    value
}

fn expand_escapes(input: &str) -> (String, bool) {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match parse_escape(&mut chars) {
            Escape::Char(escaped) => out.push(escaped),
            Escape::Text(text) => out.push_str(&text),
            Escape::Stop => return (out, true),
            Escape::None => out.push('\\'),
        }
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(format: &str, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let pieces = parse_format(format);
        let mut arg_index = 0;
        let mut out = String::new();
        loop {
            let start = arg_index;
            let mut stopped = false;
            for piece in &pieces {
                match piece {
                    Piece::Text(text) => out.push_str(text),
                    Piece::Stop => {
                        stopped = true;
                        break;
                    }
                    Piece::Spec(spec) => {
                        let value = args.get(arg_index).map(String::as_str).unwrap_or("");
                        let (text, stop) = format_value(spec, value);
                        out.push_str(&text);
                        arg_index += 1;
                        if stop {
                            stopped = true;
                            break;
                        }
                    }
                }
            }
            if stopped || arg_index >= args.len() || arg_index == start {
                break;
            }
        }
        out
    }

    #[test]
    fn reuses_the_format_for_remaining_arguments() {
        assert_eq!(render("[%s]\n", &["a", "b", "c"]), "[a]\n[b]\n[c]\n");
        assert_eq!(render("%s", &[]), "");
    }

    #[test]
    fn expands_b_and_handles_c() {
        assert_eq!(render("%b\n", &["a\\tb"]), "a\tb\n");
        assert_eq!(render("%b", &["before\\cafter"]), "before");
        assert_eq!(render("%c", &["hello"]), "h");
    }

    #[test]
    fn supports_floats_width_and_precision() {
        assert_eq!(render("%.2f", &["1.5"]), "1.50");
        assert_eq!(render("%5s", &["ab"]), "   ab");
        assert_eq!(render("%-5s", &["ab"]), "ab   ");
        assert_eq!(render("%05d", &["42"]), "00042");
        assert_eq!(render("%.3d", &["7"]), "007");
        assert_eq!(render("%x", &["255"]), "ff");
        assert_eq!(render("%%%s", &["x"]), "%x");
    }
}
