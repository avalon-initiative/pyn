//! Git's glob matching (`wildmatch`): `*`, `?`, bracket classes and `**`, on bytes.

/// Matches `text` against `pattern`. With `pathname`, `*` and `?` stop at `/` and `**` spans folders.
pub fn matches(pattern: &[u8], text: &[u8], pathname: bool) -> bool {
    walk(pattern, 0, text, pathname)
}

fn walk(p: &[u8], mut pi: usize, mut t: &[u8], pathname: bool) -> bool {
    while pi < p.len() {
        match p[pi] {
            b'*' => return star(p, pi, t, pathname),
            b'?' => {
                match t.first() {
                    Some(&c) if !(pathname && c == b'/') => t = &t[1..],
                    _ => return false,
                }
                pi += 1;
            }
            b'[' => {
                let Some((end, class)) = parse_class(p, pi) else {
                    return false;
                };
                match t.first() {
                    Some(&c) if !(pathname && c == b'/') && class.matches(c) => t = &t[1..],
                    _ => return false,
                }
                pi = end;
            }
            b'\\' => {
                pi += 1;
                match (p.get(pi), t.first()) {
                    (Some(a), Some(b)) if a == b => {
                        pi += 1;
                        t = &t[1..];
                    }
                    _ => return false,
                }
            }
            c => match t.first() {
                Some(&d) if d == c => {
                    pi += 1;
                    t = &t[1..];
                }
                _ => return false,
            },
        }
    }
    t.is_empty()
}

fn star(p: &[u8], at: usize, t: &[u8], pathname: bool) -> bool {
    let mut end = at;
    while p.get(end) == Some(&b'*') {
        end += 1;
    }
    let rest = end;
    let double = end - at >= 2;
    let after_slash = at == 0 || p[at - 1] == b'/';
    if double && pathname && after_slash && (rest == p.len() || p[rest] == b'/') {
        if rest == p.len() {
            return true;
        }
        if walk(p, rest + 1, t, pathname) {
            return true;
        }
        return t
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == b'/')
            .any(|(i, _)| walk(p, rest + 1, &t[i + 1..], pathname));
    }
    let spans_slash = !pathname;
    for i in 0..=t.len() {
        if walk(p, rest, &t[i..], pathname) {
            return true;
        }
        if i < t.len() && t[i] == b'/' && !spans_slash {
            return false;
        }
    }
    false
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
