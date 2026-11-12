//! Matching for `pyn log` filters: POSIX basic regular expressions (git's default) and the date forms `--since` takes.

use super::approxidate::{approxidate, parse_date_basic};
use anyhow::{Result, bail};
use chrono::{DateTime, Local, TimeZone};
use regex::{Regex, RegexBuilder};

/// How `--grep`, `--author` and `--committer` read their patterns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Basic,
    Extended,
    Fixed,
}

/// An extended regular expression, as `-G` and `-E` take it.
pub fn compile_ere(pattern: &str, ignore_case: bool) -> Result<Regex> {
    compile_as(pattern, ignore_case, Dialect::Extended)
}

/// A pattern in `dialect`, matched per line as git does.
pub fn compile_as(pattern: &str, ignore_case: bool, dialect: Dialect) -> Result<Regex> {
    let translated = match dialect {
        Dialect::Basic => bre_to_regex(pattern)?,
        Dialect::Extended => ere_to_regex(pattern)?,
        Dialect::Fixed => regex::escape(pattern),
    };
    RegexBuilder::new(&translated)
        .multi_line(true)
        .case_insensitive(ignore_case)
        .build()
        .or_else(|_| bail!("command line, '{pattern}': Invalid regular expression"))
}

fn literal(c: char, out: &mut String) {
    out.push_str(&regex::escape(&c.to_string()));
}

/// Translates BRE (with GNU's `\|`, `\+` and `\?`) into the `regex` crate's syntax.
fn bre_to_regex(pattern: &str) -> Result<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut at_start = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let was_start = std::mem::replace(&mut at_start, false);
        match c {
            '\\' => {
                i += 1;
                let Some(&n) = chars.get(i) else {
                    bail!("command line, '{pattern}': Trailing backslash");
                };
                match n {
                    '(' => {
                        out.push('(');
                        at_start = true;
                    }
                    '|' => {
                        out.push('|');
                        at_start = true;
                    }
                    ')' | '{' | '}' | '+' | '?' => out.push(n),
                    '<' | '>' => out.push_str("\\b"),
                    'w' | 'W' | 's' | 'S' | 'b' | 'B' => {
                        out.push('\\');
                        out.push(n);
                    }
                    '1'..='9' => {
                        bail!("command line, '{pattern}': back-references are not supported")
                    }
                    _ => literal(n, &mut out),
                }
            }
            '[' => i = bracket(&chars, i, &mut out, pattern)?,
            '.' => out.push('.'),
            '*' if !was_start => out.push('*'),
            '^' if was_start => {
                out.push('^');
                at_start = true;
            }
            '$' if i + 1 == chars.len()
                || (chars[i + 1] == '\\' && matches!(chars.get(i + 2), Some(')' | '|'))) =>
            {
                out.push('$');
            }
            _ => literal(c, &mut out),
        }
        i += 1;
    }
    Ok(out)
}

/// Translates ERE into the `regex` crate's syntax.
fn ere_to_regex(pattern: &str) -> Result<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
                let Some(&n) = chars.get(i) else {
                    bail!("command line, '{pattern}': Trailing backslash");
                };
                match n {
                    '<' | '>' => out.push_str("\\b"),
                    'w' | 'W' | 's' | 'S' | 'b' | 'B' => {
                        out.push('\\');
                        out.push(n);
                    }
                    '1'..='9' => {
                        bail!("command line, '{pattern}': back-references are not supported")
                    }
                    _ => literal(n, &mut out),
                }
            }
            '[' => i = bracket(&chars, i, &mut out, pattern)?,
            '}' => literal(c, &mut out),
            '(' if chars.get(i + 1) == Some(&'?') => {
                bail!("command line, '{pattern}': Invalid preceding regular expression")
            }
            '{' => match interval_end(&chars, i) {
                Some(end) => {
                    out.extend(&chars[i..=end]);
                    i = end;
                }
                None => bail!("command line, '{pattern}': Invalid preceding regular expression"),
            },
            '(' | ')' | '|' | '*' | '+' | '?' | '.' | '^' | '$' => out.push(c),
            _ => literal(c, &mut out),
        }
        i += 1;
    }
    Ok(out)
}

/// The index of the `}` closing an interval (`{2}`, `{2,}`, `{2,5}`) that starts at `chars[start]`.
fn interval_end(chars: &[char], start: usize) -> Option<usize> {
    let close = (start + 1..chars.len()).find(|&j| chars[j] == '}')?;
    let body: String = chars[start + 1..close].iter().collect();
    let (lo, hi) = body.split_once(',').unwrap_or((&body, &body));
    let digits = |t: &str| t.bytes().all(|b| b.is_ascii_digit());
    (!lo.is_empty() && digits(lo) && digits(hi)).then_some(close)
}

/// Copies the bracket expression starting at `chars[start]` and returns the index of its closing `]`.
fn bracket(chars: &[char], start: usize, out: &mut String, pattern: &str) -> Result<usize> {
    let mut i = start + 1;
    out.push('[');
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        out.push_str("\\]");
        i += 1;
    }
    while let Some(&c) = chars.get(i) {
        match c {
            ']' => {
                out.push(']');
                return Ok(i);
            }
            '[' if chars.get(i + 1) == Some(&':') => {
                let Some(end) = (i + 2..chars.len().saturating_sub(1))
                    .find(|&j| chars[j] == ':' && chars[j + 1] == ']')
                else {
                    break;
                };
                out.extend(&chars[i..end + 2]);
                i = end + 1;
            }
            '[' | '\\' | '&' | '~' | '|' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    bail!("command line, '{pattern}': Unmatched [, [^, [:, [., or [=")
}

/// The current time, or `GIT_TEST_DATE_NOW` (seconds) when git's own override is set.
pub fn now() -> DateTime<Local> {
    std::env::var("GIT_TEST_DATE_NOW")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|t| Local.timestamp_opt(t, 0).single())
        .unwrap_or_else(Local::now)
}

/// Seconds since the epoch for a `--since` / `--until` value.
pub fn parse_date(text: &str, now: DateTime<Local>) -> Result<i64> {
    if let Some(secs) = parse_date_basic(text, now.timestamp()) {
        return Ok(secs);
    }
    let bad = || anyhow::anyhow!("unsupported date '{text}'");
    match approxidate(text, now.timestamp()) {
        (secs, true) => Ok(secs),
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(pattern: &str, ignore_case: bool) -> Result<Regex> {
        compile_as(pattern, ignore_case, Dialect::Basic)
    }

    fn now() -> DateTime<Local> {
        Local.timestamp_opt(1_800_000_000, 0).unwrap()
    }

    #[test]
    fn basic_expressions_use_escaped_operators() {
        let m = |p: &str, t: &str| compile(p, false).unwrap().is_match(t);
        assert!(m("a\\|b", "xb") && !m("a|b", "xb") && m("a|b", "a|b"));
        assert!(m("^one$", "x\none\ny") && !m("^one$", "ones"));
        assert!(m("a\\{2\\}", "aa") && m("(x)", "(x)") && m("a+", "a+"));
        assert!(m("*a", "*a") && m("a.c", "abc") && m("[[:digit:]]\\+", "x12"));
        assert!(m("[]x]", "]") && m("a$b", "a$b") && m("\\(ab\\)*c", "c"));
        assert!(compile("[a", false).is_err() && compile("a\\", false).is_err());
    }

    #[test]
    fn extended_and_fixed_expressions() {
        let ere = |p: &str, t: &str| compile_ere(p, false).unwrap().is_match(t);
        assert!(ere("a|b", "b") && ere("(ab)+c", "ababc") && ere("a{2}", "aa"));
        assert!(ere("a\\|b", "a|b") && !ere("a\\|b", "a") && compile_ere("x{", false).is_err());
        assert!(ere("\\<ab\\>", "x ab y") && ere("a\\.b", "a.b") && !ere("a\\.b", "axb"));
        assert!(compile_ere("(", false).is_err() && compile_ere("a\\1", false).is_err());
        let fixed = compile_as("a.b*", false, Dialect::Fixed).unwrap();
        assert!(fixed.is_match("xa.b*") && !fixed.is_match("axb"));
    }

    #[test]
    fn ignore_case_is_optional() {
        assert!(compile("ABC", true).unwrap().is_match("abc"));
        assert!(!compile("ABC", false).unwrap().is_match("abc"));
    }

    #[test]
    fn dates_parse_to_seconds() {
        let at = |s: &str| parse_date(s, now()).unwrap();
        assert_eq!(at("1700003500"), 1_700_003_500);
        assert_eq!(at("@1700003500"), 1_700_003_500);
        assert_eq!(at("2023-11-14T23:10:00Z"), 1_700_003_400);
        assert_eq!(at("2023-11-14 23:10:00 +0100"), 1_700_003_400 - 3_600);
        assert_eq!(at("2023-11-14 23:10:00 -05:00"), 1_700_003_400 + 5 * 3600);
        assert_eq!(at("Tue, 14 Nov 2023 23:10:00 +0000"), 1_700_003_400);
        assert_eq!(at("2 hours ago"), 1_800_000_000 - 7200);
        assert_eq!(at("1 day 2 hours ago"), 1_800_000_000 - 86_400 - 7200);
        assert_eq!(at("now"), 1_800_000_000);
        assert_eq!(at("3 days ago"), 1_800_000_000 - 3 * 86_400);
        assert_eq!(at("2 weeks 3 days ago"), 1_800_000_000 - 17 * 86_400);
        assert_eq!(at("five minutes"), 1_800_000_000 - 300);
        assert!(parse_date("garbage", now()).is_err());
    }
}
