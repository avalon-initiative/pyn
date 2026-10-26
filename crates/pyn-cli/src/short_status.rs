//! git's short status format (`XY <path>`) shared by both homes: `-s`, `--porcelain` (v1) and `-b`.

use std::io::Write;

use anyhow::Result;
use clap::Args;

/// Flags of `pyn status` that choose the output format and its pathspecs.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Compare with the repository as of this commit (`HEAD~N`, a commit hash) instead of the newest.
    #[arg(long)]
    pub revision: Option<String>,
    /// Two-column `XY <path>` lines instead of the table.
    /// On a pyn server, lock state is shown in the default table and by `pyn lock`/`pyn locks`, not in `-s`/`--porcelain` output.
    #[arg(short = 's', long)]
    pub short: bool,
    /// Like `--short`, with paths from the repository root and no branch line unless `-b` is given.
    #[arg(
        long,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "v1"
    )]
    pub porcelain: Option<String>,
    /// With `--short` or `--porcelain`, a `## <branch>...<upstream>` line first.
    #[arg(short = 'b', long)]
    pub branch: bool,
    /// Ignore changes in submodules: `none`, `untracked`, `dirty` or `all` (the default for a bare flag).
    #[arg(
        long,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "all",
        value_name = "WHEN"
    )]
    pub ignore_submodules: Option<String>,
    /// Only these files and folders, taken relative to where you stand.
    pub paths: Vec<String>,
}

impl StatusArgs {
    /// Git's check of the version: `v1` or `1`, an unknown one is fatal (128), `v2` is not built (usage, 129).
    pub fn check_porcelain(&self) -> Result<(), (i32, String)> {
        match self.porcelain.as_deref() {
            None | Some("v1" | "1") => Ok(()),
            Some(v @ ("v2" | "2")) => {
                Err((129, format!("error: porcelain version '{v}' is not built")))
            }
            Some(v) => Err((128, format!("fatal: unsupported porcelain version '{v}'"))),
        }
    }

    pub fn format(&self) -> Format {
        match (&self.porcelain, self.short) {
            (Some(_), _) => Format::Porcelain,
            (None, true) => Format::Short,
            (None, false) => Format::Table,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Short,
    Porcelain,
}

/// One status line: the index column, the work tree column and a repository path (a folder ends in `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub x: char,
    pub y: char,
    pub path: Vec<u8>,
    /// The path a rename or copy came from.
    pub from: Option<Vec<u8>>,
}

impl Row {
    pub fn untracked(path: Vec<u8>) -> Self {
        Self {
            x: '?',
            y: '?',
            path,
            from: None,
        }
    }
}

/// `path` as seen from the folder `prefix` (both from the root, no trailing slash on `prefix`).
pub(crate) fn relative(prefix: &[u8], path: &[u8]) -> Vec<u8> {
    let is_dir = path.ends_with(b"/");
    let trimmed = path.strip_suffix(b"/").unwrap_or(path);
    let split = |p: &[u8]| -> Vec<Vec<u8>> {
        p.split(|b| *b == b'/')
            .filter(|c| !c.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    let (from, to) = (split(prefix), split(trimmed));
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = b"../".repeat(from.len() - common);
    let rest = to[common..].join(&b'/');
    if rest.is_empty() {
        if out.is_empty() {
            out.extend_from_slice(b"./");
        }
        return out;
    }
    out.extend_from_slice(&rest);
    if is_dir {
        out.push(b'/');
    }
    out
}

/// git's C-style quoting of a path, applied when it holds a space, a control character, a quote, a backslash or (with
/// `quote_high`, git's `core.quotePath`) a byte above ASCII.
pub fn quote(path: &[u8], quote_high: bool) -> String {
    quote_path(path, quote_high, true)
}

/// Git's diff path quoting, which leaves spaces unquoted.
pub fn quote_diff(path: &[u8], quote_high: bool) -> String {
    quote_path(path, quote_high, false)
}

fn quote_path(path: &[u8], quote_high: bool, quote_spaces: bool) -> String {
    let needs = |b: u8| {
        b < 0x20
            || b == b'"'
            || b == b'\\'
            || (quote_spaces && b == b' ')
            || b == 0x7f
            || (quote_high && b >= 0x80)
    };
    if !path.iter().any(|b| needs(*b)) {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut out = vec![b'"'];
    for &b in path {
        match b {
            7 => out.extend_from_slice(b"\\a"),
            8 => out.extend_from_slice(b"\\b"),
            9 => out.extend_from_slice(b"\\t"),
            10 => out.extend_from_slice(b"\\n"),
            11 => out.extend_from_slice(b"\\v"),
            12 => out.extend_from_slice(b"\\f"),
            13 => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b if b < 0x20 || b == 0x7f || (quote_high && b >= 0x80) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            b => out.push(b),
        }
    }
    out.push(b'"');
    String::from_utf8_lossy(&out).into_owned()
}

/// Prints the lines: changes first in the order given, untracked paths last, as git does. `cwd_prefix` makes
/// the paths relative to where the command ran (`--short` only; porcelain stays from the root).
pub fn print(
    rows: &[Row],
    branch: Option<&str>,
    format: Format,
    cwd_prefix: &[u8],
    quote_high: bool,
) -> Result<()> {
    let mut out = String::new();
    if let Some(line) = branch {
        out.push_str(&format!("## {line}\n"));
    }
    let (untracked, changed): (Vec<&Row>, Vec<&Row>) =
        rows.iter().partition(|r| r.x == '?' && r.y == '?');
    for r in changed.into_iter().chain(untracked) {
        let shown = |p: &[u8]| {
            let p = if format == Format::Short {
                relative(cwd_prefix, p)
            } else {
                p.to_vec()
            };
            quote(&p, quote_high)
        };
        let name = match &r.from {
            Some(from) => format!("{} -> {}", shown(from), shown(&r.path)),
            None => shown(&r.path),
        };
        out.push_str(&format!("{}{} {name}\n", r.x, r.y));
    }
    std::io::stdout().write_all(out.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_relative_to_the_folder_you_stand_in() {
        assert_eq!(relative(b"d", b"a"), b"../a");
        assert_eq!(relative(b"d", b"d/b"), b"b");
        assert_eq!(relative(b"d", b"d/"), b"./");
        assert_eq!(relative(b"d", b"ud/"), b"../ud/");
        assert_eq!(relative(b"a/b", b"a/c/x"), b"../c/x");
        assert_eq!(relative(b"", b"a/b"), b"a/b");
    }

    #[test]
    fn quoting_follows_git() {
        assert_eq!(quote(b"plain/x.txt", true), "plain/x.txt");
        assert_eq!(quote(b"a b", true), "\"a b\"");
        assert_eq!(quote(b"t\tq\"", true), "\"t\\tq\\\"\"");
        assert_eq!(quote("\u{e9}".as_bytes(), true), "\"\\303\\251\"");
        assert_eq!(quote("\u{e9}".as_bytes(), false), "\u{e9}");
    }
}
