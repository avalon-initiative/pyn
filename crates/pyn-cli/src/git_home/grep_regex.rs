//! POSIX basic and extended regular expressions (with the GNU additions git relies on) and fixed strings, translated
//! into the `regex` crate for `pyn grep`. Back-references are the one thing the target engine lacks.

use regex::bytes::{Regex, RegexBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syntax {
    Basic,
    Extended,
    Fixed,
}

const RE_DUP_MAX: u32 = 0x7fff;
const EBRACK: &str = "Unmatched [, [^, [:, [., or [=";
const BADRPT: &str = "Invalid preceding regular expression";

/// Compiles `pattern`; the error is the text after git's `command line, '<pattern>': `.
pub fn compile(pattern: &str, syntax: Syntax, ignore_case: bool) -> Result<Regex, String> {
    let translated = match syntax {
        Syntax::Fixed => regex::escape(pattern),
        _ => Parser::new(pattern, syntax == Syntax::Extended, ignore_case).translate()?,
    };
    RegexBuilder::new(&translated)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => "Regular expression too big".to_string(),
            _ => "Invalid regular expression".to_string(),
        })
}

#[derive(Clone, Copy, PartialEq)]
enum Prev {
    Nothing,
    Anchor,
    Atom,
    Repeat,
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    ere: bool,
    icase: bool,
    closed_groups: u32,
}

impl Parser {
    fn new(pattern: &str, ere: bool, icase: bool) -> Self {
        Self {
            chars: pattern.chars().collect(),
            at: 0,
            ere,
            icase,
            closed_groups: 0,
        }
    }

    fn translate(mut self) -> Result<String, String> {
        let out = self.alternation(0)?;
        if self.at < self.chars.len() {
            return Err("Unmatched ) or \\)".into());
        }
        Ok(out)
    }

    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn alternation(&mut self, depth: usize) -> Result<String, String> {
        let mut branches = vec![self.branch(depth)?];
        while self.at_bar() {
            self.at += if self.ere { 1 } else { 2 };
            branches.push(self.branch(depth)?);
        }
        Ok(branches.join("|"))
    }

    fn at_bar(&self) -> bool {
        if self.ere {
            self.peek(0) == Some('|')
        } else {
            self.peek(0) == Some('\\') && self.peek(1) == Some('|')
        }
    }

    fn at_close(&self) -> bool {
        if self.ere {
            self.peek(0) == Some(')')
        } else {
            self.peek(0) == Some('\\') && self.peek(1) == Some(')')
        }
    }

    fn branch(&mut self, depth: usize) -> Result<String, String> {
        let mut items: Vec<String> = Vec::new();
        let mut prev = Prev::Nothing;
        while self.at < self.chars.len() && !self.at_bar() {
            if self.at_close() {
                if depth > 0 {
                    break;
                }
                if self.ere {
                    self.at += 1;
                    items.push("\\)".into());
                    prev = Prev::Atom;
                    continue;
                }
                return Err("Unmatched ( or \\(".into());
            }
            let c = self.chars[self.at];
            self.at += 1;
            let (item, kind) = match c {
                '\\' => self.escape(&mut items, &mut prev, depth)?,
                '[' => (self.bracket()?, Prev::Atom),
                '.' => (".".to_string(), Prev::Atom),
                '*' => {
                    self.repeat(&mut items, &mut prev, "*", '*')?;
                    continue;
                }
                '^' if self.ere || prev == Prev::Nothing => ("^".into(), Prev::Anchor),
                '$' if self.ere || self.dollar_is_anchor() => ("$".into(), Prev::Anchor),
                '(' if self.ere => (self.group(depth)?, Prev::Atom),
                '+' | '?' if self.ere => {
                    self.repeat(&mut items, &mut prev, &c.to_string(), c)?;
                    continue;
                }
                '{' if self.ere => {
                    let bounds = self.interval()?;
                    self.bound(&mut items, &mut prev, &bounds)?;
                    continue;
                }
                _ => (regex::escape(&c.to_string()), Prev::Atom),
            };
            if !item.is_empty() {
                items.push(item);
            }
            prev = kind;
        }
        Ok(items.concat())
    }

    fn dollar_is_anchor(&self) -> bool {
        self.at == self.chars.len()
            || (self.peek(0) == Some('\\') && matches!(self.peek(1), Some(')' | '|')))
    }

    fn group(&mut self, depth: usize) -> Result<String, String> {
        let inner = self.alternation(depth + 1)?;
        if !self.at_close() {
            return Err("Unmatched ( or \\(".into());
        }
        self.at += if self.ere { 1 } else { 2 };
        self.closed_groups += 1;
        Ok(format!("(?:{inner})"))
    }

    fn escape(
        &mut self,
        items: &mut Vec<String>,
        prev: &mut Prev,
        depth: usize,
    ) -> Result<(String, Prev), String> {
        let Some(n) = self.peek(0) else {
            return Err("Trailing backslash".into());
        };
        self.at += 1;
        let literal = || (regex::escape(&n.to_string()), Prev::Atom);
        Ok(match n {
            '(' if !self.ere => (self.group(depth)?, Prev::Atom),
            '{' if !self.ere => {
                let bounds = self.interval()?;
                self.bound(items, prev, &bounds)?;
                (String::new(), *prev)
            }
            '+' | '?' if !self.ere && !matches!(prev, Prev::Nothing | Prev::Anchor) => {
                self.repeat(items, prev, &n.to_string(), n)?;
                (String::new(), *prev)
            }
            '<' => ("\\b{start}".into(), Prev::Anchor),
            '>' => ("\\b{end}".into(), Prev::Anchor),
            'b' => ("\\b".into(), Prev::Anchor),
            'B' => ("\\B".into(), Prev::Anchor),
            '`' | '\'' => return Err("buffer anchors are not supported".into()),
            c if self.icase && c.is_ascii_lowercase() && !matches!(c, 'w' | 's' | 'b') => {
                return Err("a backslash before a letter is not supported with -i".into());
            }
            'w' | 'W' | 's' | 'S' => (format!("\\{n}"), Prev::Atom),
            '1'..='9' => {
                let number = n.to_digit(10).unwrap_or(0);
                return Err(if number > self.closed_groups {
                    "Invalid back reference".into()
                } else {
                    "back-references are not supported".into()
                });
            }
            _ => literal(),
        })
    }

    fn repeat(
        &mut self,
        items: &mut Vec<String>,
        prev: &mut Prev,
        op: &str,
        c: char,
    ) -> Result<(), String> {
        match *prev {
            Prev::Nothing | Prev::Anchor if self.ere => return Err(BADRPT.into()),
            Prev::Nothing | Prev::Anchor => {
                items.push(regex::escape(&c.to_string()));
                *prev = Prev::Atom;
                return Ok(());
            }
            Prev::Repeat if !self.ere && c == '*' => return Err(BADRPT.into()),
            _ => {}
        }
        wrap_last(items, op);
        *prev = Prev::Repeat;
        Ok(())
    }

    fn bound(&mut self, items: &mut [String], prev: &mut Prev, bounds: &str) -> Result<(), String> {
        if matches!(*prev, Prev::Nothing | Prev::Anchor) || (*prev == Prev::Repeat && !self.ere) {
            return Err(BADRPT.into());
        }
        wrap_last(items, bounds);
        *prev = Prev::Repeat;
        Ok(())
    }

    /// Reads `m`, `m,`, `,n` or `m,n` and its closing brace, after the opening one.
    fn interval(&mut self) -> Result<String, String> {
        let close: &[char] = if self.ere { &['}'] } else { &['\\', '}'] };
        let start = self.at;
        let end = (start..self.chars.len())
            .find(|&i| self.chars[i..].starts_with(close))
            .ok_or("Unmatched \\{")?;
        let body: String = self.chars[start..end].iter().collect();
        self.at = end + close.len();
        let bad = || "Invalid content of \\{\\}".to_string();
        let number = |s: &str| -> Result<Option<u32>, String> {
            if s.is_empty() {
                return Ok(None);
            }
            if !s.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            let n: u32 = s.parse().unwrap_or(u32::MAX);
            if n > RE_DUP_MAX {
                return Err("Regular expression too big".into());
            }
            Ok(Some(n))
        };
        let (low, high) = match body.split_once(',') {
            None => {
                let n = number(&body)?.ok_or_else(bad)?;
                (Some(n), Some(n))
            }
            Some((l, h)) => (number(l)?, number(h)?),
        };
        if body.is_empty() {
            return Err(bad());
        }
        if let (Some(l), Some(h)) = (low, high)
            && l > h
        {
            return Err(bad());
        }
        Ok(format!(
            "{{{},{}}}",
            low.unwrap_or(0),
            high.map_or(String::new(), |h| h.to_string())
        ))
    }

    /// Reads the rest of a bracket expression after its `[`.
    fn bracket(&mut self) -> Result<String, String> {
        let mut out = String::from("[");
        if self.peek(0) == Some('^') {
            out.push('^');
            self.at += 1;
        }
        let mut first = true;
        loop {
            let Some(c) = self.peek(0) else {
                return Err(if first {
                    "Invalid regular expression".into()
                } else {
                    EBRACK.into()
                });
            };
            if c == ']' && !first {
                self.at += 1;
                out.push(']');
                return Ok(out);
            }
            first = false;
            if c == '[' && self.peek(1) == Some(':') {
                out.push_str(&self.class()?);
                self.no_range_after()?;
                continue;
            }
            let low = self.bracket_char()?;
            if !(self.peek(0) == Some('-') && self.peek(1).is_some_and(|n| n != ']')) {
                push_class_char(&mut out, low);
                continue;
            }
            self.at += 1;
            if self.peek(0) == Some('[') && self.peek(1) == Some(':') {
                return Err("Invalid range end".into());
            }
            let high = self.bracket_char()?;
            if low > high {
                return Err("Invalid range end".into());
            }
            push_class_char(&mut out, low);
            out.push('-');
            push_class_char(&mut out, high);
            self.no_range_after()?;
        }
    }

    /// A class or a finished range cannot start another range.
    fn no_range_after(&self) -> Result<(), String> {
        if self.peek(0) == Some('-') && self.peek(1).is_some_and(|n| n != ']') {
            return Err("Invalid range end".into());
        }
        Ok(())
    }

    /// One bracket member: a character, or a single-character `[.x.]` or `[=x=]`.
    fn bracket_char(&mut self) -> Result<char, String> {
        let c = self.peek(0).ok_or(EBRACK)?;
        self.at += 1;
        if c == '[' && matches!(self.peek(0), Some('.' | '=')) {
            let kind = self.chars[self.at];
            let body_start = self.at + 1;
            let end = (body_start..self.chars.len().saturating_sub(1))
                .find(|&i| self.chars[i] == kind && self.chars[i + 1] == ']')
                .ok_or(EBRACK)?;
            if end - body_start != 1 {
                return Err("Invalid collation character".into());
            }
            let single = self.chars[body_start];
            self.at = end + 2;
            return Ok(single);
        }
        Ok(c)
    }

    fn class(&mut self) -> Result<String, String> {
        let start = self.at + 2;
        let end = (start..self.chars.len().saturating_sub(1))
            .find(|&i| self.chars[i] == ':' && self.chars[i + 1] == ']')
            .ok_or(EBRACK)?;
        let name: String = self.chars[start..end].iter().collect();
        const NAMES: [&str; 12] = [
            "alpha", "digit", "alnum", "upper", "lower", "space", "blank", "punct", "print",
            "graph", "cntrl", "xdigit",
        ];
        if !NAMES.contains(&name.as_str()) {
            return Err("Invalid character class name".into());
        }
        self.at = end + 2;
        Ok(format!("[:{name}:]"))
    }
}

fn wrap_last(items: &mut [String], op: &str) {
    if let Some(last) = items.last_mut() {
        *last = format!("(?:{last}){op}");
    }
}

fn push_class_char(out: &mut String, c: char) {
    if c.is_ascii_alphanumeric() || !c.is_ascii() {
        out.push(c);
    } else {
        out.push_str(&format!("\\x{{{:X}}}", c as u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(pattern: &str, syntax: Syntax, text: &str) -> bool {
        compile(pattern, syntax, false)
            .unwrap()
            .is_match(text.as_bytes())
    }

    fn error(pattern: &str, syntax: Syntax) -> String {
        compile(pattern, syntax, false).unwrap_err()
    }

    #[test]
    fn basic_operators_are_escaped_and_extended_ones_are_literal() {
        let b = |p, t| hit(p, Syntax::Basic, t);
        assert!(b("a\\|b", "xb") && !b("a|b", "xb") && b("a|b", "a|b"));
        assert!(b("x\\+", "xx") && b("x+", "x+") && !b("x+", "xx"));
        assert!(b("a\\{2\\}", "aa") && b("(x)", "(x)") && b("\\(ab\\)*c", "c"));
        assert!(b("*a", "*a") && b("^*a", "*a") && b("a^b", "a^b") && b("a$b", "a$b"));
        assert!(b("[]x]", "]") && b("a\\.b", "a.b") && !b("a\\.b", "axb"));
    }

    #[test]
    fn extended_operators() {
        let e = |p, t| hit(p, Syntax::Extended, t);
        assert!(e("a|b", "b") && e("(ab){2}", "abab") && e("a{,2}b", "aab"));
        assert!(e("a)", "a)") && e("a\\(", "a(") && e("a||b", "zzz") && e("()", ""));
        assert!(e("a+*", "") && e("a**", "") && e("a{1}{2}", "aa"));
        assert!(e("^a$", "a") && !e("^a$", "ab") && e("a^b|b", "b"));
    }

    #[test]
    fn word_assertions_and_classes() {
        let b = |p, t| hit(p, Syntax::Basic, t);
        assert!(b("\\<ab\\>", "x ab y") && !b("\\<ab\\>", "xab y"));
        assert!(b("\\bab\\b", "ab") && b("\\w\\+", "é1") && b("\\s", "a b"));
        assert!(b("[[:digit:]]\\+", "x12") && b("[a-c]", "b") && b("[^a]", "b"));
        assert!(b("[\\]", "\\") && b("[a-]", "-") && b("[[.a.]]", "a") && b("[[=a=]]", "a"));
    }

    #[test]
    fn alternation_takes_the_first_branch_that_matches() {
        let e = |p, t| hit(p, Syntax::Extended, t);
        assert!(e("|a", "a") && e("(a|ab)(c|bcd)?", "abcd"));
        assert!(compile(r"\`a", Syntax::Basic, false).is_err());
    }

    #[test]
    fn fixed_strings_are_literal() {
        assert!(compile("o.", Syntax::Fixed, false).unwrap().is_match(b"o."));
        assert!(!compile("o.", Syntax::Fixed, false).unwrap().is_match(b"ox"));
        assert!(
            compile("ABC", Syntax::Fixed, true)
                .unwrap()
                .is_match(b"abc")
        );
    }

    #[test]
    fn errors_use_the_messages_of_the_gnu_library() {
        assert_eq!(error("\\(a", Syntax::Basic), "Unmatched ( or \\(");
        assert_eq!(error("(a", Syntax::Extended), "Unmatched ( or \\(");
        assert_eq!(error("a\\{", Syntax::Basic), "Unmatched \\{");
        assert_eq!(error("a{", Syntax::Extended), "Unmatched \\{");
        assert_eq!(
            error("a{2,1}", Syntax::Extended),
            "Invalid content of \\{\\}"
        );
        assert_eq!(error("[a", Syntax::Basic), EBRACK);
        assert_eq!(error("[^", Syntax::Basic), "Invalid regular expression");
        assert_eq!(error("[z-a]", Syntax::Basic), "Invalid range end");
        assert_eq!(
            error("[[:foo:]]", Syntax::Basic),
            "Invalid character class name"
        );
        assert_eq!(
            error("[[.ab.]]", Syntax::Basic),
            "Invalid collation character"
        );
        assert_eq!(error("a\\", Syntax::Basic), "Trailing backslash");
        assert_eq!(error("\\1", Syntax::Basic), "Invalid back reference");
        assert_eq!(error("*a", Syntax::Extended), BADRPT);
        assert_eq!(error("a**", Syntax::Basic), BADRPT);
        assert_eq!(error("\\{1\\}", Syntax::Basic), BADRPT);
        assert_eq!(
            error("a{65536}", Syntax::Extended),
            "Regular expression too big"
        );
    }
}
