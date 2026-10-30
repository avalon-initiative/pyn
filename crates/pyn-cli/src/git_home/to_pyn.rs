//! `pyn convert --home pyn`: moves a git home repository to a repository on a pyn server with its history (decision
//! #562). The server is written first; the folder changes only after every branch is on it, and `.git` is never touched.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pyn_chunk::SMALL_OBJECT;
use pyn_gitwork::Pathspec;
use pyn_object::{
    Algorithm, Commit, FileMode, Kind, ObjectId, TreeEntry, encode_tree, hash_object, parse_tree,
};
use pyn_proto as api;

use super::GitHome;
use super::convert::warn_lfs_in_commit;
use super::to_git::{ConvertArgs, fatal};
use crate::backend::Backend;
use crate::client::{Api, ApiError};
use crate::init_pyn::{POLICY, Remote};
use crate::pyn_server::PynServer;
use crate::repo_tool::{self, RepoTool};
use crate::transport::Method;
use crate::workspace::{FileState, Settings, Workspace, hash_of, resolve, set_read_only};
use crate::{address, branch, credentials, http_transport, sync, userconfig};

const TARGET: Algorithm = Algorithm::Sha256;
const POLICY_PATH: &str = ".pyn/pyn.toml";
const MAX_COMMIT_CHANGES: usize = 10_000;
const MAX_TREE_ENTRIES: usize = 100_000;
const BATCH_COMMITS: usize = 2_000;
const BATCH_CHANGES: usize = 20_000;
const BATCH_BYTES: usize = 16 << 20;
const MAX_REPORTED: usize = 8;

type Parents = HashMap<ObjectId, Vec<ObjectId>>;

/// Everything about the source history in the server's object format, keyed by the source ids.
struct Rewrite {
    from: Algorithm,
    blobs: HashMap<ObjectId, ObjectId>,
    trees: HashMap<ObjectId, ObjectId>,
    commits: HashMap<ObjectId, ObjectId>,
    parents: Parents,
    changes: HashMap<ObjectId, usize>,
}

struct Problems(Vec<String>);

impl Problems {
    fn add(&mut self, text: String) {
        self.0.push(text);
    }
}

fn read_commit(git: &GitHome, id: &ObjectId) -> Result<(Vec<u8>, Commit)> {
    let object = git.repo.objects().read(id)?;
    if object.kind != Kind::Commit {
        bail!("{id} is not a commit");
    }
    let parsed = Commit::parse_lenient(git.repo.algorithm(), &object.data)
        .with_context(|| format!("commit {id} cannot be read"))?
        .object;
    Ok((object.data, parsed))
}

/// The commit body with its `tree` and `parent` header lines pointing at the new ids; every other byte is kept.
fn rewrite_commit(body: &[u8], tree: &ObjectId, parents: &[ObjectId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let (mut next, mut in_headers) = (0, true);
    for line in body.split_inclusive(|b| *b == b'\n') {
        if in_headers && line == b"\n" {
            in_headers = false;
        }
        if in_headers && line.starts_with(b"tree ") {
            out.extend_from_slice(format!("tree {}\n", tree.to_hex()).as_bytes());
        } else if in_headers && line.starts_with(b"parent ") {
            out.extend_from_slice(format!("parent {}\n", parents[next].to_hex()).as_bytes());
            next += 1;
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// Why the server would refuse `body` as a pushed commit, if it would.
fn unpushable(body: &[u8]) -> Option<String> {
    let commit = match Commit::parse(TARGET, body) {
        Ok(c) => c,
        Err(e) => {
            return Some(format!(
                "it is not in the canonical form the server stores ({e})"
            ));
        }
    };
    let text = |b: &[u8]| std::str::from_utf8(b).is_ok();
    let clean = text(&commit.message)
        && text(&commit.author.name)
        && text(&commit.author.email)
        && text(&commit.committer.name)
        && text(&commit.committer.email);
    (!clean).then(|| "its message or a name is not UTF-8".to_string())
}

impl Rewrite {
    fn blob(&mut self, git: &GitHome, id: &ObjectId) -> Result<ObjectId> {
        if let Some(new) = self.blobs.get(id) {
            return Ok(*new);
        }
        let new = if self.from == TARGET {
            *id
        } else {
            hash_object(TARGET, Kind::Blob, &git.repo.blob(id)?)?
        };
        self.blobs.insert(*id, new);
        Ok(new)
    }

    /// The new id of tree `id` and everything under it; gitlinks and oversized trees are recorded as problems.
    fn tree(&mut self, git: &GitHome, id: &ObjectId, problems: &mut Problems) -> Result<ObjectId> {
        if let Some(new) = self.trees.get(id) {
            return Ok(*new);
        }
        let entries = git.repo.tree(id)?;
        if entries.len() > MAX_TREE_ENTRIES {
            problems.add(format!(
                "tree {id} holds {} entries, over the server's {MAX_TREE_ENTRIES} per folder",
                entries.len()
            ));
        }
        let mut mapped = Vec::with_capacity(entries.len());
        for e in entries {
            let new = match e.mode {
                FileMode::Tree => self.tree(git, &e.id, problems)?,
                FileMode::Gitlink => {
                    problems.add(format!(
                        "a submodule entry '{}' (the server does not store submodules)",
                        String::from_utf8_lossy(&e.name)
                    ));
                    continue;
                }
                _ => self.blob(git, &e.id)?,
            };
            mapped.push(TreeEntry { id: new, ..e });
        }
        let new = hash_object(TARGET, Kind::Tree, &encode_tree(TARGET, &mapped)?)?;
        self.trees.insert(*id, new);
        Ok(new)
    }

    /// The new body of tree `id`, rebuilt from the source tree.
    fn tree_body(&self, git: &GitHome, id: &ObjectId) -> Result<Vec<u8>> {
        let object = git.repo.objects().read(id)?;
        let mapped = parse_tree(self.from, &object.data)?
            .into_iter()
            .map(|e| {
                let new = if e.mode.is_tree() {
                    self.trees[&e.id]
                } else {
                    self.blobs[&e.id]
                };
                TreeEntry { id: new, ..e }
            })
            .collect::<Vec<_>>();
        Ok(encode_tree(TARGET, &mapped)?)
    }

    fn commit_body(&self, git: &GitHome, id: &ObjectId) -> Result<Vec<u8>> {
        let (body, parsed) = read_commit(git, id)?;
        let parents: Vec<ObjectId> = parsed.parents.iter().map(|p| self.commits[p]).collect();
        Ok(rewrite_commit(&body, &self.trees[&parsed.tree], &parents))
    }
}

/// The commits reachable from `tips`, parents before children.
fn topological(git: &GitHome, tips: &[ObjectId]) -> Result<(Vec<ObjectId>, Parents)> {
    let (mut order, mut parents) = (Vec::new(), HashMap::new());
    let mut stack: Vec<(ObjectId, bool)> = tips.iter().map(|t| (*t, false)).collect();
    while let Some((id, expanded)) = stack.pop() {
        if expanded {
            order.push(id);
            continue;
        }
        if parents.contains_key(&id) {
            continue;
        }
        let (_, parsed) = read_commit(git, &id)?;
        stack.push((id, true));
        stack.extend(parsed.parents.iter().map(|p| (*p, false)));
        parents.insert(id, parsed.parents);
    }
    Ok((order, parents))
}

/// Rewrites every commit and checks what the server's push route would refuse, before anything is sent.
fn analyse(git: &GitHome, tips: &[ObjectId]) -> Result<(Rewrite, Vec<ObjectId>)> {
    let (order, parents) = topological(git, tips)?;
    let mut rw = Rewrite {
        from: git.repo.algorithm(),
        blobs: HashMap::new(),
        trees: HashMap::new(),
        commits: HashMap::new(),
        parents,
        changes: HashMap::new(),
    };
    let mut problems = Problems(Vec::new());
    let mut roots = 0;
    for id in &order {
        let (_, parsed) = read_commit(git, id)?;
        let shown = id.to_hex();
        roots += usize::from(parsed.parents.is_empty());
        if parsed.parents.len() > 2
            || (parsed.parents.len() == 2 && parsed.parents[0] == parsed.parents[1])
        {
            problems.add(format!("commit {shown} is a merge of {} parents; the server stores merges of two different commits", parsed.parents.len()));
        }
        let new_tree = rw.tree(git, &parsed.tree, &mut problems)?;
        let first = parsed.parents.first();
        let before = match first {
            Some(p) => Some(read_commit(git, p)?.1.tree),
            None => None,
        };
        let changed = git
            .repo
            .diff_trees(before.as_ref(), Some(&parsed.tree), &Pathspec::default())?
            .len();
        if changed > MAX_COMMIT_CHANGES {
            problems.add(format!("commit {shown} changes {changed} files; the server takes at most {MAX_COMMIT_CHANGES} in one commit"));
        }
        if parsed.parents.len() < 2 && changed == 0 {
            problems.add(format!(
                "commit {shown} changes no file; the server does not store empty commits"
            ));
        }
        rw.changes.insert(*id, changed);
        let Some(mapped) = parsed
            .parents
            .iter()
            .map(|p| rw.commits.get(p).copied())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let (body, _) = read_commit(git, id)?;
        let new_body = rewrite_commit(&body, &new_tree, &mapped);
        if let Some(why) = unpushable(&new_body) {
            problems.add(format!("commit {shown}: {why}"));
        }
        rw.commits
            .insert(*id, hash_object(TARGET, Kind::Commit, &new_body)?);
    }
    if roots > 1 {
        problems.add(format!(
            "the history has {roots} root commits; the server takes one root, so unrelated histories cannot be pushed together"
        ));
    }
    if !problems.0.is_empty() {
        let mut seen = HashSet::new();
        let unique: Vec<&String> = problems.0.iter().filter(|p| seen.insert(*p)).collect();
        let more = unique.len().saturating_sub(MAX_REPORTED);
        let mut text = String::from("the history has what a pyn server does not store:");
        for p in unique.iter().take(MAX_REPORTED) {
            text.push_str(&format!("\n  {p}"));
        }
        if more > 0 {
            text.push_str(&format!("\n  and {more} more"));
        }
        bail!("{text}\nnothing was changed");
    }
    Ok((rw, order))
}

fn api_for(
    base: &str,
    token: &Option<String>,
    user: &Option<String>,
    repo: &str,
    branch: Option<String>,
) -> Api {
    Api {
        transport: Box::new(http_transport::HttpTransport::new(base.to_string())),
        base: base.to_string(),
        token: token.clone(),
        user: user.clone(),
        repo: Some(repo.to_string()),
        branch,
        on_git: false,
    }
}

fn server_branches(api: &Api) -> Result<Option<Vec<api::Branch>>> {
    match api.send(api.get(&api.repo_route("/branches")?)) {
        Ok(reply) => Ok(Some(reply.json()?)),
        Err(e)
            if e.downcast_ref::<ApiError>()
                .is_some_and(|a| a.code == "repo_not_found") =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Sends commits to the server in batches the push limits allow, remembering what it now holds.
struct Pusher<'a> {
    git: &'a GitHome,
    rw: &'a Rewrite,
    api: &'a Api,
    known: HashSet<ObjectId>,
    sent_trees: HashSet<ObjectId>,
    temps: Vec<String>,
}

impl Pusher<'_> {
    fn parents(&self, id: &ObjectId) -> &[ObjectId] {
        &self.rw.parents[id]
    }

    /// The first-parent chain ending at `tip`, oldest first: back to the first commit the server holds, or to the root.
    fn chain(&self, tip: &ObjectId, through_known: bool) -> Vec<ObjectId> {
        let (mut out, mut at) = (Vec::new(), Some(*tip));
        while let Some(c) = at {
            if !through_known && self.known.contains(&c) {
                break;
            }
            out.push(c);
            at = self.parents(&c).first().copied();
        }
        out.reverse();
        out
    }

    /// A chain whose merged-in commits are all on the server, pushing those to temporary branches first.
    fn ready_chain(&mut self, tip: &ObjectId, through_known: bool) -> Result<Vec<ObjectId>> {
        loop {
            let chain = self.chain(tip, through_known);
            let missing = chain
                .iter()
                .filter(|c| !self.known.contains(c))
                .filter_map(|c| self.parents(c).get(1).copied())
                .find(|p| !self.known.contains(p));
            let Some(second) = missing else {
                return Ok(chain);
            };
            let name = format!("pyn-convert-{}", self.temps.len() + 1);
            self.temps.push(name.clone());
            let side = self.ready_chain(&second, false)?;
            self.push_chain(&name, &side)?;
        }
    }

    fn new_trees(
        &self,
        tree: &ObjectId,
        seen: &mut HashSet<ObjectId>,
        out: &mut Vec<(ObjectId, Vec<u8>)>,
    ) -> Result<()> {
        let new = self.rw.trees[tree];
        if self.sent_trees.contains(&new) || !seen.insert(new) {
            return Ok(());
        }
        for e in self.git.repo.tree(tree)? {
            if e.mode.is_tree() {
                self.new_trees(&e.id, seen, out)?;
            }
        }
        out.push((new, self.rw.tree_body(self.git, tree)?));
        Ok(())
    }

    fn push_chain(&mut self, branch: &str, chain: &[ObjectId]) -> Result<()> {
        let mut at = 0;
        while at < chain.len() {
            let (mut commits, mut trees, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
            let (mut changes, mut bytes) = (0, 0);
            while at < chain.len() && commits.len() < BATCH_COMMITS {
                let id = &chain[at];
                // the server accounts a merge against the commit before the push, so a push holding the root has none
                let rooted =
                    !commits.is_empty() && self.parents(&chain[at - commits.len()]).is_empty();
                if rooted && self.parents(id).len() > 1 {
                    break;
                }
                let before = trees.len();
                let tree = read_commit(self.git, id)?.1.tree;
                self.new_trees(&tree, &mut seen, &mut trees)?;
                let body = self.rw.commit_body(self.git, id)?;
                let size: usize =
                    body.len() + trees[before..].iter().map(|(_, b)| b.len()).sum::<usize>();
                let step = self.rw.changes[id];
                if !commits.is_empty()
                    && (changes + step > BATCH_CHANGES || bytes + size > BATCH_BYTES)
                {
                    trees.truncate(before);
                    break;
                }
                commits.push((self.rw.commits[id], body));
                changes += step;
                bytes += size;
                at += 1;
            }
            let encode = |(id, body): &(ObjectId, Vec<u8>)| api::PushObject {
                id: id.to_hex(),
                data: STANDARD.encode(body),
            };
            let request = api::PushRequest {
                branch: Some(branch.to_string()),
                commits: commits.iter().map(encode).collect(),
                trees: trees.iter().map(encode).collect(),
            };
            let reply: api::PushResponse = self
                .api
                .send(
                    self.api
                        .request(Method::POST, &self.api.repo_route("/push")?)
                        .json(&request),
                )
                .map_err(|e| explain(e, branch))?
                .json()?;
            let last = commits.last().map(|(id, _)| id.to_hex());
            let policy = reply.revisions.iter().any(|r| r.path == POLICY_PATH);
            if last.as_deref() != Some(reply.head.as_str()) && !policy {
                bail!(
                    "the server's head {} of '{branch}' is not the pushed commit",
                    reply.head
                );
            }
            self.sent_trees.extend(trees.iter().map(|(id, _)| *id));
            self.known
                .extend(chain[at - commits.len()..at].iter().copied());
        }
        Ok(())
    }
}

fn explain(err: anyhow::Error, branch: &str) -> anyhow::Error {
    let locked = err
        .downcast_ref::<ApiError>()
        .is_some_and(|e| matches!(e.code.as_str(), "lock_required" | "lock_held"));
    let hint = if locked {
        "; the server's default rules make a path in this history exclusive, and a history move takes no locks, so \
         the repository would need default rules that leave it shared"
    } else {
        ""
    };
    err.context(format!("pushing '{branch}' failed{hint}"))
}

fn confirm(text: &str, yes: bool) {
    eprintln!("{text}");
    if yes {
        return;
    }
    if !std::io::stdin().is_terminal() {
        fatal(
            "`pyn convert --home pyn` needs confirmation but there is no terminal to ask on; pass --yes",
        );
    }
    eprint!("Continue? [y/N] ");
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        eprintln!("aborted; nothing was changed");
        std::process::exit(1);
    }
}

fn policy_text(globs: &[String]) -> String {
    if globs.is_empty() {
        return POLICY.to_string();
    }
    let paths: Vec<String> = globs
        .iter()
        .map(|g| toml::Value::String(g.clone()).to_string())
        .collect();
    format!(
        "# Collaboration policy: which paths are shared (merged) and which are exclusive (locked).\n\
         [meta]\ndefault = \"shared\"\n\n[exclusive]\npaths = [\n  {},\n]\n",
        paths.join(",\n  ")
    )
}

/// Branch names as the server will have them: the checked-out branch becomes the server's default `main` when the
/// repository has no `main` of its own.
fn name_branches(heads: &BTreeMap<String, ObjectId>, current: &str) -> BTreeMap<String, String> {
    let rename = !heads.contains_key(branch::DEFAULT_BRANCH) && current != branch::DEFAULT_BRANCH;
    heads
        .keys()
        .map(|name| {
            let server = if rename && name == current {
                branch::DEFAULT_BRANCH
            } else {
                name
            };
            (name.clone(), server.to_string())
        })
        .collect()
}

fn refuse_unfit(git: &GitHome, root: &Path) -> Result<()> {
    let dir = git.repo.git().git_dir();
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "BISECT_LOG",
    ] {
        if dir.join(marker).exists() {
            fatal(&format!(
                "an operation is in progress in {} ({marker}); finish or abort it first. Nothing was changed",
                root.display()
            ));
        }
    }
    if git.repo.git().common_dir().join("shallow").exists() {
        fatal(
            "this repository is shallow, so its history is incomplete; run `pyn fetch --unshallow` first. Nothing was changed",
        );
    }
    Ok(())
}

fn upload_blobs(backend: &PynServer, git: &GitHome, rw: &Rewrite, scratch: &Path) -> Result<usize> {
    let mut sent = 0;
    for (old, new) in &rw.blobs {
        let bytes = git.repo.blob(old)?;
        let stored = if bytes.len() > SMALL_OBJECT {
            std::fs::create_dir_all(scratch)?;
            let file = scratch.join("upload");
            std::fs::write(&file, &bytes)?;
            let stored = backend.upload(&file);
            let _ = std::fs::remove_file(&file);
            stored?
        } else {
            backend.upload_bytes(bytes)?
        };
        if stored != new.to_hex() {
            bail!("the server stored a file as {stored}, not {new}");
        }
        sent += 1;
    }
    Ok(sent)
}

pub fn to_pyn(args: &ConvertArgs, cwd: &Path, remote: Remote) -> Result<()> {
    let Some(git) = super::detect(cwd)? else {
        fatal(
            "`pyn convert --home pyn` moves a git repository to a pyn server, and this is not one (or any of the parent directories): .git",
        );
    };
    let root = git.repo.root()?.to_path_buf();
    refuse_unfit(&git, &root)?;

    let yours = Settings::load(&userconfig::path()?).unwrap_or_default();
    let Some(server) = resolve(remote.server, None, yours.server.as_ref(), None) else {
        crate::usage_error(
            "`pyn convert --home pyn` needs a server: pass --server, set PYN_SERVER or set `server` in the user config",
        );
    };
    let Some(repo) = remote.repo else {
        crate::usage_error(
            "`pyn convert --home pyn` needs the repository to create or fill: pass --repo owner/name",
        );
    };
    let (owner, name) = address::split_repo(&repo)?;
    let base = server.trim_end_matches('/').to_string();
    let token = credentials::token_for(remote.token, &base)?;
    let user = remote.user.or(yours.user);
    let api = api_for(&base, &token, &user, &repo, None);

    let current = match git.repo.head()? {
        pyn_gitwork::Head::Branch { name, id: Some(_) } => name,
        pyn_gitwork::Head::Branch { .. } => {
            fatal("the repository has no commit yet; commit something first. Nothing was changed")
        }
        pyn_gitwork::Head::Detached(_) => {
            fatal("HEAD is detached; switch to a branch first. Nothing was changed")
        }
    };
    let heads: BTreeMap<String, ObjectId> = git
        .repo
        .refs()
        .list("refs/heads/")?
        .into_iter()
        .map(|(n, id)| (n.trim_start_matches("refs/heads/").to_string(), id))
        .collect();
    let names = name_branches(&heads, &current);
    let tips: Vec<ObjectId> = heads.values().copied().collect();
    let (rw, order) = analyse(&git, &tips)?;
    let tip_tree = read_commit(&git, &heads[&current])?.1.tree;
    let tracked = git.repo.flatten(Some(&tip_tree), &Pathspec::default())?;
    warn_lfs_in_commit(&git, &heads[&current])?;

    let local_policy = std::fs::read(root.join(POLICY_PATH)).ok();
    let tracked_policy = tracked.contains_key(POLICY_PATH.as_bytes());
    if !args.exclusive.is_empty() && (tracked_policy || local_policy.is_some()) {
        crate::usage_error(
            "`--exclusive` writes the starter policy, and this repository already has a .pyn/pyn.toml",
        );
    }

    let existing = server_branches(&api)?;
    if existing
        .as_ref()
        .is_some_and(|b| b.iter().any(|b| b.commit.is_some()))
    {
        fatal(&format!(
            "{repo} on {base} already has history; convert into a repository that is empty or does not exist yet. Nothing was changed"
        ));
    }
    let tags = git.repo.refs().list("refs/tags/")?.len();
    let renamed = names
        .iter()
        .find(|(from, to)| from != to)
        .map(|(from, to)| format!(" ('{from}' becomes the server's default branch '{to}')"))
        .unwrap_or_default();
    let ids = if rw.from == TARGET {
        "Object ids stay as they are (SHA-256)".to_string()
    } else {
        "Every commit, tree and file gets a new SHA-256 id because the server stores SHA-256 and this repository is SHA-1; \
         authors, committers, times, messages and file contents stay, but a signature on a commit no longer verifies"
            .to_string()
    };
    confirm(
        &format!(
            "This moves the git repository at {} to {repo} on {base}{}.\n\
             It pushes {} branches{renamed} with {} commits. {ids}.\n\
             The folder keeps its `.git`, unchanged and unused: remove or move it when you no longer need it.\n\
             What is not carried over: tags ({tags} here; the server has none), stashes, the index (staged changes), \
             hooks, remotes and any other git configuration. Files on disk are left as they are.",
            root.display(),
            if existing.is_some() {
                " (it exists and is empty)"
            } else {
                " (created first)"
            },
            heads.len(),
            order.len()
        ),
        args.yes,
    );

    if existing.is_none() {
        let body = api::CreateRepoRequest {
            owner: Some(owner.to_string()),
            name: name.to_string(),
            visibility: None,
            lease_hours: None,
            max_locks_per_user: None,
        };
        let _: api::RepoInfo = api
            .send(api.request(Method::POST, "/v1/repos").json(&body))
            .map_err(|e| crate::explain_create_refusal(e, owner))?
            .json()?;
    }

    let backend = PynServer::new(api_for(&base, &token, &user, &repo, None));
    let api = backend.pyn_server("convert")?;
    let ws = Workspace {
        root: root.clone(),
        dir: root.join(crate::workspace::DIR),
    };
    let uploaded = upload_blobs(&backend, &git, &rw, &ws.dir.join("local_only/cache"))?;

    let mut pusher = Pusher {
        git: &git,
        rw: &rw,
        api,
        known: HashSet::new(),
        sent_trees: HashSet::new(),
        temps: Vec::new(),
    };
    let mut ordered: Vec<(&String, &String)> = names.iter().collect();
    ordered.sort_by_key(|(_, server)| {
        (
            server.as_str() != branch::DEFAULT_BRANCH,
            server.to_string(),
        )
    });
    for (local, server) in ordered {
        let tip = &heads[local];
        let is_default = server == branch::DEFAULT_BRANCH;
        let chain = pusher.ready_chain(tip, is_default)?;
        if chain.is_empty() {
            let body = api::CreateBranchRequest {
                name: server.clone(),
                start: Some(rw.commits[tip].to_hex()),
            };
            api.send(
                api.request(Method::POST, &api.repo_route("/branches")?)
                    .json(&body),
            )?;
        } else {
            pusher.push_chain(server, &chain)?;
        }
    }
    let temps = std::mem::take(&mut pusher.temps);
    for name in &temps {
        api.send(
            api.request(
                Method::DELETE,
                &api.repo_route(&format!("/branches/{name}"))?,
            )
            .query(&[("force", true)]),
        )?;
    }
    let on_server: BTreeMap<String, Option<String>> = server_branches(api)?
        .unwrap_or_default()
        .into_iter()
        .map(|b| (b.name, b.commit))
        .collect();
    for (local, server) in &names {
        let want = rw.commits[&heads[local]].to_hex();
        let got = on_server.get(server).cloned().flatten();
        if got.as_deref() != Some(want.as_str()) && !tracked_policy {
            bail!(
                "the server's '{server}' is at {got:?}, not at {want}; the folder was not changed"
            );
        }
    }

    let policy = match (&local_policy, tracked_policy) {
        (_, true) => None,
        (Some(bytes), false) => Some(bytes.clone()),
        (None, false) => Some(policy_text(&args.exclusive).into_bytes()),
    };
    if let Some(bytes) = &policy {
        crate::submit_policy(api, &format!("/v1/repos/{repo}"), bytes.clone())
            .context("the history is on the server, but its policy was not accepted; the folder was not changed")?;
    }

    let workspace_branch = names[&current].clone();
    let blob_of = |path: &str| -> Option<String> {
        if path == POLICY_PATH {
            return policy.as_ref().map(|b| hash_of(b));
        }
        tracked
            .get(path.as_bytes())
            .map(|e| rw.blobs[&e.id].to_hex())
    };
    finish(
        &backend,
        &base,
        &repo,
        &workspace_branch,
        &ws,
        &policy,
        blob_of,
    )?;

    println!(
        "converted {} to {repo} on {base}: {} branches, {} commits, {uploaded} files uploaded, workspace on '{workspace_branch}'",
        root.display(),
        heads.len(),
        order.len()
    );
    if tags > 0 {
        println!("{tags} tags stay in .git; the server has no tags");
    }
    println!("the old .git is kept and ignored; remove or move it when you no longer need it");
    Ok(())
}

/// Writes the workspace: its config, the revision each file is at, the policy file, and last the marker that makes
/// the folder a pyn home.
fn finish(
    backend: &PynServer,
    base: &str,
    repo: &str,
    workspace_branch: &str,
    ws: &Workspace,
    policy: &Option<Vec<u8>>,
    blob_of: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    let result = (|| -> Result<usize> {
        let ws = Workspace::create(&ws.root)?;
        Settings {
            server: Some(base.to_string()),
            user: None,
            repo: Some(repo.to_string()),
            local_store: None,
            editor: None,
        }
        .save(&ws.config_path())?;
        if workspace_branch != branch::DEFAULT_BRANCH {
            branch::set_current(&ws, workspace_branch)?;
        }
        if let Some(bytes) = policy {
            std::fs::create_dir_all(ws.dir.as_path())?;
            std::fs::write(ws.dir.join("pyn.toml"), bytes)?;
        }
        let on_branch = PynServer::new(api_for_branch(backend, workspace_branch)?);
        let me = on_branch.whoami()?;
        let mut state = BTreeMap::new();
        let mut differ = 0;
        for entry in sync::entries_at(&on_branch, None)? {
            let (Some(revision), Some(hash)) = (entry.revision, blob_of(&entry.path)) else {
                continue;
            };
            let file = ws.abs(&entry.path);
            match std::fs::read(&file) {
                Ok(bytes) if hash_of(&bytes) == hash => {
                    set_read_only(&file, sync::wants_read_only(&entry, &me))?;
                }
                _ => differ += 1,
            }
            state.insert(
                entry.path.clone(),
                FileState {
                    revision,
                    hash,
                    legacy: false,
                    mode: sync::mode_name(entry.mode).to_string(),
                    file_mode: entry.file_mode,
                },
            );
        }
        ws.save_state(&state)?;
        let ignore = ws.dir.join("local_only/ignore");
        let mut text = std::fs::read_to_string(&ignore).unwrap_or_default();
        if !text.lines().any(|l| l == ".git/") {
            text.push_str(".git/\n");
            std::fs::write(&ignore, text)?;
        }
        repo_tool::set(&ws, RepoTool::Pyn)?;
        Ok(differ)
    })();
    match result {
        Ok(0) => Ok(()),
        Ok(n) => {
            eprintln!(
                "{n} files on disk differ from the committed content (line-ending or filter conversions); `pyn status` shows them"
            );
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(ws.dir.join("local_only/state/files.toml"));
            let _ = std::fs::remove_file(ws.config_path());
            Err(e.context(
                "the history is on the server, but the folder could not be set up; it is as it was",
            ))
        }
    }
}

fn api_for_branch(backend: &PynServer, branch: &str) -> Result<Api> {
    let api = backend.pyn_server("convert")?;
    Ok(Api {
        transport: Box::new(http_transport::HttpTransport::new(api.base.clone())),
        base: api.base.clone(),
        token: api.token.clone(),
        user: api.user.clone(),
        repo: api.repo.clone(),
        branch: (branch != branch::DEFAULT_BRANCH).then(|| branch.to_string()),
        on_git: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hex: char) -> ObjectId {
        ObjectId::from_hex(TARGET, &hex.to_string().repeat(64)).unwrap()
    }

    #[test]
    fn only_the_tree_and_parent_lines_change() {
        let old = b"tree 1111\nparent 2222\nparent 3333\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\ngpgsig -----BEGIN\n parent 9999\n -----END\n\nmessage\nparent 4444\n";
        let new = rewrite_commit(old, &id('a'), &[id('b'), id('c')]);
        let text = String::from_utf8(new).unwrap();
        assert!(text.starts_with(&format!(
            "tree {}\nparent {}\nparent {}\nauthor",
            id('a'),
            id('b'),
            id('c')
        )));
        assert!(
            text.contains("gpgsig -----BEGIN\n parent 9999\n -----END\n\nmessage\nparent 4444\n")
        );
    }

    #[test]
    fn the_checked_out_branch_becomes_main_only_without_a_main() {
        let heads: BTreeMap<String, ObjectId> = [("master", 'a'), ("topic", 'b')]
            .into_iter()
            .map(|(n, c)| (n.to_string(), id(c)))
            .collect();
        let names = name_branches(&heads, "master");
        assert_eq!(names["master"], "main");
        assert_eq!(names["topic"], "topic");
        let mut with_main = heads.clone();
        with_main.insert("main".into(), id('c'));
        assert_eq!(name_branches(&with_main, "master")["master"], "master");
    }

    #[test]
    fn exclusive_globs_are_written_as_toml() {
        let text = policy_text(&["Content/".into(), "**/*.uasset".into()]);
        let rules: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(rules["exclusive"]["paths"].as_array().unwrap().len(), 2);
        assert_eq!(rules["meta"]["default"].as_str(), Some("shared"));
    }
}
