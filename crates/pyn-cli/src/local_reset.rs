//! `pyn reset` in a `local_store` workspace: drops unpublished local commits, keeping or discarding their changes.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::ObjectId;

use crate::backend::Backend;
use crate::local_store::LocalStore;
use crate::local_sync::api_mode;
use crate::snapshot::{self, Base, short};
use crate::staging::{self, Index};
use crate::workspace::{FileState, Workspace, hash_of, set_read_only};
use crate::{branch, localfile, merging, sync};

#[derive(Args, Debug)]
pub struct ResetArgs {
    /// Move only the branch; the index and files stay (a workspace: keep the dropped commits' changes staged).
    #[arg(long, overrides_with_all = ["mixed", "hard", "keep", "merge"])]
    soft: bool,
    /// Reset the index too, keeping the files (the default).
    #[arg(long, overrides_with_all = ["soft", "hard", "keep", "merge"])]
    mixed: bool,
    /// Reset the index and the files, discarding local changes.
    #[arg(long, overrides_with_all = ["soft", "mixed", "keep", "merge"])]
    hard: bool,
    /// Git repository: reset, but stop if a file with local changes would be overwritten.
    #[arg(long, overrides_with_all = ["soft", "mixed", "hard", "merge"])]
    keep: bool,
    /// Git repository: reset, keeping files whose changes are not staged; stop if one would be overwritten.
    #[arg(long, overrides_with_all = ["soft", "mixed", "hard", "keep"])]
    merge: bool,
    /// Git repository: do not list the changes left unstaged.
    #[arg(short, long)]
    pub(crate) quiet: bool,
    /// Workspace: skip the typed confirmation of --hard.
    #[arg(long)]
    pub(crate) yes: bool,
    /// A revision (a workspace: HEAD, HEAD~N or a hash of an unpublished local commit, or of the last published
    /// one); default HEAD. On a git repository, paths may follow it.
    pub(crate) revs: Vec<String>,
    /// Git repository: reset only the index entries of these paths.
    #[arg(last = true)]
    pub(crate) paths: Vec<String>,
}

impl ResetArgs {
    pub(crate) fn kind(&self) -> crate::git_home::ResetKind {
        use crate::git_home::ResetKind;
        match () {
            _ if self.soft => ResetKind::Soft,
            _ if self.hard => ResetKind::Hard,
            _ if self.keep => ResetKind::Keep,
            _ if self.merge => ResetKind::Merge,
            _ => ResetKind::Mixed,
        }
    }

    pub(crate) fn mixed(&self) -> bool {
        self.mixed
    }

    /// The one revision a workspace takes; git's other forms need a git repository.
    fn commit(&self) -> Result<Option<&str>> {
        if self.keep || self.merge || self.quiet || !self.paths.is_empty() || self.revs.len() > 1 {
            bail!(
                "`--keep`, `--merge`, `-q`, paths and more than one revision need a git repository; \
                 a workspace resets to one commit"
            );
        }
        Ok(self.revs.first().map(String::as_str))
    }
}

#[derive(PartialEq, Eq)]
enum Kind {
    Soft,
    Mixed,
    Hard,
}

/// The commits the local branch may move back to: its unpublished first-parent chain and the published tip.
fn candidates(
    store: &LocalStore,
    tip: ObjectId,
    published: Option<ObjectId>,
) -> Result<Vec<ObjectId>> {
    let known = store.ancestors(published)?;
    let mut out = vec![];
    let mut at = Some(tip);
    while let Some(id) = at {
        out.push(id);
        if known.contains(&id) {
            break;
        }
        at = store.repo().commit(&id)?.parents.first().copied();
    }
    if let Some(p) = published
        && !out.contains(&p)
    {
        out.push(p);
    }
    Ok(out)
}

fn pick(store: &LocalStore, text: &str, tip: ObjectId, chain: &[ObjectId]) -> Result<ObjectId> {
    let Some(found) = snapshot::parse_commit(text) else {
        bail!("{text} is not a commit; use HEAD, HEAD~N, HEAD^ or a commit hash");
    };
    let mut id = match &found.base {
        Base::Head => tip,
        Base::Hash(h) => {
            let hits: Vec<&ObjectId> = chain
                .iter()
                .filter(|c| c.to_hex().starts_with(h.as_str()))
                .collect();
            match hits.as_slice() {
                [one] => **one,
                _ => bail!("{text} is not an unpublished local commit or the published tip"),
            }
        }
    };
    for _ in 0..found.back {
        match store.repo().commit(&id)?.parents.first() {
            Some(p) => id = *p,
            None => bail!("{text} is before the first commit"),
        }
    }
    if !chain.contains(&id) {
        bail!(
            "{text} is older than the last published commit; only unpublished local commits can be reset"
        );
    }
    Ok(id)
}

pub fn reset(backend: &dyn Backend, ws: &Workspace, args: ResetArgs) -> Result<()> {
    merging::refuse_during_merge(ws, "reset")?;
    let commit = args.commit()?;
    let kind = match (args.soft, args.hard) {
        (true, _) => Kind::Soft,
        (_, true) => Kind::Hard,
        _ => Kind::Mixed,
    };
    let branch = branch::name(Some(ws));
    let Some(store) = LocalStore::open(ws) else {
        bail!(
            "`pyn reset` works on the local commits of a workspace that keeps them; `pyn migrate` converts an older one"
        );
    };
    let Some(tip) = store.tip(&branch)? else {
        bail!("branch '{branch}' has no local commits");
    };
    let chain = candidates(&store, tip, store.published(&branch)?)?;
    let target = pick(&store, commit.unwrap_or("HEAD"), tip, &chain)?;
    if kind == Kind::Hard && !args.yes {
        eprint!(
            "This discards local commits and uncommitted changes. Type the branch name to confirm: "
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != branch {
            bail!("not confirmed");
        }
    }
    let repo = store.repo();
    let (t_tree, tip_tree) = (store.tree_of(Some(target))?, store.tree_of(Some(tip))?);
    let mut state = ws.load_state()?;
    let mut index = if kind == Kind::Soft {
        Index::load(ws)?
    } else {
        Index::default()
    };
    let wanted = repo.flatten(t_tree.as_ref(), &Pathspec::default())?;
    let mut paths: BTreeMap<String, (Option<Entry>, Option<Entry>)> = BTreeMap::new();
    for c in repo.diff_trees(t_tree.as_ref(), tip_tree.as_ref(), &Pathspec::default())? {
        paths.insert(
            String::from_utf8_lossy(&c.path).into_owned(),
            (c.old, c.new),
        );
    }
    if kind == Kind::Hard {
        for (path, known) in &state {
            let entry = wanted.get(path.as_bytes()).copied();
            let abs = ws.abs(path);
            let same = match (&entry, localfile::exists(&abs)) {
                (Some(_), true) => {
                    let (mode, bytes) = localfile::read(&abs)?;
                    !known.changed(mode, &bytes)
                }
                (None, false) => true,
                _ => false,
            };
            if !same {
                paths.entry(path.clone()).or_insert((entry, entry));
            }
        }
    }
    let files = backend.list_files()?;
    let me = backend.whoami()?;
    for (path, (old, new)) in paths {
        let previous = state.remove(&path);
        let revision = previous.as_ref().map_or(0, |k| k.revision);
        let mode = previous.as_ref().map_or_else(
            || {
                files
                    .iter()
                    .find(|f| f.path == path)
                    .map_or("shared", |f| sync::mode_name(f.mode))
                    .to_string()
            },
            |k| k.mode.clone(),
        );
        if kind == Kind::Soft {
            let entry = match new {
                Some(n) => {
                    let bytes = store.blob(backend, &n.id.to_hex())?;
                    let hash = hash_of(&bytes);
                    staging::store(ws, &hash, &bytes)?;
                    staging::Entry {
                        hash,
                        file_mode: api_mode(n.mode)?,
                        base_revision: previous.as_ref().map(|k| k.revision),
                        deleted: false,
                    }
                }
                None => staging::Entry {
                    hash: String::new(),
                    file_mode: api_mode(old.map_or(pyn_object::FileMode::Regular, |o| o.mode))?,
                    base_revision: previous.as_ref().map(|k| k.revision),
                    deleted: true,
                },
            };
            index.entries.entry(path.clone()).or_insert(entry);
        }
        let Some(entry) = old else {
            if kind == Kind::Hard {
                let abs = ws.abs(&path);
                if localfile::exists(&abs) {
                    set_read_only(&abs, false)?;
                    std::fs::remove_file(&abs)?;
                }
            }
            continue;
        };
        let bytes = store.blob(backend, &entry.id.to_hex())?;
        let file_mode = api_mode(entry.mode)?;
        if kind == Kind::Hard {
            let target = ws.dest_with_parents(&path)?;
            set_read_only(&target, false)?;
            ws.write_file(&path, &bytes)?;
            localfile::apply(&target, &path, file_mode)?;
            let held = files
                .iter()
                .find(|f| f.path == path)
                .is_some_and(|f| !sync::wants_read_only(f, &me));
            if mode == "exclusive" && !held {
                set_read_only(&target, true)?;
            }
        }
        state.insert(
            path,
            FileState {
                revision,
                hash: hash_of(&bytes),
                legacy: false,
                mode,
                file_mode,
            },
        );
    }
    store.move_tip(&branch, tip, target)?;
    ws.save_state(&state)?;
    index.save(ws)?;
    let left = store.unpublished(&branch)?.ahead;
    println!(
        "Branch '{branch}' is now at {}; {left} local commit(s) are not yet published",
        short(&target.to_hex())
    );
    Ok(())
}
