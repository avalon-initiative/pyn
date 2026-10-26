//! Ref decorations for `log` and `show`: the style (flags, `log.decorate`, terminal), the ref filters and the names.

use std::io::IsTerminal;

use anyhow::{Result, bail};
use pyn_gitwork::{Head, wildmatch};

use super::GitHome;
use crate::commit_format::{Decorations, Pretty};

/// The decoration flags `log` and `show` share.
#[derive(clap::Args, Debug, Default)]
pub struct DecorateArgs {
    /// Name the refs at each commit: `short` (the default), `full`, `auto` or `no`.
    #[arg(
        long,
        value_name = "STYLE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "short",
        overrides_with = "no_decorate"
    )]
    decorate: Option<String>,
    /// Do not name the refs at each commit.
    #[arg(long, overrides_with = "decorate")]
    no_decorate: bool,
    /// Name only the refs that match a pattern (a glob, or a ref or a folder of refs).
    #[arg(long, value_name = "PATTERN")]
    decorate_refs: Vec<String>,
    /// Leave out the refs that match a pattern.
    #[arg(long, value_name = "PATTERN")]
    decorate_refs_exclude: Vec<String>,
}

impl DecorateArgs {
    /// Each option with whether it was given, for a pyn server that takes none.
    pub fn given(&self) -> [(&'static str, bool); 4] {
        [
            ("--decorate", self.decorate.is_some()),
            ("--no-decorate", self.no_decorate),
            ("--decorate-refs", !self.decorate_refs.is_empty()),
            (
                "--decorate-refs-exclude",
                !self.decorate_refs_exclude.is_empty(),
            ),
        ]
    }

    /// The style and the ref filters in force: the flags, then `log.decorate`, then `auto`.
    /// `--pretty=raw` is only decorated when a flag asks.
    pub fn resolve(&self, git: &GitHome, pretty: &Pretty) -> Result<Decorate> {
        let tty = std::io::stdout().is_terminal();
        let style = if self.no_decorate {
            Style::Off
        } else if let Some(value) = &self.decorate {
            match parse_style(value, tty) {
                Some(style) => style,
                None => bail!("invalid --decorate option: {value}"),
            }
        } else if *pretty == Pretty::Raw {
            Style::Off
        } else {
            configured(git, tty)
        };
        let config_excludes = if self.decorate_refs.is_empty() {
            git.config().get_all("log.excludedecoration")
        } else {
            Vec::new()
        };
        Ok(Decorate {
            style,
            filter: Filter {
                include: patterns(&self.decorate_refs)?,
                exclude: patterns(&self.decorate_refs_exclude)?,
                config_exclude: patterns(&config_excludes)?,
            },
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Off,
    Short,
    Full,
}

/// How to decorate, and which refs may be named.
pub struct Decorate {
    pub style: Style,
    filter: Filter,
}

impl Decorate {
    pub fn on(&self) -> bool {
        self.style != Style::Off
    }

    /// The names by commit; `%d` needs them too, so `wanted` loads them even when the style is off.
    pub fn names(&self, git: &GitHome, wanted: bool) -> Result<Decorations> {
        if self.on() || wanted {
            names(git, self.style == Style::Full, &self.filter)
        } else {
            Ok(Decorations::new())
        }
    }
}

pub(super) fn maybe_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        other => other.parse::<i64>().ok().map(|n| n != 0),
    }
}

fn parse_style(value: &str, tty: bool) -> Option<Style> {
    match maybe_bool(value) {
        Some(true) => return Some(Style::Short),
        Some(false) => return Some(Style::Off),
        None => {}
    }
    match value {
        "short" => Some(Style::Short),
        "full" => Some(Style::Full),
        "auto" => Some(if tty { Style::Short } else { Style::Off }),
        _ => None,
    }
}

/// `log.decorate`: a bare key is true, an unknown value is off, and nothing set is `auto`.
fn configured(git: &GitHome, tty: bool) -> Style {
    let last = git
        .config()
        .entries()
        .iter()
        .rev()
        .find(|e| e.section == "log" && e.subsection.is_none() && e.key == "decorate");
    match last {
        None => parse_style("auto", tty).unwrap_or(Style::Off),
        Some(e) => match e.value.as_deref() {
            None => Style::Short,
            Some(v) => parse_style(v, tty).unwrap_or(Style::Off),
        },
    }
}

/// A ref pattern: a glob, or a name that also matches the refs below it.
struct Pattern {
    text: String,
    glob: bool,
}

impl Pattern {
    fn new(raw: &str) -> Result<Self> {
        if raw.starts_with('/') {
            bail!("a decoration pattern must not start with '/': {raw}");
        }
        let mut text = if raw.starts_with("refs/") || raw == "HEAD" {
            raw.to_string()
        } else {
            format!("refs/{raw}")
        };
        if text.ends_with('/') {
            text.pop();
        }
        Ok(Self {
            glob: raw.contains(['*', '?', '[', '\\']),
            text,
        })
    }

    fn matches(&self, name: &str) -> bool {
        if self.glob {
            wildmatch(self.text.as_bytes(), name.as_bytes(), false)
        } else {
            name.strip_prefix(&self.text)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        }
    }
}

fn patterns<S: AsRef<str>>(raw: &[S]) -> Result<Vec<Pattern>> {
    raw.iter().map(|p| Pattern::new(p.as_ref())).collect()
}

/// `--decorate-refs`, `--decorate-refs-exclude` and `log.excludeDecoration` (ignored under `--decorate-refs`).
struct Filter {
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
    config_exclude: Vec<Pattern>,
}

impl Filter {
    fn keeps(&self, name: &str) -> bool {
        let hit = |ps: &[Pattern]| ps.iter().any(|p| p.matches(name));
        !hit(&self.exclude)
            && (self.include.is_empty() || hit(&self.include))
            && !hit(&self.config_exclude)
    }
}

fn names(git: &GitHome, full: bool, filter: &Filter) -> Result<Decorations> {
    let head = git.repo.head()?;
    let head_kept = filter.keeps("HEAD");
    let current = match &head {
        Head::Branch { name, .. } => Some(format!("refs/heads/{name}")),
        Head::Detached(_) => None,
    };
    let joined = head_kept && current.as_deref().is_some_and(|c| filter.keeps(c));
    let mut map = Decorations::new();
    let mut listed = git.repo.refs().list("refs/")?;
    listed.reverse();
    for (name, _) in listed {
        if !filter.keeps(&name) || (joined && current.as_deref() == Some(name.as_str())) {
            continue;
        }
        let Ok(id) = git.repo.resolve(&name) else {
            continue;
        };
        let short = |prefix: &str| name.strip_prefix(prefix).filter(|_| !full);
        let label = if name.starts_with("refs/tags/") {
            format!("tag: {}", short("refs/tags/").unwrap_or(&name))
        } else if let Some(n) = short("refs/heads/").or(short("refs/remotes/")) {
            n.to_string()
        } else {
            name
        };
        map.entry(id).or_default().push(label);
    }
    let first = match (&head, head.id()) {
        _ if !head_kept => None,
        (Head::Branch { name, .. }, Some(id)) if joined && full => {
            Some((id, format!("HEAD -> refs/heads/{name}")))
        }
        (Head::Branch { name, .. }, Some(id)) if joined => Some((id, format!("HEAD -> {name}"))),
        (Head::Detached(id), _) => Some((*id, "HEAD".to_string())),
        (_, Some(id)) => Some((id, "HEAD".to_string())),
        _ => None,
    };
    if let Some((id, label)) = first {
        map.entry(id).or_default().insert(0, label);
    }
    Ok(map)
}

/// Inserts ` (names)` after the id on the first line of a built-in format.
pub(super) fn with_decoration(
    mut text: String,
    pretty: &Pretty,
    names: Option<&Vec<String>>,
) -> String {
    let Some(names) = names else { return text };
    let end = match pretty {
        Pretty::User { .. } => return text,
        Pretty::Oneline { .. } => text.find([' ', '\n']),
        _ => text.find('\n'),
    };
    text.insert_str(
        end.unwrap_or(text.len()),
        &format!(" ({})", names.join(", ")),
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_spellings_follow_git() {
        assert_eq!(parse_style("auto", true), Some(Style::Short));
        assert_eq!(parse_style("auto", false), Some(Style::Off));
        assert_eq!(parse_style("YES", false), Some(Style::Short));
        assert_eq!(parse_style("2", false), Some(Style::Short));
        assert_eq!(parse_style("0", true), Some(Style::Off));
        assert_eq!(parse_style("", true), Some(Style::Off));
        assert_eq!(parse_style("full", false), Some(Style::Full));
        assert_eq!(parse_style("Full", false), None);
        assert_eq!(parse_style("AUTO", true), None);
    }

    #[test]
    fn patterns_match_like_git() {
        let m = |p: &str, n: &str| Pattern::new(p).unwrap().matches(n);
        assert!(m("tags/*", "refs/tags/v1"));
        assert!(m("refs/tags", "refs/tags/v1"));
        assert!(m("refs/tags/", "refs/tags/v1"));
        assert!(m("refs/heads/other", "refs/heads/other/deep"));
        assert!(!m("refs/heads/mai", "refs/heads/main"));
        assert!(m("refs/heads/ma*", "refs/heads/main"));
        assert!(m("HEAD", "HEAD"));
        assert!(m("", "refs/tags/v1"));
        assert!(!m("", "HEAD"));
        assert!(Pattern::new("/refs/x").is_err());
    }
}
