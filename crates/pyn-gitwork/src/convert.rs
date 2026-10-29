//! git's line-ending and `ident` conversion between the work tree and the repository (`convert.c`): the `text`,
//! `eol` and `crlf` attributes, `core.autocrlf`, `core.eol` and `core.safecrlf`, then filter drivers (`clean` runs
//! before the line-ending step, `smudge` after it). `working-tree-encoding` is not applied;
//! [`Conversion::unsupported`] says when one is configured.

use std::borrow::Cow;
use std::sync::Arc;

use pyn_git::Config;
use pyn_object::{Algorithm, Kind, hash_object};

use crate::attributes::{AttrValue, PathAttributes};
use crate::error::{Error, Result};
use crate::filter::{Driver, Filters, Op};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoCrlf {
    False,
    True,
    Input,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeCrlf {
    Off,
    Warn,
    Fail,
}

/// The configuration that decides conversions.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    autocrlf: AutoCrlf,
    /// `core.eol` (native counts as `lf` here unless on Windows).
    eol_crlf: bool,
    safecrlf: SafeCrlf,
}

impl Settings {
    pub fn from_config(config: &Config) -> Self {
        let autocrlf = match config.get("core.autocrlf") {
            Some(v) if v.eq_ignore_ascii_case("input") => AutoCrlf::Input,
            _ => match config.get_bool("core.autocrlf") {
                Ok(Some(true)) => AutoCrlf::True,
                _ => AutoCrlf::False,
            },
        };
        let eol_crlf = match config.get("core.eol") {
            Some(v) if v.eq_ignore_ascii_case("crlf") => true,
            Some(v) if v.eq_ignore_ascii_case("lf") => false,
            _ => cfg!(windows),
        };
        let safecrlf = match config.get("core.safecrlf") {
            Some(v) if v.eq_ignore_ascii_case("warn") => SafeCrlf::Warn,
            Some(_) => match config.get_bool("core.safecrlf") {
                Ok(Some(true)) => SafeCrlf::Fail,
                Ok(Some(false)) => SafeCrlf::Off,
                _ => SafeCrlf::Warn,
            },
            None => SafeCrlf::Warn,
        };
        Self {
            autocrlf,
            eol_crlf,
            safecrlf,
        }
    }

    pub fn safecrlf(&self) -> SafeCrlf {
        self.safecrlf
    }

    fn text_eol_is_crlf(&self) -> bool {
        match self.autocrlf {
            AutoCrlf::True => true,
            AutoCrlf::Input => false,
            AutoCrlf::False => self.eol_crlf,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eol {
    Binary,
    Text,
    TextInput,
    TextCrlf,
    Auto,
    AutoInput,
    AutoCrlf,
}

/// What git cannot be matched on here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    Encoding(String),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encoding(enc) => write!(f, "working-tree-encoding={enc} applies"),
        }
    }
}

/// The conversion that applies to one path.
#[derive(Debug, Clone)]
pub struct Conversion {
    action: Eol,
    ident: bool,
    settings: Settings,
    filter_name: Option<String>,
    filter: Option<(Driver, Arc<Filters>)>,
    unsupported: Option<Unsupported>,
}

/// Inputs to [`Conversion::to_git`] besides the content.
pub struct ToGit<'a> {
    /// Check round trips and report them (adding to the index); `false` when only comparing.
    pub check_roundtrip: bool,
    /// Convert CRLF even where the index blob has carriage returns (merge renormalizing).
    pub renormalize: bool,
    /// The index's blob for the path, read only when a decision needs it.
    pub index_blob: &'a dyn Fn() -> Option<Vec<u8>>,
}

/// Converted content, with the warnings git would print.
pub struct Converted<'a> {
    pub data: Cow<'a, [u8]>,
    pub warnings: Vec<String>,
}

impl Conversion {
    pub fn new(settings: Settings, attrs: &PathAttributes) -> Self {
        let from_attr = |v: AttrValue| match v {
            AttrValue::Set => Some(Eol::Text),
            AttrValue::Unset => Some(Eol::Binary),
            AttrValue::Value(v) if v == b"input" => Some(Eol::TextInput),
            AttrValue::Value(v) if v == b"auto" => Some(Eol::Auto),
            _ => None,
        };
        let mut action = from_attr(attrs.get("text")).or_else(|| from_attr(attrs.get("crlf")));
        if action != Some(Eol::Binary) {
            action = match (action, attrs.get("eol").as_str()) {
                (Some(Eol::Auto), Some("lf")) => Some(Eol::AutoInput),
                (Some(Eol::Auto), Some("crlf")) => Some(Eol::AutoCrlf),
                (_, Some("lf")) => Some(Eol::TextInput),
                (_, Some("crlf")) => Some(Eol::TextCrlf),
                (a, _) => a,
            };
        }
        let action = match action {
            Some(Eol::Text) if settings.text_eol_is_crlf() => Eol::TextCrlf,
            Some(Eol::Text) => Eol::TextInput,
            Some(a) => a,
            None => match settings.autocrlf {
                AutoCrlf::False => Eol::Binary,
                AutoCrlf::True => Eol::AutoCrlf,
                AutoCrlf::Input => Eol::AutoInput,
            },
        };
        let filter_name = match attrs.get("filter") {
            AttrValue::Value(v) => Some(String::from_utf8_lossy(&v).into_owned()),
            _ => None,
        };
        let unsupported = match attrs.get("working-tree-encoding") {
            AttrValue::Value(enc)
                if !enc.eq_ignore_ascii_case(b"utf-8") && !enc.eq_ignore_ascii_case(b"utf8") =>
            {
                Some(Unsupported::Encoding(
                    String::from_utf8_lossy(&enc).into_owned(),
                ))
            }
            _ => None,
        };
        Self {
            action,
            ident: matches!(attrs.get("ident"), AttrValue::Set),
            settings,
            filter_name,
            filter: None,
            unsupported,
        }
    }

    /// Resolves the `filter` attribute against the configured drivers.
    pub fn with_filters(mut self, config: &pyn_git::Config, filters: &Arc<Filters>) -> Self {
        self.filter = self
            .filter_name
            .as_deref()
            .and_then(|name| Driver::from_config(config, name))
            .map(|d| (d, filters.clone()));
        self
    }

    pub fn unsupported(&self) -> Option<&Unsupported> {
        self.unsupported.as_ref()
    }

    /// Whether the path carries the Git LFS filter.
    pub fn is_lfs(&self) -> bool {
        matches!(self.filter, Some((Driver::Lfs { .. }, _)))
    }

    /// Whether neither direction changes any byte.
    pub fn is_identity(&self) -> bool {
        self.action == Eol::Binary && !self.ident && self.filter.is_none()
    }

    fn output_crlf(&self) -> bool {
        match self.action {
            Eol::Binary | Eol::Text | Eol::TextInput | Eol::AutoInput => false,
            Eol::TextCrlf | Eol::AutoCrlf => true,
            Eol::Auto => self.settings.text_eol_is_crlf(),
        }
    }

    fn is_auto(&self) -> bool {
        matches!(self.action, Eol::Auto | Eol::AutoInput | Eol::AutoCrlf)
    }

    /// The repository form of work tree content.
    pub fn to_git<'a>(
        &self,
        path: &[u8],
        data: &'a [u8],
        opts: &ToGit<'_>,
    ) -> Result<Converted<'a>> {
        let mut warnings = Vec::new();
        let mut data = Cow::Borrowed(data);
        if let Some((driver, filters)) = &self.filter
            && let Some(out) = filters.apply(driver, Op::Clean, path, &data)?
        {
            data = Cow::Owned(out);
        }
        if self.action != Eol::Binary && !data.is_empty() {
            data = self.crlf_to_git(path, data, opts, &mut warnings)?;
        }
        if self.ident {
            data = match ident_to_git(&data) {
                Some(v) => Cow::Owned(v),
                None => data,
            };
        }
        Ok(Converted { data, warnings })
    }

    fn crlf_to_git<'a>(
        &self,
        path: &[u8],
        data: Cow<'a, [u8]>,
        opts: &ToGit<'_>,
        warnings: &mut Vec<String>,
    ) -> Result<Cow<'a, [u8]>> {
        let stats = Stats::gather(&data);
        let mut strip = stats.crlf > 0;
        if self.is_auto() {
            if stats.is_binary() {
                return Ok(data);
            }
            if !opts.renormalize && strip && has_cr(&(opts.index_blob)()) {
                strip = false;
            }
        }
        if opts.check_roundtrip && self.settings.safecrlf != SafeCrlf::Off {
            let mut new = stats;
            if strip {
                new.lonelf += new.crlf;
                new.crlf = 0;
            }
            if self.will_convert_lf_to_crlf(&new) {
                new.crlf += new.lonelf;
                new.lonelf = 0;
            }
            if let Some(msg) = roundtrip_message(&stats, &new) {
                let name = String::from_utf8_lossy(path);
                if self.settings.safecrlf == SafeCrlf::Fail {
                    return Err(Error::Invalid(format!("{} in {name}", msg.0)));
                }
                warnings.push(format!(
                    "in the working copy of '{name}', {} the next time Git touches it",
                    msg.1
                ));
            }
        }
        if !strip {
            return Ok(data);
        }
        let mut out = Vec::with_capacity(data.len());
        for (i, b) in data.iter().enumerate() {
            if *b == b'\r' && data.get(i + 1) == Some(&b'\n') {
                continue;
            }
            out.push(*b);
        }
        Ok(Cow::Owned(out))
    }

    /// The work tree form of repository content.
    pub fn to_worktree<'a>(
        &self,
        algorithm: Algorithm,
        path: &[u8],
        data: &'a [u8],
    ) -> Result<Cow<'a, [u8]>> {
        let mut data = Cow::Borrowed(data);
        if self.output_crlf() && !data.is_empty() {
            data = self.crlf_to_worktree(data);
        }
        if self.ident
            && let Some(v) = ident_to_worktree(algorithm, &data)
        {
            data = Cow::Owned(v);
        }
        if let Some((driver, filters)) = &self.filter
            && let Some(out) = filters.apply(driver, Op::Smudge, path, &data)?
        {
            data = Cow::Owned(out);
        }
        Ok(data)
    }

    fn will_convert_lf_to_crlf(&self, stats: &Stats) -> bool {
        if !self.output_crlf() || stats.lonelf == 0 {
            return false;
        }
        !(self.is_auto() && (stats.lonecr > 0 || stats.crlf > 0 || stats.is_binary()))
    }

    fn crlf_to_worktree<'a>(&self, data: Cow<'a, [u8]>) -> Cow<'a, [u8]> {
        let stats = Stats::gather(&data);
        if !self.will_convert_lf_to_crlf(&stats) {
            return data;
        }
        let mut out = Vec::with_capacity(data.len() + stats.lonelf);
        for (i, b) in data.iter().enumerate() {
            if *b == b'\n' && (i == 0 || data[i - 1] != b'\r') {
                out.push(b'\r');
            }
            out.push(*b);
        }
        Cow::Owned(out)
    }
}

fn has_cr(blob: &Option<Vec<u8>>) -> bool {
    blob.as_ref()
        .is_some_and(|b| b.contains(&b'\r') && !Stats::gather(b).is_binary())
}

fn roundtrip_message(old: &Stats, new: &Stats) -> Option<(&'static str, &'static str)> {
    if old.crlf > 0 && new.crlf == 0 {
        Some((
            "CRLF would be replaced by LF",
            "CRLF will be replaced by LF",
        ))
    } else if old.lonelf > 0 && new.lonelf == 0 {
        Some((
            "LF would be replaced by CRLF",
            "LF will be replaced by CRLF",
        ))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    crlf: usize,
    lonecr: usize,
    lonelf: usize,
    nul: usize,
    printable: usize,
    nonprintable: usize,
}

impl Stats {
    fn gather(data: &[u8]) -> Self {
        let mut s = Self::default();
        let mut i = 0;
        while i < data.len() {
            let c = data[i];
            i += 1;
            match c {
                b'\r' => {
                    if data.get(i) == Some(&b'\n') {
                        s.crlf += 1;
                        i += 1;
                    } else {
                        s.lonecr += 1;
                    }
                }
                b'\n' => s.lonelf += 1,
                127 => s.nonprintable += 1,
                0 => {
                    s.nul += 1;
                    s.nonprintable += 1;
                }
                0x08 | 0x09 | 0x1b | 0x0c => s.printable += 1,
                c if c < 32 => s.nonprintable += 1,
                _ => s.printable += 1,
            }
        }
        if data.last() == Some(&0x1a) {
            s.nonprintable -= 1;
        }
        s
    }

    fn is_binary(&self) -> bool {
        self.lonecr > 0 || self.nul > 0 || (self.printable >> 7) < self.nonprintable
    }
}

fn find(data: &[u8], from: usize, byte: u8) -> Option<usize> {
    data[from..]
        .iter()
        .position(|b| *b == byte)
        .map(|p| p + from)
}

fn ident_to_git(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    let mut changed = false;
    while let Some(d) = find(data, at, b'$') {
        out.extend_from_slice(&data[at..=d]);
        at = d + 1;
        if data[at..].starts_with(b"Id:")
            && let Some(end) = find(data, at + 3, b'$')
            && !data[at + 3..end].contains(&b'\n')
        {
            out.extend_from_slice(b"Id");
            at = end;
            changed = true;
        }
    }
    out.extend_from_slice(&data[at..]);
    changed.then_some(out)
}

fn ident_to_worktree(algorithm: Algorithm, data: &[u8]) -> Option<Vec<u8>> {
    let id = hash_object(algorithm, Kind::Blob, data).ok()?.to_hex();
    let mut out = Vec::with_capacity(data.len() + 48);
    let mut at = 0;
    let mut changed = false;
    while let Some(d) = find(data, at, b'$') {
        out.extend_from_slice(&data[at..=d]);
        at = d + 1;
        let rest = &data[at..];
        if rest.starts_with(b"Id$") {
            at += 3;
        } else if rest.starts_with(b"Id:")
            && let Some(end) = find(data, at + 3, b'$')
            && !data[at + 3..end].contains(&b'\n')
        {
            at = end + 1;
        } else {
            continue;
        }
        out.extend_from_slice(format!("Id: {id} $").as_bytes());
        changed = true;
    }
    out.extend_from_slice(&data[at..]);
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(config: &str, attrs: &[(&str, AttrValue)]) -> Conversion {
        let settings = Settings::from_config(&Config::parse(config.as_bytes()).unwrap());
        let text: String = attrs
            .iter()
            .map(|(n, v)| match v {
                AttrValue::Set => format!(" {n}"),
                AttrValue::Unset => format!(" -{n}"),
                AttrValue::Value(v) => format!(" {n}={}", String::from_utf8_lossy(v)),
                AttrValue::Unspecified => String::new(),
            })
            .collect();
        let line = format!("* {text}\n");
        let a = crate::attributes::Attributes::new(b"", line.as_bytes(), Box::new(|_| None), false);
        Conversion::new(settings, &a.for_path(b"f"))
    }

    fn git(c: &Conversion, data: &[u8]) -> Vec<u8> {
        let no_index = || None;
        c.to_git(
            b"f",
            data,
            &ToGit {
                check_roundtrip: false,
                renormalize: false,
                index_blob: &no_index,
            },
        )
        .unwrap()
        .data
        .into_owned()
    }

    fn val(s: &str) -> AttrValue {
        AttrValue::Value(s.as_bytes().to_vec())
    }

    #[test]
    fn plain_files_are_untouched_without_settings() {
        let c = conv("", &[]);
        assert!(c.is_identity());
        assert_eq!(git(&c, b"a\r\nb\n"), b"a\r\nb\n");
    }

    #[test]
    fn text_strips_carriage_returns_and_eol_crlf_adds_them() {
        let c = conv("", &[("text", AttrValue::Set), ("eol", val("crlf"))]);
        assert_eq!(git(&c, b"a\r\nb\rc\n"), b"a\nb\rc\n");
        assert_eq!(
            &*c.to_worktree(Algorithm::Sha1, b"f", b"a\nb\n").unwrap(),
            b"a\r\nb\r\n"
        );
        assert_eq!(
            &*c.to_worktree(Algorithm::Sha1, b"f", b"a\r\nb\n").unwrap(),
            b"a\r\nb\r\n"
        );
    }

    #[test]
    fn auto_leaves_binary_and_mixed_files_alone() {
        let c = conv("[core]\nautocrlf = true\n", &[]);
        assert_eq!(git(&c, b"a\r\nb\r\n"), b"a\nb\n");
        assert_eq!(git(&c, b"a\0\r\n"), b"a\0\r\n");
        assert_eq!(
            &*c.to_worktree(Algorithm::Sha1, b"f", b"a\nb\r\n").unwrap(),
            b"a\nb\r\n"
        );
        assert_eq!(
            &*c.to_worktree(Algorithm::Sha1, b"f", b"a\0\n").unwrap(),
            b"a\0\n"
        );
    }

    #[test]
    fn binary_attribute_and_core_eol() {
        assert!(conv("[core]\nautocrlf = true\n", &[("text", AttrValue::Unset)]).is_identity());
        let c = conv("[core]\neol = crlf\n", &[("text", AttrValue::Set)]);
        assert_eq!(
            &*c.to_worktree(Algorithm::Sha1, b"f", b"a\n").unwrap(),
            b"a\r\n"
        );
        assert!(conv("[core]\neol = crlf\n", &[]).is_identity());
    }

    #[test]
    fn ident_expands_and_collapses() {
        let c = conv("", &[("ident", AttrValue::Set)]);
        let out = c.to_worktree(Algorithm::Sha1, b"f", b"x $Id$ y\n").unwrap();
        assert!(
            out.starts_with(b"x $Id: ") && out.ends_with(b" $ y\n"),
            "{out:?}"
        );
        assert_eq!(git(&c, &out), b"x $Id$ y\n");
        assert_eq!(git(&c, b"$Id: a\nb $ $Id: z $"), b"$Id: a\nb $ $Id$");
    }

    #[test]
    fn safecrlf_fail_names_the_loss() {
        let c = conv("[core]\nautocrlf = true\nsafecrlf = true\n", &[]);
        let no_index = || None;
        let opts = ToGit {
            check_roundtrip: true,
            renormalize: false,
            index_blob: &no_index,
        };
        let err = c.to_git(b"f", b"a\n", &opts).err().unwrap();
        assert!(err.to_string().starts_with("LF would be replaced by CRLF"));
        assert!(c.to_git(b"f", b"a\r\n", &opts).is_ok());
    }
}
