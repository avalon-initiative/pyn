//! `pyn rev-parse` on a git repository: revisions to object ids, and facts about the repository's layout.

use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::plumbing::{Resolver, dwim, emit_bytes, kind_of, peel, prefix, shorten_ref};

/// Print the object ids revisions name, and facts about the repository, in the order given.
#[derive(Debug, Args)]
pub struct RevParseArgs {
    /// Options and revisions, handled in order: `--verify`, `--short[=<n>]`, `--abbrev-ref[=strict|loose]`,
    /// `--symbolic`, `--symbolic-full-name`, `-q`, `--git-dir`, `--absolute-git-dir`, `--git-common-dir`,
    /// `--show-toplevel`, `--show-prefix`, `--show-cdup`, `--is-inside-work-tree`, `--is-inside-git-dir`,
    /// `--is-bare-repository`, `--is-shallow-repository`, `--show-object-format`, `--all`, `--branches`, `--tags`, `--remotes`, then
    /// `<rev>`, `^<rev>`, `<a>..<b>`, `<a>...<b>`, `<rev>^@`, `<rev>^!`, `<tree-ish>:<path>`, `:<path>`.
    #[arg(
        allow_hyphen_values = true,
        trailing_var_arg = true,
        value_name = "ARG"
    )]
    args: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Symbolic {
    Off,
    AsIs,
    Full,
}

struct State {
    verify: bool,
    quiet: bool,
    symbolic: Symbolic,
    abbrev_ref: Option<bool>,
    short: Option<Option<usize>>,
    out: Vec<u8>,
    verified: Vec<(ObjectId, String)>,
    as_files: bool,
}

pub(super) fn rev_parse(git: &GitHome, cwd: &Path, args: RevParseArgs) -> Result<()> {
    let mut st = State {
        verify: false,
        quiet: false,
        symbolic: Symbolic::Off,
        abbrev_ref: None,
        short: None,
        out: Vec::new(),
        verified: Vec::new(),
        as_files: false,
    };
    let result = run(git, cwd, &args.args, &mut st);
    emit_bytes(&st.out);
    result
}

fn run(git: &GitHome, cwd: &Path, args: &[String], st: &mut State) -> Result<()> {
    let pre = prefix(git, cwd).unwrap_or_default();
    for arg in args {
        if st.as_files {
            file(cwd, arg, st)?;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            option(git, cwd, &pre, arg, st)?;
            continue;
        }
        revision(git, cwd, &pre, arg, st)?;
    }
    if st.verify {
        match st.verified.len() {
            1 => {
                let (id, name) = st.verified.remove(0);
                show(git, &id, &name, false, st)?;
            }
            _ if st.quiet => std::process::exit(1),
            _ => bail!("Needed a single revision"),
        }
    }
    Ok(())
}

fn line(st: &mut State, text: &str) {
    st.out.extend_from_slice(text.as_bytes());
    st.out.push(b'\n');
}

fn option(git: &GitHome, cwd: &Path, pre: &[u8], arg: &str, st: &mut State) -> Result<()> {
    let git_dir = || git.repo.git().git_dir();
    let work_tree = || git.repo.git().work_tree();
    match arg {
        "--verify" => st.verify = true,
        "-q" | "--quiet" => st.quiet = true,
        "--symbolic" => st.symbolic = Symbolic::AsIs,
        "--symbolic-full-name" => st.symbolic = Symbolic::Full,
        "--short" => st.short = Some(None),
        "--abbrev-ref" | "--abbrev-ref=strict" => st.abbrev_ref = Some(true),
        "--abbrev-ref=loose" => st.abbrev_ref = Some(false),
        "--git-dir" => {
            let at_top = work_tree().is_some_and(|w| cwd == w);
            let text = if cwd == git_dir() {
                ".".to_string()
            } else if at_top && git_dir() == work_tree().unwrap_or(cwd).join(".git") {
                ".git".to_string()
            } else {
                absolute(git_dir())
            };
            line(st, &text);
        }
        "--absolute-git-dir" => line(st, &absolute(git_dir())),
        "--git-common-dir" => {
            let common = git.repo.git().common_dir();
            let at_top = work_tree().is_some_and(|w| cwd == w);
            let text = if at_top && common == work_tree().unwrap_or(cwd).join(".git") {
                ".git".to_string()
            } else {
                absolute(common)
            };
            line(st, &text);
        }
        "--show-toplevel" => match work_tree() {
            Some(w) if !inside(cwd, git_dir()) => line(st, &absolute(w)),
            _ => bail!("this operation must be run in a work tree"),
        },
        "--show-prefix" => {
            let text = if inside(cwd, git_dir()) {
                String::new()
            } else {
                String::from_utf8_lossy(pre).into_owned()
            };
            line(st, &text);
        }
        "--show-cdup" => {
            let depth = if inside(cwd, git_dir()) {
                0
            } else {
                pre.iter().filter(|b| **b == b'/').count()
            };
            line(st, &"../".repeat(depth));
        }
        "--is-inside-work-tree" => {
            let yes = work_tree().is_some() && !inside(cwd, git_dir());
            line(st, if yes { "true" } else { "false" });
        }
        "--is-inside-git-dir" => line(
            st,
            if inside(cwd, git_dir()) {
                "true"
            } else {
                "false"
            },
        ),
        "--is-bare-repository" => line(
            st,
            if work_tree().is_none() {
                "true"
            } else {
                "false"
            },
        ),
        "--is-shallow-repository" => line(
            st,
            if super::fetch::is_shallow(git)? {
                "true"
            } else {
                "false"
            },
        ),
        "--show-object-format" | "--show-object-format=storage" | "--show-object-format=output" => {
            line(st, git.algorithm().name());
        }
        "--all" => refs(git, "refs/", st)?,
        "--branches" => refs(git, "refs/heads/", st)?,
        "--tags" => refs(git, "refs/tags/", st)?,
        "--remotes" => refs(git, "refs/remotes/", st)?,
        _ => {
            if let Some(n) = arg.strip_prefix("--short=")
                && let Ok(n) = n.parse::<usize>()
            {
                st.short = Some(Some(n));
            } else {
                line(st, arg);
            }
        }
    }
    Ok(())
}

fn refs(git: &GitHome, root: &str, st: &mut State) -> Result<()> {
    for (_, id) in git.repo.refs().list(root)? {
        line(st, &id.to_hex());
    }
    Ok(())
}

fn inside(cwd: &Path, git_dir: &Path) -> bool {
    let git_dir = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
    cwd.starts_with(git_dir)
}

fn absolute(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// A name that is no revision is printed as given and must be a file in the work tree.
fn file(cwd: &Path, arg: &str, st: &mut State) -> Result<()> {
    if st.verify {
        bail!("Needed a single revision");
    }
    line(st, arg);
    if std::fs::symlink_metadata(cwd.join(arg)).is_err() {
        bail!(
            "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'"
        );
    }
    st.as_files = true;
    Ok(())
}

fn revision(git: &GitHome, cwd: &Path, pre: &[u8], arg: &str, st: &mut State) -> Result<()> {
    let resolver = Resolver { git, prefix: pre };
    if let Some((a, b)) = arg.split_once("...").filter(|_| !st.verify) {
        let (x, y) = (side(&resolver, git, a)?, side(&resolver, git, b)?);
        if let (Some(x), Some(y)) = (x, y) {
            show(git, &y, b, false, st)?;
            show(git, &x, a, false, st)?;
            for base in git.merge_bases(&x, &y)? {
                show(git, &base, "", true, st)?;
            }
            return Ok(());
        }
    } else if let Some((a, b)) = arg.split_once("..").filter(|_| !st.verify) {
        let (x, y) = (side(&resolver, git, a)?, side(&resolver, git, b)?);
        if let (Some(x), Some(y)) = (x, y) {
            show(git, &y, b, false, st)?;
            show(git, &x, a, true, st)?;
            return Ok(());
        }
    }
    if let Some(base) = arg.strip_suffix("^@")
        && let Some(id) = resolver.rev(base)?
        && let Some(commit) = peel(git, &id, Some(Kind::Commit))?
    {
        for p in git.repo.commit(&commit)?.parents {
            show(git, &p, "", false, st)?;
        }
        return Ok(());
    }
    if let Some(base) = arg.strip_suffix("^!")
        && let Some(id) = resolver.rev(base)?
        && let Some(commit) = peel(git, &id, Some(Kind::Commit))?
    {
        show(git, &id, base, false, st)?;
        for p in git.repo.commit(&commit)?.parents {
            show(git, &p, "", true, st)?;
        }
        return Ok(());
    }
    let (negated, name) = match arg.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, arg),
    };
    if let Some(id) = resolver.object(name)?
        && (!st.verify || kind_of(git, &id)?.is_some())
    {
        if st.verify {
            st.verified.push((id, name.to_string()));
        } else {
            show(git, &id, name, negated, st)?;
        }
        return Ok(());
    }
    if st.verify {
        if st.quiet {
            std::process::exit(1);
        }
        bail!("Needed a single revision");
    }
    file(cwd, arg, st)
}

fn side(resolver: &Resolver, git: &GitHome, text: &str) -> Result<Option<ObjectId>> {
    let text = if text.is_empty() { "HEAD" } else { text };
    Ok(match resolver.rev(text)? {
        Some(id) => peel(git, &id, Some(Kind::Commit))?,
        None => None,
    })
}

fn show(git: &GitHome, id: &ObjectId, name: &str, negated: bool, st: &mut State) -> Result<()> {
    let mut text = String::new();
    if negated {
        text.push('^');
    }
    let named = st.symbolic != Symbolic::Off || st.abbrev_ref.is_some();
    if named && !name.is_empty() {
        if st.symbolic == Symbolic::Full || st.abbrev_ref.is_some() {
            match dwim(git, name)? {
                Some(full) => match st.abbrev_ref {
                    Some(strict) => text.push_str(&shorten_ref(git, &full, strict)),
                    None => text.push_str(&full),
                },
                None => text.push_str(name),
            }
        } else {
            text.push_str(name);
        }
    } else if let Some(short) = st.short {
        text.push_str(&git.repo.abbreviations(short)?.short(id));
    } else {
        text.push_str(&id.to_hex());
    }
    line(st, &text);
    Ok(())
}
