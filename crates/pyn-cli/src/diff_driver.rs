//! The diff driver a path's `diff` attribute names: whether the path is binary, the `textconv` command that turns a
//! side into text, and the `funcname`/`xfuncname` patterns that pick hunk headings.

use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use regex::bytes::Regex;

use crate::git_home::grep_regex::{self, Syntax};

pub mod builtin;

#[derive(Clone, Default)]
pub struct DiffDriver {
    /// `Some(true)` forces binary, `Some(false)` forces text.
    pub binary: Option<bool>,
    pub textconv: Option<String>,
    pub funcname: Option<FuncMatcher>,
    /// `diff.<name>.wordRegex`: the word pattern of `--word-diff` for the path.
    pub word_regex: Option<String>,
    /// Where `textconv` runs; `None` keeps the current folder.
    pub work_tree: Option<std::path::PathBuf>,
}

impl DiffDriver {
    /// The driver `path`'s `diff` attribute names, with its `diff.<name>.*` settings from `config`.
    pub fn resolve(
        attrs: &pyn_gitwork::Attributes,
        config: &pyn_git::Config,
        path: &str,
    ) -> Result<Self> {
        use pyn_gitwork::AttrValue;
        let mut driver = Self::default();
        let name = match attrs.for_path(path.as_bytes()).get("diff") {
            AttrValue::Unset => {
                driver.binary = Some(true);
                return Ok(driver);
            }
            AttrValue::Set => {
                driver.binary = Some(false);
                return Ok(driver);
            }
            AttrValue::Value(v) => String::from_utf8_lossy(&v).into_owned(),
            AttrValue::Unspecified => return Ok(driver),
        };
        let key = |k: &str| format!("diff.{name}.{k}");
        for e in config.entries() {
            let valued = ["funcname", "xfuncname", "textconv", "wordregex"];
            if e.section == "diff"
                && e.subsection.as_deref() == Some(name.as_str())
                && e.value.is_none()
                && valued.contains(&e.key.as_str())
            {
                eprintln!("error: missing value for '{}'", key(&e.key));
                bail!("bad config variable '{}'", key(&e.key));
            }
        }
        driver.word_regex = config.get(&key("wordregex")).map(str::to_string);
        driver.binary = config.get_bool(&key("binary")).ok().flatten();
        driver.textconv = config.get(&key("textconv")).map(str::to_string);
        let last = config.entries().iter().rev().find(|e| {
            e.section == "diff"
                && e.subsection.as_deref() == Some(name.as_str())
                && (e.key == "funcname" || e.key == "xfuncname")
        });
        driver.funcname = match last {
            Some(e) => Some(FuncMatcher::parse(
                e.value.as_deref().unwrap_or(""),
                e.key == "xfuncname",
                false,
            )?),
            None => builtin::funcname(&name)
                .map(|(patterns, icase)| FuncMatcher::parse(&patterns, true, icase))
                .transpose()?,
        };
        Ok(driver)
    }
}

/// Hunk-header patterns in order: the first one that matches a line decides, and a negated one rejects the line.
#[derive(Clone)]
pub struct FuncMatcher {
    patterns: Vec<(bool, Regex)>,
}

impl FuncMatcher {
    /// `value` is one pattern per line; `extended` is `xfuncname` (else the basic syntax of `funcname`).
    pub fn parse(value: &str, extended: bool, icase: bool) -> Result<Self> {
        let mut patterns = Vec::new();
        for raw in value.split('\n').filter(|l| !l.is_empty()) {
            let (negate, text) = match raw.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, raw),
            };
            let syntax = if extended {
                Syntax::Extended
            } else {
                Syntax::Basic
            };
            let re = grep_regex::compile_with(text, syntax, icase, true).map_err(|e| {
                anyhow::anyhow!("Invalid regexp to look for hunk headers: {text}: {e}")
            })?;
            patterns.push((negate, re));
        }
        Ok(Self { patterns })
    }

    /// The heading a line gives: its first group, else the whole match.
    pub fn heading(&self, line: &[u8]) -> Option<Vec<u8>> {
        for (negate, re) in &self.patterns {
            if let Some(caps) = re.captures(line) {
                if *negate {
                    return None;
                }
                let m = caps.get(1).or_else(|| caps.get(0))?;
                return Some(m.as_bytes().to_vec());
            }
        }
        None
    }
}

/// Runs `command` on a file holding `content` and returns its output, as `diff.<name>.textconv` does.
pub fn textconv(command: &str, content: &[u8], dir: Option<&std::path::Path>) -> Result<Vec<u8>> {
    let tmp = tempfile::Builder::new()
        .prefix(".diff_")
        .tempfile()
        .context("creating a file for textconv")?;
    std::fs::write(tmp.path(), content)?;
    let mut run = Command::new("sh");
    run.arg("-c")
        .arg(format!("{command} \"$@\""))
        .arg(command)
        .arg(tmp.path())
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(dir) = dir {
        run.current_dir(dir);
    }
    let out = run
        .output()
        .with_context(|| format!("running textconv `{command}`"))?;
    if !out.status.success() {
        eprintln!("error: textconv `{command}` failed");
        bail!("unable to read files to diff");
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_syntax_groups_are_escaped_parentheses() {
        let m = FuncMatcher::parse(r"^\(proc\) [a-z]*", false, false).unwrap();
        assert_eq!(m.heading(b"proc one"), Some(b"proc".to_vec()));
    }

    #[test]
    fn the_first_group_is_the_heading_and_a_negated_pattern_rejects() {
        let m = FuncMatcher::parse("!^skip\n^(sec [a-z]+)\n^[a-z]+ me", true, false).unwrap();
        assert_eq!(m.heading(b"sec alpha beta"), Some(b"sec alpha".to_vec()));
        assert_eq!(m.heading(b"skip me"), None);
        assert_eq!(m.heading(b"do me"), Some(b"do me".to_vec()));
        assert_eq!(m.heading(b"other"), None);
    }
}
