//! `pyn bundle`: create, verify, list and unpack git bundle files; clone and fetch read them as remotes.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use globset::Glob;
use pyn_git::{Bundle, IngestLimits, Prerequisite, index_pack, reachable_objects, write_pack};
use pyn_gittransport::RemoteUrl;
use pyn_object::{Algorithm, ObjectId};

use super::GitHome;
use super::plumbing::emit_bytes;

/// Create, verify and unpack git bundle files.
#[derive(Debug, Args)]
pub struct BundleArgs {
    #[command(subcommand)]
    command: BundleCommand,
}

#[derive(Debug, Subcommand)]
enum BundleCommand {
    /// Write a bundle of the commits and refs the revision arguments select.
    Create {
        /// Bundle format version: 2, or 3 (the default for SHA-256 repositories).
        #[arg(long)]
        version: Option<u8>,
        /// Accepted for compatibility; nothing is reported either way.
        #[arg(short, long)]
        quiet: bool,
        /// The bundle to write, or `-` for standard output.
        file: String,
        /// Refs and revisions: `--all`, `--branches`, `--tags`, `--remotes`, `^<rev>`, `--not`, `<a>..<b>`, ref names.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        revisions: Vec<String>,
    },
    /// Check that a bundle is valid and that this repository has what it requires.
    Verify {
        /// Accepted for compatibility.
        #[arg(short, long)]
        quiet: bool,
        file: PathBuf,
    },
    /// List the refs a bundle holds.
    ListHeads {
        file: PathBuf,
        /// Only these refs.
        refnames: Vec<String>,
    },
    /// Store a bundle's objects in this repository and list its refs.
    Unbundle {
        file: PathBuf,
        /// Only these refs are listed.
        refnames: Vec<String>,
    },
}

pub fn bundle(args: BundleArgs, git: Option<&GitHome>) -> Result<()> {
    match args.command {
        BundleCommand::Create {
            version,
            file,
            revisions,
            ..
        } => create(
            need_repo(git, "Need a repository to create a bundle.")?,
            version,
            &file,
            &revisions,
        ),
        BundleCommand::Verify { file, .. } => verify(
            need_repo(git, "Need a repository to verify a bundle.")?,
            &file,
        ),
        BundleCommand::ListHeads { file, refnames } => list_heads(&file, &refnames),
        BundleCommand::Unbundle { file, refnames } => unbundle(
            need_repo(git, "Need a repository to unbundle.")?,
            &file,
            &refnames,
        ),
    }
}

fn need_repo<'a>(git: Option<&'a GitHome>, message: &str) -> Result<&'a GitHome> {
    git.ok_or_else(|| anyhow!("{message}"))
}

fn open(file: &Path) -> Result<Bundle> {
    Bundle::open(file).map_err(|e| match e {
        pyn_git::Error::Io { .. } => {
            eprintln!("error: could not open '{}'", file.display());
            std::process::exit(1);
        }
        e => anyhow!(
            "'{}' does not look like a v2 or v3 bundle file: {e}",
            file.display()
        ),
    })
}

fn plural(n: usize, one: &str, many: &str) -> String {
    match n {
        1 => format!("this {one}"),
        n => format!("these {n} {many}"),
    }
}

fn refs_text(bundle: &Bundle, names: &[String]) -> String {
    let matches = |name: &str| {
        names.is_empty()
            || names.iter().any(|n| {
                name == n
                    || ["refs/", "refs/heads/", "refs/tags/"]
                        .iter()
                        .any(|p| name == format!("{p}{n}"))
            })
    };
    bundle
        .refs
        .iter()
        .filter(|(name, _)| matches(name))
        .map(|(name, id)| format!("{id} {name}\n"))
        .collect()
}

fn list_heads(file: &Path, names: &[String]) -> Result<()> {
    emit_bytes(refs_text(&open(file)?, names).as_bytes());
    Ok(())
}

/// Reports the prerequisite commits the repository lacks, as git does; true when there are none.
fn prerequisites_held(git: &GitHome, bundle: &Bundle) -> Result<bool> {
    let missing = bundle.missing(git.repo.objects())?;
    if !missing.is_empty() {
        eprintln!("error: Repository lacks these prerequisite commits:");
    }
    for id in &missing {
        eprintln!("error: {id} ");
    }
    Ok(missing.is_empty())
}

fn require_prerequisites(git: &GitHome, bundle: &Bundle) -> Result<()> {
    if !prerequisites_held(git, bundle)? {
        std::process::exit(1);
    }
    Ok(())
}

/// Refuses a bundle remote when this repository lacks its prerequisites: exit 1 for a fetch, a failed clone for `clone`.
pub(super) fn check_remote(git: &GitHome, url: &str, clone: bool) -> Result<()> {
    let Ok(RemoteUrl::Local { path, .. }) = RemoteUrl::parse(url) else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }
    if prerequisites_held(git, &open(&path)?)? {
        return Ok(());
    }
    if clone {
        bail!("remote transport reported error");
    }
    std::process::exit(1);
}

fn verify(git: &GitHome, file: &Path) -> Result<()> {
    let bundle = open(file)?;
    if bundle.algorithm != git.algorithm() {
        bail!(
            "the bundle uses {} but this repository uses {}",
            bundle.algorithm.name(),
            git.algorithm().name()
        );
    }
    require_prerequisites(git, &bundle)?;
    bundle
        .check_pack(file)
        .map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let mut out = format!(
        "The bundle contains {}:\n{}",
        plural(bundle.refs.len(), "ref", "refs"),
        refs_text(&bundle, &[])
    );
    if bundle.prerequisites.is_empty() {
        out.push_str("The bundle records a complete history.\n");
    } else {
        out.push_str(&format!(
            "The bundle requires {}:\n",
            plural(bundle.prerequisites.len(), "ref", "refs")
        ));
        for p in &bundle.prerequisites {
            out.push_str(&format!("{} \n", p.id));
        }
    }
    out.push_str(&format!(
        "The bundle uses this hash algorithm: {}\n",
        bundle.algorithm.name()
    ));
    emit_bytes(out.as_bytes());
    eprintln!("{} is okay", file.display());
    Ok(())
}

fn unbundle(git: &GitHome, file: &Path, names: &[String]) -> Result<()> {
    let bundle = open(file)?;
    if bundle.algorithm != git.algorithm() {
        bail!(
            "the bundle uses {} but this repository uses {}",
            bundle.algorithm.name(),
            git.algorithm().name()
        );
    }
    if bundle.filter.is_some() {
        bail!("bundles with a filter are not supported");
    }
    require_prerequisites(git, &bundle)?;
    let limits = IngestLimits::from_config(git.repo.config())?;
    let mut pack = std::io::BufReader::new(bundle.pack(file)?);
    index_pack(git.repo.objects(), &mut pack, &limits)
        .with_context(|| format!("unpacking '{}'", file.display()))?;
    emit_bytes(refs_text(&bundle, names).as_bytes());
    Ok(())
}

/// What the revision arguments select: tips to include, tips to exclude, and the refs to record.
#[derive(Default)]
struct Selection {
    include: Vec<ObjectId>,
    exclude: Vec<ObjectId>,
    refs: Vec<(String, ObjectId)>,
}

impl Selection {
    fn add(&mut self, negated: bool, name: Option<&str>, id: ObjectId) {
        if negated {
            self.exclude.push(id);
            return;
        }
        self.include.push(id);
        if let Some(n) = name
            && !self.refs.iter().any(|(r, _)| r == n)
        {
            self.refs.push((n.to_owned(), id));
        }
    }

    fn add_all(
        &mut self,
        git: &GitHome,
        negated: bool,
        prefix: &str,
        glob: Option<&str>,
    ) -> Result<()> {
        let matcher = glob
            .map(|g| Glob::new(g).map(|g| g.compile_matcher()))
            .transpose()?;
        for (name, id) in git.repo.refs().list(prefix)? {
            let short = name.strip_prefix(prefix).unwrap_or(&name);
            if matcher.as_ref().is_none_or(|m| m.is_match(short)) {
                self.add(negated, Some(&name), id);
            }
        }
        Ok(())
    }

    fn add_rev(&mut self, git: &GitHome, negated: bool, spec: &str) -> Result<()> {
        if let Some(name) = git.repo.expand_ref_name(spec)?
            && let Some(id) = git.repo.refs().resolve(&name)?
        {
            self.add(negated, Some(&name), id);
            return Ok(());
        }
        let id = git.repo.resolve(spec).map_err(|_| unknown_revision(spec))?;
        self.add(negated, None, id);
        Ok(())
    }
}

fn unknown_revision(spec: &str) -> anyhow::Error {
    anyhow!(
        "ambiguous argument '{spec}': unknown revision or path not in the working tree.\n\
         Use '--' to separate paths from revisions, like this:\n\
         'git <command> [<revision>...] -- [<file>...]'"
    )
}

fn select(git: &GitHome, args: &[String]) -> Result<Selection> {
    let mut sel = Selection::default();
    let mut negated = false;
    for arg in args {
        let (flag, value) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--not" => negated = !negated,
            "--all" => {
                sel.add_all(git, negated, "refs/", None)?;
                if let Some(id) = git.repo.refs().resolve("HEAD")? {
                    sel.add(negated, Some("HEAD"), id);
                }
            }
            "--branches" => sel.add_all(git, negated, "refs/heads/", value)?,
            "--tags" => sel.add_all(git, negated, "refs/tags/", value)?,
            "--remotes" => sel.add_all(git, negated, "refs/remotes/", value)?,
            f if f.starts_with('-') && f.len() > 1 => {
                eprintln!("error: unrecognized argument: {arg}");
                std::process::exit(1);
            }
            _ => {
                if let Some(rev) = arg.strip_prefix('^') {
                    sel.add_rev(git, !negated, rev)?;
                } else if let Some((a, b)) = arg.split_once("...") {
                    let _ = (a, b);
                    bail!("symmetric ranges ('{arg}') are not supported in a bundle");
                } else if let Some((a, b)) = arg.split_once("..") {
                    sel.add_rev(git, !negated, if a.is_empty() { "HEAD" } else { a })?;
                    sel.add_rev(git, negated, if b.is_empty() { "HEAD" } else { b })?;
                } else {
                    sel.add_rev(git, negated, arg)?;
                }
            }
        }
    }
    Ok(sel)
}

fn subject(git: &GitHome, id: &ObjectId) -> String {
    git.repo
        .commit(id)
        .map(|c| {
            String::from_utf8_lossy(c.message.split(|b| *b == b'\n').next().unwrap_or_default())
                .into_owned()
        })
        .unwrap_or_default()
}

fn create(git: &GitHome, version: Option<u8>, file: &str, revisions: &[String]) -> Result<()> {
    let algorithm = git.algorithm();
    let version = match (version, algorithm) {
        (Some(v @ (2 | 3)), Algorithm::Sha1) | (Some(v @ 3), _) => v,
        (Some(2), _) => bail!(
            "cannot write bundle version 2 with algorithm {}",
            algorithm.name()
        ),
        (Some(v), _) => bail!("unsupported bundle version {v}"),
        (None, Algorithm::Sha1) => 2,
        (None, _) => 3,
    };
    let sel = select(git, revisions)?;
    let db = git.repo.objects();
    let exclude = pyn_git::existing(db, &sel.exclude)?;
    let reachable = if sel.include.is_empty() {
        Default::default()
    } else {
        reachable_objects(db, &sel.include, &exclude)?
    };
    let included: HashSet<ObjectId> = reachable.objects.iter().copied().collect();
    let refs: Vec<(String, ObjectId)> = sel
        .refs
        .into_iter()
        .filter(|(_, id)| included.contains(id))
        .collect();
    if refs.is_empty() || reachable.objects.is_empty() {
        bail!("Refusing to create empty bundle.");
    }
    let prerequisites: Vec<Prerequisite> = reachable
        .boundary
        .iter()
        .map(|id| Prerequisite {
            id: *id,
            comment: subject(git, id),
        })
        .collect();
    let write = |out: &mut dyn Write| -> Result<()> {
        let mut out = BufWriter::new(out);
        pyn_git::write_bundle_header(&mut out, version, algorithm, &prerequisites, &refs)?;
        write_pack(db, &reachable.objects, &mut out)?;
        out.flush()?;
        Ok(())
    };
    if file == "-" {
        return write(&mut std::io::stdout().lock());
    }
    let mut out = File::create(file).with_context(|| format!("cannot create '{file}'"))?;
    write(&mut out).inspect_err(|_| {
        let _ = std::fs::remove_file(file);
    })
}
