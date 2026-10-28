//! The repository's client hooks, discovered and run the way git runs them: `core.hooksPath` or `hooks/`, executable
//! files only, in the work tree root, stdout sent to stderr. Hooks run as the user's own processes.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use pyn_object::{ObjectId, Signature};

use super::GitHome;

impl GitHome {
    /// Whether hooks run at all: on unless `pyn.hooks` is false.
    pub(super) fn hooks_enabled(&self) -> bool {
        self.repo
            .config()
            .get_bool("pyn.hooks")
            .ok()
            .flatten()
            .unwrap_or(true)
    }

    pub(super) fn hooks_dir(&self) -> PathBuf {
        let git = self.repo.git();
        match self.repo.config().get("core.hookspath") {
            Some(p) => {
                let expanded = match p.strip_prefix("~/") {
                    Some(rest) => std::env::var_os("HOME")
                        .map_or_else(|| PathBuf::from(p), |h| PathBuf::from(h).join(rest)),
                    None => PathBuf::from(p),
                };
                match git.work_tree() {
                    Some(root) if expanded.is_relative() => root.join(expanded),
                    _ => git.common_dir().join(expanded),
                }
            }
            None => git.common_dir().join("hooks"),
        }
    }

    /// Warns that hooks named in `names` exist and were skipped, for a command that runs them only when hooks are on.
    pub(super) fn warn_if_hooks_off(&self, names: &[&str]) {
        if !self.hooks_enabled() {
            super::stage::warn_about_hooks_for(self, names);
        }
    }

    /// The executable hook called `name`; a file that is not executable earns git's hint.
    pub(super) fn find_hook(&self, name: &str) -> Option<PathBuf> {
        if !self.hooks_enabled() {
            return None;
        }
        let path = self.hooks_dir().join(name);
        let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
        if meta.permissions().mode() & 0o111 == 0 {
            if self
                .repo
                .config()
                .get_bool("advice.ignoredhook")
                .ok()
                .flatten()
                .unwrap_or(true)
            {
                eprintln!(
                    "hint: The '{name}' hook was ignored because it's not set as executable.\n\
                     hint: You can disable this warning with `git config advice.ignoredHook false`."
                );
            }
            return None;
        }
        Some(path)
    }

    /// A path as git hands it to a hook: relative to the work tree root when it lies under it.
    pub(super) fn hook_path(&self, path: &Path) -> String {
        let shown = path.strip_prefix(&self.ws.root).unwrap_or(path);
        shown.display().to_string()
    }

    /// The index a commit hook sees: `.git/index` as git names it.
    pub(super) fn hook_index(&self) -> String {
        self.hook_path(&self.repo.git().index_path())
    }

    /// Runs hook `name` with `args` and `stdin`; `None` when there is no such hook, else its exit status.
    pub(super) fn run_hook(
        &self,
        name: &str,
        args: &[String],
        stdin: Option<&[u8]>,
        env: &[(&str, String)],
    ) -> Result<Option<i32>> {
        let Some(path) = self.find_hook(name) else {
            return Ok(None);
        };
        let root = self.repo.git().work_tree();
        let prefix = match (root, std::env::current_dir()) {
            (Some(r), Ok(cwd)) => cwd
                .strip_prefix(r)
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .map_or_else(String::new, |p| format!("{}/", p.display())),
            _ => String::new(),
        };
        let mut cmd = Command::new(self.hook_path(&path));
        cmd.args(args.iter().map(OsString::from))
            .env("GIT_PREFIX", prefix)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(stdout_as_stderr());
        if let Some(r) = root.or(Some(self.repo.git().git_dir())) {
            cmd.current_dir(r);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: cannot exec '{}': {e}", self.hook_path(&path));
                return Ok(Some(1));
            }
        };
        let feeder = stdin.zip(child.stdin.take()).map(|(bytes, mut pipe)| {
            let bytes = bytes.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            })
        });
        let status = child.wait()?;
        if let Some(f) = feeder {
            let _ = f.join();
        }
        Ok(Some(status.code().unwrap_or(128)))
    }

    /// Runs `post-checkout`; the status git's command ends with: 1 when the hook failed, else 0.
    pub(super) fn post_checkout(
        &self,
        old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        branch: bool,
    ) -> Result<i32> {
        let hex = |id: Option<&ObjectId>| {
            id.map_or_else(
                || "0".repeat(self.algorithm().digest_len() * 2),
                ObjectId::to_hex,
            )
        };
        let args = [hex(old), hex(new), u8::from(branch).to_string()];
        Ok(i32::from(
            self.run_hook("post-checkout", &args, None, &[])?
                .is_some_and(|s| s != 0),
        ))
    }

    /// Whether a hook called `name` is there to run, without the hint for a non-executable one.
    pub(super) fn hook_present(&self, name: &str) -> bool {
        self.hooks_enabled()
            && std::fs::metadata(self.hooks_dir().join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    /// Runs a hook that may veto; true when it exited nonzero.
    pub(super) fn vetoed(
        &self,
        name: &str,
        args: &[String],
        env: &[(&str, String)],
    ) -> Result<bool> {
        Ok(self
            .run_hook(name, args, None, env)?
            .is_some_and(|s| s != 0))
    }
}

fn stdout_as_stderr() -> Stdio {
    use std::os::fd::AsFd;
    std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_or_else(|_| Stdio::inherit(), Stdio::from)
}

/// A hook that stopped the command it ran for.
#[derive(Debug)]
pub(super) struct HookRefused(pub(super) String);

impl std::fmt::Display for HookRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}' hook failed", self.0)
    }
}

impl std::error::Error for HookRefused {}

/// The hooks around a commit made by a replaying command and the environment they see.
#[derive(Clone)]
pub(super) struct CommitHooks {
    env: Vec<(&'static str, String)>,
    /// `prepare-commit-msg` already ran, over the message the editor opened.
    message_done: bool,
    /// The replacing `post-rewrite amend` also sees the author, as the `git commit` that makes it does.
    amend_env: Vec<(&'static str, String)>,
}

impl CommitHooks {
    /// The commit is made directly: no editor, so `GIT_EDITOR=:`.
    pub(super) fn direct(git: &GitHome) -> Self {
        Self {
            env: vec![
                ("GIT_INDEX_FILE", git.hook_index()),
                ("GIT_EDITOR", ":".to_string()),
            ],
            message_done: false,
            amend_env: Vec::new(),
        }
    }

    /// The message went through the editor; the caller ran `prepare-commit-msg` before it.
    pub(super) fn edited(git: &GitHome, author: &Signature) -> Self {
        let mut env = author_env(author);
        env.push(("GIT_INDEX_FILE", git.hook_index()));
        Self {
            env,
            message_done: true,
            amend_env: author_env(author),
        }
    }

    pub(super) fn message_done(&self) -> bool {
        self.message_done
    }

    pub(super) fn env(&self) -> &[(&'static str, String)] {
        &self.env
    }
}

impl GitHome {
    /// Runs `prepare-commit-msg` on `COMMIT_EDITMSG` holding `message` and returns what the file holds afterwards.
    pub(super) fn prepare_message(
        &self,
        message: &[u8],
        source: &[&str],
        env: &[(&str, String)],
    ) -> Result<Vec<u8>> {
        if !self.hook_present("prepare-commit-msg") {
            return Ok(message.to_vec());
        }
        let file = self.repo.git().git_dir().join("COMMIT_EDITMSG");
        std::fs::write(&file, message)?;
        let mut args = vec![self.hook_path(&file)];
        args.extend(source.iter().map(|s| (*s).to_string()));
        if self.vetoed("prepare-commit-msg", &args, env)? {
            return Err(HookRefused("prepare-commit-msg".into()).into());
        }
        Ok(std::fs::read(&file)?)
    }

    /// Runs `pre-commit`, `prepare-commit-msg` and `commit-msg` as `git commit` does and returns the message they leave.
    pub(super) fn verify_message(
        &self,
        message: &[u8],
        source: &str,
        env: &[(&str, String)],
    ) -> Result<Vec<u8>> {
        if self.vetoed("pre-commit", &[], env)? {
            return Err(HookRefused("pre-commit".into()).into());
        }
        let message = self.prepare_message(message, &[source], env)?;
        if self.hook_present("commit-msg") {
            let file = self.repo.git().git_dir().join("COMMIT_EDITMSG");
            std::fs::write(&file, &message)?;
            if self.vetoed("commit-msg", &[self.hook_path(&file)], env)? {
                return Err(HookRefused("commit-msg".into()).into());
            }
            return Ok(std::fs::read(&file)?);
        }
        Ok(message)
    }

    /// `post-commit`, then `post-rewrite amend` when the new commit replaced `replaced`; neither can stop anything.
    pub(super) fn after_commit(
        &self,
        hooks: &CommitHooks,
        replaced: Option<(&ObjectId, &ObjectId)>,
    ) -> Result<()> {
        self.run_hook("post-commit", &[], None, hooks.env())?;
        if let Some((old, new)) = replaced {
            self.post_rewrite("amend", &[(*old, *new)], &hooks.amend_env)?;
        }
        Ok(())
    }

    /// Runs `post-rewrite` with `kind` and one `<old> <new>` line per pair on standard input.
    pub(super) fn post_rewrite(
        &self,
        kind: &str,
        pairs: &[(ObjectId, ObjectId)],
        env: &[(&str, String)],
    ) -> Result<()> {
        let text: String = pairs.iter().map(|(o, n)| format!("{o} {n}\n")).collect();
        self.run_hook(
            "post-rewrite",
            &[kind.to_string()],
            Some(text.as_bytes()),
            env,
        )?;
        Ok(())
    }
}

/// `GIT_AUTHOR_*` as git exports them to the commit hooks.
pub(super) fn author_env(author: &Signature) -> Vec<(&'static str, String)> {
    let sign = if author.offset_minutes < 0 { '-' } else { '+' };
    let abs = author.offset_minutes.unsigned_abs();
    vec![
        (
            "GIT_AUTHOR_NAME",
            String::from_utf8_lossy(&author.name).into_owned(),
        ),
        (
            "GIT_AUTHOR_EMAIL",
            String::from_utf8_lossy(&author.email).into_owned(),
        ),
        (
            "GIT_AUTHOR_DATE",
            format!("@{} {sign}{:02}{:02}", author.time, abs / 60, abs % 60),
        ),
    ]
}
