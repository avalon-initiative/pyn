//! `--word-diff`: the lines a hunk removes and adds, compared word by word and printed in one run of text.

use pyn_diff::{Hunk, Op, Ws};
use regex::bytes::Regex;

use crate::diff_color::{RESET, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plain,
    Color,
    Porcelain,
}

/// What `--word-diff[=<mode>]`, `--color-words` and `--word-diff-regex` ask for.
#[derive(Debug, Clone)]
pub struct Request {
    pub mode: Mode,
    pub regex: Option<String>,
}

/// A request with its word regular expression compiled.
pub struct Setup {
    pub request: Request,
    pub regex: Option<Regex>,
}

impl Setup {
    /// The word diff `opts` ask for; a bad regular expression is a usage error.
    pub fn of(
        opts: &crate::diff_opts::Opts,
        setting: &dyn Fn(&str) -> Option<String>,
    ) -> Option<Self> {
        let request = opts.word.clone()?;
        let pattern = request.regex.clone().or_else(|| setting("diff.wordregex"));
        let regex = pattern.map(|p| {
            crate::git_home::grep_regex::compile(
                &p,
                crate::git_home::grep_regex::Syntax::Extended,
                false,
            )
            .unwrap_or_else(|e| {
                crate::usage_error(&format!("invalid regular expression: {p}: {e}"))
            })
        });
        Some(Self { request, regex })
    }
}

struct Piece {
    prefix: &'static str,
    suffix: &'static str,
    color: String,
}

struct Pieces {
    old: Piece,
    new: Piece,
    context: Piece,
    newline: &'static str,
}

impl Pieces {
    fn of(mode: Mode, style: Option<&Style>) -> Self {
        let piece = |prefix, suffix, color: Option<&String>| Piece {
            prefix,
            suffix,
            color: color.cloned().unwrap_or_default(),
        };
        let p = style.map(|s| &s.palette);
        let (old, new, context) = (p.map(|p| &p.old), p.map(|p| &p.new), p.map(|p| &p.context));
        match mode {
            Mode::Plain => Self {
                old: piece("[-", "-]", old),
                new: piece("{+", "+}", new),
                context: piece("", "", context),
                newline: "\n",
            },
            Mode::Color => Self {
                old: piece("", "", old),
                new: piece("", "", new),
                context: piece("", "", context),
                newline: "\n",
            },
            Mode::Porcelain => Self {
                old: piece("-", "\n", old),
                new: piece("+", "\n", new),
                context: piece(" ", "\n", context),
                newline: "~\n",
            },
        }
    }

    /// `text` in `piece`'s markers, line by line, each line followed by the mode's newline.
    fn write(&self, out: &mut Vec<u8>, piece: &Piece, text: &[u8]) {
        let mut rest = text;
        while !rest.is_empty() {
            let end = rest.iter().position(|c| *c == b'\n');
            let line = &rest[..end.unwrap_or(rest.len())];
            if !line.is_empty() {
                out.extend_from_slice(piece.color.as_bytes());
                out.extend_from_slice(piece.prefix.as_bytes());
                out.extend_from_slice(line);
                out.extend_from_slice(piece.suffix.as_bytes());
                if !piece.color.is_empty() {
                    out.extend_from_slice(RESET.as_bytes());
                }
            }
            let Some(end) = end else { break };
            out.extend_from_slice(self.newline.as_bytes());
            rest = &rest[end + 1..];
        }
    }
}

/// The byte ranges of the words in `text`: runs of non-space characters, or the matches of `regex`.
fn words(text: &[u8], regex: Option<&Regex>) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match regex {
            None => {
                let space = |c: u8| c.is_ascii_whitespace() || c == 0x0b;
                while at < text.len() && space(text[at]) {
                    at += 1;
                }
                let start = at;
                while at < text.len() && !space(text[at]) {
                    at += 1;
                }
                if start < at {
                    out.push((start, at));
                }
            }
            Some(re) => {
                let Some(m) = re.find_at(text, at) else { break };
                if m.start() == m.end() {
                    at = m.start() + 1;
                    continue;
                }
                let end = text[m.start()..m.end()]
                    .iter()
                    .position(|c| *c == b'\n')
                    .map_or(m.end(), |p| m.start() + p);
                if end > m.start() {
                    out.push((m.start(), end));
                }
                at = end.max(m.start() + 1);
            }
        }
    }
    out
}

/// One run of changed lines: the text of the lines taken away and of the lines put in.
fn show(out: &mut Vec<u8>, minus: &[u8], plus: &[u8], re: Option<&Regex>, pieces: &Pieces) {
    if plus.is_empty() {
        pieces.write(out, &pieces.old, minus);
        return;
    }
    let (mw, pw) = (words(minus, re), words(plus, re));
    let list = |text: &[u8], w: &[(usize, usize)]| -> Vec<u8> {
        w.iter()
            .flat_map(|(a, b)| text[*a..*b].iter().copied().chain(std::iter::once(b'\n')))
            .collect()
    };
    let edits = crate::xdiff::hunks_ws(&list(minus, &mw), &list(plus, &pw), &Ws::default(), false);
    let span = |w: &[(usize, usize)], start: usize, count: usize| {
        let begin = if count > 0 {
            w[start].0
        } else if start == 0 {
            0
        } else {
            w[start - 1].1
        };
        let end = if count == 0 {
            begin
        } else {
            w[start + count - 1].1
        };
        (begin, end)
    };
    let mut current = 0;
    for e in edits {
        let (mb, me) = span(&mw, e.a_start, e.a_count);
        let (pb, pe) = span(&pw, e.b_start, e.b_count);
        if current != pb {
            pieces.write(out, &pieces.context, &plus[current..pb]);
        }
        if mb != me {
            pieces.write(out, &pieces.old, &minus[mb..me]);
        }
        if pb != pe {
            pieces.write(out, &pieces.new, &plus[pb..pe]);
        }
        current = pe;
    }
    if current != plus.len() {
        pieces.write(out, &pieces.context, &plus[current..]);
    }
}

fn with_newline(line: &[u8]) -> Vec<u8> {
    let mut text = line.to_vec();
    if !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    text
}

/// The hunks of a file as word diff text.
pub fn render(hunks: &[Hunk], setup: &Setup, style: Option<&Style>) -> Vec<u8> {
    let (request, regex) = (&setup.request, setup.regex.as_ref());
    let pieces = Pieces::of(request.mode, style);
    let mut out = Vec::new();
    for h in hunks {
        match style {
            Some(s) => out.extend(s.hunk_header(h).into_bytes()),
            None => out.extend(h.header().into_bytes()),
        }
        let (mut minus, mut plus) = (Vec::new(), Vec::new());
        for (op, line) in &h.lines {
            let text = with_newline(line);
            match op {
                Op::Delete => minus.extend(text),
                Op::Insert => plus.extend(text),
                Op::Equal => {
                    if !minus.is_empty() || !plus.is_empty() {
                        show(&mut out, &minus, &plus, regex, &pieces);
                        (minus, plus) = (Vec::new(), Vec::new());
                    }
                    let shown: &[u8] = if request.mode == Mode::Porcelain {
                        &[b" ", text.as_slice()].concat()
                    } else {
                        &text
                    };
                    match style {
                        Some(s) => out.extend(s.words_context(shown)),
                        None => out.extend_from_slice(shown),
                    }
                    if request.mode == Mode::Porcelain {
                        out.extend_from_slice(b"~\n");
                    }
                }
            }
        }
        if !minus.is_empty() || !plus.is_empty() {
            show(&mut out, &minus, &plus, regex, &pieces);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(plain: &[u8], mode: Mode) -> String {
        let setup = Setup {
            request: Request { mode, regex: None },
            regex: None,
        };
        let hunk = Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            heading: String::new(),
            lines: vec![
                (Op::Delete, b"a b c\n".to_vec()),
                (Op::Insert, plain.to_vec()),
            ],
        };
        String::from_utf8(render(&[hunk], &setup, None)).unwrap()
    }

    #[test]
    fn words_are_runs_of_non_space_or_regex_matches() {
        assert_eq!(words(b" ab  c\n", None), vec![(1, 3), (5, 6)]);
        let re = Regex::new("[a-z]+").unwrap();
        assert_eq!(words(b"ab,cd", Some(&re)), vec![(0, 2), (3, 5)]);
        let empty = Regex::new("x*").unwrap();
        assert_eq!(words(b"axb", Some(&empty)), vec![(1, 2)]);
    }

    #[test]
    fn changed_words_are_marked_in_the_new_text() {
        assert_eq!(
            pieces(b"a X c\n", Mode::Plain),
            "@@ -1 +1 @@\na [-b-]{+X+} c\n"
        );
        assert_eq!(pieces(b"a c\n", Mode::Plain), "@@ -1 +1 @@\na[-b-] c\n");
        assert_eq!(
            pieces(b"a X c\n", Mode::Porcelain),
            "@@ -1 +1 @@\n a \n-b\n+X\n  c\n~\n"
        );
    }
}
