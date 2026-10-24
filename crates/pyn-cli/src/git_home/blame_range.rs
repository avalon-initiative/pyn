//! `-L` line ranges for `pyn blame`: `<start>,<end>`, `+<n>` and `-<n>` offsets, and `/regex/` anchors.

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

/// The 1-based `(begin, end)` of `arg`, where 0 means open on that side; `anchor` is where a regex search starts.
pub(super) fn parse(arg: &str, lines: &[&[u8]], anchor: i64) -> Result<(i64, i64), RangeError> {
    let count = lines.len() as i64;
    let anchor = anchor.clamp(1, count + 1);
    if arg.starts_with(':') || arg.starts_with("^:") {
        return Err(RangeError::Unsupported(
            "-L :<funcname> needs git's per-language function patterns",
        ));
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
        parse(arg, &text(), 1).ok()
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
        assert!(matches!(parse("x", &text(), 1), Err(RangeError::Usage)));
        assert!(matches!(parse("2,3x", &text(), 1), Err(RangeError::Usage)));
        assert!(matches!(
            parse("0,3", &text(), 1),
            Err(RangeError::Fatal(_))
        ));
        assert!(matches!(
            parse(":main", &text(), 1),
            Err(RangeError::Unsupported(_))
        ));
    }
}
