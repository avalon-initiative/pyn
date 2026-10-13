use std::path::{Path, PathBuf};

use pyn_object::Algorithm;

use crate::error::{Result, invalid, io_at};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub section: String,
    pub subsection: Option<String>,
    pub key: String,
    /// `None` for a bare key, which git reads as boolean true.
    pub value: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// A parsed git config. Later entries override earlier ones; `include` directives are not followed.
#[derive(Debug, Clone, Default)]
pub struct Config {
    entries: Vec<Entry>,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(io_at(path))?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let text = String::from_utf8_lossy(bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        Parser::new(text).run()
    }

    /// Reads `$XDG_CONFIG_HOME/git/config` (default `~/.config/git/config`) then `~/.gitconfig`; missing files are skipped.
    pub fn global(home: &Path, xdg_config_home: Option<&Path>) -> Result<Self> {
        let xdg = xdg_config_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".config"));
        let mut config = Self::default();
        for path in [xdg.join("git/config"), home.join(".gitconfig")] {
            if path.is_file() {
                config.extend(Self::read(&path)?);
            }
        }
        Ok(config)
    }

    /// Appends `other`, which then takes precedence.
    pub fn extend(&mut self, other: Config) {
        self.entries.extend(other.entries);
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The last value of a dotted key such as `user.name` or `remote.origin.url`.
    /// A bare key yields an empty string; see [`Config::get_bool`].
    pub fn get(&self, name: &str) -> Option<&str> {
        self.lookup(name)
            .last()
            .map(|e| e.value.as_deref().unwrap_or(""))
    }

    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.lookup(name)
            .iter()
            .map(|e| e.value.as_deref().unwrap_or(""))
            .collect()
    }

    /// Git's boolean rules: bare key, `true`, `yes`, `on` and nonzero integers are true.
    pub fn get_bool(&self, name: &str) -> Result<Option<bool>> {
        let Some(entry) = self.lookup(name).last().copied() else {
            return Ok(None);
        };
        let Some(v) = entry.value.as_deref() else {
            return Ok(Some(true));
        };
        match v.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Ok(Some(true)),
            "false" | "no" | "off" | "" => Ok(Some(false)),
            other => match other.parse::<i64>() {
                Ok(n) => Ok(Some(n != 0)),
                Err(_) => Err(invalid("config", format!("{name} is not a boolean: {v:?}"))),
            },
        }
    }

    /// `extensions.objectFormat`, defaulting to SHA-1 when absent.
    pub fn object_format(&self) -> Result<Algorithm> {
        match self.get("extensions.objectformat") {
            None => Ok(Algorithm::Sha1),
            Some(name) => Algorithm::from_name(&name.to_ascii_lowercase())
                .ok_or_else(|| crate::Error::Unsupported(format!("object format {name:?}"))),
        }
    }

    pub fn identity(&self) -> Identity {
        Identity {
            name: self.get("user.name").map(str::to_owned),
            email: self.get("user.email").map(str::to_owned),
        }
    }

    pub(crate) fn path_in(&self, name: &str, base: &Path) -> Option<PathBuf> {
        self.get(name).map(|p| base.join(p))
    }

    fn lookup(&self, name: &str) -> Vec<&Entry> {
        let Some((head, key)) = name.rsplit_once('.') else {
            return Vec::new();
        };
        let (section, sub) = match head.split_once('.') {
            Some((s, sub)) => (s, Some(sub)),
            None => (head, None),
        };
        let (section, key) = (section.to_ascii_lowercase(), key.to_ascii_lowercase());
        self.entries
            .iter()
            .filter(|e| e.section == section && e.key == key && e.subsection.as_deref() == sub)
            .collect()
    }
}

pub(crate) struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
    pub(crate) section: Option<(String, Option<String>)>,
    pub(crate) entries: Vec<Entry>,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Self::at_line(text, 1)
    }

    pub(crate) fn at_line(text: &'a str, line: usize) -> Self {
        Self {
            chars: text.chars().peekable(),
            line,
            section: None,
            entries: Vec::new(),
        }
    }

    /// Bytes of the input not consumed yet.
    pub(crate) fn remaining(&self) -> usize {
        self.chars.clone().map(char::len_utf8).sum()
    }

    pub(crate) fn bump(&mut self) {
        self.chars.next();
    }

    pub(crate) fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn err<T>(&self, detail: &str) -> Result<T> {
        Err(invalid("config", format!("line {}: {detail}", self.line)))
    }

    fn run(mut self) -> Result<Config> {
        while let Some(&c) = self.chars.peek() {
            match c {
                '\n' => {
                    self.chars.next();
                    self.line += 1;
                }
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '#' | ';' => self.skip_line(),
                '[' => {
                    self.chars.next();
                    self.header()?;
                }
                c if c.is_ascii_alphabetic() => self.variable()?,
                _ => return self.err("unexpected character"),
            }
        }
        Ok(Config {
            entries: self.entries,
        })
    }

    fn skip_line(&mut self) {
        for c in self.chars.by_ref() {
            if c == '\n' {
                self.line += 1;
                break;
            }
        }
    }

    /// Parses a section header whose `[` is already consumed.
    pub(crate) fn header(&mut self) -> Result<()> {
        let mut name = String::new();
        let mut sub = None;
        loop {
            match self.chars.next() {
                Some(']') => break,
                Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '.' => name.push(c),
                Some(c) if c.is_whitespace() && c != '\n' => {
                    while self.chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                        self.chars.next();
                    }
                    if self.chars.next() != Some('"') {
                        return self.err("expected a quoted subsection");
                    }
                    let mut s = String::new();
                    loop {
                        match self.chars.next() {
                            Some('"') => break,
                            Some('\\') => match self.chars.next() {
                                Some(c) if c != '\n' => s.push(c),
                                _ => return self.err("bad escape in subsection"),
                            },
                            Some('\n') | None => return self.err("unterminated subsection"),
                            Some(c) => s.push(c),
                        }
                    }
                    sub = Some(s);
                    if self.chars.next() != Some(']') {
                        return self.err("expected ] after subsection");
                    }
                    break;
                }
                _ => return self.err("malformed section header"),
            }
        }
        if sub.is_none()
            && let Some((s, rest)) = name.split_once('.')
        {
            // `[section.sub]` is the legacy spelling, with a lowercased subsection.
            sub = Some(rest.to_ascii_lowercase());
            name = s.to_owned();
        }
        if name.is_empty() {
            return self.err("empty section name");
        }
        self.section = Some((name.to_ascii_lowercase(), sub));
        Ok(())
    }

    pub(crate) fn variable(&mut self) -> Result<()> {
        let mut key = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_ascii_alphanumeric() || c == '-' {
                key.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        while self.chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
            self.chars.next();
        }
        let value = match self.chars.peek() {
            Some('=') => {
                self.chars.next();
                while self.chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                    self.chars.next();
                }
                Some(self.value()?)
            }
            Some('\n') | None => None,
            Some('#') | Some(';') => {
                self.skip_line();
                None
            }
            Some('\r') => None,
            _ => return self.err("expected = after key"),
        };
        let Some((section, subsection)) = self.section.clone() else {
            return self.err("key outside any section");
        };
        self.entries.push(Entry {
            section,
            subsection,
            key: key.to_ascii_lowercase(),
            value,
        });
        Ok(())
    }

    fn value(&mut self) -> Result<String> {
        let mut out = String::new();
        let mut quoted = false;
        let mut pending_ws = String::new();
        while let Some(c) = self.chars.next() {
            match c {
                '\n' => {
                    self.line += 1;
                    if quoted {
                        return self.err("unterminated quoted value");
                    }
                    break;
                }
                '\r' if !quoted && self.chars.peek() == Some(&'\n') => {}
                '\\' => match self.chars.next() {
                    Some('\n') => self.line += 1,
                    Some('\r') if self.chars.peek() == Some(&'\n') => {
                        self.chars.next();
                        self.line += 1;
                    }
                    Some('n') => out.push_str(&take(&mut pending_ws, '\n')),
                    Some('t') => out.push_str(&take(&mut pending_ws, '\t')),
                    Some('b') => out.push_str(&take(&mut pending_ws, '\u{8}')),
                    Some('"') => out.push_str(&take(&mut pending_ws, '"')),
                    Some('\\') => out.push_str(&take(&mut pending_ws, '\\')),
                    _ => return self.err("bad escape in value"),
                },
                '"' => {
                    out.push_str(&std::mem::take(&mut pending_ws));
                    quoted = !quoted;
                }
                '#' | ';' if !quoted => {
                    self.skip_line();
                    break;
                }
                c if c.is_whitespace() && !quoted => pending_ws.push(c),
                c => out.push_str(&take(&mut pending_ws, c)),
            }
        }
        if quoted {
            return self.err("unterminated quoted value");
        }
        Ok(out)
    }
}

/// Interior whitespace is kept; trailing whitespace is dropped because it is only flushed before more text.
fn take(pending: &mut String, c: char) -> String {
    let mut s = std::mem::take(pending);
    s.push(c);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_quotes_and_continuations() {
        let c = Config::parse(
            b"[core]\n\trepositoryformatversion = 1\n\tbare = false ; c\n[extensions]\n\tobjectFormat = sha256\n[remote \"or\\\"igin\"]\n\turl = a \\\n b\n\tname = \"  x # y \"  # tail\n[branch.Main]\n\tflag\n",
        )
        .unwrap();
        assert_eq!(c.get("core.repositoryformatversion"), Some("1"));
        assert_eq!(c.get_bool("core.bare").unwrap(), Some(false));
        assert_eq!(c.object_format().unwrap(), Algorithm::Sha256);
        assert_eq!(c.get("remote.or\"igin.url"), Some("a  b"));
        assert_eq!(c.get("remote.or\"igin.name"), Some("  x # y "));
        assert_eq!(c.get_bool("branch.main.flag").unwrap(), Some(true));
    }

    #[test]
    fn last_value_wins_and_identity() {
        let mut a = Config::parse(b"[user]\nname = A\nemail = a@x\n").unwrap();
        a.extend(Config::parse(b"[user]\nname = B\n").unwrap());
        assert_eq!(a.get_all("user.name"), vec!["A", "B"]);
        assert_eq!(
            a.identity(),
            Identity {
                name: Some("B".into()),
                email: Some("a@x".into())
            }
        );
    }

    #[test]
    fn unknown_object_format_is_unsupported() {
        let c = Config::parse(b"[extensions]\nobjectformat = sha512\n").unwrap();
        assert!(matches!(
            c.object_format(),
            Err(crate::Error::Unsupported(_))
        ));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(Config::parse(b"key = 1\n").is_err());
        assert!(Config::parse(b"[core\nx=1\n").is_err());
        assert!(Config::parse(b"[core]\nx = \"open\n").is_err());
    }
}
