use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use pyn_proto as api;

mod address;
mod admin;
mod backend;
mod branch;
mod client;
mod commit_format;
mod credentials;
mod diff;
mod diff_color;
mod diff_opts;
mod diff_raw;
mod diffstat;
mod dirstat;
mod git_home;
mod http_transport;
mod init_pyn;
mod limits;
mod local_branches;
mod local_commit;
mod local_merge;
mod local_pick;
mod local_rebase;
mod local_reset;
mod local_store;
mod local_switch;
mod local_sync;
mod local_tags;
mod local_view;
mod localfile;
mod log_filter;
mod ls_remote;
mod merging;
mod migrate;
mod mv_plan;
mod no_index;
mod pathspec;
mod policy;
mod probe;
mod pyn_server;
mod quiet;
mod rename;
mod repo_tool;
mod rm_mv;
mod sanitize;
mod short_status;
mod snapshot;
mod staging;
mod stranded;
mod sync;
mod table;
mod time;
mod transfer;
mod transport;
mod unsupported;
mod userconfig;
mod verify;
mod webhook;
mod word_diff;
mod workspace;
mod xdiff;

use backend::Backend;
use client::Api;
use pyn_server::PynServer;
use transport::Method;
use workspace::{Settings, Workspace, resolve};

use userconfig::DEFAULT_SERVER;

/// pyn: version control that merges what can be merged and locks what shouldn't be.
#[derive(Parser)]
#[command(name = "pyn", version)]
struct Cli {
    /// The server to talk to; falls back to the workspace's setting, then your user setting, then localhost.
    #[arg(long, env = "PYN_SERVER")]
    server: Option<String>,
    /// API token to sign in with; defaults to the sign-in saved by `pyn login`. A value on the command line is visible in the process list: pass `-` to read the token from standard input, or use the PYN_TOKEN environment variable.
    #[arg(long, env = "PYN_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Dev identity sent as X-Pyn-User; only works against a server running with PYN_DEV_AUTH.
    #[arg(long, env = "PYN_USER")]
    user: Option<String>,
    /// The repository as owner/name; falls back to the workspace's setting, then your user setting.
    #[arg(long, env = "PYN_REPO", global = true)]
    repo: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in with a user name and password and remember it for this server.
    Login {
        username: String,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Forget the saved sign-in for this server and end that session.
    Logout,
    /// Create an account. Invite-only servers need --invite; open servers that verify addresses need --email.
    Register {
        username: String,
        #[arg(long)]
        invite: Option<String>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Change your own password.
    Password {
        /// Read the current password, then the new one, from standard input.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Download a repository into a new folder: from a pyn server, or from a git URL into a git repository.
    Clone {
        /// `<server>/owner/name` or `owner/name` for a pyn server; an `https://`, `ssh://` or `user@host:path` URL for git.
        source: String,
        /// Where to put it; defaults to a folder named after the repository.
        dir: Option<std::path::PathBuf>,
        /// Check out the repository as of this commit (`HEAD~N`, a commit hash) instead of the newest (a pyn server only).
        #[arg(long)]
        revision: Option<String>,
        /// Keep local commits in a store under `.pyn/local_only/store/` (the default on a pyn server; accepted for scripts).
        #[arg(long, conflicts_with = "no_local_store")]
        local_store: bool,
        /// Commit straight to the server, as before local commits (a pyn server only).
        #[arg(long)]
        no_local_store: bool,
        #[command(flatten)]
        git: Box<git_home::CloneOptions>,
    },
    /// Create, list and delete repositories.
    #[command(subcommand)]
    Repo(RepoCommand),
    /// Create, inspect and delete organizations, and manage who belongs to them.
    #[command(subcommand)]
    Org(OrgCommand),
    /// Create and manage an organization's teams, and grant them roles on its repositories.
    #[command(subcommand)]
    Team(TeamCommand),
    /// List, create, rename or delete branches.
    Branch(branch::BranchArgs),
    /// Switch the workspace to another branch.
    Switch(branch::SwitchArgs),
    /// Switch branches, or restore files from the workspace's revisions or from another commit (`pyn lock` takes a lock).
    Checkout(branch::CheckoutArgs),
    /// Merge a branch into the current branch.
    Merge(merging::MergeArgs),
    /// Create, list and delete tags; on a pyn server they stay in the local store and are never pushed.
    Tag(git_home::TagArgs),
    /// Save, list and restore local changes (a git repository only).
    Stash(git_home::StashArgs),
    /// Manage linked work trees of a repository (a git repository only).
    Worktree(git_home::WorktreeArgs),
    /// Remove untracked files from the work tree (a git repository only).
    Clean(git_home::CleanArgs),
    /// Search the work tree, the index (--cached) or revisions for a pattern, as `git grep` (a git repository only).
    Grep(git_home::GrepArgs),
    /// Find the commit that introduced a change by binary search, as `git bisect` (a git repository only).
    Bisect(git_home::BisectArgs),
    /// Show what commit last changed each line of a file, as `git blame` (a git repository only).
    Blame(git_home::BlameArgs),
    /// Like `blame`, in `git annotate`'s layout (a git repository only).
    Annotate(git_home::BlameArgs),
    /// Show, check and prune the reflog (a git repository only).
    Reflog(git_home::ReflogArgs),
    /// Reapply the current branch's commits on top of another base (a git repository only).
    Rebase(git_home::RebaseArgs),
    /// Convert a workspace made before local commits to one that keeps them (a pyn server only).
    Migrate,
    /// Apply the changes of existing commits as new commits on the current branch.
    CherryPick(local_pick::PickArgs),
    /// Undo the changes of existing commits with new commits on the current branch.
    Revert(local_pick::RevertArgs),
    /// Drop unpublished local commits, keeping their changes staged (--soft) or in the files
    /// (default), or discarding them (--hard, asks for confirmation).
    Reset(local_reset::ResetArgs),
    /// List and edit remotes; a pyn server has one, `origin`, set by `pyn config`.
    Remote(git_home::RemoteArgs),
    /// Download objects and refs from a git remote, or the server's new commits into the local store.
    Fetch(git_home::FetchArgs),
    /// Fetch, then merge the result into the current branch.
    Pull(git_home::PullArgs),
    /// Upload commits and refs to a git remote, or publish local commits to the pyn server.
    Push(git_home::PushArgs),
    /// List the references in a remote git repository (ssh or smart HTTP; works anywhere).
    LsRemote(ls_remote::LsRemoteArgs),
    /// Show what differs between this workspace and the server.
    Status(short_status::StatusArgs),
    /// Start a pyn repository in a folder that is not tracked yet (`pyn convert` adopts a git repository).
    Init(init_pyn::InitArgs),
    /// Start a new repository used like git: a real `.git` written by pyn, plus pyn's own folder.
    InitGit(git_home::InitGitArgs),
    /// Adopt an existing git repository (hide `.pyn/` from git), or with `--home git` move a pyn server workspace to one, keeping its history.
    Convert(git_home::ConvertArgs),
    /// Fetch new and newer files; files with local changes are left alone.
    Update {
        /// Only these paths; everything if omitted.
        paths: Vec<String>,
        /// Bring the files to the repository as of this commit (`HEAD~N`, a commit hash), older or newer than the workspace.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Show changes between the workspace and a commit, between two commits, or since the last update.
    Diff {
        /// Up to two commits (`HEAD`, `HEAD~2`, `HEAD^`, a commit hash, or `a1b2c3d4..HEAD`), then paths.
        args: Vec<String>,
        /// Paths to limit the diff to, after `--`.
        #[arg(last = true)]
        paths: Vec<String>,
        #[command(flatten)]
        format: Box<DiffFormat>,
        /// Exit with 1 if there are differences, 0 if none.
        #[arg(long)]
        exit_code: bool,
        /// Print nothing; exit with 1 if there are differences.
        #[arg(short, long)]
        quiet: bool,
        /// Compare the staged changes with the last commit instead of the work tree with the staged changes.
        #[arg(long, alias = "staged")]
        cached: bool,
        /// Compare two paths on the file system, in or out of a repository; exits with 1 if they differ.
        #[arg(long)]
        no_index: bool,
        /// Print nothing; only the exit status says anything.
        #[arg(short = 's', long = "no-patch")]
        no_patch: bool,
    },
    /// Print the object ids revisions name, and facts about the repository (a git repository only).
    RevParse(git_home::RevParseArgs),
    /// Show the files in the index and the work tree (a git repository only).
    LsFiles(git_home::LsFilesArgs),
    /// List the contents of a tree object (a git repository only).
    LsTree(git_home::LsTreeArgs),
    /// Show an object's type, size or content (a git repository only).
    CatFile(git_home::CatFileArgs),
    /// List the refs and the ids they point at (a git repository only).
    ShowRef(git_home::ShowRefArgs),
    /// Summarize the history by author (a git repository only).
    Shortlog(git_home::ShortlogArgs),
    /// Name a commit after the nearest tag it descends from (a git repository only).
    Describe(git_home::DescribeArgs),
    /// Name a commit relative to the refs that reach it (a git repository only).
    NameRev(git_home::NameRevArgs),
    /// Write commits as patch emails (a git repository only).
    FormatPatch(git_home::FormatPatchArgs),
    /// Apply a patch to the files and optionally the index (a git repository only).
    Apply(git_home::ApplyArgs),
    /// Apply patch emails from a mailbox and commit them (a git repository only).
    Am(git_home::AmArgs),
    /// Show a commit: its message and what it changed (the newest commit if none is named).
    Show {
        /// Commits (`HEAD`, `HEAD~N`, `HEAD^` or a hash of at least 4 digits), then paths; a git repository takes several.
        args: Vec<String>,
        /// Paths to limit the diff to, after `--`.
        #[arg(last = true)]
        paths: Vec<String>,
        /// Print only the commit, not its diff.
        #[arg(short = 's', long = "no-patch")]
        no_patch: bool,
        /// Diff a merge against its first parent unless an option says otherwise.
        #[arg(long)]
        first_parent: bool,
        #[command(flatten)]
        merges: git_home::MergeArgs,
        #[command(flatten)]
        fmt: commit_format::FormatArgs,
        #[command(flatten)]
        decorate: Box<git_home::DecorateArgs>,
        #[command(flatten)]
        format: Box<DiffFormat>,
    },
    /// Read and write settings, per workspace or for your user.
    #[command(subcommand)]
    Config(ConfigCommand),
    #[command(external_subcommand)]
    External(Vec<OsString>),
    /// Show who you are signed in as and, in a repository, what you may do there.
    Whoami,
    /// Link SSH public keys to your account.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Create, list and revoke invitations.
    #[command(subcommand)]
    Invite(InviteCommand),
    /// Add accounts to the current repository (needs manage_users).
    #[command(subcommand)]
    User(UserCommand),
    /// List live locks in the repository, or with --mine yours in every repository.
    Locks {
        #[arg(long)]
        mine: bool,
    },
    /// List files with their mode, head revision and lock.
    Files,
    /// List a folder of the repository (the root by default) with each entry's mode, last change and lock.
    Ls { path: Option<String> },
    /// Show the repository summary: counts, locks and recent activity.
    Summary,
    /// Take the lock on an exclusive file.
    Lock {
        path: String,
        /// Revision your copy is at (omit if you have none).
        #[arg(long)]
        base: Option<u64>,
    },
    /// Give up your lock without committing; with --force, remove someone else's (needs force_unlock and a reason).
    Unlock {
        path: String,
        /// Remove another user's lock instead of giving up your own.
        #[arg(long, requires = "reason")]
        force: bool,
        /// Why the lock is removed; recorded in the audit log.
        #[arg(long, requires = "force")]
        reason: Option<String>,
    },
    /// Stage changes for the next commit: paths, folders or `.`. Exclusive paths need your lock first.
    Add {
        paths: Vec<String>,
        /// Stage changes to files that are already tracked, not new files.
        #[arg(short = 'u', long)]
        update: bool,
        /// Stage new, changed and deleted files; with no path, in the whole repository.
        #[arg(short = 'A', long, conflicts_with = "update")]
        all: bool,
        /// Show what would be staged without staging it.
        #[arg(short = 'n', long)]
        dry_run: bool,
        #[arg(short, long)]
        verbose: bool,
        /// Stage files that ignore rules hide.
        #[arg(short, long)]
        force: bool,
        /// Choose the hunks to stage, one at a time (a git repository only).
        #[arg(short, long)]
        patch: bool,
        /// Choose what to stage from a menu (a git repository only).
        #[arg(short, long)]
        interactive: bool,
        /// Record new files as to be added later, with empty content in the index (a git repository only).
        #[arg(short = 'N', long)]
        intent_to_add: bool,
    },
    /// Remove files from the work tree and stage the removal (`--cached` keeps the files). Exclusive paths need your lock.
    Rm(rm_mv::RmArgs),
    /// Rename or move files and folders; on a pyn server the move is staged as a deletion and an addition.
    Mv(mv_plan::MvArgs),
    /// Commit the staged changes as one unit of work (or the named paths as they are). On a pyn server the commit is local
    /// until `pyn push` (a `--no-local-store` workspace publishes at once); publishing releases their locks.
    Commit {
        /// Commit only these paths, as they are on disk, instead of the staged changes.
        paths: Vec<String>,
        /// Local file to upload for a single path; in a workspace, the file at the path by default. On a git
        /// repository it is the commit message file, as `-F`.
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        #[arg(long)]
        base: Option<u64>,
        /// The commit message; repeat for more paragraphs. A merge in progress has its own.
        #[arg(short, long)]
        message: Vec<String>,
        /// Stage every changed tracked file first, as in git; new files are not included.
        #[arg(short = 'a', long)]
        all: bool,
        #[command(flatten)]
        git: git_home::GitCommitArgs,
    },
    /// Fetch a file's content at a revision (the head if omitted).
    Get {
        path: String,
        #[arg(long)]
        rev: Option<u64>,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// Inspect policy changes before committing them.
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Make an older revision the new head (needs the lock and the restore permission, asks for confirmation), or
    /// with --staged take paths out of the staged changes.
    Restore {
        /// A path and its older revision (or use --source), or with --staged the paths to unstage.
        #[arg(required_unless_present = "patch")]
        args: Vec<String>,
        /// The older revision to restore from, as in `git restore --source`; with --staged on a git repository, a commit.
        #[arg(short = 's', long)]
        source: Option<String>,
        /// Unstage the paths instead: put the staged changes back to the last commit.
        #[arg(short = 'S', long)]
        staged: bool,
        /// Restore the work tree files (with --staged, both); a git repository only.
        #[arg(short = 'W', long)]
        worktree: bool,
        /// Take the current branch's version of unmerged paths (a git repository only).
        #[arg(long, overrides_with = "theirs")]
        ours: bool,
        /// Take the merged-in version of unmerged paths (a git repository only).
        #[arg(long, overrides_with = "ours")]
        theirs: bool,
        /// Recreate the conflicted merge of unmerged paths (a git repository only).
        #[arg(long)]
        merge: bool,
        /// Choose the hunks to restore, one at a time (a git repository only).
        #[arg(short, long, conflicts_with_all = ["ours", "theirs", "merge", "message", "yes"])]
        patch: bool,
        #[arg(short, long)]
        message: Option<String>,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Show the audit log, newest first (needs the view_audit permission).
    Audit {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        /// An action such as checkout (taking a lock), release, restore, force_unlock, member_added, role_changed, token_created or repo_updated.
        #[arg(long)]
        action: Option<String>,
        /// Show events older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show a path's revisions (oldest first), or without a path the repository's, newest first.
    #[command(visible_alias = "history")]
    Log {
        path: Option<String>,
        /// Only paths matching this glob; without a slash it matches at any depth (`*.ts`, `Source/**`).
        #[arg(long, conflicts_with = "path")]
        filter: Option<String>,
        /// Show at most this many revisions (50 for the repository view, all for a path).
        #[arg(short = 'n', long = "max-count", visible_alias = "limit")]
        limit: Option<usize>,
        /// List commits instead of revisions.
        #[arg(long, conflicts_with_all = ["path", "filter"])]
        commits: bool,
        #[command(flatten)]
        git: Box<git_home::GitLogArgs>,
    },
    /// Check that history is intact: every revision's hash and its link to the one before, and the audit log's chain.
    ///
    /// Prints one line per break and exits 1 if there is any, like `git fsck`. The audit log is checked when you have
    /// the view_audit permission and no path is given.
    Verify {
        /// Check only this path.
        path: Option<String>,
        /// Also print what was checked.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Manage your API tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Run first-run setup on a new server: creates the first administrator and records the essentials.
    Setup {
        /// The first administrator's user name.
        username: String,
        /// The one-time setup token from the server's log or its PYN_SETUP_TOKEN setting. A value on the command line is visible in the process list: prefer the environment variable.
        #[arg(long, env = "PYN_SETUP_TOKEN", hide_env_values = true)]
        setup_token: Option<String>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
        /// The address people reach the web app at; verification links use it.
        #[arg(long)]
        public_url: Option<String>,
        /// `open`, `invite` or `closed`; the server's configured default if omitted.
        #[arg(long)]
        registration: Option<String>,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Server administration (server administrators and service credentials only).
    #[command(subcommand)]
    Admin(admin::AdminCommand),
    /// Show what you or an organization you own use on the server, and any limits that apply.
    Usage {
        /// A user or organization; you by default.
        owner: Option<String>,
    },
    /// List or change who has which role.
    #[command(subcommand)]
    Member(MemberCommand),
    /// List or change what each role grants.
    #[command(subcommand)]
    Role(RoleCommand),
}

#[derive(clap::Args, Debug)]
struct DiffFormat {
    /// Lines of context around each change.
    #[arg(short = 'U', long = "unified", value_name = "N", default_value_t = 3)]
    context: usize,
    /// Show a diffstat instead of the patch, optionally `=<width>[,<name-width>[,<count>]]`.
    #[arg(long, value_name = "WIDTHS", num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with_all = ["name_only", "name_status", "check"])]
    stat: Option<String>,
    /// Show only the totals line of the diffstat.
    #[arg(long, conflicts_with_all = ["stat", "name_only", "name_status", "check"])]
    shortstat: bool,
    /// Show added and deleted line counts per file.
    #[arg(long, conflicts_with_all = ["stat", "shortstat", "name_only", "name_status", "check"])]
    numstat: bool,
    /// Report trailing whitespace and space before tab in added lines; exit with 2 if any.
    #[arg(long, conflicts_with_all = ["name_only", "name_status"])]
    check: bool,
    /// Show only the names of changed files.
    #[arg(long, conflicts_with = "name_status")]
    name_only: bool,
    /// Show the names and the kind of change (A, M, D, and R or C with a similarity) of changed files.
    #[arg(long)]
    name_status: bool,
    /// Show creations, deletions, renames and mode changes.
    #[arg(long, conflicts_with = "check")]
    summary: bool,
    #[command(flatten)]
    renames: rename::Flags,
    #[command(flatten)]
    pub(crate) extra: diff_opts::OutputFlags,
}

impl DiffFormat {
    /// Whether `--stat` or an option that implies it was given.
    pub(crate) fn stat_given(&self) -> bool {
        self.stat.is_some() || self.extra.stat_implied() || self.extra.patch_with_stat()
    }

    fn format(&self) -> diff::Format {
        if self.stat.is_none()
            && self.extra.stat_implied()
            && (self.shortstat
                || self.numstat
                || self.name_only
                || self.name_status
                || self.check
                || self.extra.raw())
        {
            usage_error("a --stat option does not combine with another output format");
        }
        if self.extra.dirstat_given()
            && (self.stat_given()
                || self.shortstat
                || self.numstat
                || self.name_only
                || self.name_status
                || self.check
                || self.summary
                || self.extra.raw())
        {
            usage_error("`--dirstat` together with another output format is not built");
        }
        let mut format = self.build();
        if self.extra.patch() && format.kind != diff::Kind::Patch {
            match format.kind {
                diff::Kind::Summary => format.kind = diff::Kind::Patch,
                diff::Kind::Stat
                | diff::Kind::ShortStat
                | diff::Kind::NumStat
                | diff::Kind::Raw
                | diff::Kind::Dirstat => format.with_patch = true,
                _ => usage_error("`-p` together with another output format is not built"),
            }
        }
        format
    }

    fn build(&self) -> diff::Format {
        let kind = if self.extra.dirstat_given() {
            diff::Kind::Dirstat
        } else if self.extra.raw() || self.extra.patch_with_raw() {
            diff::Kind::Raw
        } else if self.check {
            diff::Kind::Check
        } else if self.stat_given() || self.extra.patch_with_stat() {
            diff::Kind::Stat
        } else if self.shortstat {
            diff::Kind::ShortStat
        } else if self.numstat {
            diff::Kind::NumStat
        } else if self.name_only {
            diff::Kind::NameOnly
        } else if self.name_status {
            diff::Kind::NameStatus
        } else if self.summary {
            diff::Kind::Summary
        } else {
            diff::Kind::Patch
        };
        diff::Format {
            context: self.context,
            kind,
            opts: self.extra.opts(),
            renames: self.renames.request(),
            summary: self.summary,
            with_patch: false,
            kept: Vec::new(),
        }
    }
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Create a repository in your namespace or an organization you own; you become its admin.
    Create {
        /// `name`, or `owner/name` where owner is you or an organization you own.
        name: String,
        /// `public` or `private`; private by default.
        #[arg(long)]
        visibility: Option<String>,
        /// How long a lock lasts before it expires unless renewed.
        #[arg(long)]
        lease_hours: Option<u32>,
        /// Locks one user may hold at once; the server's default if omitted, and a limit in the policy file wins.
        #[arg(long)]
        max_locks: Option<u32>,
        /// Use this file as the repository's first `.pyn/pyn.toml` instead of the default (everything exclusive).
        #[arg(long, conflicts_with = "no_policy")]
        policy: Option<PathBuf>,
        /// Create the repository without a `.pyn/pyn.toml`; the server's fallback policy applies until one is added.
        #[arg(long)]
        no_policy: bool,
    },
    /// List the repositories you belong to.
    List {
        /// Only this owner's.
        #[arg(long)]
        owner: Option<String>,
    },
    /// List public repositories on the server, including ones you have no role in; works signed out.
    Explore {
        /// At most this many repositories.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Make a repository public (anyone may read it) or private (members only). Owner and admin only.
    Visibility {
        /// `owner/name`.
        name: String,
        /// `public` or `private`.
        visibility: String,
    },
    /// Show the stored bytes, files and revisions of a repository.
    Usage {
        /// `owner/name`.
        name: String,
    },
    /// Delete a repository with its files, history, locks and access. Its audit log is kept.
    Delete {
        /// `owner/name`.
        name: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum OrgCommand {
    /// Create an organization; you become its first owner.
    Create { name: String },
    /// List the organizations you belong to.
    List,
    /// Show an organization, and its members if you belong to it.
    Show { name: String },
    /// Delete an organization that owns no repositories. Its audit log is kept.
    Delete {
        name: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Show an organization's audit log, newest first (owners only).
    Audit {
        name: String,
        /// Only entries older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List and change an organization's members.
    #[command(subcommand)]
    Member(OrgMemberCommand),
    /// Show and change who may create repositories in an organization (owners only).
    #[command(subcommand)]
    Policy(OrgPolicyCommand),
}

#[derive(Subcommand)]
enum OrgPolicyCommand {
    /// Show the base setting for members and the rules.
    Show { org: String },
    /// Set what members may create when no rule applies.
    Set {
        org: String,
        /// `none`, `private` or `both`.
        #[arg(long)]
        members: String,
    },
    /// Let a team, user or role create repositories.
    Allow {
        org: String,
        /// `team`, `user` or `role`.
        kind: String,
        /// A team slug, a user name, or `owner` or `member` for a role.
        subject: String,
        /// `public`, `private` or `both`.
        #[arg(long, default_value = "both")]
        scope: String,
    },
    /// Stop a team, user or role from creating repositories; beats any allow.
    Deny {
        org: String,
        kind: String,
        subject: String,
        #[arg(long, default_value = "both")]
        scope: String,
    },
    /// Remove an allow or deny rule.
    Remove {
        org: String,
        /// `allow` or `deny`.
        effect: String,
        kind: String,
        subject: String,
    },
}

#[derive(Subcommand)]
enum OrgMemberCommand {
    /// List an organization's members.
    List { org: String },
    /// Add an existing account to an organization (owners only).
    Add {
        org: String,
        user: String,
        /// `owner` or `member`; member by default.
        #[arg(long)]
        role: Option<String>,
    },
    /// Change a member's role to `owner` or `member` (owners only).
    Set {
        org: String,
        user: String,
        role: String,
    },
    /// Remove a member, or leave by naming yourself. Drops their direct roles on the organization's repositories.
    Remove { org: String, user: String },
}

#[derive(Subcommand)]
enum TeamCommand {
    /// Create a team (organization owners only).
    Create {
        /// `<org>/<team>`; the team is 1 to 39 lowercase letters, digits, `-` or `_`.
        team: String,
        /// Display name; the team name by default.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// List an organization's teams.
    List { org: String },
    /// Show a team with its members and the repositories it holds roles in.
    Show { team: String },
    /// Delete a team, its memberships and its roles on repositories (owners only).
    Delete {
        team: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Add or remove team members.
    #[command(subcommand)]
    Member(TeamMemberCommand),
    /// Give a team a role on one of the organization's repositories.
    Grant {
        team: String,
        /// `owner/name` of a repository the organization owns.
        repo: String,
        #[arg(long)]
        role: String,
    },
    /// Take a team's role on a repository away.
    Revoke { team: String, repo: String },
    /// List the teams that hold a role in a repository.
    Access {
        /// `owner/name`; the current repository if omitted.
        repo: Option<String>,
    },
}

#[derive(Subcommand)]
enum TeamMemberCommand {
    /// Add an organization member to a team (owners only).
    Add { team: String, user: String },
    /// Remove a user from a team (owners only).
    Remove { team: String, user: String },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Ask for the user settings and write them to the user configuration file.
    Init,
    /// Show the settings that are set (the effective values unless --global or --local is given).
    List {
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        local: bool,
    },
    /// Show one setting.
    Get {
        key: String,
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        local: bool,
    },
    /// Set one setting for this workspace, or for your user with --global.
    Set {
        key: String,
        value: String,
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Show which paths would change mode, and which locks would be released, if this policy file were committed.
    Preview {
        /// A proposed `pyn.toml`.
        file: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token. It is shown once.
    Create {
        name: String,
        /// Comma-separated permissions, for example read,lock,checkin.
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
        /// Comma-separated repositories (owner/name) the token is valid for; the current repository by default.
        #[arg(long, value_delimiter = ',')]
        repos: Vec<String>,
        /// Expire the token after this many days.
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// List your tokens, or another user's with --for (needs manage_users in a repository they belong to).
    List {
        #[arg(long = "for")]
        for_user: Option<String>,
    },
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Subcommand)]
enum KeyCommand {
    /// Add a public key. Without a file, uses ~/.ssh/id_ed25519.pub, id_ecdsa.pub or id_rsa.pub, whichever exists first.
    Add {
        /// A public key file (a `.pub` file).
        file: Option<std::path::PathBuf>,
        /// A name for the key; defaults to the comment in the key.
        #[arg(long)]
        title: Option<String>,
    },
    /// List your keys, or another user's with --for (needs manage_users in a repository they belong to).
    List {
        #[arg(long = "for")]
        for_user: Option<String>,
    },
    /// Remove a key by id.
    Remove {
        id: String,
        #[arg(long = "for")]
        for_user: Option<String>,
    },
}

#[derive(Subcommand)]
enum InviteCommand {
    /// Create a one-time invitation code. It is shown once.
    Create {
        #[arg(long, default_value = "reader")]
        role: String,
        /// How long the invitation can be used.
        #[arg(long, default_value_t = 48)]
        hours: i64,
    },
    List,
    Revoke {
        id: String,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Add an account with an initial password and a role.
    Add {
        username: String,
        #[arg(long, default_value = "reader")]
        role: String,
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
enum MemberCommand {
    List,
    /// Give a user a role, adding them if they are new.
    Set {
        user: String,
        role: String,
    },
}

#[derive(Subcommand)]
enum RoleCommand {
    List,
    /// Replace what a role grants.
    Set {
        role: String,
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
    },
}

fn fatal_message(error: &anyhow::Error) -> String {
    let mut message = String::new();
    let mut previous = String::new();
    for cause in error.chain() {
        let detail = cause.to_string();
        // A wrapper's Display may already embed its source as `{source}` or `<context>: {source}`.
        let embedded = previous == detail || previous.ends_with(&format!(": {detail}"));
        if !embedded {
            if !message.is_empty() {
                message.push_str(": ");
            }
            message.push_str(&detail);
        }
        previous = detail;
    }
    message
}

fn main() {
    if let Some(name) = subcommand_refused_on_git() {
        eprintln!("{}", unsupported::message_on_git(&name));
        std::process::exit(1);
    }
    if let Some(name) = first_subcommand()
        && unsupported::REFUSED_OFF_GIT.contains(&name.as_str())
        && !in_git_home()
        && !in_pyn_workspace()
    {
        eprintln!("fatal: not a git repository (or any of the parent directories): .git");
        std::process::exit(128);
    }
    let cli = match Cli::try_parse_from(attached_gpg_keys(attached_pull_rebase(counted_log(
        numbered_commits(rename::attached(std::env::args_os().collect())),
    )))) {
        Ok(cli) => cli,
        Err(e) => {
            let code = match (e.use_stderr(), fatal_option_error(&e)) {
                (false, _) => 0,
                (true, true) => 128,
                (true, false) => 129,
            };
            let _ = e.print();
            std::process::exit(code);
        }
    };
    if let Command::Add {
        paths,
        update: false,
        all: false,
        patch: false,
        interactive: false,
        ..
    } = &cli.command
        && paths.is_empty()
        && (in_git_home() || in_pyn_workspace())
    {
        eprintln!("Nothing specified, nothing added.");
        eprintln!("hint: Maybe you wanted to say 'git add .'?");
        eprintln!("hint: Turn this message off by running");
        eprintln!("hint: \"git config advice.addEmptyPathspec false\"");
        std::process::exit(0);
    }
    if let Command::Rm(args) = &cli.command
        && args.paths.is_empty()
    {
        eprintln!("fatal: No pathspec was given. Which files should I remove?");
        std::process::exit(128);
    }
    if let Command::External(args) = &cli.command {
        let name = args[0].to_string_lossy();
        let message = if in_git_home() {
            unsupported::message_on_git(&name)
        } else {
            unsupported::message(&name)
        };
        eprintln!("{message}");
        std::process::exit(1);
    }
    if let Err(e) = run(cli) {
        if let Some(refused) = e.downcast_ref::<git_home::Refused>() {
            eprintln!("error: {}", sanitize::text(&refused.to_string()));
            std::process::exit(1);
        }
        if let Some(exits) = e.downcast_ref::<git_home::Exits>() {
            eprintln!("error: {}", sanitize::text(&exits.1));
            std::process::exit(exits.0);
        }
        if let Some(rejected) = e.downcast_ref::<local_sync::Rejected>() {
            eprintln!("{}", sanitize::text(&rejected.to_string()));
            std::process::exit(1);
        }
        if let Some(rejected) = e.downcast_ref::<local_sync::LockRejected>() {
            eprintln!("{}", sanitize::text(&rejected.to_string()));
            std::process::exit(128);
        }
        eprintln!("fatal: {}", sanitize::text(&fatal_message(&e)));
        std::process::exit(128);
    }
}

/// Whether git reports this parser error as fatal (128) rather than as a usage error (129).
fn fatal_option_error(e: &clap::Error) -> bool {
    use clap::error::ErrorKind::{
        InvalidSubcommand, InvalidValue, UnknownArgument, ValueValidation,
    };
    let kind = e.kind();
    match first_subcommand().as_deref() {
        Some("log" | "show") => matches!(kind, InvalidValue | ValueValidation),
        Some("stash") => {
            let word = e.get(clap::error::ContextKind::InvalidArg);
            matches!(kind, UnknownArgument | InvalidSubcommand)
                && word.is_some_and(|w| !w.to_string().starts_with('-'))
        }
        _ => false,
    }
}

fn in_pyn_workspace() -> bool {
    std::env::current_dir()
        .ok()
        .is_some_and(|cwd| Workspace::discover(&cwd).is_some())
}

fn in_git_home() -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| git_home::detect(&cwd).ok().flatten())
        .is_some()
}

/// Rewrites `format-patch -<n>` to `--count=<n>`, since a number is not a clap flag name.
fn numbered_commits(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let Some(at) = args.iter().skip(1).position(|a| a == "format-patch") else {
        return args;
    };
    let mut seen_dashes = false;
    args.into_iter()
        .enumerate()
        .map(|(i, a)| {
            let text = a.to_string_lossy();
            seen_dashes |= i > at + 1 && text == "--";
            match text.strip_prefix('-') {
                Some(n)
                    if i > at + 1
                        && !seen_dashes
                        && !n.is_empty()
                        && n.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    format!("--count={n}").into()
                }
                _ => a,
            }
        })
        .collect()
}

/// Rewrites `log -<n>` to `--max-count=<n>`; clap has no option named by digits.
fn counted_log(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let Some(at) = args
        .iter()
        .skip(1)
        .position(|a| a == "log" || a == "history")
        .map(|i| i + 1)
    else {
        return args;
    };
    let mut ended = false;
    let rewritten = args.iter().enumerate().map(|(i, arg)| {
        let text = arg.to_string_lossy();
        ended |= i > at && text == "--";
        match text.strip_prefix('-') {
            Some(n)
                if i > at && !ended && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) =>
            {
                format!("--max-count={n}").into()
            }
            _ => arg.clone(),
        }
    });
    rewritten.collect()
}

/// Rewrites `pull -r<mode>` to `--rebase=<mode>`; clap takes an optional value only after `=`.
fn attached_pull_rebase(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let Some(at) = args.iter().skip(1).position(|a| a == "pull").map(|i| i + 1) else {
        return args;
    };
    let mut out = args[..=at].to_vec();
    let mut rest = args[at + 1..].iter();
    while let Some(arg) = rest.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "--" => {
                out.push(arg.clone());
                out.extend(rest.by_ref().cloned());
            }
            "-s" | "-X" | "--strategy" | "--strategy-option" => {
                out.push(arg.clone());
                out.extend(rest.next().cloned());
            }
            t if t.len() > 2 && t.starts_with("-r") => {
                out.push(format!("--rebase={}", &t[2..]).into());
            }
            _ => out.push(arg.clone()),
        }
    }
    out
}

/// Rewrites `-S<keyid>` to `--gpg-sign=<keyid>` on the commands that sign; clap takes an optional value only after `=`.
fn attached_gpg_keys(args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    const SIGNING: [&str; 7] = [
        "commit",
        "merge",
        "pull",
        "cherry-pick",
        "revert",
        "rebase",
        "am",
    ];
    let Some((at, valued)) = args.iter().enumerate().skip(1).find_map(|(i, a)| {
        let name = SIGNING.iter().find(|n| a == **n)?;
        let valued: &[&str] = match *name {
            "commit" => &["-m", "-F", "--message", "--file", "--author", "--date"],
            "merge" => &[
                "-m",
                "-F",
                "--message",
                "-s",
                "-X",
                "--strategy",
                "--strategy-option",
            ],
            "pull" => &["-s", "-X", "--strategy", "--strategy-option"],
            "rebase" => &[
                "-s",
                "-X",
                "-x",
                "--exec",
                "--onto",
                "--strategy",
                "--strategy-option",
            ],
            "am" => &[
                "-p",
                "--directory",
                "--whitespace",
                "-C",
                "--patch-format",
                "--exclude",
                "--include",
            ],
            _ => &["-m", "--mainline", "-X", "--strategy-option", "--strategy"],
        };
        Some((i, valued))
    }) else {
        return args;
    };
    let mut out = args[..=at].to_vec();
    let mut rest = args[at + 1..].iter();
    while let Some(arg) = rest.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "--" => {
                out.push(arg.clone());
                out.extend(rest.by_ref().cloned());
            }
            t if valued.contains(&t) => {
                out.push(arg.clone());
                out.extend(rest.next().cloned());
            }
            t if t.len() > 2 && t.starts_with("-S") => {
                out.push(format!("--gpg-sign={}", &t[2..]).into());
            }
            _ => out.push(arg.clone()),
        }
    }
    out
}

/// The subcommand named on the command line, before any argument parsing.
fn first_subcommand() -> Option<String> {
    let mut args = std::env::args().skip(1);
    loop {
        let arg = args.next()?;
        match arg.as_str() {
            "--server" | "--token" | "--user" | "--repo" => {
                args.next();
            }
            a if a.starts_with('-') => {}
            _ => return Some(arg),
        }
    }
}

/// The subcommand named on the command line when a git repository cannot take it, so its arguments are never parsed.
fn subcommand_refused_on_git() -> Option<String> {
    let name = first_subcommand()?;
    (unsupported::REFUSED_BEFORE_PARSING.contains(&name.as_str()) && in_git_home()).then_some(name)
}

/// Commands that address a server through the global config, so a git repository does not stop them.
fn runs_anywhere(command: &Command) -> bool {
    matches!(
        command,
        Command::Login { .. }
            | Command::Logout
            | Command::Register { .. }
            | Command::Password { .. }
            | Command::Key(_)
            | Command::Token(_)
            | Command::User(_)
            | Command::Org(_)
            | Command::Team(_)
            | Command::Repo(_)
            | Command::Usage { .. }
            | Command::Setup { .. }
            | Command::Admin(_)
            | Command::Whoami
            | Command::Member(_)
            | Command::Role(_)
            | Command::Invite(_)
            | Command::LsRemote(_)
    )
}

/// Clones from a git remote when the address is one; `None` leaves it to the pyn server clone.
fn clone_from_git(cli: &Cli, cwd: &std::path::Path) -> Result<Option<()>> {
    let Command::Clone {
        source,
        dir,
        revision,
        local_store,
        no_local_store,
        git,
    } = &cli.command
    else {
        return Ok(None);
    };
    let hint = match address::kind(source)? {
        address::Kind::Git => None,
        address::Kind::Pyn(_) => return Ok(None),
        address::Kind::Http(src) => {
            let server = src.server.as_deref().expect("an http source has a server");
            match probe::decide(probe::run(server)) {
                probe::Host::Pyn => return Ok(None),
                probe::Host::Git { inconclusive } => inconclusive.map(|why| {
                    format!(
                        "{server} gave no answer as a pyn server ({why}); it was tried as a git remote. \
                         If it is a pyn server, check that it is running"
                    )
                }),
            }
        }
    };
    if revision.is_some() {
        usage_error("`--revision` applies to a pyn server, not a git remote");
    }
    if *local_store || *no_local_store {
        usage_error(
            "`--local-store` and `--no-local-store` apply to a pyn server, not a git remote",
        );
    }
    git_home::clone(source, dir.as_deref(), cwd, git, hint.as_deref())?;
    Ok(Some(()))
}

fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    if clone_from_git(&cli, &cwd)?.is_some() {
        return Ok(());
    }
    if let Command::InitGit(args) = &cli.command {
        return git_home::init_git(args, &cwd);
    }
    if let Command::Convert(args) = &cli.command
        && args.home == Some(git_home::Home::Pyn)
    {
        eprintln!("fatal: moving to a pyn server home is not built yet (#572)");
        std::process::exit(128);
    }
    if let Command::Init(args) = &cli.command {
        let remote = init_pyn::Remote {
            server: cli.server.clone(),
            repo: cli.repo.clone(),
            token: cli.token.clone(),
            user: cli.user.clone(),
        };
        return init_pyn::init(args, &cwd, remote);
    }
    if let Command::Clone { git, .. } = &cli.command
        && let Some(flag) = git.first_given()
    {
        usage_error(&format!(
            "`{flag}` applies to a git remote, not a pyn server"
        ));
    }
    let git = if matches!(cli.command, Command::Clone { .. } | Command::Config(_)) {
        None
    } else {
        git_home::detect(&cwd)?
    };
    if let Command::LsRemote(args) = &cli.command {
        return ls_remote::run(args, git.as_ref());
    }
    if let Command::Diff {
        args,
        paths,
        format,
        quiet,
        cached,
        no_index,
        no_patch,
        ..
    } = &cli.command
        && (*no_index
            || (!cached
                && git.is_none()
                && no_index::are_paths(args.iter().chain(paths))
                && Workspace::discover(&cwd).is_none()))
    {
        let operands: Vec<String> = args.iter().chain(paths).cloned().collect();
        let shown = format.format().silenced(*no_patch);
        if no_index::run(&operands, &shown, *quiet, git.as_ref())? {
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Command::Commit { git: opts, .. } = &cli.command
        && git.is_none()
        && let Some(flag) = opts.first_given()
    {
        if flag == "--allow-empty" {
            usage_error(
                "`pyn commit --allow-empty` is not supported on a pyn server: it refuses a pushed commit \
                 that changes no file",
            );
        }
        usage_error(&format!(
            "`pyn commit {flag}` applies to a git repository, not a pyn server: commits there are immutable \
             and `--file` is the content file"
        ));
    }
    if git.is_none()
        && let Command::Commit {
            git: opts, paths, ..
        } = &cli.command
        && !paths.is_empty()
        && let Some(flag) = opts.author_flag()
    {
        usage_error(&format!(
            "`pyn commit {flag}` applies to staged changes committed to the local store, not to a path commit"
        ));
    }
    if git.is_none()
        && let Command::Commit { git: opts, .. } = &cli.command
        && opts.quiet
    {
        quiet::set();
    }
    let mut cli = cli;
    if git.is_none()
        && let Command::Commit {
            git: opts, message, ..
        } = &mut cli.command
        && let Some(text) = opts.server_message(message)?
    {
        *message = vec![text];
    }
    if let Command::Log { git: opts, .. } = &cli.command
        && git.is_some()
        && let Some(why) = opts.refused()
    {
        usage_error(&format!("`pyn log {why}"));
    }
    if let Command::Log {
        git: opts, path, ..
    } = &cli.command
        && git.is_none()
        && let Some(flag) = opts.server_unsupported().or_else(|| {
            log_filter::too_many_paths(path.as_deref(), opts, &cwd).then(|| "<path>".to_string())
        })
    {
        usage_error(&format!(
            "`pyn log {flag}` applies to a git repository, not a pyn server"
        ));
    }
    if let Command::Commit { message, .. } = &cli.command
        && message.is_empty()
        && git.is_none()
        && !Workspace::discover(&cwd)
            .is_some_and(|w| merging::in_progress(&w) && !merging::is_paused(&w))
    {
        usage_error("the following required arguments were not provided:\n  --message <MESSAGE>");
    }
    if let Some(git) = &git
        && !runs_anywhere(&cli.command)
    {
        return git_home::run(cli.command, git, &cwd);
    }
    let on_git = git.is_some();
    if let Command::Convert(args) = &cli.command {
        let server_workspace = Workspace::discover(&cwd).is_some();
        if args.home != Some(git_home::Home::Git) || !server_workspace {
            if args.home == Some(git_home::Home::Git) {
                eprintln!(
                    "fatal: `pyn convert --home git` moves a pyn server workspace to a git repository, and this is not one"
                );
            } else {
                eprintln!(
                    "fatal: `pyn convert` adopts an existing git repository, and this is not one (or any of the parent directories): .git"
                );
            }
            std::process::exit(128);
        }
    }
    let ws = if on_git {
        None
    } else {
        Workspace::discover(&cwd)
    };
    let user_config = userconfig::path()?;
    let (mine, mut yours) = (
        ws.as_ref().map(Workspace::settings).unwrap_or_default(),
        Settings::load(&user_config).unwrap_or_default(),
    );
    let source = match &cli.command {
        Command::Clone { source, .. } => Some(address::parse_source(source)?),
        _ => None,
    };
    let needs_server = !matches!(cli.command, Command::Config(_))
        && source.as_ref().is_none_or(|s| s.server.is_none());
    if needs_server
        && cli.server.is_none()
        && mine.server.is_none()
        && yours.server.is_none()
        && !user_config.exists()
    {
        yours = userconfig::first_run(
            &user_config,
            interactive(),
            &mut std::io::stdin().lock(),
            &mut std::io::stderr(),
        )?;
    }
    let server = source
        .as_ref()
        .and_then(|s| s.server.clone())
        .or_else(|| {
            resolve(
                cli.server,
                mine.server.as_ref(),
                yours.server.as_ref(),
                Some(DEFAULT_SERVER),
            )
        })
        .expect("there is a default");
    let repo = match &source {
        Some(s) => Some(s.repo.clone()),
        None => resolve(cli.repo, mine.repo.as_ref(), yours.repo.as_ref(), None),
    };
    let base = server.trim_end_matches('/').to_string();
    let token = credentials::token_for(cli.token, &base)?;
    if token.is_some()
        || matches!(
            cli.command,
            Command::Login { .. }
                | Command::Setup { .. }
                | Command::Register { .. }
                | Command::Password { .. }
        )
    {
        credentials::warn_if_cleartext(&base);
    }
    let repo_name = repo.clone();
    let server_url = base.clone();
    let store = ws.as_ref().and_then(local_store::LocalStore::open);
    let backend = PynServer::new(Api {
        transport: Box::new(http_transport::HttpTransport::new(base.clone())),
        base,
        token,
        user: resolve(cli.user, mine.user.as_ref(), yours.user.as_ref(), None),
        repo,
        branch: branch::api_branch(ws.as_ref()),
        on_git,
    })
    .with_store(store);
    let rp = |p: &str| -> Result<String> {
        match &ws {
            Some(w) => w.repo_path(&cwd, p),
            None => Ok(p.to_string()),
        }
    };
    let in_workspace = |what: &str| -> Result<&Workspace> {
        ws.as_ref().with_context(|| {
            format!("`pyn {what}` works inside a workspace; run `pyn clone <dir>` first")
        })
    };

    if let (Command::Commit { paths, .. }, Some(w)) = (&cli.command, &ws)
        && !paths.is_empty()
        && merging::in_progress(w)
    {
        bail!(
            "you have not concluded your merge, so paths cannot be committed alone; \
             stage what you resolved with `pyn add` and run `pyn commit`, or give the merge up with `pyn merge --abort`"
        );
    }

    if let Some(w) = &ws {
        match &cli.command {
            Command::Update { .. } => branch::require_published(w, "update")?,
            Command::Commit { paths, .. } if !paths.is_empty() => {
                branch::require_published(w, "commit <path>")?
            }
            _ => {}
        }
    }

    match cli.command {
        Command::External(_) => unreachable!("handled before run"),
        Command::Init(_) => {
            unreachable!("handled before the server is consulted")
        }
        Command::InitGit(_) => unreachable!("handled before the server is consulted"),
        Command::Convert(args) => git_home::to_git(&backend, in_workspace("convert")?, &args)?,
        Command::LsRemote(_) => unreachable!("handled before the server is consulted"),
        Command::Login {
            username,
            password_stdin,
        } => {
            let api = backend.pyn_server("login")?;
            let password = credentials::read_password("Password: ", password_stdin)?;
            let body = api::LoginRequest {
                username: username.clone(),
                password,
            };
            let made: api::CreatedToken = api
                .send(api.anonymous(Method::POST, "/v1/login").json(&body))?
                .json()?;
            credentials::save(
                &api.base,
                credentials::Entry {
                    user: username.clone(),
                    token: made.token,
                },
            )?;
            let until = made
                .info
                .expires_at
                .map_or("never".to_string(), time::local);
            println!(
                "signed in as {username} on {}; the sign-in expires {until}",
                api.base
            );
        }
        Command::Logout => {
            let api = backend.pyn_server("logout")?;
            let Some(saved) = credentials::load(&api.base)? else {
                println!("not signed in on {}", api.base);
                return Ok(());
            };
            if let Some(id) = credentials::token_id(&saved.token) {
                let _ = api.send(
                    api.request(Method::DELETE, &format!("/v1/tokens/{id}"))
                        .bearer_auth(&saved.token),
                );
            }
            credentials::remove(&api.base)?;
            println!("signed out {} from {}", saved.user, api.base);
        }
        Command::Register {
            username,
            invite,
            email,
            password_stdin,
        } => {
            let api = backend.pyn_server("register")?;
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::RegisterRequest {
                username: username.clone(),
                password,
                invite,
                email,
            };
            let done: api::Registered = api
                .send(api.anonymous(Method::POST, "/v1/register").json(&body))?
                .json()?;
            match done.status.as_str() {
                "pending_verification" => println!(
                    "created {username}; follow the link in the email sent to you, then sign in"
                ),
                "pending_approval" => {
                    println!(
                        "created {username}; an administrator must approve it before you can sign in"
                    )
                }
                _ => println!("created {username}; sign in with `pyn login {username}`"),
            }
        }
        Command::Password { password_stdin } => {
            let api = backend.pyn_server("password")?;
            let current = credentials::read_password("Current password: ", password_stdin)?;
            let new = credentials::read_new_password(password_stdin)?;
            let body = api::ChangePasswordRequest {
                current: Some(current),
                new,
            };
            api.send(api.request(Method::PUT, "/v1/me/password").json(&body))?;
            println!("password changed");
            report_ended_sign_in(api)?;
        }
        Command::Key(KeyCommand::Add { file, title }) => {
            let api = backend.pyn_server("key")?;
            let file = file.map_or_else(default_public_key, Ok)?;
            let key = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let body = api::AddKeyRequest { title, key };
            let added: api::SshKeyInfo = api
                .send(api.request(Method::POST, "/v1/keys").json(&body))?
                .json()?;
            println!(
                "added {} ({}) as {}",
                added.title, added.fingerprint, added.id
            );
        }
        Command::Key(KeyCommand::List { for_user }) => {
            let api = backend.pyn_server("key")?;
            let mut req = api.get("/v1/keys");
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            let keys: Vec<api::SshKeyInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = keys
                .into_iter()
                .map(|k| {
                    let used = k.last_used_at.map_or("never".to_string(), time::local);
                    vec![k.id.to_string(), k.algorithm, used, k.fingerprint, k.title]
                })
                .collect();
            table::show(
                &["ID", "ALGORITHM", "LAST USED", "FINGERPRINT", "TITLE"],
                &rows,
                "no keys",
            );
        }
        Command::Key(KeyCommand::Remove { id, for_user }) => {
            let api = backend.pyn_server("key")?;
            let mut req = api.request(Method::DELETE, &format!("/v1/keys/{id}"));
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            api.send(req)?;
            println!("removed {id}");
        }
        Command::Invite(InviteCommand::Create { role, hours }) => {
            let api = backend.pyn_server("invite")?;
            let body = api::CreateInviteRequest { role, hours };
            let made: api::CreatedInvite = api
                .send(
                    api.request(Method::POST, &api.repo_route("/invites")?)
                        .json(&body),
                )?
                .json()?;
            println!("{}", made.code);
            eprintln!(
                "invitation {} for role {}, expires {}; this is the only time the code is shown",
                made.info.id,
                made.info.role,
                time::local(made.info.expires_at)
            );
        }
        Command::Invite(InviteCommand::List) => {
            let api = backend.pyn_server("invite")?;
            let invites: Vec<api::InviteInfo> =
                api.send(api.get(&api.repo_route("/invites")?))?.json()?;
            let rows: Vec<Vec<String>> = invites
                .into_iter()
                .map(|i| {
                    let state = if i.revoked_at.is_some() {
                        "revoked".to_string()
                    } else if let Some(user) = &i.used_by {
                        format!("used by {user}")
                    } else {
                        "unused".to_string()
                    };
                    vec![i.id.to_string(), i.role, time::local(i.expires_at), state]
                })
                .collect();
            table::show(&["ID", "ROLE", "EXPIRES", "STATE"], &rows, "no invitations");
        }
        Command::Invite(InviteCommand::Revoke { id }) => {
            let api = backend.pyn_server("invite")?;
            api.send(api.request(Method::DELETE, &api.repo_route(&format!("/invites/{id}"))?))?;
            println!("revoked {id}");
        }
        Command::User(UserCommand::Add {
            username,
            role,
            password_stdin,
        }) => {
            let api = backend.pyn_server("user")?;
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::AddUserRequest {
                username: username.clone(),
                password,
                role: role.clone(),
            };
            api.send(
                api.request(Method::POST, &api.repo_route("/users")?)
                    .json(&body),
            )?;
            println!("added {username} as {role}");
        }
        Command::Whoami => {
            let api = backend.pyn_server("whoami")?;
            if api.repo.is_some() {
                let me: api::Me = api.send(api.get(&api.repo_route("/me")?))?.json()?;
                println!("{}\t{}", me.user, me.permissions.join(","));
            } else {
                let me: api::Account = api.send(api.get("/v1/me"))?.json()?;
                println!("{}", me.user);
            }
        }
        Command::Repo(cmd) => repo_command(backend.pyn_server("repo")?, cmd)?,
        Command::Org(cmd) => org_command(backend.pyn_server("org")?, cmd)?,
        Command::Setup {
            username,
            setup_token,
            email,
            server_name,
            public_url,
            registration,
            password_stdin,
        } => admin::setup(
            backend.pyn_server("setup")?,
            admin::SetupArgs {
                username,
                setup_token,
                email,
                server_name,
                public_url,
                registration,
                password_stdin,
            },
            interactive(),
        )?,
        Command::Admin(cmd) => admin::run(backend.pyn_server("admin")?, cmd)?,
        Command::Usage { owner } => limits::usage(backend.pyn_server("usage")?, owner)?,
        Command::Team(cmd) => team_command(backend.pyn_server("team")?, cmd)?,
        Command::Locks { mine: true } => {
            let api = backend.pyn_server("locks")?;
            let locks: Vec<api::MyLock> = api.send(api.get("/v1/me/locks"))?.json()?;
            let rows: Vec<Vec<String>> = locks
                .into_iter()
                .map(|l| {
                    vec![
                        format!("{}/{}", l.owner, l.name),
                        time::local(l.acquired_at),
                        time::local(l.expires_at),
                        l.path,
                    ]
                })
                .collect();
            table::show(
                &["REPOSITORY", "ACQUIRED", "EXPIRES", "PATH"],
                &rows,
                "no locks",
            );
        }
        Command::Locks { mine: false } => {
            let api = backend.pyn_server("locks")?;
            let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
            let rows: Vec<Vec<String>> = locks
                .into_iter()
                .map(|l| vec![l.owner, time::local(l.expires_at), l.path])
                .collect();
            table::show(&["OWNER", "EXPIRES", "PATH"], &rows, "no locks");
        }
        Command::Files => files_command(&backend)?,
        Command::Ls { path } => ls_command(&backend, path.as_deref())?,
        Command::Summary => {
            let api = backend.pyn_server("summary")?;
            let s: api::RepoSummary = api.send(api.get(&api.repo_route("/summary")?))?.json()?;
            println!(
                "{} ({} branch), {} files ({} exclusive, {} shared)",
                s.default_branch, s.branch_count, s.files, s.exclusive_files, s.shared_files
            );
            if !s.locks.is_empty() {
                println!();
                let rows: Vec<Vec<String>> = s
                    .locks
                    .iter()
                    .map(|l| vec![l.owner.clone(), l.path.clone()])
                    .collect();
                println!("{}", table::render(&["LOCKED BY", "PATH"], &rows));
            }
            if !s.activity.is_empty() {
                println!();
                let rows: Vec<Vec<String>> = s
                    .activity
                    .iter()
                    .map(|a| {
                        vec![
                            time::local(a.at),
                            a.actor.clone(),
                            a.action.clone(),
                            a.path.clone().unwrap_or_else(|| "-".into()),
                        ]
                    })
                    .collect();
                println!(
                    "{}",
                    table::render(&["WHEN", "ACTOR", "ACTION", "PATH"], &rows)
                );
            }
        }
        Command::Clone {
            dir,
            revision,
            local_store,
            no_local_store,
            ..
        } => {
            let repo = repo_name.expect("clone names a repository");
            let dir = dir.unwrap_or_else(|| {
                address::split_repo(&repo)
                    .map(|(_, n)| n.into())
                    .unwrap_or_default()
            });
            let target = if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            };
            let commit = revision
                .map(|r| snapshot::resolve(&backend, &r))
                .transpose()?;
            let local_store =
                local_store || (!no_local_store && yours.local_store.as_deref() != Some("false"));
            sync::clone_into(
                &backend,
                &target,
                &server,
                &repo,
                commit.as_deref(),
                local_store,
            )?;
        }
        Command::Fetch(args) => {
            require_bare(&args, "fetch");
            let w = in_workspace("fetch")?;
            if w.settings().local_store_on() {
                local_sync::fetch(&backend, w)?;
            } else {
                update_as_remote(&backend, w, ws.as_ref())?;
            }
        }
        Command::Pull(mut args) => {
            let local = ws.as_ref().is_some_and(|w| w.settings().local_store_on());
            let rebase = local && args.take_rebase();
            let control = args.take_control();
            require_bare(&args, "pull");
            let w = in_workspace("pull")?;
            if local {
                local_sync::pull(&backend, w, rebase, control)?;
            } else if control.is_some() {
                bail!("there is no pull to conclude: this workspace has no local commits to merge");
            } else {
                update_as_remote(&backend, w, ws.as_ref())?;
            }
        }
        Command::Push(mut args) => {
            let request = args.take_server();
            require_bare(&args, "push");
            match ws.as_ref().filter(|w| w.settings().local_store_on()) {
                Some(w) => local_sync::push(&backend, w, request)?,
                None if request.delete
                    || (!request.refspecs.is_empty()
                        && request.refspecs.iter().all(|r| r.starts_with(':'))) =>
                {
                    local_sync::delete_remote(&backend, None, &request)?
                }
                None if request.set_upstream
                    || request.repository.is_some()
                    || !request.refspecs.is_empty() =>
                {
                    usage_error(
                        "`pyn push` takes no arguments on a pyn server without the local store",
                    )
                }
                None => println!(
                    "Nothing to push: commits go straight to the server, so `pyn commit` has already published them."
                ),
            }
        }
        Command::Migrate => migrate::run(&backend, in_workspace("migrate")?)?,
        Command::Reset(args) => {
            local_reset::reset(&backend, in_workspace("reset")?, args)?;
        }
        Command::CherryPick(args) => {
            local_pick::pick(&backend, in_workspace("cherry-pick")?, args)?;
        }
        Command::Revert(args) => {
            local_pick::revert(&backend, in_workspace("revert")?, args)?;
        }
        Command::Tag(args) => local_tags::run(&backend, ws.as_ref(), args)?,
        Command::Stash(_) => {
            eprintln!("{}", unsupported::message("stash"));
            std::process::exit(1);
        }
        Command::Worktree(_) => {
            eprintln!("{}", unsupported::message("worktree"));
            std::process::exit(1);
        }
        Command::Clean(_) => {
            eprintln!("{}", unsupported::message("clean"));
            std::process::exit(1);
        }
        Command::Grep(_) => {
            eprintln!("{}", unsupported::message("grep"));
            std::process::exit(1);
        }
        Command::Blame(_) => {
            eprintln!("{}", unsupported::message("blame"));
            std::process::exit(1);
        }
        Command::Annotate(_) => {
            eprintln!("{}", unsupported::message("annotate"));
            std::process::exit(1);
        }
        Command::Reflog(_) => {
            eprintln!("{}", unsupported::message("reflog"));
            std::process::exit(1);
        }
        Command::RevParse(_)
        | Command::LsFiles(_)
        | Command::LsTree(_)
        | Command::CatFile(_)
        | Command::ShowRef(_)
        | Command::Shortlog(_)
        | Command::Describe(_)
        | Command::NameRev(_)
        | Command::Bisect(_)
        | Command::FormatPatch(_)
        | Command::Apply(_)
        | Command::Am(_) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            std::process::exit(128);
        }
        Command::Rebase(args) => {
            if ws.as_ref().is_some_and(|w| w.settings().local_store_on()) {
                local_rebase::rebase(&backend, in_workspace("rebase")?, &args)?;
            } else {
                eprintln!("{}", unsupported::message("rebase"));
                std::process::exit(1);
            }
        }
        Command::Remote(args) => {
            git_home::remote_on_server(&args, &server_url, repo_name.as_deref())?
        }
        Command::Merge(args) => merging::merge_command(&backend, ws.as_ref(), args)?,
        Command::Branch(args) => branch::branch_command(&backend, ws.as_ref(), args)?,
        Command::Switch(args) => branch::switch_command(&backend, ws.as_ref(), args)?,
        Command::Checkout(args) => {
            branch::checkout_command(&backend, ws.as_ref(), &cwd, args, &rp)?
        }
        Command::Status(args) => {
            let w = in_workspace("status")?;
            let format = args.format();
            let only =
                pathspec::parse(w, &cwd, &args.paths, Default::default(), false, usage_error)?;
            let commit = args
                .revision
                .map(|r| snapshot::resolve(&backend, &r))
                .transpose()?;
            if format == short_status::Format::Table {
                branch::print_header(Some(w));
                if let Some(text) = merging::status_header(w)? {
                    print!("{text}");
                }
            }
            sync::status(
                &backend,
                w,
                commit.as_deref(),
                sync::StatusView {
                    format,
                    branch: args.branch,
                    only: &only,
                    cwd: &cwd,
                },
            )?
        }
        Command::Update { paths, revision } => {
            let only = paths.iter().map(|p| rp(p)).collect::<Result<Vec<_>>>()?;
            if let Some(w) = &ws {
                merging::refuse_during_merge(w, "update")?;
            }
            let commit = revision
                .map(|r| snapshot::resolve(&backend, &r))
                .transpose()?;
            sync::update(&backend, in_workspace("update")?, &only, commit.as_deref())?;
        }
        Command::Diff {
            args,
            paths,
            format,
            exit_code,
            quiet,
            cached,
            no_patch,
            ..
        } => {
            let named = DiffArgs {
                args: &args,
                paths: &paths,
                cached,
                cwd: &cwd,
            };
            if let Some(flag) = format.extra.git_only() {
                usage_error(&format!("`pyn diff {flag}` needs a git repository"));
            }
            let differs = diff_command(
                &backend,
                ws.as_ref(),
                &named,
                &format.format().silenced(no_patch),
                quiet,
            )?;
            if differs && (exit_code || quiet) {
                std::process::exit(1);
            }
        }
        Command::Show {
            args,
            paths,
            no_patch,
            fmt,
            decorate,
            format,
            first_parent,
            merges,
        } => {
            if args.len() > 1 || !paths.is_empty() {
                usage_error("`pyn show` takes one commit and no paths on a pyn server");
            }
            if let Some(flag) = merges
                .given()
                .or(first_parent.then_some("--first-parent"))
                .or(format.extra.git_only())
            {
                usage_error(&format!("`pyn show {flag}` needs a git repository"));
            }
            if let Some((flag, _)) = decorate.given().into_iter().find(|(_, on)| *on) {
                usage_error(&format!("`pyn show {flag}` needs a git repository"));
            }
            let commit = args.first();
            let shown = ShowOpts {
                diff: format.format(),
                rendering: fmt.requested()?,
                no_patch,
            };
            show_command(&backend, ws.as_ref(), commit.map(String::as_str), &shown)?
        }
        Command::Policy(PolicyCommand::Preview { file }) => {
            let api = backend.pyn_server("policy")?;
            let content = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let body = api::PolicyPreviewRequest { content };
            let found: api::PolicyPreview = api
                .send(
                    api.request(Method::POST, &api.repo_route("/policy/preview")?)
                        .json(&body),
                )?
                .json()?;
            let rows: Vec<Vec<String>> = found
                .to_shared
                .iter()
                .map(|p| ("exclusive -> shared", p))
                .chain(
                    found
                        .to_exclusive
                        .iter()
                        .map(|p| ("shared -> exclusive", p)),
                )
                .map(|(change, path)| vec![change.to_string(), path.clone()])
                .collect();
            table::show(&["CHANGE", "PATH"], &rows, "no path changes mode");
            for lock in &found.released_locks {
                println!("would release {}'s lock on {}", lock.owner, lock.path);
            }
            stranded::print(
                "Shared versions on other branches that this change would strand:",
                &found.stranded,
            );
        }
        Command::Config(cmd) => config_command(cmd, ws.as_ref(), &mine, &yours, &user_config)?,
        Command::Lock { path, base } => {
            let api = backend.pyn_server("lock")?;
            let path = rp(&path)?;
            let known = ws
                .as_ref()
                .and_then(|w| w.load_state().ok())
                .and_then(|s| s.get(&path).map(|k| k.revision));
            let base = match base {
                Some(b) => Some(b),
                None => local_base(&backend, ws.as_ref(), &path)?.or(known),
            };
            let body = api::CheckoutRequest {
                path: path.clone(),
                base_revision: base,
                branch: api.branch.clone(),
            };
            let l: api::Lock = api
                .send(
                    api.request(Method::POST, &api.repo_route("/checkout")?)
                        .json(&body),
                )
                .map_err(|e| behind_hint(e, ws.is_some()))?
                .json()?;
            if let Some(w) = &ws {
                sync::after_lock(w, &path)?;
            }
            println!("locked {} until {}", l.path, time::local(l.expires_at));
        }
        Command::Add { patch: true, .. } => {
            usage_error("`add --patch` needs a git repository; a pyn server stages whole files")
        }
        Command::Add {
            interactive: true, ..
        } => usage_error(
            "`add --interactive` needs a git repository; a pyn server stages whole files",
        ),
        Command::Add {
            intent_to_add: true,
            ..
        } => usage_error("`add --intent-to-add` needs a git repository"),
        Command::Add {
            paths,
            update,
            dry_run,
            verbose,
            force,
            ..
        } => {
            let args = staging::AddArgs {
                paths,
                update,
                dry_run,
                verbose,
                force,
            };
            if staging::add(&backend, in_workspace("add")?, &cwd, &args)? {
                std::process::exit(1);
            }
        }
        Command::Rm(args) => rm_mv::rm(&backend, in_workspace("rm")?, &cwd, &args)?,
        Command::Mv(args) => rm_mv::mv(&backend, in_workspace("mv")?, &cwd, &args)?,
        Command::Commit {
            paths,
            file,
            base,
            message,
            all,
            git: flags,
            ..
        } if paths.is_empty() => {
            if file.is_some() || base.is_some() {
                bail!("--file and --base need a path");
            }
            let w = in_workspace("commit")?;
            if merging::in_progress(w) && !merging::is_paused(w) {
                if let Some(flag) = flags.author_flag() {
                    bail!("`pyn commit {flag}` does not apply while finishing a merge");
                }
                if all {
                    bail!(
                        "cannot do a partial commit during a merge; stage what you resolved with `pyn add`"
                    );
                }
                let message = (!message.is_empty()).then(|| message.join("\n\n"));
                merging::finish(&backend, backend.pyn_server("commit")?, w, message)?;
            } else {
                staging::commit(&backend, w, &message.join("\n\n"), all, &flags)?;
            }
        }
        Command::Commit { all: true, .. } => {
            bail!("`commit -a` commits every changed tracked file; name no paths with it")
        }
        Command::Commit {
            paths,
            file,
            base,
            message,
            ..
        } if paths.len() > 1 => {
            if file.is_some() || base.is_some() {
                bail!("--file and --base need a single path");
            }
            let Some(w) = &ws else {
                bail!("committing several paths needs a workspace");
            };
            let message = message.join("\n\n");
            let known = w.load_state().unwrap_or_default();
            let mut entries = Vec::new();
            let mut hashes = Vec::new();
            for p in &paths {
                let path = rp(p)?;
                let (file_mode, hash) = localfile::upload(&backend, &w.abs(&path))?;
                entries.push(api::ChangesetEntry {
                    path: path.clone(),
                    content: hash.clone(),
                    file_mode: Some(file_mode),
                    base_revision: known.get(&path).map(|k| k.revision),
                });
                hashes.push((path, hash, file_mode));
            }
            let body = api::ChangesetRequest {
                entries,
                deletions: Vec::new(),
                message,
                branch: None,
            };
            let r = backend
                .commit_changeset(&body)
                .map_err(|e| behind_hint(e, true))?;
            let mut done = Vec::new();
            for (rev, (path, hash, file_mode)) in r.revisions.iter().zip(&hashes) {
                sync::after_commit(&backend, w, path, rev.id, hash, *file_mode)?;
                quiet::say!("{} is now at revision {}", rev.path, rev.id);
                done.push(path.clone());
            }
            staging::Index::forget(w, &done)?;
        }
        Command::Commit {
            paths,
            file,
            base,
            message,
            ..
        } => {
            let message = message.join("\n\n");
            let path = rp(&paths[0])?;
            let explicit_file = file.is_some();
            let file = match (file, &ws) {
                (Some(file), _) => file,
                (None, Some(w)) => w.abs(&path),
                (None, None) => bail!("name the file to upload, or run this inside a workspace"),
            };
            let state = ws.as_ref().and_then(|w| w.load_state().ok());
            let known = state
                .as_ref()
                .and_then(|s| s.get(&path).map(|k| k.revision));
            if let Some(k) = state.as_ref().and_then(|s| s.get(&path))
                && !explicit_file
                && base.is_none()
                && localfile::exists(&file)
            {
                let (mode, bytes) = localfile::read(&file)?;
                if !k.changed(mode, &bytes) {
                    eprintln!(
                        "nothing to commit; {path} has not changed since revision {}",
                        k.revision
                    );
                    std::process::exit(1);
                }
            }
            let (file_mode, hash) = localfile::upload(&backend, &file)?;
            let body = api::CheckinRequest {
                path: path.clone(),
                content: hash.clone(),
                file_mode: Some(file_mode),
                base_revision: base.or(known),
                message,
                branch: None,
            };
            let r = backend
                .commit_file(&body)
                .map_err(|e| behind_hint(e, ws.is_some()))?;
            if let Some(w) = &ws {
                sync::after_commit(&backend, w, &path, r.id, &hash, file_mode)?;
                staging::Index::forget(w, std::slice::from_ref(&path))?;
            }
            quiet::say!("{} is now at revision {}", r.path, r.id);
            stranded::print(
                "The policy change left shared versions on other branches that can no longer merge:",
                &r.stranded,
            );
        }
        Command::Get { path, rev, output } => {
            let path = rp(&path)?;
            match output {
                Some(file) => {
                    let (revision, _) = backend.download(&path, rev, &file)?;
                    let len = std::fs::metadata(&file)?.len();
                    println!(
                        "wrote {path} at revision {revision} ({len} bytes) to {}",
                        file.display()
                    );
                }
                None => backend.read(&path, rev, &mut std::io::stdout())?,
            }
        }
        Command::Log {
            path,
            filter,
            commits,
            limit,
            git: log_args,
        } if commits
            || log_args.format().first_given().is_some()
            || log_args.diff_given()
            || log_filter::selects(path.as_deref(), &log_args, &cwd, false) =>
        {
            branch::note_branch(ws.as_ref());
            let limit = limit.unwrap_or(50);
            let selected = log_filter::selects(path.as_deref(), &log_args, &cwd, true);
            if selected && filter.is_some() {
                usage_error(
                    "`pyn log --filter` cannot combine with commit filters, revisions or a path",
                );
            }
            let narrow = if selected {
                log_filter::build(&backend, &log_args, path.as_deref(), &cwd, &rp)?
            } else {
                log_filter::CommitFilter::default()
            };
            let parts = log_args.diff_parts();
            let rendering = match log_args.format().requested()? {
                Some(r) => Some(r),
                None if parts.any() => Some(log_args.format().rendering()?),
                None => None,
            };
            if let Some(rendering) = rendering {
                if parts.any() && narrow.path.is_some() {
                    usage_error("`pyn log` diff output takes no path on a pyn server");
                }
                if filter.is_some() {
                    usage_error("`pyn log` with a format lists commits and takes no --filter");
                }
                return log_formatted(&backend, ws.as_ref(), limit, &rendering, &parts, &narrow);
            }
            let mut rows: Vec<Vec<String>> =
                match ws.as_ref().and_then(local_store::LocalStore::open) {
                    Some(store) if !selected => {
                        local_view::log_rows(&store, &branch::name(ws.as_ref()))?
                    }
                    _ => Vec::new(),
                };
            rows.truncate(limit);
            let mut before: Option<String> = None;
            while rows.len() < limit {
                let page = backend.commit_page_filtered(
                    before.as_deref(),
                    (limit - rows.len()).min(200),
                    &narrow,
                )?;
                for c in page.commits {
                    rows.push(vec![
                        snapshot::short(&c.id).to_string(),
                        c.author.name,
                        time::local(c.created_at),
                        c.revisions.len().to_string(),
                        c.message,
                    ]);
                }
                before = page.next_before;
                if before.is_none() {
                    break;
                }
            }
            table::show(
                &["COMMIT", "AUTHOR", "WHEN", "FILES", "MESSAGE"],
                &rows,
                "no commits",
            );
        }
        Command::Log {
            path,
            filter,
            limit,
            git: log_args,
            ..
        } => {
            branch::note_branch(ws.as_ref());
            let path = path.or_else(|| log_args.sole_dashed().map(str::to_string));
            let path = path.map(|p| rp(&p)).transpose()?;
            let store = ws.as_ref().and_then(local_store::LocalStore::open);
            let scope = match &path {
                Some(p) => local_view::LogScope::Path(p.as_str()),
                None => local_view::LogScope::Repo(filter.as_deref()),
            };
            let mut local_rows = match &store {
                Some(store) => {
                    local_view::revision_rows(store, &branch::name(ws.as_ref()), &scope)?
                }
                None => Vec::new(),
            };
            let max = limit.unwrap_or(if path.is_some() { usize::MAX } else { 50 });
            local_rows.truncate(max);
            let rest = max - local_rows.len();
            let repo_wide = path.is_none();
            let revs = match path {
                _ if rest == 0 => Vec::new(),
                Some(path) => backend
                    .path_history(&path)?
                    .into_iter()
                    .rev()
                    .take(rest)
                    .collect(),
                None => backend.repo_history(filter.as_deref(), rest)?,
            };
            let mut rows = local_rows;
            rows.extend(revs.into_iter().map(|r| {
                let restored = match (r.deleted, r.restored_from) {
                    (true, _) => " (deleted)".to_string(),
                    (false, Some(n)) => format!(" (restored from r{n})"),
                    (false, None) => String::new(),
                };
                let mut row = vec![
                    format!("r{}", r.id),
                    r.author,
                    time::local(r.created_at),
                    format!("{}{restored}", r.message),
                ];
                if repo_wide {
                    row.push(r.path);
                }
                row
            }));
            let header: &[&str] = if repo_wide {
                &["REV", "AUTHOR", "WHEN", "MESSAGE", "PATH"]
            } else {
                &["REV", "AUTHOR", "WHEN", "MESSAGE"]
            };
            table::show(header, &rows, "no history");
        }
        Command::Verify { path, verbose } => {
            let api = backend.pyn_server("verify")?;
            let path = path.map(|p| rp(&p)).transpose()?;
            if !verify::run(api, path.as_deref(), verbose)? {
                std::process::exit(1);
            }
        }
        Command::Restore { patch: true, .. } => usage_error(
            "`restore --patch` needs a git repository; a pyn server restores whole files",
        ),
        Command::Restore { ours: true, .. }
        | Command::Restore { theirs: true, .. }
        | Command::Restore { merge: true, .. } => usage_error(
            "`restore --ours`, `--theirs` and `--merge` need a git repository; a pyn server has no merge to take a side of",
        ),
        Command::Restore { worktree: true, .. } => usage_error(
            "`restore --worktree` needs a git repository; on a pyn server a restore makes an older revision the new head",
        ),
        Command::Restore {
            args,
            source,
            staged: true,
            ..
        } => {
            if source.is_some() {
                usage_error(
                    "`restore --staged --source` needs a git repository; a pyn server has no commit to restore the index from",
                );
            }
            staging::unstage(in_workspace("restore --staged")?, &cwd, &args)?;
        }
        Command::Restore {
            args,
            source,
            message,
            yes,
            ..
        } => {
            let (path, revision) = restore_target(&args, source.as_deref());
            restore(&backend, rp(&path)?, revision, message, yes)?
        }
        Command::Unlock {
            path, force: false, ..
        } => {
            let api = backend.pyn_server("unlock")?;
            let path = rp(&path)?;
            let body = api::ReleaseRequest { path: path.clone() };
            api.send(
                api.request(Method::POST, &api.repo_route("/release")?)
                    .json(&body),
            )?;
            if let Some(w) = &ws {
                sync::after_unlock(w, &path)?;
            }
            println!("unlocked {path}");
        }
        Command::Unlock {
            path,
            reason: Some(reason),
            ..
        } => {
            let api = backend.pyn_server("unlock")?;
            let path = rp(&path)?;
            let body = api::ForceUnlockRequest {
                path: path.clone(),
                reason,
            };
            let lock: api::Lock = api
                .send(
                    api.request(Method::POST, &api.repo_route("/force-unlock")?)
                        .json(&body),
                )?
                .json()?;
            println!("removed {}'s lock on {path}", lock.owner);
        }
        Command::Unlock { .. } => unreachable!("clap requires a reason with --force"),
        Command::Audit {
            path,
            actor,
            action,
            before,
            limit,
        } => {
            let api = backend.pyn_server("audit")?;
            let path = path.map(|p| rp(&p)).transpose()?;
            let mut req = api
                .get(&api.repo_route("/audit")?)
                .query(&[("limit", limit)]);
            for (key, value) in [("path", path), ("actor", actor), ("action", action)] {
                if let Some(v) = value {
                    req = req.query(&[(key, v)]);
                }
            }
            if let Some(b) = before {
                req = req.query(&[("before", b)]);
            }
            let page: api::AuditPage = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = page
                .entries
                .iter()
                .map(|e| {
                    vec![
                        e.id.to_string(),
                        time::local(e.at),
                        e.actor.clone(),
                        e.action.clone(),
                        e.path.clone().unwrap_or_else(|| "-".into()),
                        time::localize(&e.detail),
                    ]
                })
                .collect();
            table::show(
                &["ID", "WHEN", "ACTOR", "ACTION", "PATH", "DETAIL"],
                &rows,
                "no audit entries",
            );
            if let Some(next) = page.next_before {
                eprintln!("more: --before {next}");
            }
        }
        Command::Token(cmd) => token_command(backend.pyn_server("token")?, cmd)?,
        Command::Member(MemberCommand::List) => {
            let api = backend.pyn_server("member")?;
            let members: Vec<api::Member> =
                api.send(api.get(&api.repo_route("/members")?))?.json()?;
            let rows: Vec<Vec<String>> = members
                .into_iter()
                .map(|m| vec![m.role, m.source, m.user])
                .collect();
            table::show(&["ROLE", "SOURCE", "USER"], &rows, "no members");
        }
        Command::Member(MemberCommand::Set { user, role }) => {
            let api = backend.pyn_server("member")?;
            let body = api::SetMemberRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &api.repo_route(&format!("/members/{user}"))?)
                    .json(&body),
            )?;
            println!("{user} is now {role}");
        }
        Command::Role(RoleCommand::List) => {
            let api = backend.pyn_server("role")?;
            let grants: Vec<api::RoleGrant> =
                api.send(api.get(&api.repo_route("/roles")?))?.json()?;
            let rows: Vec<Vec<String>> = grants
                .into_iter()
                .map(|g| vec![g.role, g.permissions.join(",")])
                .collect();
            table::show(&["ROLE", "PERMISSIONS"], &rows, "no roles");
        }
        Command::Role(RoleCommand::Set { role, permissions }) => {
            let api = backend.pyn_server("role")?;
            let body = api::SetRoleRequest { permissions };
            api.send(
                api.request(Method::PUT, &api.repo_route(&format!("/roles/{role}"))?)
                    .json(&body),
            )?;
            println!("updated {role}");
        }
    }
    Ok(())
}

fn files_command(backend: &dyn Backend) -> Result<()> {
    let rows: Vec<Vec<String>> = backend
        .list_files()?
        .into_iter()
        .map(|f| {
            vec![
                sync::mode_name(f.mode).to_string(),
                f.revision.map_or("-".to_string(), |r| format!("r{r}")),
                f.lock.map_or("-".to_string(), |l| l.owner),
                f.path,
            ]
        })
        .collect();
    table::show(&["MODE", "REV", "LOCKED BY", "PATH"], &rows, "no files");
    Ok(())
}

fn ls_command(backend: &dyn Backend, path: Option<&str>) -> Result<()> {
    let listing = backend.tree(path)?;
    let rows: Vec<Vec<String>> = listing
        .entries
        .iter()
        .map(|e| {
            let slash = if e.kind == api::TreeEntryKind::Folder {
                "/"
            } else {
                ""
            };
            vec![
                format!("{:?}", e.mode).to_lowercase(),
                e.lock.as_ref().map_or("-".to_string(), |l| l.owner.clone()),
                format!("{}{slash}", e.name),
                e.last_change.as_ref().map_or("-".to_string(), |r| {
                    format!("{} ({}, r{})", r.message, r.author, r.id)
                }),
            ]
        })
        .collect();
    table::show(
        &["MODE", "LOCKED BY", "NAME", "LAST CHANGE"],
        &rows,
        "empty",
    );
    Ok(())
}

/// A commit reference that is not also the name of something in the current directory.
fn is_commit_arg(arg: &str) -> bool {
    snapshot::parse_commit(arg).is_some() && !std::path::Path::new(arg).exists()
}

/// What `pyn diff` was asked to compare: the commits and paths named, and whether it is the staged changes.
struct DiffArgs<'a> {
    args: &'a [String],
    paths: &'a [String],
    cached: bool,
    cwd: &'a std::path::Path,
}

/// Splits the leading commit references from the paths and prints the comparison; true if anything differs.
fn diff_command(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    named: &DiffArgs,
    format: &diff::Format,
    quiet: bool,
) -> Result<bool> {
    let DiffArgs {
        args,
        paths,
        cached,
        cwd,
    } = *named;
    let expanded = local_tags::expand_args(ws, args)?;
    let args = expanded.as_slice();
    let mut refs: Vec<String> = Vec::new();
    let mut rest = args;
    while refs.len() < 2
        && let Some((first, tail)) = rest.split_first()
    {
        if let Some((a, b)) = first.split_once("..")
            && refs.is_empty()
            && is_commit_arg(a)
            && is_commit_arg(b)
        {
            refs.extend([a.to_string(), b.to_string()]);
        } else if is_commit_arg(first) {
            refs.push(first.clone());
        } else {
            break;
        }
        rest = tail;
    }
    let named_paths: Vec<String> = rest.iter().chain(paths).cloned().collect();
    let spec = match ws {
        Some(w) => pathspec::parse(w, cwd, &named_paths, Default::default(), false, usage_error)?,
        None => pathspec::parse_from_root(&named_paths, usage_error)?,
    };
    let ws_needed = || -> Result<&Workspace> {
        ws.context(
            "`pyn diff` with fewer than two commits compares the workspace; run it inside one",
        )
    };
    let snapshot_tree = |r: &str| -> Result<diff::Tree> {
        let commit = snapshot::resolve(backend, r)?;
        Ok(diff::tree_of_snapshot(&backend.snapshot(Some(&commit))?.1))
    };
    let (old, new) = match refs.as_slice() {
        [] if cached => {
            let ws = ws_needed()?;
            let index = staging::Index::load(ws)?;
            let staged = diff::tree_of_staged(ws, &index);
            let mut old = diff::tree_of_state(&ws.load_state()?);
            old.retain(|p, _| index.entries.contains_key(p));
            (old, staged)
        }
        [_] | [_, _] if cached => {
            bail!(
                "`pyn diff --cached` compares the staged changes with the last commit and takes no commit"
            )
        }
        [] => {
            let ws = ws_needed()?;
            let index = staging::Index::load(ws)?;
            let mut old = diff::tree_of_state(&ws.load_state()?);
            old.extend(diff::tree_of_staged(ws, &index));
            old.retain(|p, _| !index.entries.get(p).is_some_and(|e| e.deleted));
            let new = diff::tree_of_disk(ws, old.keys())?;
            (old, new)
        }
        [one] => {
            let ws = ws_needed()?;
            let old = snapshot_tree(one)?;
            let tracked: std::collections::BTreeSet<String> = old
                .keys()
                .cloned()
                .chain(ws.load_state()?.into_keys())
                .collect();
            (old, diff::tree_of_disk(ws, tracked.iter())?)
        }
        [a, b] => (snapshot_tree(a)?, snapshot_tree(b)?),
        _ => unreachable!("at most two commits are taken"),
    };
    let (mut old, mut new) = (old, new);
    old.retain(|p, _| spec.matches(p.as_bytes()));
    new.retain(|p, _| spec.matches(p.as_bytes()));
    let mut out = String::new();
    let differs = diff::compare(backend, &old, &new, &[], format, &mut out)?;
    if !quiet {
        print!("{out}");
        diff::exit_if_check_failed(format, &out);
    }
    Ok(differs)
}

/// `log` with a format on a pyn server: the unpublished local commits, then the server's, newest first.
fn log_formatted(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    limit: usize,
    rendering: &commit_format::Rendering,
    parts: &diff::DiffParts,
    narrow: &log_filter::CommitFilter,
) -> Result<()> {
    let store = ws.and_then(local_store::LocalStore::open);
    let mut commits = match &store {
        Some(store) if *narrow == log_filter::CommitFilter::default() => {
            local_view::log_commits(store, &branch::name(ws))?
        }
        _ => Vec::new(),
    };
    commits.truncate(limit);
    let local = commits.len();
    let mut before: Option<String> = None;
    while commits.len() < limit {
        let page = backend.commit_page_filtered(
            before.as_deref(),
            (limit - commits.len()).min(200),
            narrow,
        )?;
        commits.extend(page.commits);
        before = page.next_before;
        if before.is_none() {
            break;
        }
    }
    let mut out = String::new();
    for (i, c) in commits.iter().enumerate() {
        if i > 0 && !rendering.pretty.terminated() {
            out.push('\n');
        }
        out.push_str(&commit_format::render_api(rendering, c)?);
        if parts.any()
            && c.parents.len() < 2
            && let Some((body, dashes)) =
                commit_sections(backend, store.as_ref(), i < local, c, parts)?
        {
            if rendering.pretty.blank_before_diff() {
                out.push_str(if dashes { "---\n" } else { "\n" });
            }
            out.push_str(&body);
        }
    }
    print!("{out}");
    Ok(())
}

/// The diff sections of one commit against its first parent, from the local store when unpublished.
fn commit_sections(
    backend: &dyn Backend,
    store: Option<&local_store::LocalStore>,
    unpublished: bool,
    c: &api::Commit,
    parts: &diff::DiffParts,
) -> Result<Option<(String, bool)>> {
    let (old, new, only) = match store.filter(|_| unpublished) {
        Some(store) => local_view::commit_trees(store, c)?,
        None => server_trees(backend, c)?,
    };
    diff::sections(backend, &old, &new, (&only, &[]), parts)
}

/// The trees before and after a published commit (against its first parent) and the paths it changed.
fn server_trees(
    backend: &dyn Backend,
    c: &api::Commit,
) -> Result<(diff::Tree, diff::Tree, Vec<String>)> {
    let new = diff::tree_of_snapshot(&backend.snapshot(Some(&c.id))?.1);
    let old = match c.parents.first() {
        Some(parent) => diff::tree_of_snapshot(&backend.snapshot(Some(parent))?.1),
        None => diff::Tree::new(),
    };
    Ok((
        old,
        new,
        c.revisions.iter().map(|r| r.path.clone()).collect(),
    ))
}

/// What `show` prints besides the commit's diff format: a pretty format, and whether to leave the diff out.
struct ShowOpts {
    diff: diff::Format,
    rendering: Option<commit_format::Rendering>,
    no_patch: bool,
}

impl ShowOpts {
    /// The commit's text and whether a blank line sets the diff off from it.
    fn head(&self, c: &api::Commit) -> Result<(String, bool)> {
        match &self.rendering {
            Some(r) => Ok((
                commit_format::render_api(r, c)?,
                r.pretty.blank_before_diff(),
            )),
            None => Ok((diff::commit_header(c), true)),
        }
    }
}

fn show_command(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    commit: Option<&str>,
    opts: &ShowOpts,
) -> Result<()> {
    let store = ws.and_then(local_store::LocalStore::open);
    let tagged = match (&store, commit) {
        (Some(store), Some(text)) => local_tags::expand(store, text)?,
        _ => None,
    };
    if let Some(note) = tagged.as_ref().and_then(|t| t.annotation.as_deref()) {
        print!("{note}");
    }
    let commit = tagged.as_ref().map_or(commit, |t| Some(t.rev.as_str()));
    if let Some(store) = &store
        && local_view::show(backend, store, &branch::name(ws), commit, opts)?
    {
        return Ok(());
    }
    let id = match commit {
        Some(c) => snapshot::resolve(backend, c)?,
        None => snapshot::resolve(backend, "HEAD")?,
    };
    let found = backend.commit(&id)?;
    if opts.no_patch {
        print!("{}", opts.head(&found)?.0);
        return Ok(());
    }
    let format = &opts.diff;
    let (old, new, only) = server_trees(backend, &found)?;
    let mut body = String::new();
    let differs = diff::compare(backend, &old, &new, &only, format, &mut body)?;
    let (head, blank) = opts.head(&found)?;
    print!("{head}");
    if diff::has_body(format, differs, &body) {
        if blank {
            println!();
        }
        print!("{body}");
    }
    diff::exit_if_check_failed(format, &body);
    Ok(())
}

/// A pyn server remote has no options yet: anything but the bare command is a usage error.
fn require_bare<T: std::fmt::Debug>(given: &T, name: &str) {
    let bare = Cli::try_parse_from(["pyn", name])
        .ok()
        .map(|c| match c.command {
            Command::Fetch(a) => format!("{a:?}"),
            Command::Pull(a) => format!("{a:?}"),
            Command::Push(a) => format!("{a:?}"),
            _ => String::new(),
        });
    if bare.as_deref() != Some(&format!("{given:?}")) {
        usage_error(&format!(
            "`pyn {name}` takes no arguments on a pyn server; it runs `pyn update`"
        ));
    }
}

fn update_as_remote(backend: &dyn Backend, w: &Workspace, ws: Option<&Workspace>) -> Result<()> {
    if let Some(w) = ws {
        merging::refuse_during_merge(w, "update")?;
    }
    sync::update(backend, w, &[], None)
}

/// A usage error, reported and exited as the argument parser does.
fn usage_error(message: &str) -> ! {
    let e = Cli::command().error(clap::error::ErrorKind::InvalidValue, message);
    let _ = e.print();
    std::process::exit(129);
}

/// The path and the older revision a `restore` names, as `<path> <revision>` or `<path> --source <revision>`.
fn restore_target(args: &[String], source: Option<&str>) -> (String, u64) {
    let revision = |text: &str| -> u64 {
        text.parse()
            .unwrap_or_else(|_| usage_error(&format!("'{text}' is not a revision number")))
    };
    match (args, source) {
        ([path], Some(source)) => (path.clone(), revision(source)),
        ([path, rev], None) => (path.clone(), revision(rev)),
        ([_], None) => usage_error(
            "a revision is required: `restore <path> <REVISION>` or `--source <revision>`",
        ),
        _ => usage_error("`restore` takes one path and one revision, or one path with --source"),
    }
}

fn restore(
    backend: &dyn Backend,
    path: String,
    revision: u64,
    message: Option<String>,
    yes: bool,
) -> Result<()> {
    let revs = backend.path_history(&path)?;
    let head = revs
        .last()
        .with_context(|| format!("{path} has no revisions"))?;
    let source = revs
        .iter()
        .find(|r| r.id == revision)
        .with_context(|| format!("{path} has no revision {revision}"))?;
    println!(
        "Restore {path} to r{} ({}, \"{}\").",
        source.id, source.author, source.message
    );
    println!(
        "The current head r{} ({}, \"{}\") stays in history; the restore becomes r{}.",
        head.id,
        head.author,
        head.message,
        head.id + 1
    );
    if !yes {
        eprint!("Type the path to confirm: ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != path {
            bail!("not confirmed");
        }
    }
    let body = api::RestoreRequest {
        path: path.clone(),
        revision,
        base_revision: head.id,
        confirm: format!("{path}@r{}", head.id),
        message,
        branch: None,
    };
    let r = backend.restore(&body)?;
    println!(
        "{} is now at revision {}, restored from r{revision}",
        r.path, r.id
    );
    Ok(())
}

const POLICY_PATH: &str = ".pyn/pyn.toml";

const DEFAULT_POLICY: &str = "[meta]\ndefault = \"exclusive\"\n";

/// Checks in the repository's first policy file, which the server accepts without a lock.
fn submit_policy(api: &Api, repo: &str, bytes: Vec<u8>) -> Result<()> {
    let put: api::PutObjectResponse = api
        .send(
            api.request(Method::PUT, &format!("{repo}/objects"))
                .body(bytes),
        )?
        .json()?;
    let body = api::CheckinRequest {
        path: POLICY_PATH.into(),
        content: put.content,
        file_mode: None,
        base_revision: None,
        message: "Initial policy".into(),
        branch: None,
    };
    api.send(
        api.request(Method::POST, &format!("{repo}/checkin"))
            .json(&body),
    )?;
    Ok(())
}

fn explain_create_refusal(err: anyhow::Error, org: &str) -> anyhow::Error {
    match err.downcast_ref::<client::ApiError>() {
        Some(e) if e.code == "repo_create_forbidden" => anyhow::anyhow!(
            "{} ({}); owners of {org} can change this with `pyn org policy`",
            e.message,
            e.code
        ),
        Some(e) if e.code == "not_org_member" => {
            anyhow::anyhow!("you are not a member of {org} ({})", e.code)
        }
        _ => err,
    }
}

fn parse_visibility(v: &str) -> Result<api::Visibility> {
    match v {
        "public" => Ok(api::Visibility::Public),
        "private" => Ok(api::Visibility::Private),
        other => bail!("unknown visibility {other:?}: use public or private"),
    }
}

fn repo_command(api: &Api, cmd: RepoCommand) -> Result<()> {
    match cmd {
        RepoCommand::Create {
            name,
            visibility,
            lease_hours,
            max_locks,
            policy,
            no_policy,
        } => {
            let policy = match (policy, no_policy) {
                (Some(file), _) => Some(
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?,
                ),
                (None, false) => Some(DEFAULT_POLICY.as_bytes().to_vec()),
                (None, true) => None,
            };
            let (owner, name) = match name.split_once('/') {
                Some((owner, name)) => (Some(owner.to_string()), name.to_string()),
                None => (None, name),
            };
            let visibility = visibility.as_deref().map(parse_visibility).transpose()?;
            let body = api::CreateRepoRequest {
                owner,
                name,
                visibility,
                lease_hours,
                max_locks_per_user: max_locks,
            };
            let org = body.owner.clone().unwrap_or_default();
            let made: api::RepoInfo = api
                .send(api.request(Method::POST, "/v1/repos").json(&body))
                .map_err(|e| explain_create_refusal(e, &org))?
                .json()?;
            println!("created {}/{}", made.owner, made.name);
            if let Some(bytes) = policy {
                let repo = format!("/v1/repos/{}/{}", made.owner, made.name);
                submit_policy(api, &repo, bytes)
                    .context("the repository was created, but its policy was not accepted")?;
                println!("added {POLICY_PATH}");
            }
        }
        RepoCommand::List { owner } => {
            let mut req = api.get("/v1/repos");
            if let Some(owner) = &owner {
                req = req.query(&[("owner", owner)]);
            }
            let repos: Vec<api::RepoInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = repos
                .into_iter()
                .map(|r| {
                    let visibility = match r.visibility {
                        api::Visibility::Public => "public",
                        api::Visibility::Private => "private",
                    };
                    vec![
                        visibility.to_string(),
                        r.role.unwrap_or_else(|| "-".into()),
                        format!("{}/{}", r.owner, r.name),
                    ]
                })
                .collect();
            table::show(
                &["VISIBILITY", "ROLE", "REPOSITORY"],
                &rows,
                "no repositories",
            );
        }
        RepoCommand::Explore { limit } => {
            let mut repos: Vec<api::RepoInfo> = Vec::new();
            let mut after: Option<String> = None;
            while repos.len() < limit {
                let mut query = vec![("limit", (limit - repos.len()).min(200).to_string())];
                query.extend(after.take().map(|a| ("after", a)));
                let page: api::RepoPage = api
                    .send(api.get("/v1/explore/repos").query(&query))?
                    .json()?;
                repos.extend(page.repos);
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
            let rows: Vec<Vec<String>> = repos
                .into_iter()
                .map(|r| {
                    vec![
                        r.role.unwrap_or_else(|| "-".into()),
                        format!("{}/{}", r.owner, r.name),
                    ]
                })
                .collect();
            table::show(&["ROLE", "REPOSITORY"], &rows, "no public repositories");
        }
        RepoCommand::Visibility { name, visibility } => {
            let (owner, short) = address::split_repo(&name)?;
            let body = api::UpdateRepoRequest {
                visibility: Some(parse_visibility(&visibility)?),
                ..Default::default()
            };
            api.send(
                api.request(Method::PATCH, &format!("/v1/repos/{owner}/{short}"))
                    .json(&body),
            )?;
            println!("{name} is now {visibility}");
        }
        RepoCommand::Usage { name } => {
            let (owner, short) = address::split_repo(&name)?;
            limits::repo_usage(api, owner, short)?;
        }
        RepoCommand::Delete { name, yes } => {
            let (owner, short) = address::split_repo(&name)?;
            if !yes {
                eprintln!(
                    "This removes {name} with its files, history, locks and access. Its audit log is kept."
                );
                eprint!("Type the repository name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != name {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/repos/{owner}/{short}")))?;
            println!("deleted {name}");
        }
    }
    Ok(())
}

fn org_command(api: &Api, cmd: OrgCommand) -> Result<()> {
    match cmd {
        OrgCommand::Create { name } => {
            let body = api::CreateOrgRequest { name };
            let made: api::OrgInfo = api
                .send(api.request(Method::POST, "/v1/orgs").json(&body))?
                .json()?;
            println!("created organization {}", made.name);
        }
        OrgCommand::List => {
            let orgs: Vec<api::OrgInfo> = api.send(api.get("/v1/orgs"))?.json()?;
            let rows: Vec<Vec<String>> = orgs
                .into_iter()
                .map(|o| {
                    vec![
                        o.role.unwrap_or_else(|| "-".into()),
                        time::local(o.created_at),
                        o.name,
                    ]
                })
                .collect();
            table::show(
                &["ROLE", "CREATED", "ORGANIZATION"],
                &rows,
                "no organizations",
            );
        }
        OrgCommand::Show { name } => {
            let org: api::OrgInfo = api.send(api.get(&format!("/v1/orgs/{name}")))?.json()?;
            println!("organization  {}", org.name);
            println!("created       {}", time::local(org.created_at));
            println!("your role     {}", org.role.as_deref().unwrap_or("-"));
            if org.role.is_some() {
                println!();
                org_members(api, &org.name)?;
            }
        }
        OrgCommand::Delete { name, yes } => {
            if !yes {
                eprintln!(
                    "This removes the organization {name} and its memberships. It must own no repositories. Its audit log is kept."
                );
                eprint!("Type the organization name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != name {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{name}")))?;
            println!("deleted organization {name}");
        }
        OrgCommand::Audit {
            name,
            before,
            limit,
        } => {
            let mut req = api
                .get(&format!("/v1/orgs/{name}/audit"))
                .query(&[("limit", limit)]);
            if let Some(b) = before {
                req = req.query(&[("before", b)]);
            }
            let page: api::AuditPage = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = page
                .entries
                .iter()
                .map(|e| {
                    vec![
                        e.id.to_string(),
                        time::local(e.at),
                        e.actor.clone(),
                        e.action.clone(),
                        time::localize(&e.detail),
                    ]
                })
                .collect();
            table::show(
                &["ID", "WHEN", "ACTOR", "ACTION", "DETAIL"],
                &rows,
                "no audit entries",
            );
            if let Some(next) = page.next_before {
                eprintln!("more: --before {next}");
            }
        }
        OrgCommand::Member(cmd) => org_member_command(api, cmd)?,
        OrgCommand::Policy(cmd) => org_policy_command(api, cmd)?,
    }
    Ok(())
}

fn team_command(api: &Api, cmd: TeamCommand) -> Result<()> {
    match cmd {
        TeamCommand::Create {
            team,
            name,
            description,
        } => {
            let (org, slug) = address::split_team(&team)?;
            let body = api::CreateTeamRequest {
                slug: slug.into(),
                name,
                description,
            };
            let made: api::TeamInfo = api
                .send(
                    api.request(Method::POST, &format!("/v1/orgs/{org}/teams"))
                        .json(&body),
                )?
                .json()?;
            println!("created team {org}/{}", made.slug);
        }
        TeamCommand::List { org } => {
            let teams: Vec<api::TeamInfo> = api
                .send(api.get(&format!("/v1/orgs/{org}/teams")))?
                .json()?;
            let rows: Vec<Vec<String>> = teams
                .into_iter()
                .map(|t| {
                    vec![
                        t.members.len().to_string(),
                        t.repos.len().to_string(),
                        time::local(t.created_at),
                        t.name,
                        t.slug,
                    ]
                })
                .collect();
            table::show(
                &["MEMBERS", "REPOS", "CREATED", "NAME", "TEAM"],
                &rows,
                "no teams",
            );
        }
        TeamCommand::Show { team } => {
            let (org, slug) = address::split_team(&team)?;
            let t: api::TeamInfo = api
                .send(api.get(&format!("/v1/orgs/{org}/teams/{slug}")))?
                .json()?;
            println!("team         {org}/{}", t.slug);
            println!("name         {}", t.name);
            println!("description  {}", t.description);
            println!("created      {}", time::local(t.created_at));
            println!();
            let rows: Vec<Vec<String>> = t.members.into_iter().map(|m| vec![m]).collect();
            table::show(&["MEMBER"], &rows, "no members");
            println!();
            let rows: Vec<Vec<String>> =
                t.repos.into_iter().map(|r| vec![r.role, r.repo]).collect();
            table::show(&["ROLE", "REPOSITORY"], &rows, "no repositories");
        }
        TeamCommand::Delete { team, yes } => {
            let (org, slug) = address::split_team(&team)?;
            if !yes {
                eprintln!(
                    "This removes the team {team}, its memberships and its roles on repositories."
                );
                eprint!("Type the team name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != team {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{org}/teams/{slug}")))?;
            println!("deleted team {team}");
        }
        TeamCommand::Member(TeamMemberCommand::Add { team, user }) => {
            let (org, slug) = address::split_team(&team)?;
            api.send(api.request(
                Method::PUT,
                &format!("/v1/orgs/{org}/teams/{slug}/members/{user}"),
            ))?;
            println!("added {user} to {team}");
        }
        TeamCommand::Member(TeamMemberCommand::Remove { team, user }) => {
            let (org, slug) = address::split_team(&team)?;
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/orgs/{org}/teams/{slug}/members/{user}"),
            ))?;
            println!("removed {user} from {team}");
        }
        TeamCommand::Grant { team, repo, role } => {
            let (_, slug) = address::split_team(&team)?;
            address::split_repo(&repo)?;
            let body = api::SetTeamRoleRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &format!("/v1/repos/{repo}/teams/{slug}"))
                    .json(&body),
            )?;
            println!("{team} is now {role} on {repo}");
        }
        TeamCommand::Revoke { team, repo } => {
            let (_, slug) = address::split_team(&team)?;
            address::split_repo(&repo)?;
            api.send(api.request(Method::DELETE, &format!("/v1/repos/{repo}/teams/{slug}")))?;
            println!("{team} no longer has a role on {repo}");
        }
        TeamCommand::Access { repo } => {
            let route = match repo {
                Some(r) => {
                    address::split_repo(&r)?;
                    format!("/v1/repos/{r}/teams")
                }
                None => api.repo_route("/teams")?,
            };
            let teams: Vec<api::RepoTeam> = api.send(api.get(&route))?.json()?;
            let rows: Vec<Vec<String>> = teams
                .into_iter()
                .map(|t| vec![t.role, t.name, t.slug])
                .collect();
            table::show(&["ROLE", "NAME", "TEAM"], &rows, "no teams");
        }
    }
    Ok(())
}

fn org_policy_command(api: &Api, cmd: OrgPolicyCommand) -> Result<()> {
    match cmd {
        OrgPolicyCommand::Show { org } => {
            let policy: api::RepoPolicyInfo = api
                .send(api.get(&format!("/v1/orgs/{org}/repo-policy")))?
                .json()?;
            println!("members can create  {}", policy.member_creation);
            println!();
            let rows: Vec<Vec<String>> = policy
                .rules
                .into_iter()
                .map(|r| vec![r.effect, r.kind, r.scope, r.subject])
                .collect();
            table::show(&["EFFECT", "KIND", "SCOPE", "SUBJECT"], &rows, "no rules");
        }
        OrgPolicyCommand::Set { org, members } => {
            let body = api::SetRepoPolicyRequest {
                member_creation: members,
            };
            let set: api::RepoPolicyInfo = api
                .send(
                    api.request(Method::PUT, &format!("/v1/orgs/{org}/repo-policy"))
                        .json(&body),
                )?
                .json()?;
            println!("members of {org} can create: {}", set.member_creation);
        }
        OrgPolicyCommand::Allow {
            org,
            kind,
            subject,
            scope,
        } => set_creation_rule(api, &org, "allow", &kind, &subject, scope)?,
        OrgPolicyCommand::Deny {
            org,
            kind,
            subject,
            scope,
        } => set_creation_rule(api, &org, "deny", &kind, &subject, scope)?,
        OrgPolicyCommand::Remove {
            org,
            effect,
            kind,
            subject,
        } => {
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}"),
            ))?;
            println!("removed the {effect} rule for {kind} {subject} in {org}");
        }
    }
    Ok(())
}

fn set_creation_rule(
    api: &Api,
    org: &str,
    effect: &str,
    kind: &str,
    subject: &str,
    scope: String,
) -> Result<()> {
    let body = api::SetCreationRuleRequest { scope };
    let rule: api::CreationRuleInfo = api
        .send(
            api.request(
                Method::PUT,
                &format!("/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}"),
            )
            .json(&body),
        )?
        .json()?;
    println!(
        "{org}: {} {} {} ({} repositories)",
        rule.effect, rule.kind, rule.subject, rule.scope
    );
    Ok(())
}

fn org_members(api: &Api, org: &str) -> Result<()> {
    let members: Vec<api::OrgMember> = api
        .send(api.get(&format!("/v1/orgs/{org}/members")))?
        .json()?;
    let rows: Vec<Vec<String>> = members.into_iter().map(|m| vec![m.role, m.user]).collect();
    table::show(&["ROLE", "USER"], &rows, "no members");
    Ok(())
}

fn org_member_command(api: &Api, cmd: OrgMemberCommand) -> Result<()> {
    match cmd {
        OrgMemberCommand::List { org } => org_members(api, &org)?,
        OrgMemberCommand::Add { org, user, role } => {
            let body = api::AddOrgMemberRequest {
                user: user.clone(),
                role,
            };
            let added: api::OrgMember = api
                .send(
                    api.request(Method::POST, &format!("/v1/orgs/{org}/members"))
                        .json(&body),
                )?
                .json()?;
            println!("added {user} to {org} as {}", added.role);
        }
        OrgMemberCommand::Set { org, user, role } => {
            let body = api::SetOrgRoleRequest { role: role.clone() };
            api.send(
                api.request(Method::PATCH, &format!("/v1/orgs/{org}/members/{user}"))
                    .json(&body),
            )?;
            println!("{user} is now {role} of {org}");
        }
        OrgMemberCommand::Remove { org, user } => {
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{org}/members/{user}")))?;
            println!("removed {user} from {org}");
        }
    }
    Ok(())
}

fn token_command(api: &Api, cmd: TokenCommand) -> Result<()> {
    match cmd {
        TokenCommand::Create {
            name,
            permissions,
            repos,
            expires_days,
        } => {
            let repos = if repos.is_empty() {
                vec![api.repo.clone().context(if api.on_git {
                        "this is a git repository, so name the repositories the token is for with --repos owner/name"
                    } else {
                        "name the repositories the token is for with --repos owner/name"
                    })?]
            } else {
                repos
            };
            let body = api::CreateTokenRequest {
                name,
                permissions,
                repos,
                expires_at: expires_days.map(|d| chrono::Utc::now() + chrono::Duration::days(d)),
            };
            let made: api::CreatedToken = api
                .send(api.request(Method::POST, "/v1/tokens").json(&body))?
                .json()?;
            println!("{}", made.token);
            eprintln!(
                "token {} created; this is the only time it is shown",
                made.info.id
            );
        }
        TokenCommand::List { for_user } => {
            let mut req = api.get("/v1/tokens");
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            let tokens: Vec<api::TokenInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = tokens
                .into_iter()
                .map(|t| {
                    let state = if t.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    vec![
                        t.id.to_string(),
                        state.to_string(),
                        t.expires_at.map_or("never".to_string(), time::local),
                        t.name,
                        t.permissions.join(","),
                    ]
                })
                .collect();
            table::show(
                &["ID", "STATE", "EXPIRES", "NAME", "PERMISSIONS"],
                &rows,
                "no tokens",
            );
        }
        TokenCommand::Revoke { id } => {
            api.send(api.request(Method::DELETE, &format!("/v1/tokens/{id}")))?;
            println!("revoked {id}");
        }
    }
    Ok(())
}

/// In a `local_store` workspace, the server's head revision of a path a local commit changed: the lock covers
/// the merge that brings that revision in, so the stale revision the files were last updated at is not the base.
fn local_base(backend: &PynServer, ws: Option<&Workspace>, path: &str) -> Result<Option<u64>> {
    let Some(store) = ws.and_then(local_store::LocalStore::open) else {
        return Ok(None);
    };
    if !store
        .unpublished(&branch::name(ws))?
        .paths
        .contains_key(path)
    {
        return Ok(None);
    }
    Ok(backend
        .list_files()?
        .into_iter()
        .find(|e| e.path == path)
        .and_then(|e| e.revision))
}

/// A stale-base refusal in a workspace means the file moved on: say how to catch up.
fn behind_hint(err: anyhow::Error, in_workspace: bool) -> anyhow::Error {
    if in_workspace && err.to_string().starts_with("stale_base") {
        err.context("the server has a newer revision; run `pyn update` first")
    } else {
        err
    }
}

fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn config_command(
    cmd: ConfigCommand,
    ws: Option<&Workspace>,
    mine: &Settings,
    yours: &Settings,
    user_config: &std::path::Path,
) -> Result<()> {
    let show = |name: &str, s: &Settings, rows: &mut Vec<Vec<String>>| -> Result<()> {
        for key in workspace::SETTING_KEYS {
            if let Some(v) = s.get(key)? {
                rows.push(vec![key.to_string(), name.to_string(), v.to_string()]);
            }
        }
        Ok(())
    };
    match cmd {
        ConfigCommand::Init => {
            if !interactive() {
                bail!(
                    "`pyn config init` asks questions and needs a terminal; use `pyn config set --global <key> <value>` instead"
                );
            }
            let settings =
                userconfig::prompt(yours, &mut std::io::stdin().lock(), &mut std::io::stderr())?;
            settings.save(user_config)?;
            println!("wrote {}", user_config.display());
        }
        ConfigCommand::List { global, local } => {
            let mut rows = Vec::new();
            if global {
                show("user", yours, &mut rows)?;
            } else if local {
                show("workspace", mine, &mut rows)?;
            } else {
                show("workspace", mine, &mut rows)?;
                show("user", yours, &mut rows)?;
            }
            table::show(&["KEY", "SCOPE", "VALUE"], &rows, "no settings");
        }
        ConfigCommand::Get { key, global, local } => {
            let value = match (global, local) {
                (true, _) => yours.get(&key)?.cloned(),
                (_, true) => mine.get(&key)?.cloned(),
                _ => resolve(
                    None,
                    mine.get(&key)?,
                    yours.get(&key)?,
                    (key == "server").then_some(DEFAULT_SERVER),
                ),
            };
            match value {
                Some(v) => println!("{v}"),
                None => bail!("{key} is not set"),
            }
        }
        ConfigCommand::Set { key, value, global } => {
            if global {
                let mut settings = yours.clone();
                settings.set(&key, value)?;
                settings.save(user_config)?;
            } else {
                let w =
                    ws.context("not in a workspace; run `pyn clone <dir>` first, or use --global")?;
                let mut settings = mine.clone();
                settings.set(&key, value)?;
                settings.save(&w.config_path())?;
            }
        }
    }
    Ok(())
}

/// The server ends the sign-in tokens `pyn login` issued when a password changes. If this CLI's token was one,
/// forget it and say so rather than leave a dead token stored.
fn report_ended_sign_in(api: &Api) -> Result<()> {
    let Some(token) = &api.token else {
        return Ok(());
    };
    let ended = matches!(
        api.send(api.get("/v1/me")),
        Err(e) if e.downcast_ref::<client::ApiError>().is_some_and(|e| e.status == 401)
    );
    if !ended {
        return Ok(());
    }
    let saved = credentials::load(&api.base)?.filter(|e| &e.token == token);
    match saved {
        Some(entry) => {
            credentials::remove(&api.base)?;
            println!(
                "your sign-in on {} ended with the password change; run `pyn login {}` to sign in again",
                api.base, entry.user
            );
        }
        None => println!(
            "the token this command used ended with the password change; sign in again with `pyn login`"
        ),
    }
    Ok(())
}

fn default_public_key() -> Result<std::path::PathBuf> {
    let home =
        std::env::var("HOME").context("cannot find your home directory; name the key file")?;
    ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"]
        .iter()
        .map(|name| std::path::Path::new(&home).join(".ssh").join(name))
        .find(|p| p.is_file())
        .context("no public key found in ~/.ssh; name the key file, or create one with `ssh-keygen -t ed25519`")
}

#[cfg(test)]
mod fatal_formatter_tests {
    use super::fatal_message;

    #[test]
    fn flat_error() {
        assert_eq!(fatal_message(&anyhow::anyhow!("flat")), "flat");
    }

    #[test]
    fn distinct_context_and_source() {
        let error = anyhow::anyhow!("source").context("context");
        assert_eq!(fatal_message(&error), "context: source");
    }

    #[test]
    fn source_embedded_in_display() {
        let error = anyhow::Error::new(pyn_gitwork::Error::Io {
            path: "path".into(),
            source: std::io::Error::other("source"),
        });
        assert_eq!(fatal_message(&error), "path: source");
    }

    #[test]
    fn short_cause_that_is_a_suffix_of_its_context() {
        let error = anyhow::anyhow!("file").context("reading file");
        assert_eq!(fatal_message(&error), "reading file: file");
    }

    #[test]
    fn overlapping_suffix_text() {
        let error = anyhow::anyhow!("inner shared tail").context("outer shared tail");
        assert_eq!(
            fatal_message(&error),
            "outer shared tail: inner shared tail"
        );
    }
}
