//! `pyn show-ref` on a git repository: the refs and the objects they point at.

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::plumbing::{emit_bytes, kind_of, peel, usage};

/// List the refs of the repository with the ids they point at.
#[derive(Debug, Args)]
pub struct ShowRefArgs {
    /// Show `HEAD` too, if it resolves.
    #[arg(long)]
    head: bool,
    /// Show only refs under `refs/tags/`.
    #[arg(long)]
    tags: bool,
    /// Show only refs under `refs/heads/`.
    #[arg(long)]
    heads: bool,
    /// Also show what an annotated tag points at, as `<ref>^{}`.
    #[arg(short = 'd', long)]
    dereference: bool,
    /// Show only the ids, abbreviated to at least this many digits when one is given.
    #[arg(short = 's', long = "hash", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    hash: Option<String>,
    /// Abbreviate the ids to at least this many digits (7 if omitted).
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
    abbrev: Option<String>,
    /// Require the exact, full ref names given, and fail if one is missing.
    #[arg(long)]
    verify: bool,
    /// Exit with 0 if the one ref given exists, 2 if not.
    #[arg(long)]
    exists: bool,
    /// Print nothing; the exit status tells whether a ref matched.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Show only refs whose name ends with one of these, on a `/` boundary.
    patterns: Vec<String>,
}

struct Show<'a> {
    git: &'a GitHome,
    args: &'a ShowRefArgs,
    abbreviations: Option<pyn_gitwork::Abbreviations>,
    out: Vec<u8>,
}

pub(super) fn show_ref(git: &GitHome, args: ShowRefArgs) -> Result<()> {
    let digits = |v: &String| match v.parse::<usize>() {
        Ok(n) => git.repo.abbreviations(Some(n)),
        Err(_) if v.is_empty() => git.repo.abbreviations(None),
        Err(_) => usage(&format!(
            "option `abbrev' expects a numerical value, got '{v}'"
        )),
    };
    let abbreviations = match (&args.abbrev, &args.hash) {
        (Some(v), _) => Some(digits(v)?),
        (None, Some(v)) if !v.is_empty() => Some(digits(v)?),
        _ => None,
    };
    if args.exists {
        return exists(git, &args);
    }
    let mut show = Show {
        git,
        args: &args,
        abbreviations,
        out: Vec::new(),
    };
    let result = if args.verify {
        show.verify()
    } else {
        show.list()
    };
    emit_bytes(&show.out);
    match result? {
        true => Ok(()),
        false => std::process::exit(1),
    }
}

fn exists(git: &GitHome, args: &ShowRefArgs) -> Result<()> {
    let [name] = args.patterns.as_slice() else {
        if args.patterns.is_empty() {
            bail!("--exists requires a reference");
        }
        bail!("--exists requires exactly one reference");
    };
    if git.repo.refs().read(name).ok().flatten().is_some() {
        return Ok(());
    }
    eprintln!("error: reference does not exist");
    std::process::exit(2);
}

impl Show<'_> {
    fn hex(&self, id: &ObjectId) -> String {
        match &self.abbreviations {
            Some(a) => a.short(id),
            None => id.to_hex(),
        }
    }

    fn one(&mut self, name: &str, id: &ObjectId) {
        if self.args.quiet {
            return;
        }
        let hex = self.hex(id);
        let line = if self.args.hash.is_some() {
            format!("{hex}\n")
        } else {
            format!("{hex} {name}\n")
        };
        self.out.extend_from_slice(line.as_bytes());
    }

    fn shown(&mut self, name: &str, id: &ObjectId) -> Result<()> {
        self.one(name, id);
        if self.args.dereference
            && !self.args.quiet
            && kind_of(self.git, id)? == Some(Kind::Tag)
            && let Some(target) = peel(self.git, id, None)?
        {
            let line = format!("{} {name}^{{}}\n", self.hex(&target));
            self.out.extend_from_slice(line.as_bytes());
        }
        Ok(())
    }

    fn wanted(&self, name: &str) -> bool {
        let a = self.args;
        if (a.tags || a.heads)
            && !((a.heads && name.starts_with("refs/heads/"))
                || (a.tags && name.starts_with("refs/tags/")))
        {
            return false;
        }
        a.patterns.is_empty()
            || a.patterns.iter().any(|p| {
                name == p
                    || name
                        .strip_suffix(p.as_str())
                        .is_some_and(|rest| rest.ends_with('/'))
            })
    }

    fn list(&mut self) -> Result<bool> {
        let mut found = false;
        if self.args.head
            && let Some(id) = self.git.repo.refs().resolve("HEAD")?
        {
            found = true;
            self.shown("HEAD", &id)?;
        }
        for (name, id) in self.git.repo.refs().list("refs/")? {
            if self.wanted(&name) {
                found = true;
                self.shown(&name, &id)?;
            }
        }
        Ok(found)
    }

    fn verify(&mut self) -> Result<bool> {
        if self.args.patterns.is_empty() {
            bail!("--verify requires a reference");
        }
        for name in self.args.patterns.clone() {
            let id = if name.starts_with("refs/") || name == "HEAD" {
                self.git.repo.refs().resolve(&name).ok().flatten()
            } else {
                None
            };
            match id {
                Some(id) => self.shown(&name, &id)?,
                None if self.args.quiet => return Ok(false),
                None => bail!("'{name}' - not a valid ref"),
            }
        }
        Ok(true)
    }
}
