//! The `merge` attribute: which driver merges a file both sides changed (`text`, `binary`, `union` or a
//! `merge.<name>.driver` command).

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use pyn_gitwork::AttrValue;

use super::GitHome;
use super::treemerge::Labels;

pub(super) enum Driver {
    Text,
    Binary,
    Union,
    External(String),
}

/// The driver for `path`: the attribute names it, `merge.default` names it when the attribute is unspecified,
/// and a name that is no driver merges as text.
pub(super) fn find(git: &GitHome, path: &[u8]) -> Driver {
    let name = match git.repo.attributes().map(|a| a.for_path(path).get("merge")) {
        Ok(AttrValue::Set) | Err(_) => return Driver::Text,
        Ok(AttrValue::Unset) => return Driver::Binary,
        Ok(AttrValue::Value(v)) => String::from_utf8_lossy(&v).into_owned(),
        Ok(AttrValue::Unspecified) => match git.repo.config().get("merge.default") {
            Some(v) => v.to_string(),
            None => return Driver::Text,
        },
    };
    if let Some(cmd) = git.repo.config().get(&format!("merge.{name}.driver")) {
        return Driver::External(cmd.to_string());
    }
    match name.as_str() {
        "binary" => Driver::Binary,
        "union" => Driver::Union,
        _ => Driver::Text,
    }
}

/// What a driver command needs besides the three contents.
pub(super) struct Call<'a> {
    pub path: &'a [u8],
    pub labels: &'a Labels<'a>,
    pub marker_size: usize,
}

/// Runs `cmd` over the three versions; the result is the file it left as `%A` and whether it merged cleanly.
pub(super) fn run_external(
    git: &GitHome,
    cmd: &str,
    [base, ours, theirs]: [&[u8]; 3],
    call: &Call,
) -> Result<(Vec<u8>, bool)> {
    let dir = tempfile::tempdir().context("creating a merge driver scratch folder")?;
    let name = |n: char| dir.path().join(n.to_string());
    for (n, bytes) in [('O', base), ('A', ours), ('B', theirs)] {
        std::fs::write(name(n), bytes)?;
    }
    let path = String::from_utf8_lossy(call.path).into_owned();
    let line = expand(cmd, |c| {
        Some(match c {
            'O' | 'A' | 'B' => quote(&name(c)),
            'L' => call.marker_size.to_string(),
            'P' => sq(&path),
            'S' => sq(call.labels.base),
            'X' => sq(call.labels.ours),
            'Y' => sq(call.labels.theirs),
            _ => return None,
        })
    });
    let mut child = Command::new("sh");
    child.arg("-c").arg(&line).stdin(Stdio::null());
    if let Ok(root) = git.repo.root() {
        child.current_dir(root);
    }
    let status = child
        .status()
        .with_context(|| format!("running merge driver `{cmd}`"))?;
    if status.code().is_none_or(|c| c > 128) {
        bail!("merge driver `{cmd}` failed");
    }
    Ok((std::fs::read(name('A'))?, status.success()))
}

fn quote(path: &Path) -> String {
    sq(&path.to_string_lossy())
}

fn sq(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn expand(cmd: &str, value: impl Fn(char) -> Option<String>) -> String {
    let mut out = String::new();
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(n) => match value(n) {
                Some(v) => {
                    chars.next();
                    out.push_str(&v);
                }
                None => out.push('%'),
            },
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_expand_and_percent_doubles() {
        let got = expand("m %A %% %Z", |c| (c == 'A').then(|| "'a'".to_string()));
        assert_eq!(got, "m 'a' % %Z");
    }

    #[test]
    fn single_quotes_are_escaped() {
        assert_eq!(sq("a'b"), "'a'\\''b'");
    }
}
