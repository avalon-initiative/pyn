//! The text `git blame` prints: the aligned default and annotate layouts and the porcelain formats.

use std::collections::{HashMap, HashSet};

use pyn_object::{ObjectId, Signature};

use super::GitHome;
use super::blame_walk::Origin;
use super::plumbing::{abbrev, default_abbrev, quote};
use crate::commit_format::DateStyle;

/// What is shown of a commit.
pub(super) struct Info {
    pub author: Signature,
    pub committer: Signature,
    pub summary: Vec<u8>,
    pub boundary: bool,
}

/// A run of final lines blamed on consecutive lines of one origin.
pub(super) struct Entry {
    pub origin: usize,
    pub s_lno: usize,
    pub lno: usize,
    pub count: usize,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Flags {
    pub porcelain: bool,
    pub line_porcelain: bool,
    pub annotate: bool,
    pub long_names: bool,
    pub raw_time: bool,
    pub show_name: bool,
    pub show_number: bool,
    pub no_author: bool,
    pub show_email: bool,
    pub blank_boundary: bool,
}

pub(super) struct Layout<'a> {
    pub git: &'a GitHome,
    pub flags: Flags,
    /// `Some(n)` when the abbreviation length was given (already raised by one for the boundary mark).
    pub abbrev: Option<usize>,
    pub date: DateStyle,
    pub now: i64,
    pub path: &'a [u8],
    pub origins: &'a [Origin],
    pub infos: &'a HashMap<ObjectId, Info>,
    pub lines: &'a [&'a [u8]],
}

fn tz_string(offset_minutes: i32) -> String {
    let abs = offset_minutes.unsigned_abs();
    format!(
        "{}{:02}{:02}",
        if offset_minutes < 0 { '-' } else { '+' },
        abs / 60,
        abs % 60
    )
}

fn date_width(style: DateStyle) -> usize {
    match style {
        DateStyle::Default => 30,
        DateStyle::Iso | DateStyle::IsoStrict => 25,
        DateStyle::Rfc => 31,
        DateStyle::Short | DateStyle::Unix => 10,
        DateStyle::Relative => 22,
        DateStyle::Raw => 16,
    }
}

fn decimal_width(n: usize) -> usize {
    n.to_string().len()
}

/// Display columns of `text` as git counts them; bytes when it is not valid UTF-8.
fn str_width(text: &[u8]) -> usize {
    let Ok(s) = std::str::from_utf8(text) else {
        return text.len();
    };
    s.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let u = c as u32;
    match u {
        0..=0x1f | 0x7f..=0x9f => 0,
        0x300..=0x36f | 0x200b..=0x200f | 0x20d0..=0x20ff | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f => 0,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

fn pad(out: &mut Vec<u8>, count: usize) {
    out.extend(std::iter::repeat_n(b' ', count));
}

impl Layout<'_> {
    fn info(&self, e: &Entry) -> &Info {
        &self.infos[&self.origins[e.origin].commit]
    }

    fn time(&self, who: &Signature) -> String {
        let width = date_width(self.date);
        if self.flags.raw_time {
            return format!("{} {}", who.time, tz_string(who.offset_minutes));
        }
        let mut text = self.date.format(who, self.now);
        let used = str_width(text.as_bytes());
        if used < width {
            text.push_str(&" ".repeat(width - used));
        }
        text
    }

    fn name_of(&self, who: &Signature) -> Vec<u8> {
        if self.flags.show_email {
            let mut v = b"<".to_vec();
            v.extend_from_slice(&who.email);
            v.push(b'>');
            v
        } else {
            who.name.clone()
        }
    }

    pub fn render(&self, entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        if self.flags.porcelain {
            self.porcelain(entries, &mut out);
        } else {
            self.columns(entries, &mut out);
        }
        out
    }

    fn line_text(&self, out: &mut Vec<u8>, lno: usize) {
        out.extend_from_slice(self.lines[lno]);
        if !self.lines[lno].ends_with(b"\n") {
            out.push(b'\n');
        }
    }

    fn columns(&self, entries: &[Entry], out: &mut Vec<u8>) {
        let (mut longest_file, mut longest_author) = (0, 0);
        let (mut longest_src, mut longest_dst) = (0, 0);
        let mut show_name = self.flags.show_name;
        let mut seen: HashSet<ObjectId> = HashSet::new();
        let mut auto = default_abbrev(self.git);
        for e in entries {
            let o = &self.origins[e.origin];
            auto = auto.max(abbrev(self.git, &o.commit, auto).len());
            if o.path != self.path {
                show_name = true;
            }
            longest_file = longest_file.max(o.path.len());
            if seen.insert(o.commit) {
                let who = &self.info(e).author;
                longest_author = longest_author.max(str_width(&self.name_of(who)));
            }
            longest_src = longest_src.max(e.s_lno + e.count);
            longest_dst = longest_dst.max(e.lno + e.count);
        }
        let hex_len = self.git.algorithm().digest_len() * 2;
        let abbrev_len = self.abbrev.unwrap_or(auto + 1);
        let (orig_digits, digits) = (decimal_width(longest_src), decimal_width(longest_dst));
        for e in entries {
            let o = &self.origins[e.origin];
            let info = self.info(e);
            let hex = o.commit.to_hex();
            for i in 0..e.count {
                let mut length = if self.flags.long_names {
                    hex_len
                } else {
                    abbrev_len
                };
                let mut shown = hex.clone();
                if info.boundary {
                    if self.flags.blank_boundary {
                        shown = " ".repeat(hex.len());
                    } else if !self.flags.annotate {
                        length -= 1;
                        out.push(b'^');
                    }
                }
                out.extend_from_slice(&shown.as_bytes()[..length.min(shown.len())]);
                let lno = e.lno + i;
                let name = self.name_of(&info.author);
                if self.flags.annotate {
                    out.extend_from_slice(b"\t(");
                    pad(out, 10usize.saturating_sub(name.len()));
                    out.extend_from_slice(&name);
                    out.push(b'\t');
                    out.extend_from_slice(
                        format!("{:>10}\t{})", self.time(&info.author), lno + 1).as_bytes(),
                    );
                } else {
                    if show_name {
                        out.push(b' ');
                        let mut p = o.path.clone();
                        p.truncate(longest_file);
                        out.extend_from_slice(&p);
                        pad(out, longest_file - p.len());
                    }
                    if self.flags.show_number {
                        out.extend_from_slice(
                            format!(" {:>w$}", e.s_lno + 1 + i, w = orig_digits).as_bytes(),
                        );
                    }
                    if !self.flags.no_author {
                        out.extend_from_slice(b" (");
                        out.extend_from_slice(&name);
                        pad(out, longest_author - str_width(&name));
                        out.extend_from_slice(
                            format!(" {:>10}", self.time(&info.author)).as_bytes(),
                        );
                    }
                    out.extend_from_slice(format!(" {:>w$}) ", lno + 1, w = digits).as_bytes());
                }
                self.line_text(out, lno);
            }
        }
    }

    fn details(
        &self,
        e: &Entry,
        shown: &mut HashSet<ObjectId>,
        more: &HashSet<ObjectId>,
        repeat: bool,
        out: &mut Vec<u8>,
    ) {
        let o = &self.origins[e.origin];
        let info = self.info(e);
        let first = repeat || shown.insert(o.commit);
        if first {
            let push = |out: &mut Vec<u8>, key: &str, value: &[u8]| {
                out.extend_from_slice(key.as_bytes());
                out.push(b' ');
                out.extend_from_slice(value);
                out.push(b'\n');
            };
            push(out, "author", &info.author.name);
            let mut mail = b"<".to_vec();
            mail.extend_from_slice(&info.author.email);
            mail.push(b'>');
            push(out, "author-mail", &mail);
            push(out, "author-time", info.author.time.to_string().as_bytes());
            push(
                out,
                "author-tz",
                tz_string(info.author.offset_minutes).as_bytes(),
            );
            push(out, "committer", &info.committer.name);
            let mut mail = b"<".to_vec();
            mail.extend_from_slice(&info.committer.email);
            mail.push(b'>');
            push(out, "committer-mail", &mail);
            push(
                out,
                "committer-time",
                info.committer.time.to_string().as_bytes(),
            );
            push(
                out,
                "committer-tz",
                tz_string(info.committer.offset_minutes).as_bytes(),
            );
            push(out, "summary", &info.summary);
            if info.boundary {
                out.extend_from_slice(b"boundary\n");
            }
        }
        if first || more.contains(&o.commit) {
            if let Some(p) = o.previous {
                let prev = &self.origins[p];
                out.extend_from_slice(
                    format!(
                        "previous {} {}\n",
                        prev.commit.to_hex(),
                        quote(self.git, &prev.path)
                    )
                    .as_bytes(),
                );
            }
            out.extend_from_slice(format!("filename {}\n", quote(self.git, &o.path)).as_bytes());
        }
    }

    fn porcelain(&self, entries: &[Entry], out: &mut Vec<u8>) {
        let mut guilty: HashMap<ObjectId, usize> = HashMap::new();
        for o in self.origins.iter().filter(|o| o.guilty) {
            *guilty.entry(o.commit).or_insert(0) += 1;
        }
        let more: HashSet<ObjectId> = guilty
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(c, _)| c)
            .collect();
        let mut shown = HashSet::new();
        for e in entries {
            let o = &self.origins[e.origin];
            let hex = o.commit.to_hex();
            for i in 0..e.count {
                if i == 0 {
                    out.extend_from_slice(
                        format!("{hex} {} {} {}\n", e.s_lno + 1, e.lno + 1, e.count).as_bytes(),
                    );
                    self.details(e, &mut shown, &more, self.flags.line_porcelain, out);
                } else {
                    out.extend_from_slice(
                        format!("{hex} {} {}\n", e.s_lno + 1 + i, e.lno + 1 + i).as_bytes(),
                    );
                    if self.flags.line_porcelain {
                        self.details(e, &mut shown, &more, true, out);
                    }
                }
                out.push(b'\t');
                self.line_text(out, e.lno + i);
            }
        }
    }
}
