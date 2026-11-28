use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use pyn_proto as api;

mod access_session;
mod account;
mod address;
mod admin;
mod backend;
mod branch;
mod client;
mod closed_pipe;
mod commit_format;
mod credentials;
mod date_mode;
mod diff;
mod diff_color;
mod diff_driver;
mod diff_opts;
mod diff_raw;
mod diffstat;
mod dirstat;
mod ext_diff;
mod freeze;
mod git_home;
mod git_log_options;
mod http_transport;
mod init_pyn;
mod json_out;
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
mod lock_watch;
mod lockcheck;
mod log_filter;
mod ls_remote;
mod merging;
mod migrate;
mod mv_plan;
mod no_index;
mod org_settings;
mod pathspec;
mod policy;
mod policy_lint;
mod pr;
mod probe;
mod profile;
mod pyn_server;
mod quiet;
mod rename;
mod repo_tool;
mod rm_mv;
mod sanitize;
mod server_tags;
mod short_status;
mod snapshot;
mod staging;
mod stranded;
mod sync;
mod table;
mod time;
mod token_args;
mod transfer;
mod transport;
mod two_factor;
mod unsupported;
mod update;
mod userconfig;
mod verify;
mod watch;
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
    /// Machine-readable output for whoami, status, locks, lock, unlock, files, ls, summary and admin access-session; see docs/cli-json.md.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

/// Contiguous groups in `pyn --help` order; each gets its own clap-generated function, which keeps
/// unoptimized builds within a normal thread's stack.
#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Accounts(Accounts),
    #[command(flatten)]
    Branching(Branching),
    #[command(flatten)]
    Syncing(Syncing),
    #[command(flatten)]
    Workspaces(Workspaces),
    #[command(flatten)]
    Inspection(Inspection),
    #[command(flatten)]
    Plumbing(Plumbing),
    #[command(flatten)]
    Identity(Identity),
    #[command(flatten)]
    Locking(Locking),
    #[command(flatten)]
    Editing(Editing),
    #[command(flatten)]
    History(History),
    #[command(flatten)]
    Administration(Administration),
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum Accounts {
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
    /// Manage your own account: email addresses, username and deletion.
    #[command(subcommand)]
    Account(account::AccountCommand),
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
    /// List, accept and decline your invitations to organizations.
    #[command(subcommand)]
    Invitation(InvitationCommand),
    /// Create and manage an organization's teams, and grant them roles on its repositories.
    #[command(subcommand)]
    Team(TeamCommand),
}

#[derive(Subcommand)]
enum Branching {
    /// List, create, rename or delete branches.
    Branch(branch::BranchArgs),
    /// Switch the workspace to another branch.
    Switch(branch::SwitchArgs),
    /// Switch branches, or restore files from the workspace's revisions or from another commit (`pyn lock` takes a lock).
    Checkout(branch::CheckoutArgs),
    /// Merge a branch into the current branch.
    Merge(merging::MergeArgs),
    /// Propose, discuss and merge changes between branches as pull requests.
    #[command(subcommand)]
    Pr(pr::PrCommand),
    /// Create, list and delete tags; on a pyn server they stay in the local store and are never pushed.
    Tag(git_home::TagArgs),
    /// Save, list and restore local changes (a git repository only).
    Stash(git_home::StashArgs),
    /// Manage linked work trees of a repository (a git repository only).
    Worktree(git_home::WorktreeArgs),
    /// Initialize, update and inspect nested repositories recorded as gitlinks (a git repository only).
    Submodule(git_home::SubmoduleArgs),
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
    /// Track paths with Git LFS, and fetch, pull, push, list and prune its objects (a git repository only).
    Lfs(git_home::LfsArgs),
}

#[derive(Subcommand)]
enum Syncing {
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
}

#[derive(Subcommand)]
enum Workspaces {
    /// List the references in a remote git repository (ssh or smart HTTP; works anywhere).
    LsRemote(ls_remote::LsRemoteArgs),
    /// Create, verify and unpack git bundle files (works anywhere; creating needs a git repository).
    Bundle(git_home::BundleArgs),
    /// Show what differs between this workspace and the server.
    Status(short_status::StatusArgs),
    /// Start a pyn repository in a folder that is not tracked yet (`pyn convert` adopts a git repository).
    Init(init_pyn::InitArgs),
    /// Start a new repository used like git: a real `.git` written by pyn, plus pyn's own folder.
    InitGit(git_home::InitGitArgs),
    /// Adopt an existing git repository (hide `.pyn/` from git), or move it between homes keeping its history: `--home git` from a pyn server workspace, `--home pyn` from a git repository.
    Convert(git_home::ConvertArgs),
    /// Fetch new and newer files; files with local changes are left alone.
    Update {
        /// Only these paths; everything if omitted.
        paths: Vec<String>,
        /// Bring the files to the repository as of this commit (`HEAD~N`, a commit hash), older or newer than the workspace.
        #[arg(long)]
        revision: Option<String>,
    },
}

#[derive(Subcommand)]
enum Inspection {
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
        /// Ignore changes in submodules: `none`, `untracked`, `dirty` or `all` (a git repository only).
        #[arg(
            long,
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "all",
            value_name = "WHEN"
        )]
        ignore_submodules: Option<String>,
    },
    /// Print the object ids revisions name, and facts about the repository (a git repository only).
    RevParse(git_home::RevParseArgs),
    /// Show the attributes that apply to paths (a git repository only).
    CheckAttr(git_home::CheckAttrArgs),
    /// Show the files in the index and the work tree (a git repository only).
    LsFiles(git_home::LsFilesArgs),
    /// List the contents of a tree object (a git repository only).
    LsTree(git_home::LsTreeArgs),
    /// Show an object's type, size or content (a git repository only).
    CatFile(git_home::CatFileArgs),
    /// Check the signature of commits (a git repository only).
    VerifyCommit(git_home::VerifyCommitArgs),
    /// Check the signature of tags (a git repository only).
    VerifyTag(git_home::VerifyTagArgs),
    /// List the refs and the ids they point at (a git repository only).
    ShowRef(git_home::ShowRefArgs),
    /// Choose which tracked files the work tree holds (a git repository only).
    SparseCheckout(git_home::SparseCheckoutArgs),
}

#[derive(Subcommand)]
enum Plumbing {
    /// Count loose objects and packs and the space they take (a git repository only).
    CountObjects(git_home::CountObjectsArgs),
    /// Check the connectivity and validity of the objects (a git repository only).
    Fsck(git_home::FsckArgs),
    /// Remove unreachable loose objects (a git repository only).
    Prune(git_home::PruneArgs),
    /// Clean up and optimize the repository's storage (a git repository only).
    Gc(git_home::GcArgs),
    /// Pack loose refs into one file (a git repository only).
    PackRefs(git_home::PackRefsArgs),
    /// Remove loose objects that a pack also holds (a git repository only).
    PrunePacked(git_home::PrunePackedArgs),
    /// Write a pack of the listed objects (a git repository only).
    PackObjects(git_home::PackObjectsArgs),
    /// Unpack a pack read from standard input into loose objects (a git repository only).
    UnpackObjects(git_home::UnpackObjectsArgs),
    /// Run housekeeping tasks and register the repository for them (a git repository only).
    Maintenance(git_home::MaintenanceArgs),
    /// Pack objects into packs and drop what they replace (a git repository only).
    Repack(git_home::RepackArgs),
    /// Summarize the history by author (a git repository only).
    Shortlog(git_home::ShortlogArgs),
    /// Show the canonical name and email of contacts (a git repository only).
    CheckMailmap(git_home::CheckMailmapArgs),
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
        /// Check and print each signed commit's signature (git repository only).
        #[arg(long)]
        show_signature: bool,
        #[command(flatten)]
        format: Box<DiffFormat>,
    },
}

#[derive(Subcommand)]
enum Identity {
    /// Read and write settings, per workspace or for your user; on a git repository also git's `config` flag forms.
    Config(ConfigArgs),
    /// Show who you are signed in as and, in a repository, what you may do there.
    Whoami,
    /// Show the pyn version; with --check, ask the server you are signed in to whether a newer release exists.
    Version {
        /// Ask the server (administrators only; the server must have its update check on).
        #[arg(long)]
        check: bool,
    },
    /// Show profiles, and edit your own display name, bio, links and avatar.
    #[command(subcommand)]
    Profile(profile::ProfileCommand),
    /// Link SSH public keys to your account.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Create, list and revoke invitations.
    #[command(subcommand)]
    Invite(InviteCommand),
    /// Add accounts to the current repository (needs manage_users).
    #[command(subcommand)]
    User(UserCommand),
}

#[derive(Subcommand)]
enum Locking {
    /// List live locks in the repository, or with --mine yours in every repository.
    Locks {
        #[arg(long)]
        mine: bool,
        /// Keep running and print a line per lock change (with --json, one document per line).
        #[arg(long, conflicts_with = "mine")]
        watch: bool,
        /// With --watch, resume after this cursor (a `cursor` from an earlier line) instead of starting now.
        #[arg(long, requires = "watch", value_name = "CURSOR")]
        after: Option<String>,
        /// With --watch, seconds between polls when the server has no change stream.
        #[arg(long, requires = "watch", value_name = "SECONDS", default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
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
        /// Revision your copy is at; by default the workspace's, or the server's head outside a workspace.
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
    /// Make a read-only exclusive file writable for offline work, without the server and without taking a lock.
    Edit {
        #[arg(required = true)]
        paths: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Editing {
    /// Stage changes for the next commit: paths, folders or `.`. An exclusive path needs your lock only to publish.
    Add {
        paths: Vec<String>,
        #[command(flatten)]
        pathspec_file: pathspec::PathspecFile,
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
        /// Stage every tracked file again through the clean conversion; implies --update (a git repository only).
        #[arg(long)]
        renormalize: bool,
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
        /// Stage the untracked files that are not ignored first (a pyn extension; git's `commit -u` is `--untracked-files`).
        #[arg(short = 'u', long)]
        untracked: bool,
        #[command(flatten)]
        git: Box<git_home::GitCommitArgs>,
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
}

#[derive(Subcommand)]
enum History {
    /// Make an older revision the new head (needs the lock and the restore permission, asks for confirmation), or
    /// with --staged take paths out of the staged changes.
    Restore {
        /// A path and its older revision (or use --source), or with --staged the paths to unstage.
        #[arg(required_unless_present_any = ["patch", "pathspec_from_file"])]
        args: Vec<String>,
        #[command(flatten)]
        pathspec_file: pathspec::PathspecFile,
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
        /// Restore paths outside the sparse-checkout patterns too (a git repository only).
        #[arg(long)]
        ignore_skip_worktree_bits: bool,
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
}

#[derive(Subcommand)]
enum Administration {
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
    /// Watch a repository you can read: hear about all its activity (the default), only its locks, or nothing.
    Watch {
        /// `owner/name`.
        name: String,
        #[arg(long, value_enum, default_value = "all")]
        level: watch::Level,
    },
    /// Stop watching a repository.
    Unwatch {
        /// `owner/name`.
        name: String,
    },
    /// List the repositories you watch, with your level in each.
    Watching,
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
    #[arg(long, value_name = "WIDTHS", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    stat: Option<String>,
    /// Show only the totals line of the diffstat.
    #[arg(long)]
    shortstat: bool,
    /// Show added and deleted line counts per file.
    #[arg(long)]
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
    #[arg(long)]
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
        if self.check || self.name_only || self.name_status {
            return self.build();
        }
        if self.stat.is_none() && self.extra.stat_implied() && self.extra.raw() {
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

    /// `--check`, `--name-only` and `--name-status` replace every other output format, as in git.
    fn build(&self) -> diff::Format {
        let mut stats = Vec::new();
        if self.stat_given() || self.extra.patch_with_stat() {
            stats.push(diff::Kind::Stat);
        }
        if self.shortstat {
            stats.push(diff::Kind::ShortStat);
        }
        if self.numstat {
            stats.push(diff::Kind::NumStat);
        }
        let named = self.check || self.name_only || self.name_status;
        let kind = if self.check {
            diff::Kind::Check
        } else if self.name_only {
            diff::Kind::NameOnly
        } else if self.name_status {
            diff::Kind::NameStatus
        } else if self.extra.dirstat_given() {
            diff::Kind::Dirstat
        } else if self.extra.raw() || self.extra.patch_with_raw() {
            diff::Kind::Raw
        } else if let Some(first) = stats.first() {
            *first
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
            summary: self.summary && !named,
            with_patch: false,
            also: if stats.contains(&kind) {
                stats.split_off(1)
            } else {
                Vec::new()
            },
            kept: Vec::new(),
            unmerged: Vec::new(),
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
        /// Use this file as the repository's first `.pyn/pyn.toml` instead of the starter policy.
        #[arg(long, conflicts_with = "no_policy")]
        policy: Option<PathBuf>,
        /// Create the repository without a `.pyn/pyn.toml`; the repository's initial policy applies until one is added.
        #[arg(long)]
        no_policy: bool,
        /// Whether paths no policy entry covers are `shared` or `exclusive` in the starter policy. Asked in a terminal
        /// when omitted; `shared` otherwise. Not with `--policy` or `--no-policy`.
        #[arg(long, value_enum, conflicts_with_all = ["policy", "no_policy"])]
        default: Option<init_pyn::DefaultMode>,
        /// One line describing the repository.
        #[arg(long)]
        description: Option<String>,
        /// An http or https link shown with the description.
        #[arg(long)]
        website: Option<String>,
        /// A topic; repeat for more.
        #[arg(long = "topic")]
        topics: Vec<String>,
    },
    /// Show a repository: its description, website, topics, languages, visibility, counts and your role.
    Show {
        /// `owner/name`.
        name: String,
    },
    /// Change a repository's description, website or topics; an empty value clears it. Owner and admin only.
    Edit {
        /// `owner/name`.
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        website: Option<String>,
        /// Replace the topics with these; repeat for more.
        #[arg(long = "topic", conflicts_with = "no_topics")]
        topics: Vec<String>,
        /// Remove every topic.
        #[arg(long)]
        no_topics: bool,
        /// Whether people who can read the repository may fork it while it is private.
        #[arg(long)]
        allow_private_forks: Option<bool>,
    },
    /// Set or clear a repository's icon. Owner and admin only.
    #[command(subcommand)]
    Icon(RepoIconCommand),
    /// Fork a repository you can read into your namespace or an organization: a copy of its default branch with its
    /// history and tags, and its policy, without its locks, linked to it as its source.
    Fork {
        /// `owner/name` of the repository to fork.
        name: String,
        /// The fork's name; the source's by default.
        #[arg(long = "name")]
        new_name: Option<String>,
        /// An organization to fork into; you by default.
        #[arg(long)]
        owner: Option<String>,
        /// Copy every branch and tag, not only the default branch.
        #[arg(long)]
        all_branches: bool,
    },
    /// List the forks of a repository that you may see.
    Forks {
        /// `owner/name`.
        name: String,
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
    /// Star a repository you can read; starring it again changes nothing.
    Star {
        /// `owner/name`.
        name: String,
    },
    /// Remove your star from a repository.
    Unstar {
        /// `owner/name`.
        name: String,
    },
    /// List the repositories someone starred that you may see, newest star first.
    Starred {
        /// The account; you by default.
        user: Option<String>,
    },
    /// Make a repository public (anyone may read it) or private (members only). Owner and admin only.
    Visibility {
        /// `owner/name`.
        name: String,
        /// `public` or `private`.
        visibility: String,
    },
    /// Show the stored bytes, cap, files and revisions of a repository.
    Usage {
        /// `owner/name`.
        name: String,
        /// Also list stored bytes by top-level folder and the largest paths (repository admins).
        #[arg(long)]
        breakdown: bool,
    },
    /// Set the most distinct content a repository may store: bytes, or a size like 50G; `default` removes the cap.
    /// Owner, server administrator or a service credential with manage_limits.
    StorageCap {
        /// `owner/name`.
        name: String,
        size: limits::LimitValue,
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
enum RepoIconCommand {
    /// Upload a png, jpeg or webp image of at most 1 MiB as the icon.
    Set {
        /// `owner/name`.
        name: String,
        file: PathBuf,
    },
    /// Remove the icon; the owner's avatar shows instead.
    Clear {
        /// `owner/name`.
        name: String,
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
    /// List and cancel an organization's pending invitations.
    #[command(subcommand)]
    Invitation(OrgInvitationCommand),
    /// Show and change who may create repositories in an organization (owners only).
    #[command(subcommand)]
    Policy(OrgPolicyCommand),
    /// Show and change an organization's repository defaults (owners only).
    #[command(subcommand)]
    Settings(org_settings::OrgSettingsCommand),
}

#[derive(Subcommand)]
enum InvitationCommand {
    /// List the organization invitations waiting for you.
    List,
    /// Accept an invitation and join the organization with the offered role.
    Accept { org: String },
    /// Decline an invitation.
    Decline { org: String },
}

#[derive(Subcommand)]
enum OrgInvitationCommand {
    /// List an organization's pending invitations (owners only).
    List { org: String },
    /// Withdraw a pending invitation (owners only).
    Cancel { org: String, user: String },
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
    /// Invite an existing account to an organization (owners only); a server administrator may be able to add directly.
    #[command(alias = "invite")]
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
        org: String,
        /// Display name; the slug is derived from it unless `--slug` is given.
        name: String,
        /// 1 to 39 lowercase letters, digits, `-` or `_`, starting with a letter or digit.
        #[arg(long)]
        slug: Option<String>,
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

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
struct ConfigArgs {
    #[command(subcommand)]
    cmd: Option<ConfigCommand>,
    #[command(flatten)]
    git: git_home::ConfigFlags,
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
    /// Set one setting for this workspace or git repository (--local, the default), or for your user with --global.
    Set {
        key: String,
        value: String,
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        local: bool,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Show which paths would change mode, and which locks would be released, if this policy file were committed.
    Preview {
        /// A proposed `pyn.toml`.
        file: std::path::PathBuf,
    },
    /// List tracked paths where `lockable` in `.gitattributes` and `exclusive` in `.pyn/pyn.toml` disagree (git repositories).
    Lint {
        /// The policy to check against; the work tree's `.pyn/pyn.toml` by default.
        #[arg(long)]
        policy: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token. It is shown once, and cannot be changed afterwards.
    Create {
        name: String,
        /// Comma-separated resource:level grants, for example contents:write,locks:admin, org_members:read or account_ssh_keys:write. Repository resources: metadata, contents, locks, policy, members, administration, audit. Organization: org_members, org_teams, org_administration, org_audit. Account: account_profile, account_ssh_keys, account_locks. Levels: read, write, and admin for locks.
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
        /// The resource owner: your account (the default) or an organization you belong to; taken from --repos when those are given.
        #[arg(long)]
        owner: Option<String>,
        /// Repository access: public, all (every repository of the owner) or selected. Selected when --repos is given, or when permissions name repository resources; public otherwise.
        #[arg(long)]
        access: Option<String>,
        /// Comma-separated repositories (owner/name) of a selected token; the current repository by default.
        #[arg(long, value_delimiter = ',')]
        repos: Vec<String>,
        /// Expire the token after this many days; 30 by default.
        #[arg(long, conflicts_with_all = ["never", "expires"])]
        expires_days: Option<i64>,
        /// Expire the token at the end of this date (YYYY-MM-DD, UTC).
        #[arg(long, conflicts_with = "never")]
        expires: Option<String>,
        /// Never expire the token.
        #[arg(long)]
        never: bool,
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
    /// Give an existing account a role; register new people with `pyn user add` or an invitation.
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

/// Puts `branch.<current>.mergeOptions` right after `merge`, so the command line's own options win; with
/// `--abort`, `--continue` or `--quit` they are only checked.
fn with_merge_options(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    const ENDS: [&str; 3] = ["--abort", "--continue", "--quit"];
    let mut i = 1;
    while i < args.len() {
        match args[i].to_str() {
            Some("--server" | "--token" | "--user" | "--repo") => i += 1,
            Some(a) if a.starts_with('-') => {}
            _ => break,
        }
        i += 1;
    }
    if args.get(i).is_none_or(|a| a != "merge") {
        return args;
    }
    let git = std::env::current_dir()
        .ok()
        .and_then(|cwd| git_home::detect(&cwd).ok().flatten());
    if args.iter().any(|a| ENDS.iter().any(|e| a == e)) {
        if let Some(git) = &git {
            git_home::check_merge_options(git);
        }
        return args;
    }
    let configured = git.and_then(|git| git_home::configured_merge_options(&git));
    if let Some(words) = configured {
        args.splice(i + 1..i + 1, words.into_iter().map(Into::into));
    }
    args
}

fn parse_cli(args: Vec<std::ffi::OsString>) -> Result<Cli, clap::Error> {
    use clap::FromArgMatches;
    let matches = Cli::command().try_get_matches_from(args)?;
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Command::Branching(Branching::Merge(merge)) = &mut cli.command
        && let Some(("merge", sub)) = matches.subcommand()
    {
        merge.order_message_parts(sub);
    }
    Ok(cli)
}

fn main() {
    closed_pipe::install();
    pyn_gittransport::promisor::install();
    git_home::lfs_net::install();
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
    let mut cli = match parse_cli(with_merge_options(attached_gpg_keys(attached_pull_rebase(
        counted_log(numbered_commits(rename::attached(
            std::env::args_os().collect(),
        ))),
    )))) {
        Ok(cli) => cli,
        Err(e) => {
            let code = match (e.use_stderr(), fatal_option_error(&e)) {
                (false, _) => 0,
                (true, true) => 128,
                (true, false) => 129,
            };
            if let Some(arg) = unrecognized_log_option(&e) {
                eprintln!("fatal: unrecognized argument: {arg}");
                std::process::exit(128);
            }
            let _ = e.print();
            std::process::exit(code);
        }
    };
    if cli.json {
        if !json_supported(&cli.command) {
            usage_error("`--json` is not available for this command");
        }
        json_out::set();
    }
    if let Err(e) = expand_pathspec_files(&mut cli.command) {
        eprintln!("fatal: {e}");
        std::process::exit(128);
    }
    if let Command::Editing(Editing::Add {
        paths,
        update: false,
        all: false,
        patch: false,
        interactive: false,
        ..
    }) = &cli.command
        && paths.is_empty()
        && (in_git_home() || in_pyn_workspace())
        && !in_bare_git_home()
    {
        eprintln!("Nothing specified, nothing added.");
        eprintln!("hint: Maybe you wanted to say 'git add .'?");
        eprintln!("hint: Turn this message off by running");
        eprintln!("hint: \"git config advice.addEmptyPathspec false\"");
        std::process::exit(0);
    }
    if let Command::Identity(Identity::Config(args)) = &cli.command
        && args.cmd.is_none()
        && !args.git.is_empty()
    {
        let result = std::env::current_dir()
            .map_err(Into::into)
            .and_then(|cwd| git_home::config_flags(&cwd, &args.git));
        if let Err(e) = result {
            eprintln!("fatal: {}", sanitize::text(&fatal_message(&e)));
            std::process::exit(128);
        }
        return;
    }
    if let Command::Plumbing(Plumbing::Shortlog(args)) = &cli.command
        && !in_git_home()
        && !in_pyn_workspace()
    {
        if let Err(e) = git_home::shortlog_outside_repo(args) {
            eprintln!("fatal: {}", sanitize::text(&fatal_message(&e)));
            std::process::exit(128);
        }
        return;
    }
    if let Command::Workspaces(Workspaces::Status(args)) = &cli.command
        && let Err((code, message)) = args.check_porcelain()
    {
        eprintln!("{message}");
        std::process::exit(code);
    }
    if let Command::Editing(Editing::Rm(args)) = &cli.command
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
    let json = json_out::on();
    let report = |prefix: &str, e: &anyhow::Error, message: &str| {
        let message = sanitize::text(message);
        if json {
            json_out::print_error(e, &message);
        } else {
            eprintln!("{prefix}{message}");
        }
    };
    if let Err(e) = run(cli) {
        if let Some(refused) = e.downcast_ref::<git_home::Refused>() {
            report("error: ", &e, &refused.to_string());
            std::process::exit(1);
        }
        if let Some(exits) = e.downcast_ref::<git_home::Exits>() {
            report("error: ", &e, &exits.1);
            std::process::exit(exits.0);
        }
        if let Some(rejected) = e.downcast_ref::<local_sync::Rejected>() {
            report("", &e, &rejected.to_string());
            std::process::exit(1);
        }
        if let Some(rejected) = e.downcast_ref::<local_sync::LockRejected>() {
            report("", &e, &rejected.to_string());
            std::process::exit(128);
        }
        report("fatal: ", &e, &fatal_message(&e));
        std::process::exit(128);
    }
}

fn json_supported(command: &Command) -> bool {
    matches!(
        command,
        Command::Identity(Identity::Whoami)
            | Command::Identity(Identity::Version { check: true })
            | Command::Workspaces(Workspaces::Status(_))
            | Command::Locking(Locking::Locks { .. })
            | Command::Locking(Locking::Lock { .. })
            | Command::Locking(Locking::Unlock { .. })
            | Command::Locking(Locking::Files)
            | Command::Locking(Locking::Ls { .. })
            | Command::Locking(Locking::Summary)
            | Command::Administration(Administration::Admin(admin::AdminCommand::AccessSession(_)))
    )
}

fn expand_pathspec_files(command: &mut Command) -> Result<()> {
    match command {
        Command::Editing(Editing::Add {
            paths,
            pathspec_file,
            patch,
            interactive,
            ..
        }) => pathspec_file.expand(
            paths,
            (*patch || *interactive).then_some("--interactive/--patch"),
        ),
        Command::History(History::Restore {
            args,
            pathspec_file,
            patch,
            ..
        }) => {
            pathspec_file.expand(args, patch.then_some("--patch"))?;
            if pathspec_file.pathspec_from_file.is_some() && args.is_empty() {
                bail!("you must specify path(s) to restore");
            }
            Ok(())
        }
        Command::Editing(Editing::Rm(a)) => a.pathspec_file.expand(&mut a.paths, None),
        Command::Syncing(Syncing::Reset(a)) => a.expand_pathspec_file(),
        Command::Branching(Branching::Checkout(a)) => a.expand_pathspec_file(),
        Command::Branching(Branching::Stash(a)) => a.expand_pathspec_file(),
        _ => Ok(()),
    }
}

/// The option git's revision parser rejects with `fatal: unrecognized argument` (128) in `log`, `show` and `reflog`.
fn unrecognized_log_option(e: &clap::Error) -> Option<String> {
    if e.kind() != clap::error::ErrorKind::UnknownArgument
        || !matches!(
            first_subcommand().as_deref(),
            Some("log" | "show" | "reflog")
        )
    {
        return None;
    }
    if std::env::args().any(|a| matches!(a.as_str(), "expire" | "delete" | "exists")) {
        return None;
    }
    let arg = e.get(clap::error::ContextKind::InvalidArg)?.to_string();
    (arg.starts_with('-') && !git_log_options::is_git_option(&arg)).then_some(arg)
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

fn in_bare_git_home() -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| git_home::detect(&cwd).ok().flatten())
        .is_some_and(|g| g.is_bare())
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
        Command::Accounts(Accounts::Login { .. })
            | Command::Accounts(Accounts::Logout)
            | Command::Accounts(Accounts::Register { .. })
            | Command::Accounts(Accounts::Password { .. })
            | Command::Accounts(Accounts::Account(_))
            | Command::Identity(Identity::Key(_))
            | Command::Identity(Identity::Profile(_))
            | Command::History(History::Token(_))
            | Command::Identity(Identity::User(_))
            | Command::Accounts(Accounts::Org(_))
            | Command::Accounts(Accounts::Invitation(_))
            | Command::Accounts(Accounts::Team(_))
            | Command::Accounts(Accounts::Repo(_))
            | Command::Administration(Administration::Watch { .. })
            | Command::Administration(Administration::Unwatch { .. })
            | Command::Administration(Administration::Watching)
            | Command::Administration(Administration::Usage { .. })
            | Command::Administration(Administration::Setup { .. })
            | Command::Administration(Administration::Admin(_))
            | Command::Identity(Identity::Whoami)
            | Command::Identity(Identity::Version { .. })
            | Command::Administration(Administration::Member(_))
            | Command::Administration(Administration::Role(_))
            | Command::Identity(Identity::Invite(_))
            | Command::Workspaces(Workspaces::LsRemote(_))
            | Command::Workspaces(Workspaces::Bundle(_))
    )
}

/// Clones from a git remote when the address is one; `None` leaves it to the pyn server clone.
fn clone_from_git(cli: &Cli, cwd: &std::path::Path) -> Result<Option<()>> {
    let Command::Accounts(Accounts::Clone {
        source,
        dir,
        revision,
        local_store,
        no_local_store,
        git,
    }) = &cli.command
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
    if let Command::Workspaces(Workspaces::InitGit(args)) = &cli.command {
        return git_home::init_git(args, &cwd);
    }
    if let Command::Workspaces(Workspaces::Convert(args)) = &cli.command
        && args.home == Some(git_home::Home::Pyn)
    {
        let remote = init_pyn::Remote {
            server: cli.server.clone(),
            repo: cli.repo.clone(),
            token: cli.token.clone(),
            user: cli.user.clone(),
        };
        return git_home::to_pyn(args, &cwd, remote);
    }
    if let Command::Workspaces(Workspaces::Init(args)) = &cli.command {
        let remote = init_pyn::Remote {
            server: cli.server.clone(),
            repo: cli.repo.clone(),
            token: cli.token.clone(),
            user: cli.user.clone(),
        };
        return init_pyn::init(args, &cwd, remote);
    }
    if let Command::Accounts(Accounts::Clone { git, .. }) = &cli.command
        && let Some(flag) = git.first_given()
    {
        usage_error(&format!(
            "`{flag}` applies to a git remote, not a pyn server"
        ));
    }
    let git = if matches!(
        cli.command,
        Command::Accounts(Accounts::Clone { .. }) | Command::Identity(Identity::Config(_))
    ) {
        None
    } else {
        git_home::detect(&cwd)?
    };
    if let Command::Workspaces(Workspaces::LsRemote(args)) = &cli.command {
        return ls_remote::run(args, git.as_ref());
    }
    if let Command::Workspaces(Workspaces::Bundle(args)) = cli.command {
        return git_home::bundle(args, git.as_ref());
    }
    if let Command::Inspection(Inspection::Diff {
        args,
        paths,
        format,
        quiet,
        cached,
        no_index,
        no_patch,
        ..
    }) = &cli.command
        && (*no_index
            || (!cached
                && git.is_none()
                && no_index::are_paths(args.iter().chain(paths))
                && Workspace::discover(&cwd).is_none()))
    {
        let operands: Vec<String> = args.iter().chain(paths).cloned().collect();
        let shown = format.format().silenced(*no_patch);
        diff::exit_for_diff(no_index::run(&operands, &shown, *quiet, git.as_ref())?);
        return Ok(());
    }
    if let Command::Editing(Editing::Commit { git: opts, .. }) = &cli.command
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
        && let Command::Editing(Editing::Commit {
            git: opts, paths, ..
        }) = &cli.command
        && !paths.is_empty()
        && let Some(flag) = opts.author_flag()
    {
        usage_error(&format!(
            "`pyn commit {flag}` applies to staged changes committed to the local store, not to a path commit"
        ));
    }
    if git.is_none()
        && let Command::Editing(Editing::Commit { git: opts, .. }) = &cli.command
        && opts.quiet
    {
        quiet::set();
    }
    let mut cli = cli;
    if git.is_none()
        && let Command::Editing(Editing::Commit {
            git: opts, message, ..
        }) = &mut cli.command
        && let Some(text) = opts.server_message(message)?
    {
        *message = vec![text];
    }
    if git.is_none()
        && let Command::Editing(Editing::Commit { message, .. }) = &cli.command
        && !message.is_empty()
        && git_home::editor::edited_message(message.join("\n\n").as_bytes()).is_none()
    {
        eprintln!("Aborting commit due to empty commit message.");
        std::process::exit(1);
    }
    if let Command::History(History::Log { git: opts, .. }) = &cli.command
        && git.is_some()
        && let Some(why) = opts.refused()
    {
        usage_error(&format!("`pyn log {why}"));
    }
    if let Command::History(History::Log {
        git: opts, path, ..
    }) = &cli.command
        && git.is_none()
        && let Some(flag) = opts.server_unsupported().or_else(|| {
            log_filter::too_many_paths(path.as_deref(), opts, &cwd).then(|| "<path>".to_string())
        })
    {
        usage_error(&format!(
            "`pyn log {flag}` applies to a git repository, not a pyn server"
        ));
    }
    if let Command::Editing(Editing::Commit { message, paths, .. }) = &cli.command
        && message.is_empty()
        && !paths.is_empty()
        && git.is_none()
        && !Workspace::discover(&cwd)
            .is_some_and(|w| merging::in_progress(&w) && !merging::is_paused(&w))
    {
        usage_error("the following required arguments were not provided:\n  --message <MESSAGE>");
    }
    if json_out::on() && git.is_some() && !runs_anywhere(&cli.command) {
        usage_error("`--json` needs a pyn server, and this is a git repository");
    }
    if let Some(git) = &git
        && !runs_anywhere(&cli.command)
    {
        return git_home::run(cli.command, git, &cwd);
    }
    let on_git = git.is_some();
    if let Command::Workspaces(Workspaces::Convert(args)) = &cli.command {
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
    } else if matches!(cli.command, Command::Identity(Identity::Config(_))) {
        match git_home::workspace_at(&cwd)? {
            Some(ws) => Some(ws),
            None => Workspace::discover(&cwd),
        }
    } else {
        Workspace::discover(&cwd)
    };
    let user_config = userconfig::path()?;
    let (mine, mut yours) = (
        ws.as_ref().map(Workspace::settings).unwrap_or_default(),
        Settings::load(&user_config).unwrap_or_default(),
    );
    let source = match &cli.command {
        Command::Accounts(Accounts::Clone { source, .. }) => Some(address::parse_source(source)?),
        _ => None,
    };
    let needs_server = !matches!(cli.command, Command::Identity(Identity::Config(_)))
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
    if matches!(cli.command, Command::Accounts(Accounts::Login { .. })) {
        credentials::warn_cleartext(&base, true);
    } else if token.is_some()
        || matches!(
            cli.command,
            Command::Administration(Administration::Setup { .. })
                | Command::Accounts(Accounts::Register { .. })
                | Command::Accounts(Accounts::Password { .. })
        )
    {
        credentials::warn_cleartext(&base, false);
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

    if let (Command::Editing(Editing::Commit { paths, .. }), Some(w)) = (&cli.command, &ws)
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
            Command::Workspaces(Workspaces::Update { .. }) => {
                branch::require_published(w, "update")?
            }
            Command::Editing(Editing::Commit { paths, .. }) if !paths.is_empty() => {
                branch::require_published(w, "commit <path>")?
            }
            _ => {}
        }
    }

    match cli.command {
        Command::External(_) => unreachable!("handled before run"),
        Command::Workspaces(Workspaces::Init(_)) => {
            unreachable!("handled before the server is consulted")
        }
        Command::Workspaces(Workspaces::InitGit(_)) => {
            unreachable!("handled before the server is consulted")
        }
        Command::Workspaces(Workspaces::Convert(args)) => {
            git_home::to_git(&backend, in_workspace("convert")?, &args)?
        }
        Command::Workspaces(Workspaces::LsRemote(_)) => {
            unreachable!("handled before the server is consulted")
        }
        Command::Workspaces(Workspaces::Bundle(_)) => {
            unreachable!("handled before the server is consulted")
        }
        Command::Accounts(Accounts::Login {
            username,
            password_stdin,
        }) => {
            let api = backend.pyn_server("login")?;
            let password = credentials::read_password("Password: ", password_stdin)?;
            let made = two_factor::login(api, &username, &password)?;
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
        Command::Accounts(Accounts::Logout) => {
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
        Command::Accounts(Accounts::Register {
            username,
            invite,
            email,
            password_stdin,
        }) => {
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
        Command::Accounts(Accounts::Password { password_stdin }) => {
            let api = backend.pyn_server("password")?;
            let current = credentials::read_password("Current password: ", password_stdin)?;
            let new = credentials::read_new_password(password_stdin)?;
            let body = api::ChangePasswordRequest {
                current: Some(current),
                new,
            };
            api.send_sensitive(api.request(Method::PUT, "/v1/me/password").json(&body))?;
            println!("password changed");
            report_ended_sign_in(api)?;
        }
        Command::Accounts(Accounts::Account(cmd)) => {
            account::run(backend.pyn_server("account")?, cmd)?
        }
        Command::Identity(Identity::Key(KeyCommand::Add { file, title })) => {
            let api = backend.pyn_server("key")?;
            let file = file.map_or_else(default_public_key, Ok)?;
            let key = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let body = api::AddKeyRequest { title, key };
            let added: api::SshKeyInfo = api
                .send_sensitive(api.request(Method::POST, "/v1/keys").json(&body))?
                .json()?;
            println!(
                "added {} ({}) as {}",
                added.title, added.fingerprint, added.id
            );
        }
        Command::Identity(Identity::Key(KeyCommand::List { for_user })) => {
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
        Command::Identity(Identity::Key(KeyCommand::Remove { id, for_user })) => {
            let api = backend.pyn_server("key")?;
            let mut req = api.request(Method::DELETE, &format!("/v1/keys/{id}"));
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            api.send_sensitive(req)?;
            println!("removed {id}");
        }
        Command::Identity(Identity::Invite(InviteCommand::Create { role, hours })) => {
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
        Command::Identity(Identity::Invite(InviteCommand::List)) => {
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
        Command::Identity(Identity::Invite(InviteCommand::Revoke { id })) => {
            let api = backend.pyn_server("invite")?;
            api.send(api.request(Method::DELETE, &api.repo_route(&format!("/invites/{id}"))?))?;
            println!("revoked {id}");
        }
        Command::Identity(Identity::User(UserCommand::Add {
            username,
            role,
            password_stdin,
        })) => {
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
        Command::Identity(Identity::Whoami) => {
            let api = backend.pyn_server("whoami")?;
            if api.repo.is_some() {
                let me: api::Me = api.send(api.get(&api.repo_route("/me")?))?.json()?;
                if json_out::on() {
                    json_out::print(serde_json::json!({
                        "user": me.user,
                        "user_id": me.user_id,
                        "repo": api.repo,
                        "permissions": me.permissions,
                    }));
                } else {
                    println!("{}\t{}", me.user, me.permissions.join(","));
                }
            } else {
                let me: api::Account = api.send(api.get("/v1/me"))?.json()?;
                if json_out::on() {
                    json_out::print(serde_json::json!({
                        "user": me.user,
                        "user_id": me.id,
                        "display_name": me.profile.display_name,
                        "admin": me.admin,
                    }));
                    return Ok(());
                }
                match me.profile.display_name {
                    Some(name) => println!("{} ({})", me.user, sanitize::text(&name)),
                    None => println!("{}", me.user),
                }
            }
        }
        Command::Identity(Identity::Version { check: false }) => {
            println!("pyn {}", env!("CARGO_PKG_VERSION"))
        }
        Command::Identity(Identity::Version { check: true }) => {
            update::check(backend.pyn_server("version")?)?
        }
        Command::Accounts(Accounts::Repo(cmd)) => repo_command(backend.pyn_server("repo")?, cmd)?,
        Command::Accounts(Accounts::Org(cmd)) => org_command(backend.pyn_server("org")?, cmd)?,
        Command::Accounts(Accounts::Invitation(cmd)) => {
            invitation_command(backend.pyn_server("invitation")?, cmd)?
        }
        Command::Administration(Administration::Setup {
            username,
            setup_token,
            email,
            server_name,
            public_url,
            registration,
            password_stdin,
        }) => admin::setup(
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
        Command::Administration(Administration::Admin(cmd)) => {
            admin::run(backend.pyn_server("admin")?, cmd)?
        }
        Command::Administration(Administration::Watch { name, level }) => {
            watch::watch(backend.pyn_server("watch")?, &name, level)?
        }
        Command::Administration(Administration::Unwatch { name }) => {
            watch::unwatch(backend.pyn_server("unwatch")?, &name)?
        }
        Command::Administration(Administration::Watching) => {
            watch::watching(backend.pyn_server("watching")?)?
        }
        Command::Administration(Administration::Usage { owner }) => {
            limits::usage(backend.pyn_server("usage")?, owner)?
        }
        Command::Accounts(Accounts::Team(cmd)) => team_command(backend.pyn_server("team")?, cmd)?,
        Command::Locking(Locking::Locks { mine: true, .. }) => {
            let api = backend.pyn_server("locks")?;
            let locks: Vec<api::MyLock> = api.send(api.get("/v1/me/locks"))?.json()?;
            if json_out::on() {
                json_out::print_as("locks", &locks);
                return Ok(());
            }
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
        Command::Locking(Locking::Locks {
            watch: true,
            after,
            interval,
            ..
        }) => {
            let api = backend.pyn_server("locks")?;
            watch_locks(api, after, interval)?;
        }
        Command::Locking(Locking::Locks { mine: false, .. }) => {
            let api = backend.pyn_server("locks")?;
            let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
            if json_out::on() {
                json_out::print_as("locks", &locks);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = locks
                .into_iter()
                .map(|l| vec![l.owner, time::local(l.expires_at), l.path])
                .collect();
            table::show(&["OWNER", "EXPIRES", "PATH"], &rows, "no locks");
        }
        Command::Locking(Locking::Files) => files_command(&backend)?,
        Command::Locking(Locking::Ls { path }) => ls_command(&backend, path.as_deref())?,
        Command::Locking(Locking::Summary) => {
            let api = backend.pyn_server("summary")?;
            let s: api::RepoSummary = api.send(api.get(&api.repo_route("/summary")?))?.json()?;
            if json_out::on() {
                json_out::print_as("summary", &s);
                return Ok(());
            }
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
                            commit_format::shown_name(&a.actor, a.actor_current.as_deref()),
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
        Command::Accounts(Accounts::Clone {
            dir,
            revision,
            local_store,
            no_local_store,
            ..
        }) => {
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
        Command::Syncing(Syncing::Fetch(args)) => {
            require_bare(&args, "fetch");
            let w = in_workspace("fetch")?;
            if w.settings().local_store_on() {
                local_sync::fetch(&backend, w)?;
            } else {
                update_as_remote(&backend, w, ws.as_ref())?;
            }
        }
        Command::Syncing(Syncing::Pull(mut args)) => {
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
        Command::Syncing(Syncing::Push(mut args)) => {
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
        Command::Syncing(Syncing::Migrate) => migrate::run(&backend, in_workspace("migrate")?)?,
        Command::Syncing(Syncing::Reset(args)) => {
            local_reset::reset(&backend, in_workspace("reset")?, args)?;
        }
        Command::Syncing(Syncing::CherryPick(args)) => {
            local_pick::pick(&backend, in_workspace("cherry-pick")?, args)?;
        }
        Command::Syncing(Syncing::Revert(args)) => {
            local_pick::revert(&backend, in_workspace("revert")?, args)?;
        }
        Command::Branching(Branching::Tag(args)) => local_tags::run(&backend, ws.as_ref(), args)?,
        Command::Branching(Branching::Stash(_)) => {
            eprintln!("{}", unsupported::message("stash"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Worktree(_)) => {
            eprintln!("{}", unsupported::message("worktree"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Submodule(_)) => {
            eprintln!("{}", unsupported::message("submodule"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Clean(_)) => {
            eprintln!("{}", unsupported::message("clean"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Grep(_)) => {
            eprintln!("{}", unsupported::message("grep"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Blame(_)) => {
            eprintln!("{}", unsupported::message("blame"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Annotate(_)) => {
            eprintln!("{}", unsupported::message("annotate"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Reflog(_)) => {
            eprintln!("{}", unsupported::message("reflog"));
            std::process::exit(1);
        }
        Command::Branching(Branching::Lfs(_)) => {
            eprintln!("{}", unsupported::message("lfs"));
            std::process::exit(1);
        }
        Command::Inspection(Inspection::RevParse(_))
        | Command::Inspection(Inspection::CheckAttr(_))
        | Command::Inspection(Inspection::LsFiles(_))
        | Command::Inspection(Inspection::LsTree(_))
        | Command::Inspection(Inspection::CatFile(_))
        | Command::Inspection(Inspection::VerifyCommit(_))
        | Command::Inspection(Inspection::VerifyTag(_))
        | Command::Inspection(Inspection::ShowRef(_))
        | Command::Inspection(Inspection::SparseCheckout(_))
        | Command::Plumbing(Plumbing::CountObjects(_))
        | Command::Plumbing(Plumbing::Fsck(_))
        | Command::Plumbing(Plumbing::Prune(_))
        | Command::Plumbing(Plumbing::Gc(_))
        | Command::Plumbing(Plumbing::PackRefs(_))
        | Command::Plumbing(Plumbing::PrunePacked(_))
        | Command::Plumbing(Plumbing::PackObjects(_))
        | Command::Plumbing(Plumbing::UnpackObjects(_))
        | Command::Plumbing(Plumbing::Maintenance(_))
        | Command::Plumbing(Plumbing::Repack(_))
        | Command::Plumbing(Plumbing::Shortlog(_))
        | Command::Plumbing(Plumbing::CheckMailmap(_))
        | Command::Plumbing(Plumbing::Describe(_))
        | Command::Plumbing(Plumbing::NameRev(_))
        | Command::Branching(Branching::Bisect(_))
        | Command::Plumbing(Plumbing::FormatPatch(_))
        | Command::Plumbing(Plumbing::Apply(_))
        | Command::Plumbing(Plumbing::Am(_)) => {
            eprintln!("fatal: not a git repository (or any of the parent directories): .git");
            std::process::exit(128);
        }
        Command::Syncing(Syncing::Rebase(args)) => {
            if ws.as_ref().is_some_and(|w| w.settings().local_store_on()) {
                local_rebase::rebase(&backend, in_workspace("rebase")?, &args)?;
            } else {
                eprintln!("{}", unsupported::message("rebase"));
                std::process::exit(1);
            }
        }
        Command::Syncing(Syncing::Remote(args)) => {
            git_home::remote_on_server(&args, &server_url, repo_name.as_deref())?
        }
        Command::Branching(Branching::Merge(args)) => {
            merging::merge_command(&backend, ws.as_ref(), args)?
        }
        Command::Branching(Branching::Pr(cmd)) => pr::pr_command(&backend, cmd)?,
        Command::Branching(Branching::Branch(args)) => {
            branch::branch_command(&backend, ws.as_ref(), args)?
        }
        Command::Branching(Branching::Switch(args)) => {
            branch::switch_command(&backend, ws.as_ref(), args)?
        }
        Command::Branching(Branching::Checkout(args)) => {
            branch::checkout_command(&backend, ws.as_ref(), &cwd, args, &rp)?
        }
        Command::Workspaces(Workspaces::Status(args)) => {
            if args.ignore_submodules.is_some() {
                usage_error("`pyn status --ignore-submodules` needs a git repository");
            }
            if args.renames_given() {
                usage_error("`pyn status` rename flags need a git repository");
            }
            let w = in_workspace("status")?;
            let format = args.format();
            if json_out::on() && (format != short_status::Format::Table || args.branch) {
                usage_error("`pyn status --json` does not combine with --short, --porcelain or -b");
            }
            let only =
                pathspec::parse(w, &cwd, &args.paths, Default::default(), false, usage_error)?;
            let commit = args
                .revision
                .map(|r| snapshot::resolve(&backend, &r))
                .transpose()?;
            if format == short_status::Format::Table && !json_out::on() {
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
        Command::Workspaces(Workspaces::Update { paths, revision }) => {
            let only = paths.iter().map(|p| rp(p)).collect::<Result<Vec<_>>>()?;
            if let Some(w) = &ws {
                merging::refuse_during_merge(w, "update")?;
            }
            let commit = revision
                .map(|r| snapshot::resolve(&backend, &r))
                .transpose()?;
            sync::update(&backend, in_workspace("update")?, &only, commit.as_deref())?;
        }
        Command::Inspection(Inspection::Diff {
            args,
            paths,
            format,
            exit_code,
            quiet,
            cached,
            no_patch,
            ignore_submodules,
            ..
        }) => {
            if ignore_submodules.is_some() {
                usage_error("`pyn diff --ignore-submodules` needs a git repository");
            }
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
            diff::exit_for_diff(differs && (exit_code || quiet));
        }
        Command::Plumbing(Plumbing::Show {
            args,
            paths,
            no_patch,
            fmt,
            decorate,
            show_signature,
            format,
            first_parent,
            merges,
        }) => {
            if show_signature {
                usage_error("`pyn show --show-signature` needs a git repository");
            }
            if args.len() > 1 || !paths.is_empty() {
                usage_error("`pyn show` takes one commit and no paths on a pyn server");
            }
            if let Some(flag) = merges
                .given()
                .or(first_parent.then_some("--first-parent"))
                .or(format.extra.git_only())
                .or(fmt.mailmap_given())
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
        Command::Editing(Editing::Policy(PolicyCommand::Lint { .. })) => {
            bail!("`pyn policy lint` checks a git repository's `.gitattributes`; this is not one")
        }
        Command::Editing(Editing::Policy(PolicyCommand::Preview { file })) => {
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
        Command::Identity(Identity::Config(args)) => match args.cmd {
            Some(cmd) => config_command(cmd, ws.as_ref(), &mine, &yours, &user_config)?,
            None => {
                usage_error("`pyn config` needs a subcommand (list, get, set, init) or a git flag")
            }
        },
        Command::Locking(Locking::Lock { path, base }) => {
            let api = backend.pyn_server("lock")?;
            let path = rp(&path)?;
            let known = ws
                .as_ref()
                .and_then(|w| w.load_state().ok())
                .and_then(|s| s.get(&path).map(|k| k.revision));
            let (base, committed) = match (base, &ws) {
                (Some(b), _) => (Some(b), false),
                (None, Some(_)) => local_base(&backend, ws.as_ref(), &path, known)?,
                (None, None) => (server_head(&backend, &path)?, false),
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
                .map_err(|e| {
                    if committed {
                        stale_local_hint(e)
                    } else {
                        behind_hint(e, ws.is_some())
                    }
                })
                .map_err(|e| held_by(api, &path, e))?
                .json()?;
            if let Some(w) = &ws {
                sync::after_lock(w, &path)?;
            }
            if json_out::on() {
                json_out::print_as("lock", &l);
            } else {
                println!("locked {} until {}", l.path, time::local(l.expires_at));
            }
        }
        Command::Locking(Locking::Edit { paths }) => {
            let w = in_workspace("edit")?;
            let paths = paths
                .iter()
                .map(|p| w.repo_prefix(&cwd, p))
                .collect::<Result<Vec<_>>>()?;
            sync::edit(w, &paths)?;
        }
        Command::Editing(Editing::Add { patch: true, .. }) => {
            usage_error("`add --patch` needs a git repository; a pyn server stages whole files")
        }
        Command::Editing(Editing::Add {
            interactive: true, ..
        }) => usage_error(
            "`add --interactive` needs a git repository; a pyn server stages whole files",
        ),
        Command::Editing(Editing::Add {
            intent_to_add: true,
            ..
        }) => usage_error("`add --intent-to-add` needs a git repository"),
        Command::Editing(Editing::Add {
            renormalize: true, ..
        }) => usage_error("`add --renormalize` needs a git repository"),
        Command::Editing(Editing::Add {
            paths,
            update,
            dry_run,
            verbose,
            force,
            ..
        }) => {
            let args = staging::AddArgs {
                paths,
                update,
                dry_run,
                verbose,
                force,
                new_only: false,
            };
            if staging::add(&backend, in_workspace("add")?, &cwd, &args)? {
                std::process::exit(1);
            }
        }
        Command::Editing(Editing::Rm(args)) => {
            rm_mv::rm(&backend, in_workspace("rm")?, &cwd, &args)?
        }
        Command::Editing(Editing::Mv(args)) => {
            rm_mv::mv(&backend, in_workspace("mv")?, &cwd, &args)?
        }
        Command::Editing(Editing::Commit {
            paths,
            file,
            base,
            message,
            all,
            untracked,
            git: flags,
        }) if paths.is_empty() => {
            if file.is_some() || base.is_some() {
                bail!("--file and --base need a path");
            }
            let w = in_workspace("commit")?;
            if merging::in_progress(w) && !merging::is_paused(w) {
                if let Some(flag) = flags.author_flag() {
                    bail!("`pyn commit {flag}` does not apply while finishing a merge");
                }
                if all || untracked {
                    bail!(
                        "cannot do a partial commit during a merge; stage what you resolved with `pyn add`"
                    );
                }
                let message = (!message.is_empty()).then(|| message.join("\n\n"));
                merging::finish(&backend, backend.pyn_server("commit")?, w, message)?;
            } else {
                let message = (!message.is_empty()).then(|| message.join("\n\n"));
                staging::commit(&backend, w, message.as_deref(), (all, untracked), &flags)?;
            }
        }
        Command::Editing(Editing::Commit { all, untracked, .. }) if all || untracked => {
            bail!("`commit -a` and `-u` stage what changed; name no paths with them")
        }
        Command::Editing(Editing::Commit {
            paths,
            file,
            base,
            message,
            ..
        }) if paths.len() > 1 => {
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
        Command::Editing(Editing::Commit {
            paths,
            file,
            base,
            message,
            ..
        }) => {
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
        Command::Editing(Editing::Get { path, rev, output }) => {
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
                None => backend.read(&path, rev, &mut closed_pipe::stdout())?,
            }
        }
        Command::History(History::Log {
            path,
            filter,
            commits,
            limit,
            git: log_args,
        }) if commits
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
                        commit_format::shown_name(&c.author.name, c.author_current.as_deref()),
                        time::local(c.created_at),
                        c.revisions.len().to_string(),
                        commit_format::subject_and_body(&c.message).0,
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
        Command::History(History::Log {
            path,
            filter,
            limit,
            git: log_args,
            ..
        }) => {
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
                let (subject, body) = commit_format::subject_and_body(&r.message);
                let restored = match (r.deleted, r.restored_from) {
                    (true, _) => " (deleted)".to_string(),
                    (false, Some(n)) => format!(" (restored from r{n})"),
                    (false, None) => String::new(),
                };
                let mut row = vec![
                    format!("r{}", r.id),
                    commit_format::shown_name(&r.author, r.author_current.as_deref()),
                    time::local(r.created_at),
                    format!("{subject}{restored}"),
                ];
                if repo_wide {
                    row.push(r.path);
                }
                (row, body.to_string())
            }));
            let header: &[&str] = if repo_wide {
                &["REV", "AUTHOR", "WHEN", "MESSAGE", "PATH"]
            } else {
                &["REV", "AUTHOR", "WHEN", "MESSAGE"]
            };
            table::show_notes(header, &rows, "no history");
        }
        Command::History(History::Verify { path, verbose }) => {
            let api = backend.pyn_server("verify")?;
            let path = path.map(|p| rp(&p)).transpose()?;
            if !verify::run(api, path.as_deref(), verbose)? {
                std::process::exit(1);
            }
        }
        Command::History(History::Restore { patch: true, .. }) => usage_error(
            "`restore --patch` needs a git repository; a pyn server restores whole files",
        ),
        Command::History(History::Restore { ours: true, .. })
        | Command::History(History::Restore { theirs: true, .. })
        | Command::History(History::Restore { merge: true, .. }) => usage_error(
            "`restore --ours`, `--theirs` and `--merge` need a git repository; a pyn server has no merge to take a side of",
        ),
        Command::History(History::Restore { worktree: true, .. }) => usage_error(
            "`restore --worktree` needs a git repository; on a pyn server a restore makes an older revision the new head",
        ),
        Command::History(History::Restore {
            args,
            source,
            staged: true,
            ..
        }) => {
            if source.is_some() {
                usage_error(
                    "`restore --staged --source` needs a git repository; a pyn server has no commit to restore the index from",
                );
            }
            staging::unstage(in_workspace("restore --staged")?, &cwd, &args)?;
        }
        Command::History(History::Restore {
            args,
            source,
            message,
            yes,
            ..
        }) => {
            let (path, revision) = restore_target(&args, source.as_deref());
            restore(&backend, rp(&path)?, revision, message, yes)?
        }
        Command::Locking(Locking::Unlock {
            path, force: false, ..
        }) => {
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
            if json_out::on() {
                json_out::print(serde_json::json!({ "unlocked": path }));
            } else {
                println!("unlocked {path}");
            }
        }
        Command::Locking(Locking::Unlock {
            path,
            reason: Some(reason),
            ..
        }) => {
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
            if json_out::on() {
                json_out::print(serde_json::json!({ "removed": lock }));
            } else {
                println!("removed {}'s lock on {path}", lock.owner);
            }
        }
        Command::Locking(Locking::Unlock { .. }) => {
            unreachable!("clap requires a reason with --force")
        }
        Command::History(History::Audit {
            path,
            actor,
            action,
            before,
            limit,
        }) => {
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
                        commit_format::actor_with_admin(e),
                        e.action.clone(),
                        e.path.clone().unwrap_or_else(|| "-".into()),
                        commit_format::audit_detail(e),
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
        Command::Identity(Identity::Profile(cmd)) => {
            profile::run(backend.pyn_server("profile")?, cmd)?
        }
        Command::History(History::Token(cmd)) => token_command(backend.pyn_server("token")?, cmd)?,
        Command::Administration(Administration::Member(MemberCommand::List)) => {
            let api = backend.pyn_server("member")?;
            let members: Vec<api::Member> =
                api.send(api.get(&api.repo_route("/members")?))?.json()?;
            let rows: Vec<Vec<String>> = members
                .into_iter()
                .map(|m| vec![m.role, m.source, m.user])
                .collect();
            table::show(&["ROLE", "SOURCE", "USER"], &rows, "no members");
        }
        Command::Administration(Administration::Member(MemberCommand::Set { user, role })) => {
            let api = backend.pyn_server("member")?;
            let body = api::SetMemberRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &api.repo_route(&format!("/members/{user}"))?)
                    .json(&body),
            )?;
            println!("{user} is now {role}");
        }
        Command::Administration(Administration::Role(RoleCommand::List)) => {
            let api = backend.pyn_server("role")?;
            let grants: Vec<api::RoleGrant> =
                api.send(api.get(&api.repo_route("/roles")?))?.json()?;
            let rows: Vec<Vec<String>> = grants
                .into_iter()
                .map(|g| vec![g.role, g.permissions.join(",")])
                .collect();
            table::show(&["ROLE", "PERMISSIONS"], &rows, "no roles");
        }
        Command::Administration(Administration::Role(RoleCommand::Set { role, permissions })) => {
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

/// With `--json`, attaches the live lock on `path` to a `lock_held` refusal.
fn held_by(api: &client::Api, path: &str, e: anyhow::Error) -> anyhow::Error {
    if !json_out::on()
        || e.downcast_ref::<client::ApiError>()
            .is_none_or(|a| a.code != "lock_held")
    {
        return e;
    }
    let locks: Result<Vec<api::Lock>> = api
        .repo_route("/locks")
        .and_then(|r| api.send(api.get(&r)))
        .and_then(|r| r.json());
    match locks
        .ok()
        .and_then(|l| l.into_iter().find(|l| l.path == path))
    {
        Some(lock) => e.context(json_out::HeldBy(lock)),
        None => e,
    }
}

/// Prints lock changes until the output closes; a closed pipe ends the watch quietly.
fn watch_locks(api: &client::Api, after: Option<String>, interval: u64) -> Result<()> {
    use std::io::Write;
    let json = json_out::on();
    let mut out = std::io::stdout().lock();
    let mut emit = |c: &lock_watch::Change| {
        let line = if json {
            c.document().to_string()
        } else {
            c.line()
        };
        writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
    };
    lock_watch::run(
        api,
        after,
        std::time::Duration::from_secs(interval),
        &mut emit,
        &std::thread::sleep,
    )
}

fn files_command(backend: &dyn Backend) -> Result<()> {
    if json_out::on() {
        json_out::print_as("files", &backend.list_files()?);
        return Ok(());
    }
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
    if json_out::on() {
        json_out::print(serde_json::json!({ "path": listing.path, "entries": listing.entries }));
        return Ok(());
    }
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
                    format!(
                        "{} ({}, r{})",
                        commit_format::subject_and_body(&r.message).0,
                        commit_format::shown_name(&r.author, r.author_current.as_deref()),
                        r.id
                    )
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
        diff::note_check_failed(format, &out);
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
        Some(store) => local_view::commit_trees(store, c, 0)?,
        None => server_trees(backend, c, 0)?,
    };
    diff::sections(backend, &old, &new, (&only, &[]), parts)
}

/// The trees before and after a published commit (against its `parent`th parent) and the paths it changed.
fn server_trees(
    backend: &dyn Backend,
    c: &api::Commit,
    parent: usize,
) -> Result<(diff::Tree, diff::Tree, Vec<String>)> {
    let new = diff::tree_of_snapshot(&backend.snapshot(Some(&c.id))?.1);
    let old = match c.parents.get(parent) {
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
    let mut body = String::new();
    let mut differs = false;
    for n in 0..found.parents.len().max(1) {
        let (old, new, only) = server_trees(backend, &found, n)?;
        let only = if found.parents.len() > 1 {
            vec![]
        } else {
            only
        };
        let mut part = String::new();
        differs |= diff::compare(backend, &old, &new, &only, format, &mut part)?;
        diff::parent_heading(&found, n, &part, &mut body);
        body.push_str(&part);
    }
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
            Command::Syncing(Syncing::Fetch(a)) => format!("{a:?}"),
            Command::Syncing(Syncing::Pull(a)) => format!("{a:?}"),
            Command::Syncing(Syncing::Push(a)) => format!("{a:?}"),
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
        source.id,
        commit_format::shown_name(&source.author, source.author_current.as_deref()),
        commit_format::subject_and_body(&source.message).0
    );
    println!(
        "The current head r{} ({}, \"{}\") stays in history; the restore becomes r{}.",
        head.id,
        commit_format::shown_name(&head.author, head.author_current.as_deref()),
        commit_format::subject_and_body(&head.message).0,
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

fn set_star(api: &Api, name: &str, on: bool) -> Result<()> {
    let (owner, short) = address::split_repo(name)?;
    let method = if on { Method::PUT } else { Method::DELETE };
    let state: api::StarState = api
        .send(api.request(method, &format!("/v1/repos/{owner}/{short}/star")))?
        .json()?;
    let verb = if on { "starred" } else { "unstarred" };
    println!("{verb} {name} ({} stars)", state.count);
    Ok(())
}

/// `Rust 62.5%, C++ 37.5%`, or `-` when nothing was recognised.
fn language_line(languages: &[api::LanguageShare]) -> String {
    let total: u64 = languages.iter().map(|l| l.bytes).sum();
    if total == 0 {
        return "-".into();
    }
    languages
        .iter()
        .map(|l| format!("{} {:.1}%", l.name, l.bytes as f64 * 100.0 / total as f64))
        .collect::<Vec<_>>()
        .join(", ")
}

fn show_repo(r: &api::RepoInfo) {
    let visibility = match r.visibility {
        api::Visibility::Public => "public",
        api::Visibility::Private => "private",
    };
    println!("repository   {}/{}", r.owner, r.name);
    println!("description  {}", r.description.as_deref().unwrap_or("-"));
    println!("website      {}", r.website.as_deref().unwrap_or("-"));
    let topics = if r.topics.is_empty() {
        "-".to_string()
    } else {
        r.topics.join(", ")
    };
    println!("topics       {topics}");
    println!("icon         {}", r.icon_url.as_deref().unwrap_or("-"));
    println!("languages    {}", language_line(&r.languages));
    let updated = r.updated_at.map_or_else(|| "-".to_string(), time::local);
    println!("updated      {updated}");
    println!("visibility   {visibility}");
    if let Some(src) = &r.fork_of {
        println!("forked from  {}/{}", src.owner, src.name);
    }
    println!("forks        {}", r.fork_count);
    println!("created      {}", time::local(r.created_at));
    println!("stars        {}", r.star_count);
    println!("watchers     {}", r.watcher_count);
    println!("your role    {}", r.role.as_deref().unwrap_or("-"));
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
            default,
            description,
            website,
            topics,
        } => {
            let file = match &policy {
                Some(file) => Some(
                    std::fs::read(file).with_context(|| format!("reading {}", file.display()))?,
                ),
                None => None,
            };
            let starter = file.is_none() && !no_policy;
            let starter_default = if starter {
                Some(init_pyn::choose_default(
                    default,
                    interactive(),
                    &mut std::io::stdin().lock(),
                    &mut std::io::stderr(),
                )?)
            } else {
                None
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
                starter_policy: Some(starter),
                starter_default: starter_default.map(|mode| match mode {
                    init_pyn::DefaultMode::Shared => api::Mode::Shared,
                    init_pyn::DefaultMode::Exclusive => api::Mode::Exclusive,
                }),
                description,
                website,
                topics: (!topics.is_empty()).then_some(topics),
                allow_private_forks: None,
            };
            let org = body.owner.clone().unwrap_or_default();
            let made: api::RepoInfo = api
                .send(api.request(Method::POST, "/v1/repos").json(&body))
                .map_err(|e| explain_create_refusal(e, &org))?
                .json()?;
            println!("created {}/{}", made.owner, made.name);
            if starter {
                println!("added {POLICY_PATH}");
            }
            if let Some(bytes) = file {
                let repo = format!("/v1/repos/{}/{}", made.owner, made.name);
                submit_policy(api, &repo, bytes)
                    .context("the repository was created, but its policy was not accepted")?;
                println!("added {POLICY_PATH}");
            }
        }
        RepoCommand::Show { name } => {
            let (owner, short) = address::split_repo(&name)?;
            let r: api::RepoInfo = api
                .send(api.get(&format!("/v1/repos/{owner}/{short}")))?
                .json()?;
            show_repo(&r);
        }
        RepoCommand::Edit {
            name,
            description,
            website,
            topics,
            no_topics,
            allow_private_forks,
        } => {
            let topics = (no_topics || !topics.is_empty()).then_some(topics);
            if description.is_none()
                && website.is_none()
                && topics.is_none()
                && allow_private_forks.is_none()
            {
                bail!(
                    "nothing to change: give --description, --website, --topic, --no-topics or --allow-private-forks"
                );
            }
            let (owner, short) = address::split_repo(&name)?;
            let body = api::UpdateRepoRequest {
                description: description.map(Some),
                website: website.map(Some),
                topics,
                allow_private_forks,
                ..Default::default()
            };
            let r: api::RepoInfo = api
                .send(
                    api.request(Method::PATCH, &format!("/v1/repos/{owner}/{short}"))
                        .json(&body),
                )?
                .json()?;
            show_repo(&r);
        }
        RepoCommand::Icon(RepoIconCommand::Set { name, file }) => {
            let (owner, short) = address::split_repo(&name)?;
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let req = api
                .request(Method::PUT, &format!("/v1/repos/{owner}/{short}/icon"))
                .header("content-type", "application/octet-stream")
                .body(bytes);
            api.send(req)?;
            println!("icon updated for {owner}/{short}");
        }
        RepoCommand::Icon(RepoIconCommand::Clear { name }) => {
            let (owner, short) = address::split_repo(&name)?;
            api.send(api.request(Method::DELETE, &format!("/v1/repos/{owner}/{short}/icon")))?;
            println!("icon removed from {owner}/{short}");
        }
        RepoCommand::Fork {
            name,
            new_name,
            owner,
            all_branches,
        } => {
            let (src_owner, src_name) = address::split_repo(&name)?;
            let org = owner.clone().unwrap_or_default();
            let body = api::ForkRepoRequest {
                owner,
                name: new_name,
                default_branch_only: Some(!all_branches),
            };
            let made: api::RepoInfo = api
                .send(
                    api.request(
                        Method::POST,
                        &format!("/v1/repos/{src_owner}/{src_name}/forks"),
                    )
                    .json(&body),
                )
                .map_err(|e| explain_create_refusal(e, &org))?
                .json()?;
            println!("forked {name} to {}/{}", made.owner, made.name);
        }
        RepoCommand::Forks { name } => {
            let (owner, short) = address::split_repo(&name)?;
            let forks: Vec<api::RepoInfo> = api
                .send(api.get(&format!("/v1/repos/{owner}/{short}/forks")))?
                .json()?;
            let rows: Vec<Vec<String>> = forks
                .into_iter()
                .map(|r| {
                    let visibility = match r.visibility {
                        api::Visibility::Public => "public",
                        api::Visibility::Private => "private",
                    };
                    vec![
                        visibility.to_string(),
                        time::local(r.created_at),
                        format!("{}/{}", r.owner, r.name),
                    ]
                })
                .collect();
            table::show(&["VISIBILITY", "CREATED", "REPOSITORY"], &rows, "no forks");
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
        RepoCommand::Star { name } => set_star(api, &name, true)?,
        RepoCommand::Unstar { name } => set_star(api, &name, false)?,
        RepoCommand::Starred { user } => {
            let user = match user {
                Some(user) => user,
                None => api.send(api.get("/v1/me"))?.json::<api::Account>()?.user,
            };
            let starred: Vec<api::StarredRepo> = api
                .send(api.get(&format!("/v1/users/{user}/starred")))?
                .json()?;
            let rows: Vec<Vec<String>> = starred
                .into_iter()
                .map(|s| {
                    vec![
                        time::local(s.starred_at),
                        s.repo.star_count.to_string(),
                        format!("{}/{}", s.repo.owner, s.repo.name),
                    ]
                })
                .collect();
            table::show(
                &["STARRED", "STARS", "REPOSITORY"],
                &rows,
                "no starred repositories",
            );
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
        RepoCommand::Usage { name, breakdown } => {
            let (owner, short) = address::split_repo(&name)?;
            limits::repo_usage(api, owner, short, breakdown)?;
        }
        RepoCommand::StorageCap { name, size } => {
            let (owner, short) = address::split_repo(&name)?;
            limits::set_repo_storage_cap(api, owner, short, size)?;
        }
        RepoCommand::Delete { name, yes } => {
            let (owner, short) = address::split_repo(&name)?;
            let route = format!("/v1/repos/{owner}/{short}");
            if !yes {
                may_delete_repo(api, &route, &name)?;
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
            api.send(api.request(Method::DELETE, &route)).map_err(|e| {
                if e.to_string().starts_with("forbidden") {
                    e.context(delete_needs(&name))
                } else {
                    e
                }
            })?;
            println!("deleted {name}");
        }
    }
    Ok(())
}

/// The server's delete rule, checked before asking to confirm: a user's repository only by its owner, and the
/// caller's permissions there (the role as a token narrows it) must include manage_roles.
fn may_delete_repo(api: &Api, route: &str, name: &str) -> Result<()> {
    let info: api::RepoInfo = api.send(api.get(route))?.json()?;
    let me: api::Me = api.send(api.get(&format!("{route}/me")))?.json()?;
    if me.user != info.owner && owned_by_user(api, &info.owner) {
        bail!("only {} can delete {name}", info.owner);
    }
    if !me.permissions.iter().any(|p| p == "manage_roles") {
        bail!("{}", delete_needs(name));
    }
    Ok(())
}

/// A name the organization lookup does not know is a user's; any other failure leaves the decision to the server.
fn owned_by_user(api: &Api, owner: &str) -> bool {
    match api.send(api.get(&format!("/v1/orgs/{owner}"))) {
        Ok(_) => false,
        Err(e) => e
            .downcast_ref::<client::ApiError>()
            .is_some_and(|e| e.code == "org_not_found"),
    }
}

fn delete_needs(name: &str) -> String {
    format!("deleting {name} needs the admin role on it, and a token needs administration:write")
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
                        commit_format::shown_name(&e.actor, e.actor_current.as_deref()),
                        e.action.clone(),
                        commit_format::audit_detail(e),
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
        OrgCommand::Invitation(cmd) => org_invitation_command(api, cmd)?,
        OrgCommand::Policy(cmd) => org_policy_command(api, cmd)?,
        OrgCommand::Settings(cmd) => org_settings::run(api, cmd)?,
    }
    Ok(())
}

fn team_command(api: &Api, cmd: TeamCommand) -> Result<()> {
    match cmd {
        TeamCommand::Create {
            org,
            name,
            slug,
            description,
        } => {
            let body = api::CreateTeamRequest {
                name,
                slug,
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
            let reply = api.send(
                api.request(Method::POST, &format!("/v1/orgs/{org}/members"))
                    .json(&body),
            )?;
            if reply.status == 202 {
                let invited: api::OrgInvitation = reply.json()?;
                println!(
                    "invited {user} to {org} as {}; expires {}",
                    invited.role,
                    time::local(invited.expires_at)
                );
            } else {
                let added: api::OrgMember = reply.json()?;
                println!("added {user} to {org} as {}", added.role);
            }
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

fn invitation_rows(
    invitations: Vec<api::OrgInvitation>,
    last: fn(&api::OrgInvitation) -> String,
) -> Vec<Vec<String>> {
    invitations
        .into_iter()
        .map(|i| {
            vec![
                i.role.clone(),
                time::local(i.expires_at),
                i.invited_by.clone(),
                last(&i),
            ]
        })
        .collect()
}

fn org_invitation_command(api: &Api, cmd: OrgInvitationCommand) -> Result<()> {
    match cmd {
        OrgInvitationCommand::List { org } => {
            let found: Vec<api::OrgInvitation> = api
                .send(api.get(&format!("/v1/orgs/{org}/invitations")))?
                .json()?;
            let rows = invitation_rows(found, |i| i.user.clone());
            table::show(
                &["ROLE", "EXPIRES", "INVITED BY", "USER"],
                &rows,
                "no pending invitations",
            );
        }
        OrgInvitationCommand::Cancel { org, user } => {
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/orgs/{org}/invitations/{user}"),
            ))?;
            println!("cancelled the invitation of {user} to {org}");
        }
    }
    Ok(())
}

fn invitation_command(api: &Api, cmd: InvitationCommand) -> Result<()> {
    match cmd {
        InvitationCommand::List => {
            let found: Vec<api::OrgInvitation> = api.send(api.get("/v1/me/invitations"))?.json()?;
            let rows = invitation_rows(found, |i| i.org.clone());
            table::show(
                &["ROLE", "EXPIRES", "INVITED BY", "ORG"],
                &rows,
                "no pending invitations",
            );
        }
        InvitationCommand::Accept { org } => {
            let joined: api::OrgInfo = api
                .send(api.request(Method::POST, &format!("/v1/me/invitations/{org}/accept")))?
                .json()?;
            println!("joined {org} as {}", joined.role.unwrap_or_default());
        }
        InvitationCommand::Decline { org } => {
            api.send(api.request(Method::POST, &format!("/v1/me/invitations/{org}/decline")))?;
            println!("declined the invitation to {org}");
        }
    }
    Ok(())
}

fn token_command(api: &Api, cmd: TokenCommand) -> Result<()> {
    match cmd {
        TokenCommand::Create {
            name,
            permissions,
            owner,
            access,
            repos,
            expires_days,
            expires,
            never,
        } => {
            let permissions = token_args::permissions(&permissions)?;
            let (repository_access, owner) = token_args::access(
                access.as_deref(),
                repos,
                owner,
                &permissions,
                api.repo.as_deref().ok_or(if api.on_git {
                    "this is a git repository, so name the repositories the token is for with --repos owner/name"
                } else {
                    "name the repositories the token is for with --repos owner/name"
                }),
            )?;
            token_args::check_owner_kind(owner.as_deref(), &permissions)?;
            let (expires_at, never) =
                token_args::expiry(expires_days, expires.as_deref(), never, chrono::Utc::now())?;
            let body = api::CreateTokenRequest {
                name,
                description: None,
                owner,
                expires_at,
                never,
                repository_access,
                permissions,
            };
            let made: api::CreatedToken = api
                .send_sensitive(api.request(Method::POST, "/v1/tokens").json(&body))?
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
                    let access = match (t.sign_in, t.repository_access.mode.as_str()) {
                        (true, _) => "full-scope".to_string(),
                        (_, "selected") => {
                            format!("selected:{}", t.repository_access.repos.join(","))
                        }
                        (_, mode) => mode.to_string(),
                    };
                    vec![
                        t.id.to_string(),
                        state.to_string(),
                        t.owner,
                        t.expires_at.map_or("never".to_string(), time::local),
                        access,
                        t.name,
                        t.permissions
                            .iter()
                            .map(|p| format!("{}:{}", p.resource, p.level))
                            .collect::<Vec<_>>()
                            .join(","),
                    ]
                })
                .collect();
            table::show(
                &[
                    "ID",
                    "STATE",
                    "OWNER",
                    "EXPIRES",
                    "ACCESS",
                    "NAME",
                    "PERMISSIONS",
                ],
                &rows,
                "no tokens",
            );
        }
        TokenCommand::Revoke { id } => {
            api.send_sensitive(api.request(Method::DELETE, &format!("/v1/tokens/{id}")))?;
            println!("revoked {id}");
        }
    }
    Ok(())
}

/// The base a lock asks for when a local commit changed the path, and whether one did: the newest server revision the
/// local work has seen, the working file's or the one at the server's tip once the branch or a merge in progress took it.
fn local_base(
    backend: &PynServer,
    ws: Option<&Workspace>,
    path: &str,
    known: Option<u64>,
) -> Result<(Option<u64>, bool)> {
    let Some((ws, store)) = ws.and_then(|w| Some((w, local_store::LocalStore::open(w)?))) else {
        return Ok((known, false));
    };
    let branch = branch::name(Some(ws));
    if !store.unpublished(&branch)?.paths.contains_key(path) {
        return Ok((known, false));
    }
    // Revision 0 is a file the server has never had.
    let known = known.filter(|&r| r != 0);
    let (Some(tip), Some(server)) = (store.tip(&branch)?, store.published(&branch)?) else {
        return Ok((known, true));
    };
    let mut heads = vec![tip];
    if let Some(merge) = merging::load(ws)?
        && let Ok(id) = local_store::parse_id(&merge.source_commit)
        && store.has(&id)?
    {
        heads.push(id);
    }
    if !store.reach(heads)?.contains(&server) {
        return Ok((known, true));
    }
    let taken = backend
        .snapshot(Some(&server.to_hex()))?
        .1
        .into_iter()
        .find(|e| e.path == path)
        .map(|e| e.revision);
    Ok((known.max(taken), true))
}

/// The head revision of `path` on the server's branch: outside a workspace there is no local copy to be stale.
/// No base when the listing is refused (a missing folder, a locks-only token): the lock request gets the real answer.
fn server_head(backend: &PynServer, path: &str) -> Result<Option<u64>> {
    let folder = path.rsplit_once('/').map(|(dir, _)| dir);
    let listing = match backend.tree(folder) {
        Ok(listing) => listing,
        Err(e) if e.downcast_ref::<client::ApiError>().is_some() => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(listing
        .entries
        .into_iter()
        .find(|e| e.path == path && e.kind == api::TreeEntryKind::File)
        .and_then(|e| e.last_change)
        .map(|r| r.id))
}

/// A stale-base refusal for a path a local commit changed: the newer revision has to be pulled in and resolved first.
fn stale_local_hint(err: anyhow::Error) -> anyhow::Error {
    if err.to_string().starts_with("stale_base") {
        err.context(
            "your local commit changed an older revision of the file; run `pyn pull` and resolve it first",
        )
    } else {
        err
    }
}

/// A stale-base refusal means the file moved on: in a workspace say how to catch up, outside one how to name a base.
/// When the server has no revision of the path, neither helps.
fn behind_hint(err: anyhow::Error, in_workspace: bool) -> anyhow::Error {
    let text = err.to_string();
    if !text.starts_with("stale_base") || text.ends_with("head is none") {
        err
    } else if in_workspace {
        err.context("the server has a newer revision; run `pyn update` first")
    } else {
        err.context("pass `--base <revision>` with the revision your copy is at, or lock it from a workspace")
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
            warn_stored_server("server", &settings);
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
        ConfigCommand::Set {
            key, value, global, ..
        } => {
            if global {
                let mut settings = yours.clone();
                settings.set(&key, value)?;
                settings.save(user_config)?;
                warn_stored_server(&key, &settings);
            } else {
                let w =
                    ws.context("not in a workspace; run `pyn clone <dir>` first, or use --global")?;
                let mut settings = mine.clone();
                settings.set(&key, value)?;
                settings.save(&w.config_path())?;
                warn_stored_server(&key, &settings);
            }
        }
    }
    Ok(())
}

fn warn_stored_server(key: &str, settings: &Settings) {
    if let ("server", Some(server)) = (key, &settings.server) {
        credentials::warn_cleartext(server, true);
    }
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

#[cfg(test)]
mod cli_tests {
    use clap::CommandFactory;

    #[test]
    fn every_command_passes_clap_definition_checks() {
        // Catches two flattened groups that define one argument, such as `--abbrev` in `show`.
        super::Cli::command().debug_assert();
    }
}
