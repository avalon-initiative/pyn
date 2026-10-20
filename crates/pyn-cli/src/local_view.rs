//! `pyn log --commits` and `pyn show` for the unpublished commits of a `local_store` workspace.

use anyhow::Result;
use chrono::{DateTime, Utc};
use pyn_gitwork::Pathspec;
use pyn_object::{ObjectId, Signature};
use pyn_proto as api;

use crate::backend::Backend;
use crate::diff::{self, Blob, Source, Tree};
use crate::local_store::LocalStore;
use crate::local_sync::api_mode;
use crate::snapshot::{self, Base, short};
use crate::time;

fn person(s: &Signature) -> api::Person {
    api::Person {
        name: String::from_utf8_lossy(&s.name).into_owned(),
        email: String::from_utf8_lossy(&s.email).into_owned(),
        offset_minutes: s.offset_minutes,
    }
}

fn at(t: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(t, 0).unwrap_or_default()
}

fn api_commit(store: &LocalStore, id: &ObjectId) -> Result<api::Commit> {
    let c = store.repo().commit(id)?;
    Ok(api::Commit {
        id: id.to_hex(),
        parents: c.parents.iter().map(ObjectId::to_hex).collect(),
        tree: c.tree.to_hex(),
        author: person(&c.author),
        committer: person(&c.committer),
        created_at: at(c.committer.time),
        authored_at: Some(at(c.author.time)),
        message: String::from_utf8_lossy(&c.message).into_owned(),
        signature: None,
        changeset: None,
        revisions: Vec::new(),
    })
}

/// The unpublished commits of `branch`, newest first, along its first parents.
fn unpublished_chain(store: &LocalStore, branch: &str) -> Result<Vec<ObjectId>> {
    let (Some(tip), published) = (store.tip(branch)?, store.published(branch)?) else {
        return Ok(Vec::new());
    };
    let known = store.ancestors(published)?;
    let mut out = Vec::new();
    let mut at = Some(tip);
    while let Some(id) = at.filter(|id| !known.contains(id)) {
        out.push(id);
        at = store.repo().commit(&id)?.parents.first().copied();
    }
    Ok(out)
}

fn changed_paths(store: &LocalStore, c: &api::Commit) -> Result<Vec<String>> {
    let parent = match c.parents.first() {
        Some(p) => store.tree_of(Some(crate::local_store::parse_id(p)?))?,
        None => None,
    };
    let tree = crate::local_store::parse_id(&c.tree)?;
    Ok(store
        .repo()
        .diff_trees(parent.as_ref(), Some(&tree), &Pathspec::default())?
        .iter()
        .map(|d| String::from_utf8_lossy(&d.path).into_owned())
        .collect())
}

/// The unpublished commits of `branch`, newest first.
pub fn log_commits(store: &LocalStore, branch: &str) -> Result<Vec<api::Commit>> {
    unpublished_chain(store, branch)?
        .iter()
        .map(|id| api_commit(store, id))
        .collect()
}

/// The rows of `log --commits` for the unpublished commits, newest first.
pub fn log_rows(store: &LocalStore, branch: &str) -> Result<Vec<Vec<String>>> {
    unpublished_chain(store, branch)?
        .iter()
        .map(|id| {
            let c = api_commit(store, id)?;
            Ok(vec![
                short(&c.id).to_string(),
                c.author.name.clone(),
                time::local(c.created_at),
                changed_paths(store, &c)?.len().to_string(),
                c.message.lines().next().unwrap_or_default().to_string(),
            ])
        })
        .collect()
}

fn tree_of(store: &LocalStore, tree: Option<ObjectId>) -> Result<Tree> {
    let mut out = Tree::new();
    for (path, e) in store.repo().flatten(tree.as_ref(), &Pathspec::default())? {
        out.insert(
            String::from_utf8_lossy(&path).into_owned(),
            Blob {
                hash: e.id.to_hex(),
                mode: api_mode(e.mode)?,
                source: Source::Object { id: e.id.to_hex() },
            },
        );
    }
    Ok(out)
}

/// The trees before and after an unpublished commit (against its first parent) and the paths it changed.
pub fn commit_trees(store: &LocalStore, commit: &api::Commit) -> Result<(Tree, Tree, Vec<String>)> {
    let old = match commit.parents.first() {
        Some(p) => tree_of(
            store,
            store.tree_of(Some(crate::local_store::parse_id(p)?))?,
        )?,
        None => Tree::new(),
    };
    let new = tree_of(store, Some(crate::local_store::parse_id(&commit.tree)?))?;
    Ok((old, new, changed_paths(store, commit)?))
}

/// Prints the unpublished commit `text` names (HEAD by default) and returns true; false if it names none.
pub fn show(
    backend: &dyn Backend,
    store: &LocalStore,
    branch: &str,
    text: Option<&str>,
    opts: &crate::ShowOpts,
) -> Result<bool> {
    let chain = unpublished_chain(store, branch)?;
    let text = text.unwrap_or("HEAD");
    let Some(found) = snapshot::parse_commit(text) else {
        return Ok(false);
    };
    let mut id = match &found.base {
        Base::Head => chain.first().copied(),
        Base::Hash(h) => chain
            .iter()
            .find(|c| c.to_hex().starts_with(h.as_str()))
            .copied(),
    };
    let Some(mut cur) = id.take() else {
        return Ok(false);
    };
    for _ in 0..found.back {
        match store.repo().commit(&cur)?.parents.first() {
            Some(p) => cur = *p,
            None => anyhow::bail!("{text} is before the first commit"),
        }
    }
    let commit = api_commit(store, &cur)?;
    if opts.no_patch {
        print!("{}", opts.head(&commit)?.0);
        return Ok(true);
    }
    let format = &opts.diff;
    let (old, new, only) = commit_trees(store, &commit)?;
    let mut body = String::new();
    let differs = diff::compare(backend, &old, &new, &only, format, &mut body)?;
    let (head, blank) = opts.head(&commit)?;
    print!("{head}");
    if diff::has_body(format, differs, &body) {
        if blank {
            println!();
        }
        print!("{body}");
    }
    diff::exit_if_check_failed(format, &body);
    Ok(true)
}
