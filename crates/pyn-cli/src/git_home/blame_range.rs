//! `-L` line ranges for `pyn blame`: `<start>,<end>`, `+<n>` and `-<n>` offsets, `/regex/` anchors and `:funcname`.

use super::grep_regex::{Syntax, compile};

pub(super) enum RangeError {
    /// git prints its usage and exits 129.
    Usage,
    /// git dies with this message (128).
    Fatal(String),
    /// A form pyn does not build.
    Unsupported(&'static str),
}

/// `strtol` for a base-10 number: the value and how many bytes it used (0 when there is no number).
fn strtol(text: &str) -> (i64, usize) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = bytes.get(i) == Some(&b'-');
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits = i;
    let mut value: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(bytes[i] - b'0'));
        i += 1;
    }
    if i == digits {
        return (0, 0);
    }
    (if negative { -value } else { value }, i)
}

fn parse_loc<'a>(
    spec: &'a str,
    lines: &[&[u8]],
    begin: i64,
    ret: &mut i64,
) -> Result<&'a str, RangeError> {
    if begin >= 1 && (spec.starts_with('+') || spec.starts_with('-')) {
        let (num, used) = strtol(&spec[1..]);
        if used == 0 {
            return Ok(spec);
        }
        if num == 0 {
            return Err(RangeError::Fatal("-L invalid empty range".into()));
        }
        let num = if spec.starts_with('-') { -num } else { num };
        *ret = match num {
            n if n > 0 => begin + n - 2,
            n if begin + n > 0 => begin + n,
            _ => 1,
        };
        return Ok(&spec[1 + used..]);
    }
    let (num, used) = strtol(spec);
    if used > 0 {
        if num <= 0 {
            return Err(RangeError::Fatal(format!("-L invalid line number: {num}")));
        }
        *ret = num;
        return Ok(&spec[used..]);
    }
    let (mut begin, mut spec) = (begin, spec);
    if begin < 0 {
        if let Some(rest) = spec.strip_prefix('^') {
            begin = 1;
            spec = rest;
        } else {
            begin = -begin;
        }
    }
    if !spec.starts_with('/') {
        return Ok(spec);
    }
    let bytes = spec.as_bytes();
    let mut end = 1;
    while end < bytes.len() && bytes[end] != b'/' {
        if bytes[end] == b'\\' {
            end += 1;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return Ok(spec);
    }
    let pattern = &spec[1..end];
    let regex = compile(pattern, Syntax::Basic, false).map_err(|e| {
        RangeError::Fatal(format!(
            "-L parameter '{pattern}' starting at line {begin}: {e}"
        ))
    })?;
    let from = usize::try_from(begin - 1).unwrap_or(0);
    let found = lines
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, line)| regex.is_match(line.strip_suffix(b"\n").unwrap_or(line)));
    match found {
        Some((at, _)) => {
            *ret = at as i64 + 1;
            Ok(&spec[end + 1..])
        }
        None => Err(RangeError::Fatal(format!(
            "-L parameter '{pattern}' starting at line {begin}: No match"
        ))),
    }
}

/// Whether a line (without its newline) starts a function; `None` when the path's diff driver is a built-in one.
pub(super) type FuncLines<'a> = Option<&'a dyn Fn(&[u8]) -> bool>;

/// `:<regex>`: from the first function line that matches to the line before the next function line.
fn parse_funcname(
    arg: &str,
    lines: &[&[u8]],
    anchor: i64,
    funcs: FuncLines<'_>,
) -> Result<(i64, i64), RangeError> {
    let (anchor, rest) = match arg.strip_prefix('^') {
        Some(rest) => (1, rest),
        None => (anchor, arg),
    };
    let body = &rest[1..];
    let pattern = body.split(':').next().unwrap_or_default();
    if pattern.is_empty() {
        return Err(RangeError::Usage);
    }
    let Some(is_func) = funcs else {
        return Err(RangeError::Unsupported(
            "-L :<funcname> with a built-in diff driver is not supported on a git repository yet",
        ));
    };
    let regex = compile(pattern, Syntax::Basic, false)
        .map_err(|e| RangeError::Fatal(format!("-L parameter '{pattern}': {e}")))?;
    let plain = |line: &[u8]| line.strip_suffix(b"\n").unwrap_or(line).to_vec();
    let from = usize::try_from(anchor - 1).unwrap_or(0);
    let begin = lines.iter().enumerate().skip(from).find(|(_, line)| {
        let line = plain(line);
        regex.is_match(&line) && is_func(&line)
    });
    let Some((begin, _)) = begin else {
        return Err(RangeError::Fatal(format!(
            "-L parameter '{pattern}' starting at line {anchor}: no match"
        )));
    };
    if body.len() != pattern.len() {
        return Err(RangeError::Usage);
    }
    let end = lines
        .iter()
        .enumerate()
        .skip(begin + 1)
        .find(|(_, line)| is_func(&plain(line)))
        .map_or(lines.len(), |(at, _)| at);
    Ok((begin as i64 + 1, end as i64))
}

/// The 1-based `(begin, end)` of `arg`, where 0 means open on that side; `anchor` is where a regex search starts.
pub(super) fn parse(
    arg: &str,
    lines: &[&[u8]],
    anchor: i64,
    funcs: FuncLines<'_>,
) -> Result<(i64, i64), RangeError> {
    let count = lines.len() as i64;
    let anchor = anchor.clamp(1, count + 1);
    if arg.starts_with(':') || arg.starts_with("^:") {
        return parse_funcname(arg, lines, anchor, funcs);
    }
    let (mut begin, mut end) = (0, 0);
    let mut rest = parse_loc(arg, lines, -anchor, &mut begin)?;
    if let Some(after) = rest.strip_prefix(',') {
        rest = parse_loc(after, lines, begin + 1, &mut end)?;
    }
    if !rest.is_empty() {
        return Err(RangeError::Usage);
    }
    if begin != 0 && end != 0 && end < begin {
        std::mem::swap(&mut begin, &mut end);
    }
    Ok((begin, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Vec<&'static [u8]> {
        vec![&b"one\n"[..], b"two\n", b"three\n", b"four\n"]
    }

    fn range(arg: &str) -> Option<(i64, i64)> {
        parse(arg, &text(), 1, Some(&|_| true)).ok()
    }

    #[test]
    fn numbers_offsets_and_swaps() {
        assert_eq!(range("2,3"), Some((2, 3)));
        assert_eq!(range("2,+2"), Some((2, 3)));
        assert_eq!(range("3,-2"), Some((2, 3)));
        assert_eq!(range("4,2"), Some((2, 4)));
        assert_eq!(range("2"), Some((2, 0)));
        assert_eq!(range(",3"), Some((0, 3)));
    }

    #[test]
    fn regex_anchors_search_from_the_anchor_line() {
        assert_eq!(range("/t/"), Some((2, 0)));
        assert_eq!(range("/^f/,+1"), Some((4, 4)));
        assert_eq!(range("2,/t/"), Some((2, 3)));
        assert!(range("/zzz/").is_none());
    }

    #[test]
    fn junk_and_function_names_are_errors() {
        assert!(matches!(
            parse("x", &text(), 1, None),
            Err(RangeError::Usage)
        ));
        assert!(matches!(
            parse("2,3x", &text(), 1, None),
            Err(RangeError::Usage)
        ));
        assert!(matches!(
            parse("0,3", &text(), 1, None),
            Err(RangeError::Fatal(_))
        ));
        assert!(matches!(
            parse(":main", &text(), 1, None),
            Err(RangeError::Unsupported(_))
        ));
        assert!(matches!(
            parse(":main", &text(), 1, Some(&|_| true)),
            Err(RangeError::Fatal(_))
        ));
    }

    #[test]
    fn a_function_ends_before_the_next_one() {
        let starts = |l: &[u8]| l.starts_with(b"t");
        assert_eq!(parse(":two", &text(), 1, Some(&starts)).ok(), Some((2, 2)));
        assert_eq!(
            parse(":three", &text(), 1, Some(&starts)).ok(),
            Some((3, 4))
        );
        assert!(matches!(
            parse(":two:x", &text(), 1, Some(&starts)),
            Err(RangeError::Usage)
        ));
    }
}
