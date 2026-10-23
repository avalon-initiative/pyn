//! `pyn tag` on a git repository: lightweight and annotated tags, listing and deleting, as git writes them.

use anyhow::{Result, bail};
use clap::Args;
use globset::Glob;
use pyn_git::Expect;
use pyn_gitwork::Repo;
use pyn_object::{Kind, ObjectId, Signature, Tag};

use super::GitHome;
use super::refwrite::TAGS;
use super::signing::{SignFlag, signer_for};
use super::stage::{clean_message, identity};

const ABBREV: usize = 7;

/// Create, list and delete tags. On a pyn server they live in the local store and are never pushed.
#[derive(Debug, Args)]
pub struct TagArgs {
    /// `<name> [<commit>]` creates a tag; with -d the tags to delete; with -l the patterns to list.
    pub(crate) names: Vec<String>,
    /// Make an annotated tag, which has a message and a tagger (implied by -m).
    #[arg(short = 'a', long)]
    pub(crate) annotate: bool,
    /// The tag message; repeat for more paragraphs.
    #[arg(short = 'm', long = "message")]
    pub(crate) message: Vec<String>,
    /// Delete the named tags.
    #[arg(short = 'd', long)]
    pub(crate) delete: bool,
    /// List tags, all or those matching the patterns (the default with no arguments).
    #[arg(short = 'l', long)]
    pub(crate) list: bool,
    /// Replace a tag that exists.
    #[arg(short = 'f', long)]
    pub(crate) force: bool,
    /// Make a signed annotated tag with `user.signingkey`.
    #[arg(short = 's', long, overrides_with_all = ["local_user", "no_sign"])]
    pub(crate) sign: bool,
    /// Make a signed annotated tag with this key id.
    #[arg(short = 'u', long, value_name = "KEYID", overrides_with_all = ["sign", "no_sign"])]
    pub(crate) local_user: Option<String>,
    /// Do not sign, whatever `tag.gpgsign` says.
    #[arg(long, overrides_with_all = ["sign", "local_user"])]
    pub(crate) no_sign: bool,
}

pub(crate) fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub(crate) fn tag_ref(name: &str) -> String {
    format!("{TAGS}{name}")
}

pub(crate) fn list(repo: &Repo, patterns: &[String]) -> Result<()> {
    let matchers: Vec<_> = patterns
        .iter()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<_, _>>()?;
    for (full, _) in repo.refs().list(TAGS)? {
        let name = &full[TAGS.len()..];
        if matchers.is_empty() || matchers.iter().any(|m| m.is_match(name)) {
            println!("{name}");
        }
    }
    Ok(())
}

pub(crate) fn delete(repo: &Repo, names: &[String]) -> Result<()> {
    if names.is_empty() {
        bail!("tag name required");
    }
    let mut failed = false;
    for name in names {
        let full = tag_ref(name);
        let id = if pyn_git::check_ref_name(&full).is_ok() {
            repo.refs().resolve(&full)?
        } else {
            None
        };
        let Some(id) = id else {
            eprintln!("error: tag '{name}' not found.");
            failed = true;
            continue;
        };
        repo.refs().delete(&full, Expect::Id(id))?;
        repo.refs().write_reflog(&full, &[])?;
        println!("Deleted tag '{name}' (was {})", short(&id));
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// The reflog line git writes for a tag: `tag: tagging <abbreviation> (<subject>, <date>)`.
fn tag_log(git: &GitHome, object: &ObjectId) -> Result<String> {
    let what = match git.repo.objects().read(object)?.kind {
        Kind::Commit => {
            let commit = git.repo.commit(object)?;
            let subject = super::switch::subject(git, object)?;
            let date = chrono::DateTime::from_timestamp(commit.committer.time, 0)
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            format!("{subject}, {date}")
        }
        Kind::Tag => "tag".to_string(),
        Kind::Tree => "tree".to_string(),
        Kind::Blob => "blob".to_string(),
    };
    Ok(format!("tag: tagging {} ({what})", short(object)))
}

/// The full ref of a tag, refusing names git refuses.
pub(crate) fn check_name(name: &str) -> Result<String> {
    let full = tag_ref(name);
    if name.starts_with('-') || name == "HEAD" || pyn_git::check_ref_name(&full).is_err() {
        bail!("'{name}' is not a valid tag name.");
    }
    Ok(full)
}

/// The unsigned tag object for `object`, with the message cleaned as git does.
pub(crate) fn annotation(
    repo: &Repo,
    name: &str,
    object: ObjectId,
    tagger: Signature,
    messages: &[String],
) -> Result<Tag> {
    if messages.is_empty() {
        bail!("no tag message; give one with -m (pyn has no editor)");
    }
    Ok(Tag {
        object,
        kind: repo.objects().read(&object)?.kind,
        name: name.as_bytes().to_vec(),
        tagger: Some(tagger),
        message: clean_message(messages),
    })
}

fn create(git: &GitHome, args: &TagArgs, name: &str, target: Option<&str>) -> Result<()> {
    let full = check_name(name)?;
    let old = git.repo.refs().resolve(&full)?;
    if old.is_some() && !args.force {
        bail!("tag '{name}' already exists");
    }
    let tagger = identity(git, "committer").ok();
    let flag = SignFlag {
        gpg_sign: match (&args.local_user, args.sign) {
            (Some(key), _) => Some(Some(key.clone())),
            (None, true) => Some(None),
            _ => None,
        },
        no_gpg_sign: args.no_sign,
    };
    let signer = signer_for(git, &flag, "tag.gpgsign", tagger.as_ref())?;
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
    let annotated = args.annotate || !args.message.is_empty() || signer.is_some();
    let id = if annotated {
        let mut tag = annotation(
            &git.repo,
            name,
            object,
            identity(git, "committer")?,
            &args.message,
        )?;
        if let Some(signer) = &signer {
            let sig = signer.sign(&tag.encode(git.algorithm())?)?;
            tag.message.extend_from_slice(&sig);
        }
        git.repo
            .objects()
            .write(Kind::Tag, &tag.encode(git.algorithm())?)?
    } else {
        object
    };
    let expect = old.map_or(Expect::Absent, Expect::Id);
    git.repo.refs().update_checked(&full, expect, &id)?;
    git.log_ref(&full, old.as_ref(), &id, &tag_log(git, &object)?, false)?;
    if let Some(old) = old {
        println!("Updated tag '{name}' (was {})", short(&old));
    }
    Ok(())
}

pub fn tag(git: &GitHome, args: TagArgs) -> Result<()> {
    if args.delete {
        return delete(&git.repo, &args.names);
    }
    if args.list || args.names.is_empty() {
        return list(&git.repo, &args.names);
    }
    match args.names.as_slice() {
        [name] => create(git, &args, name, None),
        [name, target] => create(git, &args, name, Some(target)),
        _ => bail!("too many arguments to create a tag; the usage is `pyn tag <name> [<commit>]`"),
    }
}
