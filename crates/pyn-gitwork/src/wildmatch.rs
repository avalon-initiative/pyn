//! Git's glob matching (`wildmatch`): `*`, `?`, bracket classes and `**`, on bytes.

/// Matches `text` against `pattern`. With `pathname`, `*` and `?` stop at `/` and `**` spans folders.
pub fn matches(pattern: &[u8], text: &[u8], pathname: bool) -> bool {
    Matcher::new(pattern, pathname).walk(0, text) == Outcome::Match
}

/// Most `*` groups one pattern may nest; deeper patterns never match.
const MAX_STAR_DEPTH: usize = 1024;

/// Like git's `WM_*` results: the abort variants say no later backtracking point can succeed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Match,
    NoMatch,
    AbortAll,
    AbortToStarStar,
}

struct Matcher<'a> {
    p: &'a [u8],
    pathname: bool,
    depth: usize,
    steps: u64,
}

impl<'a> Matcher<'a> {
    fn new(p: &'a [u8], pathname: bool) -> Self {
        Self {
            p,
            pathname,
            depth: 0,
            steps: 0,
        }
    }

    fn walk(&mut self, mut pi: usize, mut t: &[u8]) -> Outcome {
        let p = self.p;
        while pi < p.len() {
            self.steps += 1;
            if t.is_empty() && p[pi] != b'*' {
                return Outcome::AbortAll;
            }
            match p[pi] {
                b'*' => return self.star(pi, t),
                b'?' => {
                    if self.pathname && t[0] == b'/' {
                        return Outcome::NoMatch;
                    }
                    t = &t[1..];
                    pi += 1;
                }
                b'[' => {
                    let Some((end, class)) = parse_class(p, pi) else {
                        return Outcome::AbortAll;
                    };
                    if (self.pathname && t[0] == b'/') || !class.matches(t[0]) {
                        return Outcome::NoMatch;
                    }
                    t = &t[1..];
                    pi = end;
                }
                b'\\' => {
                    pi += 1;
                    match p.get(pi) {
                        Some(a) if *a == t[0] => {
                            pi += 1;
                            t = &t[1..];
                        }
                        _ => return Outcome::NoMatch,
                    }
                }
                c => {
                    if c != t[0] {
                        return Outcome::NoMatch;
                    }
                    pi += 1;
                    t = &t[1..];
                }
            }
        }
        if t.is_empty() {
            Outcome::Match
        } else {
            Outcome::NoMatch
        }
    }

    fn star(&mut self, at: usize, t: &[u8]) -> Outcome {
        let p = self.p;
        let mut end = at;
        while p.get(end) == Some(&b'*') {
            end += 1;
        }
        let double = end - at >= 2;
        let after_slash = at == 0 || p[at - 1] == b'/';
        let mut match_slash = !self.pathname;
        if double && self.pathname && after_slash && (end == p.len() || p[end] == b'/') {
            if end == p.len() {
                return Outcome::Match;
            }
            if self.nested(end + 1, t) == Outcome::Match {
                return Outcome::Match;
            }
            match_slash = true;
        }
        if end == p.len() {
            return if !match_slash && t.contains(&b'/') {
                Outcome::NoMatch
            } else {
                Outcome::Match
            };
        }
        let mut t = t;
        if !match_slash && p[end] == b'/' {
            match t.iter().position(|c| *c == b'/') {
                Some(i) => t = &t[i..],
                None => return Outcome::NoMatch,
            }
        }
        while !t.is_empty() {
            let r = self.nested(end, t);
            if r != Outcome::NoMatch {
                if !match_slash || r != Outcome::AbortToStarStar {
                    return r;
                }
            } else if !match_slash && t[0] == b'/' {
                return Outcome::AbortToStarStar;
            }
            t = &t[1..];
        }
        Outcome::AbortAll
    }

    fn nested(&mut self, pi: usize, t: &[u8]) -> Outcome {
        if self.depth >= MAX_STAR_DEPTH {
            return Outcome::AbortAll;
        }
        self.depth += 1;
        let r = self.walk(pi, t);
        self.depth -= 1;
        r
    }
}

struct Class {
    negated: bool,
    items: Vec<Item>,
}

enum Item {
    Range(u8, u8),
    Named(&'static str),
}

impl Class {
    fn matches(&self, c: u8) -> bool {
        let hit = self.items.iter().any(|i| match i {
            Item::Range(a, b) => (*a..=*b).contains(&c),
            Item::Named(name) => named(name, c),
        });
        hit != self.negated
    }
}

fn named(name: &str, c: u8) -> bool {
    match name {
        "alnum" => c.is_ascii_alphanumeric(),
        "alpha" => c.is_ascii_alphabetic(),
        "blank" => c == b' ' || c == b'\t',
        "cntrl" => c.is_ascii_control(),
        "digit" => c.is_ascii_digit(),
        "graph" => c.is_ascii_graphic(),
        "lower" => c.is_ascii_lowercase(),
        "print" => c.is_ascii_graphic() || c == b' ',
        "punct" => c.is_ascii_punctuation(),
        "space" => c.is_ascii_whitespace() || c == 0x0b,
        "upper" => c.is_ascii_uppercase(),
        "xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

const NAMES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

/// Parses the bracket expression at `p[at]`; returns the index after `]` and the class, or `None` if unterminated.
fn parse_class(p: &[u8], at: usize) -> Option<(usize, Class)> {
    let mut i = at + 1;
    let negated = matches!(p.get(i), Some(b'!') | Some(b'^'));
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == b']' && !first {
            return Some((i + 1, Class { negated, items }));
        }
        first = false;
        if c == b'[' && p.get(i + 1) == Some(&b':') {
            let close = p[i + 2..].windows(2).position(|w| w == b":]");
            if let Some(len) = close {
                let name = std::str::from_utf8(&p[i + 2..i + 2 + len]).unwrap_or("");
                if let Some(n) = NAMES.iter().find(|n| **n == name) {
                    items.push(Item::Named(n));
                    i += len + 4;
                    continue;
                }
            }
        }
        let lo = if c == b'\\' {
            i += 1;
            *p.get(i)?
        } else {
            c
        };
        i += 1;
        if p.get(i) == Some(&b'-') && p.get(i + 1).is_some_and(|n| *n != b']') {
            i += 1;
            let mut hi = p[i];
            if hi == b'\\' {
                i += 1;
                hi = *p.get(i)?;
            }
            i += 1;
            items.push(Item::Range(lo, hi));
        } else {
            items.push(Item::Range(lo, lo));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str, t: &str) -> bool {
        matches(p.as_bytes(), t.as_bytes(), true)
    }

    fn base(p: &str, t: &str) -> bool {
        matches(p.as_bytes(), t.as_bytes(), false)
    }

    fn steps(p: &str, t: &str, pathname: bool) -> (bool, u64) {
        let mut m = Matcher::new(p.as_bytes(), pathname);
        let r = m.walk(0, t.as_bytes()) == Outcome::Match;
        (r, m.steps)
    }

    #[test]
    fn many_stars_against_a_non_matching_name_stay_cheap() {
        let pattern = format!("{}b", "a*".repeat(40));
        let name = "a".repeat(2000);
        for pathname in [false, true] {
            let (hit, n) = steps(&pattern, &name, pathname);
            assert!(!hit);
            assert!(n < 500_000, "{n} steps");
        }
        let pattern = format!("{}/z", "*/".repeat(30));
        let name = format!("{}q", "a/".repeat(500));
        let (hit, n) = steps(&pattern, &name, true);
        assert!(!hit);
        assert!(n < 500_000, "{n} steps");
        let (hit, n) = steps(&"**/a".repeat(30), &"a/".repeat(500), true);
        assert!(!hit);
        assert!(n < 5_000_000, "{n} steps");
    }

    #[test]
    fn patterns_nesting_stars_beyond_the_limit_do_not_match() {
        let pattern = "a*".repeat(100_000);
        assert!(!base(&pattern, &"a".repeat(10)));
    }

    #[test]
    fn single_star_and_question_mark_stop_at_slashes_in_paths() {
        assert!(path("src/*.rs", "src/a.rs"));
        assert!(!path("src/*.rs", "src/deep/a.rs"));
        assert!(path("a?c", "abc") && !path("a?c", "a/c"));
        assert!(base("*.log", "x.log") && base("*", "anything"));
        assert!(path("*", "name") && !path("*", "dir/name"));
    }

    #[test]
    fn double_star_spans_folders_only_as_a_whole_segment() {
        assert!(path("**/foo", "foo") && path("**/foo", "a/b/foo"));
        assert!(path("a/**/b", "a/b") && path("a/**/b", "a/x/y/b"));
        assert!(path("a/**", "a/x/y") && !path("a/**", "b/x"));
        assert!(!path("a**b", "a/b") && path("a**b", "axxb"));
        assert!(path("**", "any/thing"));
    }

    #[test]
    fn bracket_classes_ranges_negation_and_names() {
        assert!(path("[abc].txt", "b.txt") && !path("[abc].txt", "d.txt"));
        assert!(path("[a-c]x", "bx") && path("[!a-c]x", "dx") && !path("[!a-c]x", "bx"));
        assert!(path("[^a]x", "bx"));
        assert!(path("[[:digit:]]x", "7x") && !path("[[:digit:]]x", "ax"));
        assert!(path("[]]x", "]x"));
        assert!(!path("[ab", "a"), "an unterminated class never matches");
        assert!(!path("[a-z]", "/"));
    }

    #[test]
    fn backslash_makes_the_next_character_literal() {
        assert!(path(r"\*.txt", "*.txt") && !path(r"\*.txt", "a.txt"));
        assert!(path(r"\#file", "#file") && path(r"a\ b", "a b"));
        assert!(!path(r"a\", "a"));
    }
}
