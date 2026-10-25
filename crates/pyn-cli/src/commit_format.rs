//! The text `log` and `show` print for a commit on either home: pretty formats, `%` placeholders and date styles.
//! A commit is a `pyn_object::Commit` with its id; a server home's `api::Commit` converts into one.

use std::collections::HashMap;

use anyhow::{Result, bail};
use chrono::{DateTime, FixedOffset};
use pyn_object::{Commit, ObjectId, Signature};
use pyn_proto as api;

use crate::time;

/// The commit abbreviation `%h`, `%p` and `Merge:` lines use.
const ABBREV: usize = 7;

/// The format options `log` and `show` share.
#[derive(clap::Args, Debug, Default)]
pub struct FormatArgs {
    /// One line per commit: the abbreviated id and the subject.
    #[arg(long)]
    oneline: bool,
    /// `oneline`, `short`, `medium`, `full`, `fuller`, `raw`, or a template such as `%h %s`.
    #[arg(long, value_name = "FORMAT", require_equals = true)]
    format: Option<String>,
    /// Like `--format`; without a value it is `medium`.
    #[arg(
        long,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "medium"
    )]
    pretty: Option<String>,
    /// `default`, `iso`, `iso-strict`, `rfc`, `short`, `relative`, `unix` or `raw`.
    #[arg(long, value_name = "STYLE")]
    date: Option<String>,
}

impl FormatArgs {
    /// The first option given, for a message naming it.
    pub fn first_given(&self) -> Option<&'static str> {
        [
            ("--oneline", self.oneline),
            ("--format", self.format.is_some()),
            ("--pretty", self.pretty.is_some()),
            ("--date", self.date.is_some()),
        ]
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(f, _)| f)
    }

    /// The pretty format; `medium` when none is named. `--format` and `--pretty` win over `--oneline`.
    pub fn pretty(&self) -> Result<Pretty> {
        Ok(
            match (self.format.as_ref().or(self.pretty.as_ref()), self.oneline) {
                (Some(f), _) => Pretty::parse(f)?,
                (None, true) => Pretty::Oneline { abbrev: true },
                (None, false) => Pretty::Medium,
            },
        )
    }

    pub fn style(&self) -> Result<DateStyle> {
        match &self.date {
            Some(d) => DateStyle::parse(d),
            None => Ok(DateStyle::Default),
        }
    }

    /// The format and date style, or `None` when no option asks for one.
    pub fn requested(&self) -> Result<Option<Rendering>> {
        match self.first_given() {
            Some(_) => self.rendering().map(Some),
            None => Ok(None),
        }
    }

    /// The format and date style, `medium` and `default` when no option names them.
    pub fn rendering(&self) -> Result<Rendering> {
        let mut pretty = self.pretty()?;
        if self.oneline
            && let Pretty::Oneline { abbrev } = &mut pretty
        {
            *abbrev = true;
        }
        Ok(Rendering {
            pretty,
            style: self.style()?,
        })
    }
}

/// A chosen format and date style.
pub struct Rendering {
    pub pretty: Pretty,
    pub style: DateStyle,
}

fn person(p: &api::Person, time: i64) -> Signature {
    Signature {
        name: p.name.clone().into_bytes(),
        email: p.email.clone().into_bytes(),
        time,
        offset_minutes: p.offset_minutes,
    }
}

/// A server home's commit as the renderer's view of it, with its id.
pub fn from_api(c: &api::Commit) -> Result<(ObjectId, Commit)> {
    let ids = |hex: &str| ObjectId::parse(hex).map_err(anyhow::Error::from);
    let view = Commit {
        tree: ids(&c.tree)?,
        parents: c.parents.iter().map(|p| ids(p)).collect::<Result<_>>()?,
        author: person(&c.author, c.authored_at.unwrap_or(c.created_at).timestamp()),
        committer: person(&c.committer, c.created_at.timestamp()),
        extra_headers: Vec::new(),
        message: c.message.clone().into_bytes(),
    };
    Ok((ids(&c.id)?, view))
}

/// A server home's commit as `r` prints it, with its terminator; no ref names are known there.
pub fn render_api(r: &Rendering, c: &api::Commit) -> Result<String> {
    let (id, view) = from_api(c)?;
    let none = Decorations::new();
    let ctx = Context {
        style: r.style,
        now: chrono::Utc::now().timestamp(),
        decorations: &none,
        commit_color: None,
    };
    Ok(format!(
        "{}{}",
        render(&r.pretty, &ctx, &id, &view),
        r.pretty.terminator()
    ))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DateStyle {
    Default,
    Iso,
    IsoStrict,
    Rfc,
    Short,
    Relative,
    Unix,
    Raw,
}

impl DateStyle {
    pub fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "default" => Self::Default,
            "iso" | "iso8601" => Self::Iso,
            "iso-strict" | "iso8601-strict" => Self::IsoStrict,
            "rfc" | "rfc2822" => Self::Rfc,
            "short" => Self::Short,
            "relative" => Self::Relative,
            "unix" => Self::Unix,
            "raw" => Self::Raw,
            _ => bail!("unknown date format {name}"),
        })
    }

    pub fn format(self, who: &Signature, now: i64) -> String {
        let (secs, off) = (who.time, who.offset_minutes);
        match self {
            Self::Default => time::git_date(secs, off),
            Self::Iso => at(secs, off).format("%Y-%m-%d %H:%M:%S %z").to_string(),
            Self::IsoStrict if off == 0 => at(secs, off).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            Self::IsoStrict => at(secs, off).format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
            Self::Rfc => at(secs, off)
                .format("%a, %-d %b %Y %H:%M:%S %z")
                .to_string(),
            Self::Short => at(secs, off).format("%Y-%m-%d").to_string(),
            Self::Relative => relative(now - secs),
            Self::Unix => secs.to_string(),
            Self::Raw => format!("{secs} {}", at(secs, off).format("%z")),
        }
    }
}

fn at(secs: i64, offset_minutes: i32) -> DateTime<FixedOffset> {
    let zone = FixedOffset::east_opt(offset_minutes * 60)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero is a valid offset"));
    DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .with_timezone(&zone)
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `n units ago` with git's rounding thresholds.
pub fn relative(diff: i64) -> String {
    if diff < 0 {
        return "in the future".into();
    }
    if diff < 90 {
        return format!("{} ago", plural(diff, "second", "seconds"));
    }
    let minutes = (diff + 30) / 60;
    if minutes < 90 {
        return format!("{} ago", plural(minutes, "minute", "minutes"));
    }
    let hours = (minutes + 30) / 60;
    if hours < 36 {
        return format!("{} ago", plural(hours, "hour", "hours"));
    }
    let days = (hours + 12) / 24;
    if days < 14 {
        return format!("{} ago", plural(days, "day", "days"));
    }
    if days < 70 {
        return format!("{} ago", plural((days + 3) / 7, "week", "weeks"));
    }
    if days < 365 {
        return format!("{} ago", plural((days + 15) / 30, "month", "months"));
    }
    if days < 1825 {
        let months = (days * 12 * 2 + 365) / (365 * 2);
        let (years, rest) = (months / 12, months % 12);
        if rest > 0 {
            return format!(
                "{}, {} ago",
                plural(years, "year", "years"),
                plural(rest, "month", "months")
            );
        }
        return format!("{} ago", plural(years, "year", "years"));
    }
    format!("{} ago", plural((days + 183) / 365, "year", "years"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pretty {
    /// `--oneline` abbreviates the id, `--pretty=oneline` does not.
    Oneline {
        abbrev: bool,
    },
    Short,
    Medium,
    Full,
    Fuller,
    Raw,
    /// `terminated` is `tformat:` (and a bare template): a newline after each commit, not between them.
    User {
        template: String,
        terminated: bool,
    },
}

impl Pretty {
    pub fn parse(spec: &str) -> Result<Self> {
        if let Some(t) = spec.strip_prefix("tformat:") {
            return Ok(Self::user(t, true));
        }
        if let Some(t) = spec.strip_prefix("format:") {
            return Ok(Self::user(t, false));
        }
        Ok(match spec {
            "" => Self::user("", true),
            "oneline" => Self::Oneline { abbrev: false },
            "short" => Self::Short,
            "medium" => Self::Medium,
            "full" => Self::Full,
            "fuller" => Self::Fuller,
            "raw" => Self::Raw,
            t if t.contains('%') => Self::user(t, true),
            _ => bail!("invalid --pretty format: {spec}"),
        })
    }

    fn user(template: &str, terminated: bool) -> Self {
        Self::User {
            template: template.to_string(),
            terminated,
        }
    }

    /// Whether the entries are lines of their own with no blank line between them.
    pub fn terminated(&self) -> bool {
        matches!(
            self,
            Self::Oneline { .. }
                | Self::User {
                    terminated: true,
                    ..
                }
        )
    }

    /// What follows each entry's text: a newline for the line formats.
    pub fn terminator(&self) -> &'static str {
        match self {
            Self::Oneline { .. } => "\n",
            Self::User {
                template,
                terminated: true,
            } if !template.is_empty() => "\n",
            _ => "",
        }
    }

    /// Whether a diff is set off from the entry by a blank line.
    pub fn blank_before_diff(&self) -> bool {
        match self {
            Self::Oneline { .. } => false,
            Self::User { template, .. } => !template.is_empty(),
            _ => true,
        }
    }

    pub fn uses_decorations(&self) -> bool {
        matches!(self, Self::User { template, .. } if template.contains("%d") || template.contains("%D"))
    }
}

/// Ref names by the commit they point at, in the order `%d` prints them.
pub type Decorations = HashMap<ObjectId, Vec<String>>;

pub struct Context<'a> {
    pub style: DateStyle,
    pub now: i64,
    pub decorations: &'a Decorations,
    /// The escape sequence that paints a commit id, when output is colored.
    pub commit_color: Option<&'a str>,
}

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ident(who: &Signature) -> String {
    format!("{} <{}>", lossy(&who.name), lossy(&who.email))
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

fn skip_blank_lines(text: &str) -> &str {
    let mut rest = text;
    while let Some((line, tail)) = rest.split_once('\n') {
        if !is_blank(line) {
            break;
        }
        rest = tail;
    }
    if is_blank(rest) { "" } else { rest }
}

/// The first paragraph joined on single spaces, and the text after the blank lines that follow it.
pub(crate) fn subject_and_body(message: &str) -> (String, &str) {
    let mut rest = skip_blank_lines(message);
    let mut subject = String::new();
    while !rest.is_empty() {
        let (line, tail) = rest.split_once('\n').unwrap_or((rest, ""));
        if is_blank(line) {
            break;
        }
        if !subject.is_empty() {
            subject.push(' ');
        }
        subject.push_str(line.trim_end());
        rest = tail;
    }
    (subject, skip_blank_lines(rest))
}

/// The message indented by four spaces, as `medium` prints it; `first_paragraph` stops at the first blank line.
fn indented(message: &[u8], first_paragraph: bool) -> String {
    let text = lossy(message);
    let mut lines: Vec<&str> = text.split('\n').collect();
    while lines.last().is_some_and(|l| is_blank(l)) {
        lines.pop();
    }
    let skip = lines.iter().take_while(|l| is_blank(l)).count();
    let mut out = String::new();
    for line in &lines[skip..] {
        if first_paragraph && is_blank(line) {
            break;
        }
        out.push_str("    ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn merge_line(c: &Commit) -> String {
    if c.parents.len() < 2 {
        return String::new();
    }
    let merged: Vec<String> = c.parents.iter().map(short).collect();
    format!("Merge: {}\n", merged.join(" "))
}

/// One commit as a format prints it, without the separator or terminator between commits.
pub fn render(pretty: &Pretty, ctx: &Context<'_>, id: &ObjectId, c: &Commit) -> String {
    let date = |who: &Signature| ctx.style.format(who, ctx.now);
    let paint = |text: String| match ctx.commit_color {
        Some(color) => format!("{color}{text}{}", crate::diff_color::RESET),
        None => text,
    };
    let head = format!("{}\n{}", paint(format!("commit {id}")), merge_line(c));
    match pretty {
        Pretty::Oneline { abbrev } => {
            let (subject, _) = subject_and_body(&lossy(&c.message));
            let name = if *abbrev { short(id) } else { id.to_hex() };
            format!("{} {subject}", paint(name))
        }
        Pretty::Short => format!(
            "{head}Author: {}\n\n{}",
            ident(&c.author),
            indented(&c.message, true)
        ),
        Pretty::Medium => format!(
            "{head}Author: {}\nDate:   {}\n\n{}",
            ident(&c.author),
            date(&c.author),
            indented(&c.message, false)
        ),
        Pretty::Full => format!(
            "{head}Author: {}\nCommit: {}\n\n{}",
            ident(&c.author),
            ident(&c.committer),
            indented(&c.message, false)
        ),
        Pretty::Fuller => format!(
            "{head}Author:     {}\nAuthorDate: {}\nCommit:     {}\nCommitDate: {}\n\n{}",
            ident(&c.author),
            date(&c.author),
            ident(&c.committer),
            date(&c.committer),
            indented(&c.message, false)
        ),
        Pretty::Raw => {
            let mut out = format!("{}\ntree {}\n", paint(format!("commit {id}")), c.tree);
            for p in &c.parents {
                out.push_str(&format!("parent {p}\n"));
            }
            let raw = |who: &Signature| {
                format!(
                    "{} {} {}",
                    ident(who),
                    who.time,
                    at(who.time, who.offset_minutes).format("%z")
                )
            };
            out.push_str(&format!(
                "author {}\ncommitter {}\n\n{}",
                raw(&c.author),
                raw(&c.committer),
                indented(&c.message, false)
            ));
            out
        }
        Pretty::User { template, .. } => expand(template, ctx, id, c),
    }
}

fn expand(template: &str, ctx: &Context<'_>, id: &ObjectId, c: &Commit) -> String {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        match placeholder(&chars[i + 1..], ctx, id, c) {
            Some((text, used)) => {
                out.push_str(&text);
                i += 1 + used;
            }
            None => {
                out.push('%');
                i += 1;
            }
        }
    }
    out
}

/// The expansion of the placeholder at the start of `rest` and how many characters it used.
fn placeholder(
    rest: &[char],
    ctx: &Context<'_>,
    id: &ObjectId,
    c: &Commit,
) -> Option<(String, usize)> {
    let message = lossy(&c.message);
    let names = ctx.decorations.get(id);
    let one = |text: String| Some((text, 1));
    match *rest.first()? {
        '%' => one("%".into()),
        'n' => one("\n".into()),
        'H' => one(id.to_hex()),
        'h' => one(short(id)),
        'T' => one(c.tree.to_hex()),
        't' => one(short(&c.tree)),
        'P' => one(join(c.parents.iter().map(ObjectId::to_hex))),
        'p' => one(join(c.parents.iter().map(short))),
        's' => one(subject_and_body(&message).0),
        'b' => one(subject_and_body(&message).1.to_string()),
        'B' => one(message),
        'd' => one(names.map_or(String::new(), |n| format!(" ({})", n.join(", ")))),
        'D' => one(names.map_or(String::new(), |n| n.join(", "))),
        which @ ('a' | 'c') => {
            let who = if which == 'a' {
                &c.author
            } else {
                &c.committer
            };
            let text = match *rest.get(1)? {
                'n' => lossy(&who.name),
                'e' => lossy(&who.email),
                'd' => ctx.style.format(who, ctx.now),
                'D' => DateStyle::Rfc.format(who, ctx.now),
                'i' => DateStyle::Iso.format(who, ctx.now),
                'I' => DateStyle::IsoStrict.format(who, ctx.now),
                's' => DateStyle::Short.format(who, ctx.now),
                't' => DateStyle::Unix.format(who, ctx.now),
                'r' => DateStyle::Relative.format(who, ctx.now),
                _ => return None,
            };
            Some((text, 2))
        }
        _ => None,
    }
}

fn join(items: impl Iterator<Item = String>) -> String {
    items.collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_dates_round_like_git() {
        assert_eq!(relative(1), "1 second ago");
        assert_eq!(relative(89), "89 seconds ago");
        assert_eq!(relative(90), "2 minutes ago");
        assert_eq!(relative(3 * 3600), "3 hours ago");
        assert_eq!(relative(5 * 86_400), "5 days ago");
        assert_eq!(relative(21 * 86_400), "3 weeks ago");
        assert_eq!(relative(100 * 86_400), "3 months ago");
        assert_eq!(relative(400 * 86_400), "1 year, 1 month ago");
        assert_eq!(relative(730 * 86_400), "2 years ago");
        assert_eq!(relative(3650 * 86_400), "10 years ago");
        assert_eq!(relative(-5), "in the future");
    }

    #[test]
    fn pretty_names_and_templates_parse() {
        assert_eq!(
            Pretty::parse("oneline").unwrap(),
            Pretty::Oneline { abbrev: false }
        );
        assert!(Pretty::parse("%h").unwrap().terminated());
        assert!(!Pretty::parse("format:%h").unwrap().terminated());
        assert!(Pretty::parse("nonsense").is_err());
    }

    #[test]
    fn subject_joins_the_first_paragraph_and_body_follows_it() {
        let (s, b) = subject_and_body("\none\ntwo  \n\n\nbody\nmore\n");
        assert_eq!((s.as_str(), b), ("one two", "body\nmore\n"));
        assert_eq!(subject_and_body("only\n"), ("only".to_string(), ""));
    }

    #[test]
    fn message_indents_and_stops_at_a_paragraph_for_short() {
        let m = b"a\n\nb  \n\n";
        assert_eq!(indented(m, false), "    a\n    \n    b\n");
        assert_eq!(indented(m, true), "    a\n");
    }

    #[test]
    fn date_styles_follow_git() {
        let who = Signature {
            name: b"n".to_vec(),
            email: b"e".to_vec(),
            time: 1_700_000_000,
            offset_minutes: 60,
        };
        let f = |s: DateStyle| s.format(&who, 0);
        assert_eq!(f(DateStyle::Iso), "2023-11-14 23:13:20 +0100");
        assert_eq!(f(DateStyle::IsoStrict), "2023-11-14T23:13:20+01:00");
        let utc = Signature {
            offset_minutes: 0,
            ..who.clone()
        };
        assert_eq!(DateStyle::IsoStrict.format(&utc, 0), "2023-11-14T22:13:20Z");
        assert_eq!(f(DateStyle::Rfc), "Tue, 14 Nov 2023 23:13:20 +0100");
        assert_eq!(f(DateStyle::Short), "2023-11-14");
        assert_eq!(f(DateStyle::Unix), "1700000000");
        assert_eq!(f(DateStyle::Raw), "1700000000 +0100");
        assert!(DateStyle::parse("human").is_err());
    }
}
