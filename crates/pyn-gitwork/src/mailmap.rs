//! git's `.mailmap`: maps the name and email recorded in a commit to the canonical ones.
//! Emails and names compare ignoring ASCII case; later lines override earlier ones.

use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
struct Target {
    name: Option<Vec<u8>>,
    email: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    simple: Target,
    by_name: HashMap<Vec<u8>, Target>,
}

#[derive(Clone, Debug, Default)]
pub struct Mailmap {
    entries: HashMap<Vec<u8>, Entry>,
}

fn key(text: &[u8]) -> Vec<u8> {
    text.to_ascii_lowercase()
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn trim(text: &[u8]) -> &[u8] {
    let start = text
        .iter()
        .position(|b| !is_space(*b))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|b| !is_space(*b))
        .map_or(start, |e| e + 1);
    &text[start..end]
}

/// The trimmed name before the first `<`, the email up to the next `>`, and the text after it.
type Part<'a> = (Option<&'a [u8]>, &'a [u8], &'a [u8]);

fn part(buf: &[u8], allow_empty_email: bool) -> Option<Part<'_>> {
    let left = buf.iter().position(|b| *b == b'<')?;
    let right = left + 1 + buf[left + 1..].iter().position(|b| *b == b'>')?;
    if !allow_empty_email && left + 1 == right {
        return None;
    }
    let name = trim(&buf[..left]);
    Some((
        (!name.is_empty()).then_some(name),
        &buf[left + 1..right],
        &buf[right + 1..],
    ))
}

impl Mailmap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds the mappings of a `.mailmap` file's bytes.
    pub fn parse(&mut self, bytes: &[u8]) {
        for line in bytes.split(|b| *b == b'\n') {
            self.line(line);
        }
    }

    fn line(&mut self, line: &[u8]) {
        if line.first() == Some(&b'#') {
            return;
        }
        let Some((new_name, new_email, rest)) = part(line, false) else {
            return;
        };
        let (old_name, old_email) = match part(rest, true) {
            Some((name, email, _)) => (name, Some(email)),
            None => (None, None),
        };
        let (new_email, old_email) = match old_email {
            Some(old) => (Some(new_email), old),
            None => (None, new_email),
        };
        let entry = self.entries.entry(key(old_email)).or_default();
        let owned = |t: Option<&[u8]>| t.map(<[u8]>::to_vec);
        match old_name {
            None => {
                if let Some(name) = new_name {
                    entry.simple.name = Some(name.to_vec());
                }
                if let Some(email) = new_email {
                    entry.simple.email = Some(email.to_vec());
                }
            }
            Some(old) => {
                entry.by_name.insert(
                    key(old),
                    Target {
                        name: owned(new_name),
                        email: owned(new_email),
                    },
                );
            }
        }
    }

    /// The canonical name and email for a commit's, or `None` when no line applies.
    pub fn lookup(&self, name: &[u8], email: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        let entry = self.entries.get(&key(email))?;
        let target = entry.by_name.get(&key(name)).unwrap_or(&entry.simple);
        if target.name.is_none() && target.email.is_none() {
            return None;
        }
        Some((
            target.name.as_deref().unwrap_or(name).to_vec(),
            target.email.as_deref().unwrap_or(email).to_vec(),
        ))
    }

    /// The canonical name and email, unchanged when no line applies.
    pub fn map(&self, name: &[u8], email: &[u8]) -> (Vec<u8>, Vec<u8>) {
        self.lookup(name, email)
            .unwrap_or_else(|| (name.to_vec(), email.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(map: &Mailmap, name: &str, email: &str) -> String {
        let (n, e) = map.map(name.as_bytes(), email.as_bytes());
        format!(
            "{} <{}>",
            String::from_utf8(n).unwrap(),
            String::from_utf8(e).unwrap()
        )
    }

    fn from(text: &str) -> Mailmap {
        let mut m = Mailmap::new();
        m.parse(text.as_bytes());
        m
    }

    #[test]
    fn the_four_line_forms() {
        let m = from(
            "Proper One <a@x>\n<p2@x> <b@x>\nProper Three <p3@x> <c@x>\n\
             Proper Four <p4@x> Old Four <d@x>\n",
        );
        assert_eq!(mapped(&m, "Any", "a@x"), "Proper One <a@x>");
        assert_eq!(mapped(&m, "Any", "b@x"), "Any <p2@x>");
        assert_eq!(mapped(&m, "Any", "c@x"), "Proper Three <p3@x>");
        assert_eq!(mapped(&m, "Old Four", "d@x"), "Proper Four <p4@x>");
        assert_eq!(mapped(&m, "Other", "d@x"), "Other <d@x>");
        assert_eq!(mapped(&m, "Any", "none@x"), "Any <none@x>");
    }

    #[test]
    fn emails_and_names_ignore_ascii_case() {
        let m = from("Proper <p@x> Old Name <OLD@X>\n");
        assert_eq!(mapped(&m, "old NAME", "old@x"), "Proper <p@x>");
        assert_eq!(mapped(&m, "Else", "old@x"), "Else <old@x>");
    }

    #[test]
    fn a_name_specific_line_beats_the_email_only_one() {
        let m = from("Simple <s@x> <o@x>\nSpecific <t@x> Who <o@x>\n");
        assert_eq!(mapped(&m, "Who", "o@x"), "Specific <t@x>");
        assert_eq!(mapped(&m, "Else", "o@x"), "Simple <s@x>");
    }

    #[test]
    fn later_lines_override_and_partial_lines_keep_the_rest() {
        let m = from("First <a@x>\n<new@x> <a@x>\nSecond <a@x>\n");
        assert_eq!(mapped(&m, "N", "a@x"), "Second <new@x>");
        let m = from("Name <a@x>\n<a@x>\n");
        assert_eq!(mapped(&m, "N", "a@x"), "Name <a@x>");
    }

    #[test]
    fn comments_blanks_and_malformed_lines_are_ignored() {
        let m = from("# Name <a@x>\n\nno brackets\nName <>\nName <b@x\n  # Late <c@x>\n");
        assert_eq!(mapped(&m, "N", "a@x"), "N <a@x>");
        assert_eq!(mapped(&m, "N", "b@x"), "N <b@x>");
        assert_eq!(mapped(&m, "N", "c@x"), "# Late <c@x>");
    }

    #[test]
    fn whitespace_and_trailing_text_are_tolerated() {
        let m = from("  Proper Name   <p@x>   <o@x> # note\r\n");
        assert_eq!(mapped(&m, "N", "o@x"), "Proper Name <p@x>");
    }

    #[test]
    fn an_empty_commit_email_can_be_mapped() {
        let m = from("Fixed <f@x> <>\n");
        assert_eq!(mapped(&m, "N", ""), "Fixed <f@x>");
    }
}
