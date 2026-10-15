//! `pyn tag` on a git repository: lightweight and annotated tags, listing and deleting, as git writes them.

use anyhow::{Result, bail};
use clap::Args;
use globset::Glob;
use pyn_git::Expect;
use pyn_object::{Kind, ObjectId, Tag};

use super::GitHome;
use super::refwrite::TAGS;
use super::stage::{clean_message, identity};

const ABBREV: usize = 7;

/// Create, list and delete tags.
#[derive(Debug, Args)]
pub struct TagArgs {
    /// `<name> [<commit>]` creates a tag; with -d the tags to delete; with -l the patterns to list.
    names: Vec<String>,
    /// Make an annotated tag, which has a message and a tagger (implied by -m).
    #[arg(short = 'a', long)]
    annotate: bool,
    /// The tag message; repeat for more paragraphs.
    #[arg(short = 'm', long = "message")]
    message: Vec<String>,
    /// Delete the named tags.
    #[arg(short = 'd', long)]
    delete: bool,
    /// List tags, all or those matching the patterns (the default with no arguments).
    #[arg(short = 'l', long)]
    list: bool,
    /// Replace a tag that exists.
    #[arg(short = 'f', long)]
    force: bool,
}

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

fn tag_ref(name: &str) -> String {
    format!("{TAGS}{name}")
}

fn list(git: &GitHome, patterns: &[String]) -> Result<()> {
    let matchers: Vec<_> = patterns
        .iter()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<_, _>>()?;
    for (full, _) in git.repo.refs().list(TAGS)? {
        let name = &full[TAGS.len()..];
        if matchers.is_empty() || matchers.iter().any(|m| m.is_match(name)) {
            println!("{name}");
        }
    }
    Ok(())
}

fn delete(git: &GitHome, names: &[String]) -> Result<()> {
    if names.is_empty() {
        bail!("tag name required");
    }
    let mut failed = false;
    for name in names {
        let full = tag_ref(name);
        let id = if pyn_git::check_ref_name(&full).is_ok() {
            git.repo.refs().resolve(&full)?
        } else {
            None
        };
        let Some(id) = id else {
            eprintln!("error: tag '{name}' not found.");
            failed = true;
            continue;
        };
        git.repo.refs().delete(&full, Expect::Id(id))?;
        println!("Deleted tag '{name}' (was {})", short(&id));
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn create(git: &GitHome, args: &TagArgs, name: &str, target: Option<&str>) -> Result<()> {
    let full = tag_ref(name);
    if name.starts_with('-') || name == "HEAD" || pyn_git::check_ref_name(&full).is_err() {
        bail!("'{name}' is not a valid tag name.");
    }
    let old = git.repo.refs().resolve(&full)?;
    if old.is_some() && !args.force {
        bail!("tag '{name}' already exists");
    }
    if git.repo.bool_setting("tag.gpgsign", false) {
        bail!("tag.gpgsign is set and pyn cannot sign tags yet; unset it to tag with pyn");
    }
    let object = match target {
        Some(spec) => match git.repo.refs().resolve(&tag_ref(spec)) {
            Ok(Some(id)) if pyn_git::check_ref_name(&tag_ref(spec)).is_ok() => id,
            _ => git.repo.resolve(spec)?,
        },
        None => match git.repo.head_commit()? {
            Some(id) => id,
            None => bail!("Failed to resolve 'HEAD' as a valid ref."),
        },
    };
    let annotated = args.annotate || !args.message.is_empty();
    let id = if annotated {
        if args.message.is_empty() {
            bail!("no tag message; give one with -m (pyn has no editor)");
        }
        let kind = git.repo.objects().read(&object)?.kind;
        let tag = Tag {
            object,
            kind,
            name: name.as_bytes().to_vec(),
            tagger: Some(identity(git, "committer")?),
            message: clean_message(&args.message),
        };
        git.repo
            .objects()
            .write(Kind::Tag, &tag.encode(git.algorithm())?)?
    } else {
        object
    };
    let expect = old.map_or(Expect::Absent, Expect::Id);
    git.repo.refs().update_checked(&full, expect, &id)?;
    if let Some(old) = old {
        println!("Updated tag '{name}' (was {})", short(&old));
    }
    Ok(())
}

pub fn tag(git: &GitHome, args: TagArgs) -> Result<()> {
    if args.delete {
        return delete(git, &args.names);
    }
    if args.list || args.names.is_empty() {
        return list(git, &args.names);
    }
    match args.names.as_slice() {
        [name] => create(git, &args, name, None),
        [name, target] => create(git, &args, name, Some(target)),
        _ => bail!("too many arguments to create a tag; the usage is `pyn tag <name> [<commit>]`"),
    }
}
