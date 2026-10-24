//! The remote end-to-end matrix (#312): every transport against every hash format, with real git as the oracle server.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

#[derive(Clone, Copy, Debug)]
enum Way {
    Remote(Via),
    LocalPath,
    FileUrl,
}

const REMOTE_WAYS: [Via; 3] = [Via::Ssh, Via::Http, Via::HttpAuth];

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    _server: Option<Server>,
    way: Way,
    format: &'static str,
}

/// Fails loudly, with a reason, when the oracle cannot serve what the matrix needs.
fn require_oracle(format: &str) {
    let exec = run(Path::new("/"), &["--exec-path"]);
    assert!(
        Path::new(exec.trim()).join("git-http-backend").exists(),
        "the matrix needs `git http-backend` next to git ({})",
        exec.trim()
    );
    let tmp = tempdir().unwrap();
    let probe = git(tmp.path())
        .args(["init", "-q", "--object-format", format, "probe"])
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "the matrix needs a git that can create {format} repositories: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
}

impl Rig {
    fn new(format: &'static str, way: Way) -> Self {
        require_oracle(format);
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = sample_bare(&root, format);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
        let (server, url) = match way {
            Way::Remote(via) => {
                let server = via.serve(&root);
                let url = via.url(&server, "served.git", &bare);
                (server, url)
            }
            Way::LocalPath => (None, bare.display().to_string()),
            Way::FileUrl => (None, format!("file://{}", bare.display())),
        };
        Rig {
            shim: ssh_shim(&root, false),
            work: root.join("seed"),
            _tmp: tmp,
            root,
            bare,
            home,
            url,
            _server: server,
            way,
            format,
        }
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", &self.shim)
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        Out {
            code: out.status.code().unwrap(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ctx(&self) -> String {
        format!("{} {:?}", self.format, self.way)
    }

    fn assert_ok(&self, out: &Out, what: &str) {
        assert_eq!(out.code, 0, "{} {what}: {}", self.ctx(), out.stderr);
    }

    /// A pyn clone of the served repository, in `root/name`.
    fn clone_to(&self, name: &str) -> PathBuf {
        let out = self.pyn(&self.root, &["clone", &self.url, name]);
        self.assert_ok(&out, "clone");
        self.root.join(name)
    }

    /// The remote's branches and tags as git reports them, tracking-ref names mapped back.
    fn served(&self) -> BTreeMap<String, String> {
        oracle_refs(&self.bare, &[])
            .into_iter()
            .filter(|(n, _)| !n.ends_with("^{}") && n != "HEAD")
            .collect()
    }

    fn advance(&self, file: &str) -> String {
        run(&self.work, &["switch", "-q", "main"]);
        let id = commit(&self.work, file, file.as_bytes(), file);
        run(
            &self.work,
            &["push", "-q", self.bare.to_str().unwrap(), "main"],
        );
        id
    }
}

fn local_refs(dir: &Path) -> BTreeMap<String, String> {
    run(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(n, id)| (n.to_owned(), id.to_owned()))
        .collect()
}

/// The branches and tags of `dir`, as the names the server uses for them.
fn tracked_as_served(dir: &Path) -> BTreeMap<String, String> {
    local_refs(dir)
        .into_iter()
        .filter_map(|(n, id)| {
            if let Some(b) = n.strip_prefix("refs/remotes/origin/") {
                (b != "HEAD").then(|| (format!("refs/heads/{b}"), id))
            } else {
                n.starts_with("refs/tags/").then_some((n, id))
            }
        })
        .collect()
}

fn ways() -> Vec<(&'static str, Way)> {
    let mut all = Vec::new();
    for format in FORMATS {
        for via in REMOTE_WAYS {
            all.push((format, Way::Remote(via)));
        }
        all.push((format, Way::LocalPath));
        all.push((format, Way::FileUrl));
    }
    all
}

#[test]
fn ls_remote_matches_git_on_every_transport_and_format() {
    for (format, way) in ways() {
        let t = Rig::new(format, way);
        let out = t.pyn(&t.root, &["ls-remote", &t.url]);
        t.assert_ok(&out, "ls-remote");
        let mut got: Vec<&str> = out.stdout.lines().collect();
        got.sort_unstable();
        let want = run(Path::new("/"), &["ls-remote", t.bare.to_str().unwrap()]);
        let mut want: Vec<&str> = want.lines().collect();
        want.sort_unstable();
        assert_eq!(got, want, "{}", t.ctx());
    }
}

#[test]
fn clone_brings_every_branch_and_tag_whole() {
    for (format, way) in ways() {
        let t = Rig::new(format, way);
        let dir = t.clone_to("dest");
        assert_eq!(tracked_as_served(&dir), t.served(), "{}", t.ctx());
        assert_eq!(
            run(&dir, &["rev-parse", "HEAD"]).trim(),
            t.served()["refs/heads/main"],
            "{}",
            t.ctx()
        );
        assert_eq!(
            run(&dir, &["rev-parse", "--show-object-format"]).trim(),
            format
        );
        fsck(&dir);
    }
}

#[test]
fn fetch_follows_the_server_across_updates() {
    for (format, way) in ways() {
        let t = Rig::new(format, way);
        let dir = t.root.join("side");
        init(&dir, format, false);
        run(&dir, &["remote", "add", "origin", &t.url]);
        t.assert_ok(&t.pyn(&dir, &["fetch", "origin"]), "first fetch");
        assert_eq!(tracked_as_served(&dir), t.served(), "{}", t.ctx());
        let tip = t.advance("more.txt");
        t.assert_ok(&t.pyn(&dir, &["fetch", "origin"]), "second fetch");
        assert_eq!(
            local_refs(&dir)["refs/remotes/origin/main"],
            tip,
            "{}",
            t.ctx()
        );
        assert_eq!(tracked_as_served(&dir), t.served(), "{}", t.ctx());
        fsck(&dir);
    }
}

#[test]
fn pull_fast_forwards_to_what_the_server_holds() {
    for (format, way) in ways() {
        let t = Rig::new(format, way);
        let dir = t.clone_to("side");
        let tip = t.advance("pulled.txt");
        t.assert_ok(&t.pyn(&dir, &["pull"]), "pull");
        assert_eq!(run(&dir, &["rev-parse", "HEAD"]).trim(), tip, "{}", t.ctx());
        assert_eq!(
            fs::read_to_string(dir.join("pulled.txt")).unwrap(),
            "pulled.txt"
        );
        fsck(&dir);
    }
}

#[test]
fn push_lands_commits_branches_and_tags_that_git_can_verify() {
    for (format, way) in ways() {
        let t = Rig::new(format, way);
        let dir = t.clone_to("side");
        let tip = commit(&dir, "pushed.txt", b"pushed\n", "pushed");
        run(&dir, &["tag", "-a", "v3", "-m", "third"]);
        run(&dir, &["branch", "topic"]);
        let out = t.pyn(&dir, &["push", "origin", "main", "topic", "v3"]);
        t.assert_ok(&out, "push");
        let served = t.served();
        assert_eq!(served["refs/heads/main"], tip, "{}", t.ctx());
        assert_eq!(served["refs/heads/topic"], tip, "{}", t.ctx());
        assert_eq!(
            served["refs/tags/v3"],
            run(&dir, &["rev-parse", "v3"]).trim(),
            "{}",
            t.ctx()
        );
        fsck(&t.bare);
        let out = t.pyn(&dir, &["push", "origin", "main"]);
        t.assert_ok(&out, "up-to-date push");
    }
}
