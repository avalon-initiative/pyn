//! `pyn tag` on a pyn server workspace: tags are refs in the local store, never pushed, and `show` and `diff` resolve them.

use anyhow::{Result, bail};
use pyn_git::Expect;
use pyn_object::{Kind, Tag};

use crate::backend::Backend;
use crate::branch;
use crate::git_home::{self, TagArgs};
use crate::local_commit::signature;
use crate::local_store::LocalStore;
use crate::time;
use crate::workspace::Workspace;

/// A tag name resolved: the commit's id with any `~` and `^` steps kept, and the tag message of an annotated tag.
pub struct Named {
    pub rev: String,
    pub annotation: Option<String>,
}

const TAGS: &str = "refs/tags/";

fn open(ws: Option<&Workspace>) -> Result<LocalStore> {
    match ws.and_then(LocalStore::open) {
        Some(store) => Ok(store),
        None => bail!(
            "tags live in the local store, which this workspace does not have; turn it on with `pyn config set local_store true` and run `pyn update`"
        ),
    }
}

/// `HEAD` means the workspace branch's local tip here, not whatever the store's own HEAD names.
fn resolve_target(store: &LocalStore, branch: &str, spec: &str) -> Result<pyn_object::ObjectId> {
    let split = spec.find(['~', '^']).unwrap_or(spec.len());
    let spec = match &spec[..split] {
        "HEAD" | "@" => format!("refs/heads/{branch}{}", &spec[split..]),
        _ => spec.to_string(),
    };
    Ok(store.repo().resolve(&spec)?)
}

pub fn run(backend: &dyn Backend, ws: Option<&Workspace>, args: TagArgs) -> Result<()> {
    args.check_modes();
    if args.sign || args.local_user.is_some() {
        bail!("signed tags need a git repository; a pyn server workspace has no signing key");
    }
    let store = open(ws)?;
    if args.delete {
        return git_home::delete_tags(store.repo(), &args.names, None);
    }
    if args.list || args.names.is_empty() {
        return git_home::list_tags(store.repo(), &args.names);
    }
    let branch = branch::name(ws);
    match args.names.as_slice() {
        [name] => create(backend, &store, &branch, &args, name, None),
        [name, target] => create(backend, &store, &branch, &args, name, Some(target)),
        _ => bail!("too many arguments to create a tag; the usage is `pyn tag <name> [<commit>]`"),
    }
}

fn create(
    backend: &dyn Backend,
    store: &LocalStore,
    branch: &str,
    args: &TagArgs,
    name: &str,
    target: Option<&str>,
) -> Result<()> {
    let full = git_home::check_name(name)?;
    let refs = store.repo().refs();
    let old = refs.resolve(&full)?;
    if old.is_some() && !args.force {
        bail!("tag '{name}' already exists");
    }
    let object = match resolve_target(store, branch, target.unwrap_or("HEAD")) {
        Ok(id) => id,
        Err(_) if target.is_none() => bail!("Failed to resolve 'HEAD' as a valid ref."),
        Err(e) => return Err(e),
    };
    let id = if args.annotate || !args.message.is_empty() {
        let tagger = signature(&backend.whoami()?);
        let tag = git_home::annotation(store.repo(), name, object, tagger, &args.message)?;
        let body = tag.encode(crate::local_store::ALGORITHM)?;
        store.repo().objects().write(Kind::Tag, &body)?
    } else {
        object
    };
    refs.update_checked(&full, old.map_or(Expect::Absent, Expect::Id), &id)?;
    if let Some(old) = old {
        println!("Updated tag '{name}' (was {})", git_home::short(&old));
    }
    Ok(())
}

/// The commit a tag name leads, if `text` starts with a tag of the local store.
pub fn expand(store: &LocalStore, text: &str) -> Result<Option<Named>> {
    let split = text.find(['~', '^']).unwrap_or(text.len());
    let (base, steps) = text.split_at(split);
    if matches!(base, "" | "HEAD" | "@") || git_home::check_name(base).is_err() {
        return Ok(None);
    }
    let full = git_home::tag_ref(base);
    let Some(id) = store.repo().refs().resolve(&full)? else {
        return Ok(None);
    };
    let commit = store.repo().resolve(&full)?;
    let object = store.repo().objects().read(&id)?;
    let annotation = if steps.is_empty() && object.kind == Kind::Tag {
        let tag = Tag::parse(crate::local_store::ALGORITHM, &object.data)?;
        Some(describe(base, &tag))
    } else {
        None
    };
    Ok(Some(Named {
        rev: format!("{}{steps}", commit.to_hex()),
        annotation,
    }))
}

fn describe(name: &str, tag: &Tag) -> String {
    let mut out = format!("tag {name}\n");
    if let Some(t) = &tag.tagger {
        let when = chrono::DateTime::from_timestamp(t.time, 0).unwrap_or_default();
        out.push_str(&format!(
            "Tagger: {} <{}>\nDate:   {}\n",
            String::from_utf8_lossy(&t.name),
            String::from_utf8_lossy(&t.email),
            time::local(when)
        ));
    }
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(&tag.message));
    out.push('\n');
    out
}

/// Each commit argument of `diff` with a leading tag name replaced by its commit, for `a..b` both sides.
pub fn expand_args(ws: Option<&Workspace>, args: &[String]) -> Result<Vec<String>> {
    let Some(store) = ws.and_then(LocalStore::open) else {
        return Ok(args.to_vec());
    };
    let one = |text: &str| -> Result<String> {
        if std::path::Path::new(text).exists() {
            return Ok(text.to_string());
        }
        Ok(expand(&store, text)?.map_or_else(|| text.to_string(), |n| n.rev))
    };
    args.iter()
        .map(|a| match a.split_once("..") {
            Some((x, y)) => Ok(format!("{}..{}", one(x)?, one(y)?)),
            None => one(a),
        })
        .collect()
}

fn store_at(cwd: &std::path::Path) -> Option<LocalStore> {
    Workspace::discover(cwd).as_ref().and_then(LocalStore::open)
}

/// The tag names of the workspace around `cwd`, none outside one with a local store.
pub fn names(cwd: &std::path::Path) -> Vec<String> {
    let Some(store) = store_at(cwd) else {
        return Vec::new();
    };
    let listed = store.repo().refs().list(TAGS);
    listed
        .into_iter()
        .flatten()
        .map(|(full, _)| full[TAGS.len()..].to_string())
        .collect()
}

/// [`expand`] for the workspace around `cwd`.
pub fn expand_at(cwd: &std::path::Path, text: &str) -> Result<Option<Named>> {
    match store_at(cwd) {
        Some(store) => expand(&store, text),
        None => Ok(None),
    }
}
