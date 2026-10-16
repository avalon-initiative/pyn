//! Text that came from a server, made safe to print: control characters other than newline, carriage return and tab
//! are dropped, including the C1 range.

use std::borrow::Cow;

fn keep(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\t') || !(c.is_control())
}

/// `s` without control characters; borrowed when there were none.
pub fn text(s: &str) -> Cow<'_, str> {
    if s.chars().all(keep) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.chars().filter(|c| keep(*c)).collect())
    }
}

/// Like [`text`] for bytes; bytes that are not UTF-8 become U+FFFD.
pub fn bytes(b: &[u8]) -> Cow<'_, [u8]> {
    match std::str::from_utf8(b) {
        Ok(s) => match text(s) {
            Cow::Borrowed(_) => Cow::Borrowed(b),
            Cow::Owned(o) => Cow::Owned(o.into_bytes()),
        },
        Err(_) => Cow::Owned(text(&String::from_utf8_lossy(b)).into_owned().into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_untouched() {
        for s in [
            "",
            "plain",
            "a\tb\nc\r",
            "caf\u{e9} \u{65e5}\u{672c} \u{a0}",
        ] {
            assert!(matches!(text(s), Cow::Borrowed(_)), "{s:?}");
            assert_eq!(text(s), s);
        }
    }

    #[test]
    fn control_characters_are_dropped_and_layout_kept() {
        let cases = [
            ("a\x1b[31mred\x1b[0m", "a[31mred[0m"),
            ("bell\x07", "bell"),
            ("nul\0here", "nulhere"),
            ("del\x7fx", "delx"),
            ("csi\u{9b}2Jx", "csi2Jx"),
            ("c1\u{85}\u{80}\u{9f}x", "c1x"),
            ("keep\nthis\rand\tthis", "keep\nthis\rand\tthis"),
            ("\x08\x0b\x0c", ""),
        ];
        for (input, want) in cases {
            assert_eq!(text(input), want, "{input:?}");
        }
    }

    #[test]
    fn bytes_follow_the_same_rules() {
        assert_eq!(bytes(b"ok\x1b]0;title\x07\n"), &b"ok]0;title\n"[..]);
        assert_eq!(bytes("x\u{9b}y".as_bytes()), &b"xy"[..]);
        assert_eq!(bytes(b"a\xffb\x1b"), "a\u{fffd}b".as_bytes());
        assert!(matches!(bytes(b"plain\r\n"), Cow::Borrowed(_)));
    }
}
