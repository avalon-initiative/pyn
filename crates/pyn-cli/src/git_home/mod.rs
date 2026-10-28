//! The backend for a repository that lives in git: a real `.git` read through `pyn-gitwork`, never written.
//! Nothing here changes what git sees; the only file pyn adds is its own `.pyn/` folder, hidden by `info/exclude`.

mod add_interactive;
mod am;
mod am_3way;
mod am_mail;
mod ancestry;
mod apply;
mod apply_hunk;
mod apply_parse;
mod apply_ws;
mod bisect;
mod bisect_bases;
mod bisect_walk;
mod blame;
mod blame_format;
mod blame_range;
mod blame_rename;
mod blame_walk;
mod branches;
mod bundle;
mod cat_file;
mod clean;
mod clean_menu;
mod clone;
mod combined_diff;
mod commands;
mod commit;
mod config_flags;
mod convert;
mod count_objects;
mod decorate;
mod describe;
mod dwim;
pub(crate) mod editor;
mod fetch;
mod fetch_apply;
mod fetch_map;
mod format_patch;
mod fsck;
mod fsck_check;
mod gc;
mod grep;
pub(crate) mod grep_regex;
mod hooks;
mod hunk_pick;
mod init_git;
mod log;
mod log_gate;
mod log_graph;
mod log_match;
mod ls_files;
mod ls_tree;
mod maintenance;
pub(crate) mod meld;
mod merge;
mod merge_diff;
mod name_rev;
mod pack_plumbing;
mod pack_refs;
mod partial;
mod patch_file;
mod patch_mode;
mod pick;
mod plumbing;
mod prune;
mod pull;
mod push;
mod push_plan;
mod rebase;
mod rebase_merges;
mod reflog;
mod refwrite;
mod remerge;
mod remerge_diff;
mod remote;
mod remote_query;
mod remove;
mod repack;
mod reset;
mod rev_parse;
mod rewrite;
mod shallow;
mod shortlog;
mod show_object;
mod show_ref;
mod signing;
mod sparse;
mod stage;
mod stash;
mod stash_show;
mod storage_links;
mod storage_roots;
mod strategy;
mod submodule;
mod submodule_cfg;
mod submodule_summary;
mod switch;
mod tags;
mod to_git;
mod to_pyn;
pub(crate) mod todo;
mod treemerge;
mod upstream;
mod verify_sig;
mod virtual_base;
mod worktree;
mod worktrees;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use pyn_gitwork::{Pathspec, Repo};
use pyn_object::{FileMode, ObjectId, Signature};
use pyn_proto as api;

use crate::backend::Backend;
use crate::repo_tool::{self, RepoTool};
use crate::workspace::{self, Workspace};

pub use am::AmArgs;
pub use apply::ApplyArgs;
pub use bisect::BisectArgs;
pub use blame::BlameArgs;
pub use bundle::{BundleArgs, bundle};
pub use cat_file::CatFileArgs;
pub use clean::CleanArgs;
pub use clone::{CloneOptions, clone};
pub use commands::run;
pub use commit::GitCommitArgs;
pub(crate) use commit::append_trailer;
pub use config_flags::{ConfigFlags, run as config_flags};
pub use count_objects::CountObjectsArgs;
pub use decorate::DecorateArgs;
pub use describe::DescribeArgs;
pub use fetch::FetchArgs;
pub use format_patch::FormatPatchArgs;
pub use fsck::FsckArgs;
pub use gc::GcArgs;
pub use grep::GrepArgs;
pub(crate) use init_git::is_tracked;
pub use init_git::{InitGitArgs, init_git};
pub use log::GitLogArgs;
pub use ls_files::LsFilesArgs;
pub use ls_tree::LsTreeArgs;
pub use maintenance::MaintenanceArgs;
pub use merge_diff::MergeArgs;
pub use name_rev::NameRevArgs;
pub use pack_plumbing::{PackObjectsArgs, PrunePackedArgs, UnpackObjectsArgs};
pub use pack_refs::PackRefsArgs;
pub use prune::PruneArgs;
pub use pull::PullArgs;
pub use push::PushArgs;
pub use push::ServerPush;
pub use rebase::RebaseArgs;
pub use reflog::ReflogArgs;
pub use remote::{RemoteArgs, on_server as remote_on_server};
pub use repack::RepackArgs;
pub use reset::ResetKind;
pub use rev_parse::RevParseArgs;
pub use shortlog::ShortlogArgs;
pub(crate) use shortlog::shortlog_outside_repo;
pub use show_ref::ShowRefArgs;
pub use signing::SignFlag;
pub use sparse::SparseCheckoutArgs;
pub(crate) use stage::clean_message;
pub use stash::StashArgs;
pub use submodule::SubmoduleArgs;
pub use tags::TagArgs;
pub(crate) use tags::{
    annotation, check_name, delete as delete_tags, list as list_tags, short, tag_ref,
};
pub use to_git::{ConvertArgs, Home, to_git};
pub use to_pyn::to_pyn;
pub use verify_sig::{VerifyCommitArgs, VerifyTagArgs};
pub use worktrees::WorktreeArgs;

/// A refusal git reports as `error: <message>` with exit status 1, rather than as a fatal error.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// An `error: <message>` failure that exits with a status of its own.
#[derive(Debug)]
pub struct Exits(pub i32, pub String);

impl std::fmt::Display for Exits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

impl std::error::Error for Exits {}

pub struct GitHome {
    repo: Repo,
    /// The work tree root with a `.pyn` folder beside `.git`, whether or not it exists yet.
    ws: Workspace,
}

/// A `.pyn` is a pyn server workspace once a clone has recorded which revision each file is at.
fn is_server_workspace(ws: &Workspace) -> bool {
    ws.dir.join("local_only/state/files.toml").is_file()
}

fn user_dirs() -> (Option<PathBuf>, Option<PathBuf>) {
    let dir = |name| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    (dir("HOME"), dir("XDG_CONFIG_HOME"))
}

/// The `.pyn` that belongs to folder `p`: `PYN_DIR` when it names one directly under `p`.
fn pyn_dir_in(p: &Path) -> Workspace {
    let dir = std::env::var_os("PYN_DIR")
        .map(PathBuf::from)
        .filter(|d| d.parent() == Some(p))
        .unwrap_or_else(|| p.join(workspace::DIR));
    Workspace {
        root: p.to_path_buf(),
        dir,
    }
}

/// Whether `p` is itself a git directory (a bare repository), as git's discovery tests it.
pub(crate) fn is_bare_dir(p: &Path) -> bool {
    p.file_name().is_none_or(|n| n != ".git")
        && p.join("HEAD").is_file()
        && p.join("objects").is_dir()
        && p.join("refs").is_dir()
}

/// The folder whose `.git` makes `start` a git home, or `None` when it is a pyn home.
/// The nearest folder with a claim decides: a `repo_tool` marker wins over a `.git` or a server workspace beside it,
/// a `git` marker needs a `.git` there, and without a marker a server workspace wins over the `.git` in its folder.
fn git_root_of(start: &Path) -> Result<Option<&Path>> {
    for p in start.ancestors() {
        let ws = pyn_dir_in(p);
        let marker = repo_tool::read(&ws)?;
        let has_git = p.join(".git").exists() || is_bare_dir(p);
        match (marker, has_git, is_server_workspace(&ws)) {
            (Some(RepoTool::Pyn), _, _) => return Ok(None),
            (_, true, false) | (Some(RepoTool::Git), true, true) => return Ok(Some(p)),
            (None, true, true) | (None, false, true) => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

/// The git home `start` is in, if any: see [`git_root_of`] for which folder decides.
pub fn detect(start: &Path) -> Result<Option<GitHome>> {
    let Some(git_root) = git_root_of(start)? else {
        return Ok(None);
    };
    let (home, xdg) = user_dirs();
    let repo = Repo::open(git_root, home.as_deref(), xdg.as_deref())
        .with_context(|| format!("opening the git repository at {}", git_root.display()))?;
    let root = repo.root().unwrap_or(git_root).to_path_buf();
    let ws = Workspace {
        dir: root.join(workspace::DIR),
        root,
    };
    Ok(Some(GitHome { repo, ws }))
}

fn unavailable(what: &str) -> anyhow::Error {
    anyhow!("{what} is not available on a git repository yet")
}

fn person(sig: &Signature) -> api::Person {
    api::Person {
        name: String::from_utf8_lossy(&sig.name).into_owned(),
        email: String::from_utf8_lossy(&sig.email).into_owned(),
        offset_minutes: sig.offset_minutes,
    }
}

/// The file mode for server-style listings, excluding trees and gitlinks.
fn file_mode(mode: FileMode) -> Option<api::FileMode> {
    match mode {
        FileMode::Regular => Some(api::FileMode::Regular),
        FileMode::Executable => Some(api::FileMode::Executable),
        FileMode::Symlink => Some(api::FileMode::Symlink),
        FileMode::Tree | FileMode::Gitlink => None,
    }
}

fn path_string(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

impl GitHome {
    /// Opens the repository whose work tree root, or bare git directory, is `path`.
    pub(super) fn open_at(path: &Path) -> Result<GitHome> {
        let (home, xdg) = user_dirs();
        let repo = Repo::open(path, home.as_deref(), xdg.as_deref())
            .with_context(|| format!("opening the git repository at {}", path.display()))?;
        let root = repo
            .root()
            .map_or_else(|_| path.to_path_buf(), Path::to_path_buf);
        let ws = Workspace {
            dir: root.join(workspace::DIR),
            root,
        };
        Ok(GitHome { repo, ws })
    }

    pub fn is_bare(&self) -> bool {
        self.repo.git().work_tree().is_none()
    }

    /// Fails as git does for a command that reads or writes the work tree.
    pub(crate) fn need_work_tree(&self) -> Result<()> {
        self.repo.root().map(|_| ()).map_err(Into::into)
    }

    /// Whether `name` is a path in `cwd`; a bare repository has no work tree to hold one.
    pub(crate) fn cwd_has(&self, cwd: &Path, name: &str) -> bool {
        !self.is_bare() && std::fs::symlink_metadata(cwd.join(name)).is_ok()
    }

    /// Opens the same repository again, to see a configuration or object format changed on disk.
    pub(super) fn reopen(&self) -> Result<GitHome> {
        let git = self.repo.git();
        Self::open_at(git.work_tree().unwrap_or(git.git_dir()))
    }

    pub(super) fn local_config_path(&self) -> PathBuf {
        self.repo.git().common_dir().join("config")
    }

    /// The git configuration a transport reads: the repository's, over the user's.
    pub fn config(&self) -> &pyn_git::Config {
        self.repo.config()
    }

    /// A connector whose relative local remote paths resolve from the work tree root, as git's do.
    pub fn connector(&self) -> pyn_gittransport::Connector {
        let c = pyn_gittransport::Connector::new(
            self.config().clone(),
            pyn_gittransport::Env::process(),
        );
        match self.repo.git().work_tree() {
            Some(root) => c.with_base(root),
            None => c,
        }
    }

    pub fn algorithm(&self) -> pyn_object::Algorithm {
        self.repo.algorithm()
    }

    fn api_commit(&self, id: &ObjectId) -> Result<api::Commit> {
        let c = self.repo.commit(id)?;
        let signature = c.gpgsig().map(|s| String::from_utf8_lossy(s).into_owned());
        Ok(api::Commit {
            id: id.to_hex(),
            parents: c.parents.iter().map(ObjectId::to_hex).collect(),
            tree: c.tree.to_hex(),
            author: person(&c.author),
            committer: person(&c.committer),
            created_at: DateTime::<Utc>::from_timestamp(c.author.time, 0).unwrap_or_default(),
            authored_at: DateTime::<Utc>::from_timestamp(c.author.time, 0),
            message: String::from_utf8_lossy(&c.message).into_owned(),
            signature,
            changeset: None,
            revisions: Vec::new(),
        })
    }
}

impl GitHome {
    /// The rename detection `<scope>.renames` and `<scope>.renameLimit` turn on, falling back to `diff.*`.
    pub(crate) fn rename_settings(&self, scope: &str) -> crate::rename::Defaults {
        use crate::rename::{Defaults, Mode};
        let cfg = self.repo.config();
        let mode_of = |key: String| -> Option<Option<Mode>> {
            let v = cfg.get(&key)?.to_ascii_lowercase();
            Some(if v == "copies" || v == "copy" {
                Some(Mode::Copies)
            } else {
                cfg.get_bool(&key)
                    .ok()
                    .flatten()
                    .unwrap_or(false)
                    .then_some(Mode::Renames)
            })
        };
        let limit_of = |key: String| -> Option<i64> {
            let v = cfg.get(&key)?.trim().to_ascii_lowercase();
            let (digits, scale) = match v.chars().last()? {
                'k' => (&v[..v.len() - 1], 1 << 10),
                'm' => (&v[..v.len() - 1], 1 << 20),
                'g' => (&v[..v.len() - 1], 1 << 30),
                _ => (v.as_str(), 1),
            };
            digits.parse::<i64>().ok().map(|n| n * scale)
        };
        Defaults {
            mode: mode_of(format!("{scope}.renames"))
                .or_else(|| mode_of("diff.renames".into()))
                .unwrap_or(Some(Mode::Renames)),
            limit: limit_of(format!("{scope}.renameLimit"))
                .or_else(|| limit_of("diff.renameLimit".into()))
                .unwrap_or(crate::rename::DEFAULT_LIMIT as i64),
        }
    }
}

impl Backend for GitHome {
    fn whole_trees(&self) -> bool {
        true
    }

    fn rename_defaults(&self) -> crate::rename::Defaults {
        self.rename_settings("diff")
    }

    fn quote_diff_path(&self, path: &[u8]) -> String {
        crate::short_status::quote_diff(path, self.repo.bool_setting("core.quotePath", true))
    }

    fn diff_binary(&self, path: &str) -> Option<bool> {
        let attrs = self.repo.attributes().ok()?.for_path(path.as_bytes());
        match attrs.get("diff") {
            pyn_gitwork::AttrValue::Unset => Some(true),
            pyn_gitwork::AttrValue::Set => Some(false),
            _ => None,
        }
    }

    fn diff_setting(&self, key: &str) -> Option<String> {
        self.repo.config().get(key).map(str::to_string)
    }

    fn whoami(&self) -> Result<String> {
        self.repo
            .config()
            .get("user.name")
            .map(str::to_string)
            .context("user.name is not set in the git configuration")
    }

    fn list_files(&self) -> Result<Vec<api::FileEntry>> {
        let index = self.repo.read_index()?;
        Ok(self
            .repo
            .index_leaves(&index)
            .into_iter()
            .filter_map(|(path, e)| {
                Some(api::FileEntry {
                    path: path_string(&path),
                    mode: api::Mode::Shared,
                    revision: None,
                    file_mode: file_mode(e.mode)?,
                    lock: None,
                })
            })
            .collect())
    }

    fn tree(&self, path: Option<&str>) -> Result<api::TreeListing> {
        let path = path.unwrap_or("").trim_matches('/');
        let Some(head) = self.repo.head_commit()? else {
            return Ok(api::TreeListing {
                path: path.into(),
                entries: Vec::new(),
            });
        };
        let mut tree = self.repo.commit(&head)?.tree;
        if !path.is_empty() {
            let found = self.repo.entry_at(&tree, path.as_bytes())?;
            match found {
                Some(e) if e.mode.is_tree() => tree = e.id,
                _ => bail!("{path} is not a folder in HEAD"),
            }
        }
        let mut entries: Vec<api::TreeEntry> = Vec::new();
        for e in self.repo.tree(&tree)? {
            let name = path_string(&e.name);
            let kind = if e.mode.is_tree() {
                api::TreeEntryKind::Folder
            } else if file_mode(e.mode).is_some() {
                api::TreeEntryKind::File
            } else {
                continue;
            };
            entries.push(api::TreeEntry {
                path: if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                },
                name,
                kind,
                mode: api::TreeMode::Shared,
                last_change: None,
                lock: None,
            });
        }
        entries.sort_by(|a, b| {
            (a.kind == api::TreeEntryKind::File, &a.name)
                .cmp(&(b.kind == api::TreeEntryKind::File, &b.name))
        });
        Ok(api::TreeListing {
            path: path.into(),
            entries,
        })
    }

    fn head_commit(&self) -> Result<Option<String>> {
        Ok(self.repo.head_commit()?.map(|id| id.to_hex()))
    }

    fn commit_page(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage> {
        let start: Vec<ObjectId> = match before {
            Some(b) => self.repo.commit(&self.repo.resolve(b)?)?.parents,
            None => self.repo.head_commit()?.into_iter().collect(),
        };
        let mut log = self.repo.log(&start, Pathspec::default())?;
        let mut commits = Vec::new();
        while commits.len() < limit {
            let Some(entry) = log.next_entry()? else {
                break;
            };
            commits.push(self.api_commit(&entry.id)?);
        }
        let more = commits.len() == limit && log.next_entry()?.is_some();
        let next_before = more.then(|| commits.last().map(|c| c.id.clone())).flatten();
        Ok(api::CommitPage {
            commits,
            next_before,
        })
    }

    fn commit(&self, id: &str) -> Result<api::Commit> {
        self.api_commit(&self.repo.resolve(id)?)
    }

    fn snapshot(&self, commit: Option<&str>) -> Result<(Option<String>, Vec<api::SnapshotEntry>)> {
        let id = match commit {
            Some(c) => Some(self.repo.resolve(c)?),
            None => self.repo.head_commit()?,
        };
        let Some(id) = id else {
            return Ok((None, Vec::new()));
        };
        let tree = self.repo.commit(&id)?.tree;
        let entries = self
            .repo
            .flatten(Some(&tree), &Pathspec::default())?
            .into_iter()
            .filter_map(|(path, e)| {
                Some(api::SnapshotEntry {
                    path: path_string(&path),
                    mode: api::Mode::Shared,
                    revision: 0,
                    content: e.id.to_hex(),
                    file_mode: file_mode(e.mode)?,
                })
            })
            .collect();
        Ok((Some(id.to_hex()), entries))
    }

    fn blob(&self, id: &str, probe: Option<usize>) -> Result<Vec<u8>> {
        let id = ObjectId::from_hex(self.repo.algorithm(), id)?;
        let mut bytes = self.repo.blob(&id)?;
        if let Some(n) = probe {
            bytes.truncate(n);
        }
        Ok(bytes)
    }

    fn content(&self, _: &str, _: u64, _: Option<usize>) -> Result<Vec<u8>> {
        Err(unavailable("reading a file at a pyn revision"))
    }

    fn read(&self, _: &str, _: Option<u64>, _: &mut dyn std::io::Write) -> Result<()> {
        Err(unavailable("`pyn get`"))
    }

    fn download(&self, _: &str, _: Option<u64>, _: &Path) -> Result<(u64, String)> {
        Err(unavailable("`pyn get`"))
    }

    fn upload(&self, _: &Path) -> Result<String> {
        Err(unavailable("writing objects"))
    }

    fn upload_bytes(&self, _: Vec<u8>) -> Result<String> {
        Err(unavailable("writing objects"))
    }

    fn commit_file(&self, _: &api::CheckinRequest) -> Result<api::Revision> {
        Err(unavailable("`pyn commit`"))
    }

    fn commit_changeset(&self, _: &api::ChangesetRequest) -> Result<api::ChangesetResponse> {
        Err(unavailable("`pyn commit`"))
    }

    fn path_history(&self, _: &str) -> Result<Vec<api::Revision>> {
        Err(unavailable("per-path revisions"))
    }

    fn repo_history(&self, _: Option<&str>, _: usize) -> Result<Vec<api::Revision>> {
        Err(unavailable("per-path revisions"))
    }

    fn restore(&self, _: &api::RestoreRequest) -> Result<api::Revision> {
        Err(unavailable("`pyn restore`"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_dir(at: &Path) {
        let git = at.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("config"), "[core]\n").unwrap();
    }

    fn server_workspace(at: &Path) {
        let state = at.join(".pyn/local_only/state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("files.toml"), "version = 2\n").unwrap();
    }

    fn is_git(at: &Path) -> bool {
        detect(at).unwrap().is_some()
    }

    #[test]
    fn a_git_directory_alone_is_a_git_home_and_nothing_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert!(!is_git(&root));
        git_dir(&root);
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        assert!(is_git(&root) && is_git(&root.join("a/b")));
    }

    #[test]
    fn a_pyn_folder_that_no_clone_filled_in_does_not_change_the_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git_dir(&root);
        std::fs::create_dir_all(root.join(".pyn/local_only")).unwrap();
        std::fs::write(
            root.join(".pyn/pyn.toml"),
            "[meta]\ndefault = \"exclusive\"\n",
        )
        .unwrap();
        assert!(is_git(&root));
    }

    #[test]
    fn a_pyn_server_workspace_beside_a_git_directory_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git_dir(&root);
        server_workspace(&root);
        assert!(!is_git(&root));
    }

    #[test]
    fn a_repository_setting_alone_does_not_make_a_server_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git_dir(&root);
        std::fs::create_dir_all(root.join(".pyn/local_only")).unwrap();
        std::fs::write(root.join(".pyn/local_only/config.toml"), "repo = \"a/b\"\n").unwrap();
        assert!(is_git(&root));
    }

    #[test]
    fn the_nearer_of_the_two_decides_when_they_are_in_different_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        git_dir(&root);
        let clone = root.join("clones/game");
        std::fs::create_dir_all(&clone).unwrap();
        server_workspace(&clone);
        assert!(
            !is_git(&clone),
            "a clone made inside a git repository is a pyn workspace"
        );
        assert!(is_git(&root.join("clones")));

        let outer = tempfile::tempdir().unwrap();
        let outer = outer.path().canonicalize().unwrap();
        server_workspace(&outer);
        let vendored = outer.join("vendor/lib");
        std::fs::create_dir_all(&vendored).unwrap();
        git_dir(&vendored);
        assert!(
            is_git(&vendored),
            "a git repository inside a pyn workspace is a git home"
        );
        assert!(!is_git(&outer));
    }

    fn marker(at: &Path, value: &str) {
        let dir = at.join(".pyn/local_only");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("repo.toml"), format!("repo_tool = \"{value}\"\n")).unwrap();
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    #[test]
    fn a_pyn_marker_makes_a_pyn_home_even_with_a_git_directory() {
        let (_keep, root) = root();
        git_dir(&root);
        marker(&root, "pyn");
        std::fs::create_dir_all(root.join("a")).unwrap();
        assert!(!is_git(&root) && !is_git(&root.join("a")));
    }

    #[test]
    fn a_git_marker_beats_a_server_workspace_in_the_same_folder() {
        let (_keep, root) = root();
        git_dir(&root);
        server_workspace(&root);
        marker(&root, "git");
        assert!(is_git(&root));
    }

    #[test]
    fn a_git_marker_without_a_git_directory_decides_nothing() {
        let (_keep, root) = root();
        marker(&root, "git");
        assert!(!is_git(&root));
        server_workspace(&root);
        assert!(!is_git(&root));
    }

    #[test]
    fn a_pyn_marker_alone_is_a_pyn_home_and_stops_the_search() {
        let (_keep, root) = root();
        git_dir(&root);
        let inner = root.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        marker(&inner, "pyn");
        assert!(!is_git(&inner) && is_git(&root));
    }

    #[test]
    fn the_nearest_folder_with_a_claim_wins_over_a_marker_above() {
        let (_keep, root) = root();
        git_dir(&root);
        marker(&root, "pyn");
        let inner = root.join("vendor/lib");
        std::fs::create_dir_all(&inner).unwrap();
        git_dir(&inner);
        assert!(is_git(&inner), "a nearer .git beats a pyn marker above");
        assert!(!is_git(&root.join("vendor")));

        let (_keep2, outer) = self::root();
        git_dir(&outer);
        marker(&outer, "git");
        let clone = outer.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        server_workspace(&clone);
        assert!(
            !is_git(&clone),
            "a nearer server workspace beats a git marker above"
        );
        assert!(is_git(&outer));
    }

    #[test]
    fn an_unknown_marker_value_is_an_error() {
        let (_keep, root) = root();
        git_dir(&root);
        marker(&root, "svn");
        let err = detect(&root).err().expect("an error").to_string();
        assert!(err.contains("repo.toml"), "{err}");
    }
}
