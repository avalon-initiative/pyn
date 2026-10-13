use std::path::Path;

use crate::config::Parser;
use crate::error::{Error, Result, invalid, io_at};

#[derive(Debug, Clone)]
enum Kind {
    /// `close` is the byte offset in the item text just past the `]`.
    Header {
        section: String,
        sub: Option<String>,
        close: usize,
        comment: bool,
    },
    Var {
        section: String,
        sub: Option<String>,
        key: String,
        value: Option<String>,
    },
    Other,
}

#[derive(Debug, Clone)]
struct Item {
    text: String,
    kind: Kind,
}

impl Item {
    fn blank(&self) -> bool {
        matches!(self.kind, Kind::Other) && self.text.trim().is_empty()
    }
}

/// A git config file kept as its lines, so edits leave every other line, comment and the ordering as they were.
#[derive(Debug, Clone, Default)]
pub struct ConfigFile {
    bom: bool,
    items: Vec<Item>,
}

fn line_of(text: &str, pos: usize) -> usize {
    text[..pos].bytes().filter(|b| *b == b'\n').count() + 1
}

impl ConfigFile {
    /// Reads `path`; a missing file is an empty config.
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_at(path)(e)),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let (bom, text) = match text.strip_prefix('\u{feff}') {
            Some(t) => (true, t),
            None => (false, text),
        };
        let mut items = Vec::new();
        let mut section: Option<(String, Option<String>)> = None;
        let mut pos = 0;
        while pos < text.len() {
            let tail = &text[pos..];
            let body = tail.trim_start_matches([' ', '\t', '\r']);
            let lead = tail.len() - body.len();
            let line = line_of(text, pos);
            let end = match body.chars().next() {
                Some('\n') => lead + 1,
                Some('#' | ';') | None => lead + body.find('\n').map_or(body.len(), |n| n + 1),
                Some('[') => {
                    let mut p = Parser::at_line(&body[1..], line);
                    p.header()?;
                    section = p.section.clone();
                    let close = lead + 1 + (body.len() - 1 - p.remaining());
                    let rest = &tail[close..];
                    let trimmed = rest.trim_start_matches([' ', '\t', '\r']);
                    let comment = matches!(trimmed.chars().next(), Some('#' | ';'));
                    let end = if comment || trimmed.starts_with('\n') || trimmed.is_empty() {
                        close + rest.find('\n').map_or(rest.len(), |n| n + 1)
                    } else {
                        close
                    };
                    let (section, sub) = section.clone().expect("a header sets the section");
                    items.push(Item {
                        text: tail[..end].to_owned(),
                        kind: Kind::Header {
                            section,
                            sub,
                            close,
                            comment,
                        },
                    });
                    pos += end;
                    continue;
                }
                Some(c) if c.is_ascii_alphabetic() => {
                    let mut p = Parser::at_line(body, line);
                    p.section = section.clone();
                    p.variable()?;
                    let mut end = body.len() - p.remaining();
                    if !body[..end].ends_with('\n') {
                        while let Some(c) = p.peek().filter(|c| *c != '\n' && c.is_whitespace()) {
                            end += c.len_utf8();
                            p.bump();
                        }
                        if p.peek() == Some('\n') {
                            end += 1;
                        }
                    }
                    let entry = p.entries.pop().expect("a variable adds an entry");
                    items.push(Item {
                        text: tail[..lead + end].to_owned(),
                        kind: Kind::Var {
                            section: entry.section,
                            sub: entry.subsection,
                            key: entry.key,
                            value: entry.value,
                        },
                    });
                    pos += lead + end;
                    continue;
                }
                Some(_) => {
                    return Err(invalid(
                        "config",
                        format!("line {line}: unexpected character"),
                    ));
                }
            };
            items.push(Item {
                text: tail[..end].to_owned(),
                kind: Kind::Other,
            });
            pos += end;
        }
        Ok(Self { bom, items })
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        for item in &self.items {
            out.push_str(&item.text);
        }
        out
    }

    /// Writes through `<path>.lock` and a rename, keeping the file's permissions.
    pub fn write(&self, path: &Path) -> Result<()> {
        let lock = std::path::PathBuf::from(format!("{}.lock", path.display()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = std::io::Write::write_all(&mut file, self.render().as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| match std::fs::metadata(path) {
                Ok(meta) => std::fs::set_permissions(&lock, meta.permissions()),
                Err(_) => Ok(()),
            })
            .and_then(|()| std::fs::rename(&lock, path));
        if let Err(e) = result {
            let _ = std::fs::remove_file(&lock);
            return Err(io_at(path)(e));
        }
        Ok(())
    }

    fn is_header(&self, i: usize, section: &str, sub: Option<&str>) -> bool {
        matches!(&self.items[i].kind, Kind::Header { section: s, sub: b, .. }
            if s.eq_ignore_ascii_case(section) && b.as_deref() == sub)
    }

    fn var_value(&self, i: usize, section: &str, sub: Option<&str>, key: &str) -> Option<&str> {
        match &self.items[i].kind {
            Kind::Var {
                section: s,
                sub: b,
                key: k,
                value,
            } if s.eq_ignore_ascii_case(section)
                && b.as_deref() == sub
                && k.eq_ignore_ascii_case(key) =>
            {
                Some(value.as_deref().unwrap_or(""))
            }
            _ => None,
        }
    }

    fn vars(&self, section: &str, sub: Option<&str>, key: &str) -> Vec<usize> {
        (0..self.items.len())
            .filter(|&i| self.var_value(i, section, sub, key).is_some())
            .collect()
    }

    /// The values of `section.sub.key` in file order.
    pub fn values(&self, section: &str, sub: Option<&str>, key: &str) -> Vec<String> {
        self.vars(section, sub, key)
            .into_iter()
            .map(|i| {
                self.var_value(i, section, sub, key)
                    .unwrap_or("")
                    .to_owned()
            })
            .collect()
    }

    /// The distinct subsections of `section`, in file order.
    pub fn subsections(&self, section: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for item in &self.items {
            if let Kind::Header {
                section: s,
                sub: Some(sub),
                ..
            } = &item.kind
                && s.eq_ignore_ascii_case(section)
                && !out.contains(sub)
            {
                out.push(sub.clone());
            }
        }
        out
    }

    pub fn has_section(&self, section: &str, sub: Option<&str>) -> bool {
        (0..self.items.len()).any(|i| self.is_header(i, section, sub))
    }

    /// Index one past the last item of the section whose header is at `h`.
    fn section_end(&self, h: usize) -> usize {
        (h + 1..self.items.len())
            .find(|&i| matches!(self.items[i].kind, Kind::Header { .. }))
            .unwrap_or(self.items.len())
    }

    fn ensure_newline_at(&mut self, i: usize) {
        if let Some(item) = self.items.get_mut(i)
            && !item.text.ends_with('\n')
        {
            item.text.push('\n');
        }
    }

    fn header_text(section: &str, sub: Option<&str>) -> Result<String> {
        Ok(match sub {
            Some(sub) => {
                if sub.contains(['\n', '\0']) {
                    return Err(invalid("config", "a subsection name cannot hold a newline"));
                }
                let escaped = sub.replace('\\', "\\\\").replace('"', "\\\"");
                format!("[{section} \"{escaped}\"]\n")
            }
            None => format!("[{section}]\n"),
        })
    }

    fn var_text(key: &str, value: &str) -> Result<String> {
        let ok = key.starts_with(|c: char| c.is_ascii_alphabetic())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !ok || value.contains('\0') {
            return Err(invalid("config", format!("cannot write {key:?}")));
        }
        Ok(format!("\t{key} = {}\n", quote_value(value)))
    }

    /// Adds a value after the last variable of the last section of that name, or in a new section at the end.
    pub fn add(&mut self, section: &str, sub: Option<&str>, key: &str, value: &str) -> Result<()> {
        let text = Self::var_text(key, value)?;
        let var = Item {
            text,
            kind: Kind::Var {
                section: section.to_ascii_lowercase(),
                sub: sub.map(str::to_owned),
                key: key.to_ascii_lowercase(),
                value: Some(value.to_owned()),
            },
        };
        let header = (0..self.items.len())
            .rev()
            .find(|&i| self.is_header(i, section, sub));
        let Some(h) = header else {
            let (s, b) = (section.to_ascii_lowercase(), sub.map(str::to_owned));
            let text = Self::header_text(section, sub)?;
            if let Some(last) = self.items.len().checked_sub(1) {
                self.ensure_newline_at(last);
            }
            self.items.push(Item {
                text,
                kind: Kind::Header {
                    section: s,
                    sub: b,
                    close: 0,
                    comment: false,
                },
            });
            self.items.push(var);
            return Ok(());
        };
        let end = self.section_end(h);
        let at = (h + 1..end)
            .rev()
            .find(|&i| matches!(self.items[i].kind, Kind::Var { .. }))
            .unwrap_or(h)
            + 1;
        self.ensure_newline_at(at - 1);
        self.items.insert(at, var);
        Ok(())
    }

    /// Sets the one value of a key, or adds it; fails when the key has several.
    pub fn set(&mut self, section: &str, sub: Option<&str>, key: &str, value: &str) -> Result<()> {
        match self.vars(section, sub, key).as_slice() {
            [] => self.add(section, sub, key, value),
            [i] => {
                self.replace_item(*i, key, value)?;
                Ok(())
            }
            _ => Err(Error::MultipleValues(format!(
                "{section}{}.{key}",
                sub.map(|s| format!(".{s}")).unwrap_or_default()
            ))),
        }
    }

    fn replace_item(&mut self, i: usize, key: &str, value: &str) -> Result<()> {
        let text = Self::var_text(key, value)?;
        if i > 0 {
            self.ensure_newline_at(i - 1);
        }
        let item = &mut self.items[i];
        item.text = text;
        if let Kind::Var { value: v, .. } = &mut item.kind {
            *v = Some(value.to_owned());
        }
        Ok(())
    }

    /// Replaces, in place, every value of the key for which `matches` holds; returns how many.
    pub fn replace_where(
        &mut self,
        section: &str,
        sub: Option<&str>,
        key: &str,
        value: &str,
        matches: impl Fn(&str) -> bool,
    ) -> Result<usize> {
        let hits: Vec<usize> = self
            .vars(section, sub, key)
            .into_iter()
            .filter(|&i| matches(self.var_value(i, section, sub, key).unwrap_or("")))
            .collect();
        for &i in &hits {
            self.replace_item(i, key, value)?;
        }
        Ok(hits.len())
    }

    /// Removes every value of the key for which `matches` holds, and a section left holding nothing; returns how many.
    pub fn unset_where(
        &mut self,
        section: &str,
        sub: Option<&str>,
        key: &str,
        matches: impl Fn(&str) -> bool,
    ) -> usize {
        let hits: Vec<usize> = self
            .vars(section, sub, key)
            .into_iter()
            .filter(|&i| matches(self.var_value(i, section, sub, key).unwrap_or("")))
            .collect();
        for &i in hits.iter().rev() {
            if i > 0 {
                self.ensure_newline_at(i - 1);
            }
            self.items.remove(i);
            if let Some(h) = (0..i)
                .rev()
                .find(|&j| matches!(self.items[j].kind, Kind::Header { .. }))
            {
                self.drop_if_empty(h);
            }
        }
        hits.len()
    }

    pub fn unset_all(&mut self, section: &str, sub: Option<&str>, key: &str) -> usize {
        self.unset_where(section, sub, key, |_| true)
    }

    /// Removes the section at `h` when only blank lines are left and no comment sits above it, as git does.
    fn drop_if_empty(&mut self, h: usize) {
        let Kind::Header { comment: false, .. } = self.items[h].kind else {
            return;
        };
        let end = self.section_end(h);
        if !(h + 1..end).all(|i| self.items[i].blank()) {
            return;
        }
        let mut start = h;
        while start > 0 && self.items[start - 1].blank() {
            start -= 1;
        }
        if start > 0 && matches!(self.items[start - 1].kind, Kind::Other) {
            return;
        }
        self.items.drain(start..end);
    }

    /// Removes every occurrence of the section with its lines; returns whether there was one.
    pub fn remove_section(&mut self, section: &str, sub: Option<&str>) -> bool {
        let mut found = false;
        while let Some(h) = (0..self.items.len()).find(|&i| self.is_header(i, section, sub)) {
            let end = self.section_end(h);
            self.items.drain(h..end);
            found = true;
        }
        found
    }

    /// Renames the subsection of every occurrence of the section; returns how many headers changed.
    pub fn rename_section(&mut self, section: &str, sub: &str, new_sub: &str) -> Result<usize> {
        let mut changed = 0;
        for i in 0..self.items.len() {
            if !self.is_header(i, section, Some(sub)) {
                continue;
            }
            let Kind::Header {
                section: name,
                close,
                ..
            } = self.items[i].kind.clone()
            else {
                continue;
            };
            let tail = self.items[i].text.get(close..).unwrap_or("").to_owned();
            let head = Self::header_text(&name, Some(new_sub))?;
            let head = head.trim_end_matches('\n');
            let item = &mut self.items[i];
            item.text = format!("{head}{tail}");
            if let Kind::Header {
                sub: s, close: c, ..
            } = &mut item.kind
            {
                *s = Some(new_sub.to_owned());
                *c = head.len();
            }
            for j in i + 1..self.section_end(i) {
                if let Kind::Var { sub: s, .. } = &mut self.items[j].kind {
                    *s = Some(new_sub.to_owned());
                }
            }
            changed += 1;
        }
        Ok(changed)
    }
}

/// Git's value quoting: quotes when the value has an edge space or `;` or `#`, and always escapes `\`, `"`, newline and tab.
pub fn quote_value(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    let quote = value.starts_with(' ') || value.ends_with(' ') || value.contains([';', '#']);
    if quote { format!("\"{out}\"") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn file(text: &str) -> ConfigFile {
        ConfigFile::parse(text).unwrap()
    }

    #[test]
    fn rendering_an_unedited_file_is_the_input() {
        for text in [
            "",
            "# c\n\n[core]\n\tbare = false ; x\n\n[remote \"o\"]\n  url = a \\\n b\n\tflag\n",
            "[core] bare = true\n[a \"s\"] # note\n\tk = \"v # q\"\n",
            "\u{feff}[core]\r\n\tbare = true\r\n",
            "[core]\n\tbare = false",
            "[a.Sub]\n\tk =\n",
        ] {
            assert_eq!(file(text).render(), text);
        }
    }

    #[test]
    fn rejects_what_the_reader_rejects() {
        assert!(ConfigFile::parse("k = 1\n").is_err());
        assert!(ConfigFile::parse("[core\n").is_err());
        assert!(ConfigFile::parse("[core]\nk = \"open\n").is_err());
    }

    #[test]
    fn add_goes_after_the_last_variable_of_the_last_section() {
        let mut f = file(
            "[a \"s\"]\n\tx = 1\n\t# note\n\n\ty = 2\n\t# tail\n\n[b]\n\tz = 1\n[a \"s\"]\n\tq = 1\n",
        );
        f.add("a", Some("s"), "w", "new").unwrap();
        f.add("b", None, "z", "2").unwrap();
        f.add("empty", None, "k", "v").unwrap();
        assert_eq!(
            f.render(),
            "[a \"s\"]\n\tx = 1\n\t# note\n\n\ty = 2\n\t# tail\n\n[b]\n\tz = 1\n\tz = 2\n[a \"s\"]\n\tq = 1\n\tw = new\n[empty]\n\tk = v\n"
        );
    }

    #[test]
    fn add_to_a_file_without_a_final_newline_and_after_an_inline_header() {
        let mut f = file("[core]\n\tbare = false");
        f.add("x", None, "y", "z").unwrap();
        assert_eq!(f.render(), "[core]\n\tbare = false\n[x]\n\ty = z\n");
        let mut f = file("[core] bare = true\n");
        f.set("core", None, "bare", "false").unwrap();
        assert_eq!(f.render(), "[core]\n\tbare = false\n");
    }

    #[test]
    fn set_replaces_one_value_and_refuses_several() {
        let mut f = file("[a]\n\tk = 1 # c\n\tother = 2\n");
        f.set("a", None, "k", "9").unwrap();
        f.set("a", None, "new", "n").unwrap();
        assert_eq!(f.render(), "[a]\n\tk = 9\n\tother = 2\n\tnew = n\n");
        f.add("a", None, "k", "10").unwrap();
        assert!(matches!(
            f.set("a", None, "k", "x"),
            Err(Error::MultipleValues(k)) if k == "a.k"
        ));
    }

    #[test]
    fn replace_where_edits_matches_in_place() {
        let mut f = file("[r \"o\"]\n\turl = a\n\turl = b\n\tpushurl = a\n");
        let n = f
            .replace_where("r", Some("o"), "url", "c", |v| v == "a")
            .unwrap();
        assert_eq!(
            (n, f.values("r", Some("o"), "url")),
            (1, vec!["c".into(), "b".into()])
        );
        assert_eq!(
            f.render(),
            "[r \"o\"]\n\turl = c\n\turl = b\n\tpushurl = a\n"
        );
    }

    #[test]
    fn unset_drops_a_section_left_empty_unless_a_comment_is_near() {
        let cases = [
            ("[b]\n\ta = 1\n[core]\n\tx = 1\n", "[core]\n\tx = 1\n"),
            (
                "[core]\n\tx = 1\n\n[b]\n\ta = 1\n[z]\n\tq = 1\n",
                "[core]\n\tx = 1\n[z]\n\tq = 1\n",
            ),
            ("[b]\n\ta = 1 # c\n", ""),
            (
                "[b]\n\ta = 1\n# c\n[x]\n\ty = 1\n",
                "[b]\n# c\n[x]\n\ty = 1\n",
            ),
            ("# about b\n[b]\n\ta = 1\n", "# about b\n[b]\n"),
            ("[b] # keep\n\ta = 1\n", "[b] # keep\n"),
            ("[b]\n\ta = 1\n\tc = 2\n", "[b]\n\tc = 2\n"),
        ];
        for (before, after) in cases {
            let mut f = file(before);
            assert_eq!(f.unset_all("b", None, "a"), 1, "{before:?}");
            assert_eq!(f.render(), after, "{before:?}");
        }
    }

    #[test]
    fn remove_and_rename_sections() {
        let mut f = file("[a \"s\"]\n\tx = 1\n# c\n[b]\n\tk = 1\n[a \"s\"]\n\ty = 2\n");
        assert!(f.remove_section("a", Some("s")));
        assert!(!f.remove_section("a", Some("s")));
        assert_eq!(f.render(), "[b]\n\tk = 1\n");
        let mut f = file(
            "[remote \"a\"] # c\n\turl = u\n[remote.A]\n\turl = v\n[remote \"a\"]\n[remote \"z\"]\n",
        );
        assert_eq!(f.rename_section("remote", "a", "b\"x").unwrap(), 3);
        assert_eq!(
            f.render(),
            "[remote \"b\\\"x\"] # c\n\turl = u\n[remote \"b\\\"x\"]\n\turl = v\n[remote \"b\\\"x\"]\n[remote \"z\"]\n"
        );
        assert_eq!(f.values("remote", Some("b\"x"), "url"), ["u", "v"]);
    }

    #[test]
    fn written_values_read_back_and_follow_gits_quoting() {
        let mut f = ConfigFile::default();
        for (key, value, text) in [
            ("a", "plain", "plain"),
            ("b", "  lead", "\"  lead\""),
            ("c", "trail ", "\"trail \""),
            ("d", "x;y#z", "\"x;y#z\""),
            ("e", "tab\tnl\nq\"b\\", "tab\\tnl\\nq\\\"b\\\\"),
            ("f", "", ""),
        ] {
            f.add("s", Some("sub \"q\" \\x"), key, value).unwrap();
            assert!(f.render().contains(&format!("\t{key} = {text}\n")), "{key}");
        }
        let text = f.render();
        assert!(text.starts_with("[s \"sub \\\"q\\\" \\\\x\"]\n"), "{text}");
        let read = Config::parse(text.as_bytes()).unwrap();
        for (key, value) in [
            ("a", "plain"),
            ("b", "  lead"),
            ("d", "x;y#z"),
            ("e", "tab\tnl\nq\"b\\"),
        ] {
            assert_eq!(read.get(&format!("s.sub \"q\" \\x.{key}")), Some(value));
        }
        assert!(f.add("s", Some("a\nb"), "k", "v").is_err());
        assert!(f.add("s", None, "bad key", "v").is_err());
    }

    #[test]
    fn write_goes_through_a_lock_and_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "[a]\n\tk = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut f = ConfigFile::read(&path).unwrap();
        f.set("a", None, "k", "2").unwrap();
        f.write(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[a]\n\tk = 2\n");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!dir.path().join("config.lock").exists());
        std::fs::write(dir.path().join("config.lock"), "").unwrap();
        assert!(f.write(&path).is_err());
        assert!(
            ConfigFile::read(&dir.path().join("none"))
                .unwrap()
                .render()
                .is_empty()
        );
    }
}
