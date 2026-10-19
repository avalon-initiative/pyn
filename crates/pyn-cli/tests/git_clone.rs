//! `pyn clone` from a git URL against real servers (ssh through a shim, smart HTTP through `git http-backend`), compared
//! with the real `git clone` of the same remote: refs, config, index, work tree, reflogs and the exit status. Real git is
//! the oracle in these tests only; semantics are compared (ids, names, values, modes), never message wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
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
    server: Option<Server>,
}

impl Rig {
    fn new(format: &str, via: Via) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = sample_bare(&root, format);
        Self::around(tmp, root, bare, via)
    }

    /// A remote that has no refs at all.
    fn empty(format: &str, via: Via) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = root.join("served.git");
        init(&bare, format, true);
        run(&bare, &["symbolic-ref", "HEAD", "refs/heads/trunk"]);
        Self::around(tmp, root, bare, via)
    }

    fn around(tmp: TempDir, root: PathBuf, bare: PathBuf, via: Via) -> Self {
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
        let server = via.serve(&root);
        let url = via.url(&server, "served.git", &bare);
        Rig {
            work: root.join("seed"),
            shim: ssh_shim(&root, false),
            _tmp: tmp,
            root,
            bare,
            home,
            url,
            server,
        }
    }

    /// Adds an executable, a symlink and a nested file on `main`.
    fn add_rich_files(&self) {
        let w = &self.work;
        fs::create_dir_all(w.join("bin")).unwrap();
        fs::write(w.join("bin/tool.sh"), "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(w.join("bin/tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("a.txt", w.join("link")).unwrap();
        fs::create_dir_all(w.join("d/e")).unwrap();
        fs::write(w.join("d/e/deep.txt"), "deep\n").unwrap();
        run(w, &["add", "."]);
        run(w, &["commit", "-q", "-m", "rich"]);
        run(w, &["push", "-q", self.bare.to_str().unwrap(), "main"]);
    }

    fn side(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn git_clone(&self, url: &str, flags: &[&str], dest: Option<&str>) -> Out {
        let o = git(&self.side("git-side"))
            .arg("clone")
            .args(argv(url, flags, dest))
            .env("HOME", &self.home)
            .env("GIT_SSH_COMMAND", &self.shim)
            .env("GIT_SSH_VARIANT", "ssh")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        out_of(o)
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_pyn"))
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
        out_of(o)
    }

    fn pyn_clone(&self, url: &str, flags: &[&str], dest: Option<&str>) -> Out {
        let mut full = vec!["clone".to_owned()];
        full.extend(argv(url, flags, dest));
        let full: Vec<&str> = full.iter().map(String::as_str).collect();
        self.pyn(&self.side("pyn-side"), &full)
    }

    fn git_dir(&self, name: &str) -> PathBuf {
        self.root.join("git-side").join(name)
    }

    fn pyn_dir(&self, name: &str) -> PathBuf {
        self.root.join("pyn-side").join(name)
    }

    /// Clones the rig's remote with `git clone <flags> <url> clone` and `pyn clone ...`, requires the same exit status
    /// and the same repository, and returns pyn's result.
    fn both(&self, flags: &[&str]) -> Out {
        self.both_at(&self.url, flags)
    }

    fn both_at(&self, url: &str, flags: &[&str]) -> Out {
        let bare = flags.contains(&"--bare");
        let dest = "clone";
        let g = self.git_clone(url, flags, Some(dest));
        let p = self.pyn_clone(url, flags, Some(dest));
        let ctx = format!("{flags:?} {url}\ngit: {}\npyn: {}", g.stderr, p.stderr);
        assert_eq!(g.code, p.code, "{ctx}");
        assert_eq!(announce(&g.stderr), announce(&p.stderr), "{ctx}");
        if p.code == 0 {
            let (gd, pd) = (self.git_dir(dest), self.pyn_dir(dest));
            assert_eq!(state(&gd, bare), state(&pd, bare), "{ctx}");
            assert!(!pd.join(".pyn").exists(), "a clone leaves pyn out of it");
            assert!(!pd.join(".git/FETCH_HEAD").exists());
            fsck(&pd);
            if !bare && !flags.contains(&"--no-checkout") && !flags.contains(&"-n") {
                let status = run(&pd, &["status", "--porcelain"]);
                assert_eq!(status, "", "{ctx}");
            }
        } else {
            assert!(!self.pyn_dir(dest).exists(), "a failed clone is cleaned up");
            assert!(!self.git_dir(dest).exists());
        }
        p
    }
}

/// `<flags> -- <url> [<dest>]`.
fn argv(url: &str, flags: &[&str], dest: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = flags.iter().map(|s| (*s).to_owned()).collect();
    v.push("--".into());
    v.push(url.to_owned());
    v.extend(dest.map(str::to_owned));
    v
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// The lines that say what is being cloned and any warning, without wording that differs between git versions.
/// git's "redirecting to" warning is skipped: with credentials in the URL it fires with no redirect on the wire.
fn announce(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| !l.starts_with("warning: redirecting to "))
        .filter_map(|l| {
            if l.starts_with("Cloning into") || l.starts_with("fatal: destination path") {
                Some(l.to_owned())
            } else {
                l.starts_with("warning:").then(|| "warning".to_owned())
            }
        })
        .collect()
}

fn config_lines(dir: &Path) -> Vec<String> {
    let mut lines: Vec<String> = run(dir, &["config", "--local", "-l"])
        .lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap();
            key.starts_with("remote.")
                || key.starts_with("branch.")
                || key.starts_with("extensions.")
                || key == "core.bare"
                || key == "core.repositoryformatversion"
        })
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn work_files(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for e in fs::read_dir(dir).unwrap().flatten() {
        let path = e.path();
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if name == ".git" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            out.insert(
                name,
                format!("link {}", fs::read_link(&path).unwrap().display()),
            );
        } else if meta.is_dir() {
            work_files(root, &path, out);
        } else {
            let exec = meta.permissions().mode() & 0o111 != 0;
            let body = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
            out.insert(name, format!("file exec={exec} {body:?}"));
        }
    }
}

fn reflogs(git_dir: &Path) -> BTreeMap<String, Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<String>>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let lines = fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(|l| {
                    let (head, msg) = l.split_once('\t').unwrap_or((l, ""));
                    let mut ids = head.split(' ');
                    let (old, new) = (ids.next().unwrap(), ids.next().unwrap());
                    let op = msg.split_once(':').map_or(msg, |(op, _)| op);
                    format!("{old} {new} {op}")
                })
                .collect();
            out.insert(name, lines);
        }
    }
    let base = git_dir.join("logs");
    let mut out = BTreeMap::new();
    walk(&base, &base, &mut out);
    out
}

fn is_remote_head(name: &str) -> bool {
    name.starts_with("refs/remotes/") && name.ends_with("/HEAD")
}

/// Everything about a clone that has a meaning: refs and where `HEAD` is, config, index, files and reflog operations.
fn state(dir: &Path, bare: bool) -> String {
    let refs = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(symref) %(objectname) %(objecttype)",
        ],
    );
    let head_ref = git(dir)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    let head_ref = String::from_utf8_lossy(&head_ref.stdout).trim().to_owned();
    let head_id = git(dir)
        .args(["rev-parse", "-q", "--verify", "HEAD"])
        .output()
        .unwrap();
    let head_id = String::from_utf8_lossy(&head_id.stdout).trim().to_owned();
    let index = if bare {
        String::new()
    } else {
        run(dir, &["ls-files", "-s"])
    };
    let mut files = BTreeMap::new();
    if !bare {
        work_files(dir, dir, &mut files);
    }
    let git_dir = if bare {
        dir.to_path_buf()
    } else {
        dir.join(".git")
    };
    let mut logs = reflogs(&git_dir);
    logs.retain(|k, _| k == "HEAD" || (k.starts_with("refs/heads/") && !is_remote_head(k)));
    let remote_head_log = reflogs(&git_dir)
        .into_iter()
        .filter(|(k, _)| is_remote_head(k))
        .map(|(k, v)| (k, v.len()))
        .collect::<Vec<_>>();
    let config = config_lines(dir);
    format!(
        "refs:\n{refs}head: {head_ref} {head_id}\nconfig: {config:#?}\nindex:\n{index}files: {files:#?}\nlogs: {logs:#?}\nremote head logs: {}",
        remote_head_log.len()
    )
}

fn assert_ok(o: &Out) {
    assert_eq!(o.code, 0, "{}", o.stderr);
}

#[test]
fn a_default_clone_matches_git_over_ssh_and_http_for_both_hash_formats() {
    for (format, via) in ALL_WAYS {
        let t = Rig::new(format, via);
        t.add_rich_files();
        assert_ok(&t.both(&[]));
        let pd = t.pyn_dir("clone");
        assert!(
            fs::symlink_metadata(pd.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::metadata(pd.join("bin/tool.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0o111
        );
    }
}

#[test]
fn branch_and_tag_selection_match_git() {
    let t = Rig::new("sha1", Via::Ssh);
    t.add_rich_files();
    for flags in [
        vec!["-b", "feature"],
        vec!["--branch", "main"],
        vec!["-b", "v1"],
        vec!["--single-branch"],
        vec!["--single-branch", "-b", "feature"],
        vec!["--single-branch", "-b", "v1"],
        vec!["--no-tags"],
        vec!["--no-tags", "--single-branch"],
        vec!["--tags"],
        vec!["-o", "up"],
        vec!["--origin", "up", "-b", "feature", "--single-branch"],
        vec!["--no-checkout"],
        vec!["-n", "-b", "feature"],
        vec!["--quiet"],
        vec!["-q", "--no-single-branch", "--progress", "-j", "2"],
        vec!["--no-progress", "-v"],
    ] {
        let out = t.both(&flags);
        assert_ok(&out);
        let _ = fs::remove_dir_all(t.git_dir("clone"));
        let _ = fs::remove_dir_all(t.pyn_dir("clone"));
    }
}

// Newer git handles an annotated tag after `-b` differently, so this is checked against the commit, not against git.
#[test]
fn an_annotated_tag_checkout_is_detached_at_the_peeled_commit() {
    let t = Rig::new("sha1", Via::Ssh);
    let p = t.pyn_clone(&t.url, &["-b", "v2"], Some("clone"));
    assert_ok(&p);
    let d = t.pyn_dir("clone");
    let commit = run(&t.bare, &["rev-parse", "v2^{commit}"])
        .trim()
        .to_owned();
    let tag_object = run(&t.bare, &["rev-parse", "v2"]).trim().to_owned();
    assert_eq!(run(&d, &["rev-parse", "HEAD"]).trim(), commit);
    assert_eq!(run(&d, &["rev-parse", "refs/tags/v2"]).trim(), tag_object);
    let attached = git(&d)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    assert!(attached.stdout.is_empty());
    let tree = run(&d, &["ls-tree", "-r", &commit]);
    let index = run(&d, &["ls-files", "-s"]);
    let strip = |l: &str| {
        l.split_once('\t')
            .map(|(m, p)| (m.replace(" 0", "").to_owned(), p.to_owned()))
    };
    let a: Vec<_> = tree
        .lines()
        .filter_map(|l| strip(&l.replacen(" blob", "", 1)))
        .collect();
    let b: Vec<_> = index.lines().filter_map(strip).collect();
    assert_eq!(a, b);
    let mut files = BTreeMap::new();
    work_files(&d, &d, &mut files);
    assert_eq!(files.len(), a.len());
    fsck(&d);
    assert_eq!(run(&d, &["status", "--porcelain"]), "");
}

#[test]
fn bare_clones_copy_heads_and_tags_without_tracking_refs() {
    for (format, via) in ALL_WAYS {
        let t = Rig::new(format, via);
        assert_ok(&t.both(&["--bare"]));
        let d = t.pyn_dir("clone");
        assert_eq!(run(&d, &["config", "core.bare"]).trim(), "true");
        assert!(!d.join(".git").exists());
        assert!(!run(&d, &["for-each-ref", "refs/remotes"]).contains("refs/remotes"));
    }
    let t = Rig::new("sha1", Via::Ssh);
    for flags in [
        vec!["--bare", "-b", "feature"],
        vec!["--bare", "--single-branch"],
        vec!["--bare", "--single-branch", "-b", "feature"],
        vec!["--bare", "-b", "v1"],
        vec!["--bare", "--no-tags"],
    ] {
        assert_ok(&t.both(&flags));
        let _ = fs::remove_dir_all(t.git_dir("clone"));
        let _ = fs::remove_dir_all(t.pyn_dir("clone"));
    }
}

#[test]
fn directories_default_from_the_url() {
    let t = Rig::new("sha1", Via::Ssh);
    let p = t.pyn_clone(&t.url, &[], None);
    assert_ok(&p);
    assert!(t.pyn_dir("served").join(".git").is_dir());
    let p = t.pyn_clone(&t.url, &["--bare"], None);
    assert_ok(&p);
    assert!(t.pyn_dir("served.git").join("HEAD").is_file());
    assert!(
        p.stderr
            .starts_with("Cloning into bare repository 'served.git'...")
    );
}

#[test]
fn an_empty_remote_gives_an_unborn_branch_and_a_warning() {
    for (format, via) in ALL_WAYS {
        let t = Rig::empty(format, via);
        let o = t.both(&[]);
        assert_ok(&o);
        assert!(
            o.stderr
                .contains("warning: You appear to have cloned an empty repository."),
            "{}",
            o.stderr
        );
        let d = t.pyn_dir("clone");
        assert_eq!(
            fs::read_to_string(d.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/trunk\n"
        );
        let _ = fs::remove_dir_all(t.git_dir("clone"));
        let _ = fs::remove_dir_all(t.pyn_dir("clone"));
        assert_ok(&t.both(&["--bare"]));
    }
}

#[test]
fn a_remote_head_on_an_unborn_branch_is_kept_without_a_checkout() {
    let t = Rig::new("sha1", Via::Ssh);
    run(&t.bare, &["symbolic-ref", "HEAD", "refs/heads/nothere"]);
    let o = t.both(&[]);
    assert_ok(&o);
    assert!(o.stderr.contains("warning:"), "{}", o.stderr);
    let d = t.pyn_dir("clone");
    assert_eq!(
        fs::read_to_string(d.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/nothere\n"
    );
    assert_eq!(run(&d, &["ls-files"]), "");
}

#[test]
fn a_remote_head_that_is_not_a_branch_tip_is_checked_out_detached() {
    let t = Rig::new("sha1", Via::Ssh);
    let id = run(&t.bare, &["rev-parse", "main~1"]).trim().to_owned();
    fs::write(t.bare.join("HEAD"), format!("{id}\n")).unwrap();
    assert_ok(&t.both(&[]));
    let d = t.pyn_dir("clone");
    assert_eq!(run(&d, &["rev-parse", "HEAD"]).trim(), id);
    assert!(
        git(&d)
            .args(["symbolic-ref", "-q", "HEAD"])
            .output()
            .unwrap()
            .stdout
            .is_empty()
    );
}

#[test]
fn a_remote_head_with_no_symref_picks_the_branch_at_that_commit() {
    let t = Rig::new("sha1", Via::Ssh);
    let id = run(&t.bare, &["rev-parse", "feature"]).trim().to_owned();
    fs::write(t.bare.join("HEAD"), format!("{id}\n")).unwrap();
    assert_ok(&t.both(&[]));
    assert_eq!(
        fs::read_to_string(t.pyn_dir("clone").join(".git/HEAD")).unwrap(),
        "ref: refs/heads/feature\n"
    );
}

#[test]
fn an_existing_target_is_refused_unless_it_is_an_empty_folder() {
    let t = Rig::new("sha1", Via::Ssh);
    for side in ["git-side", "pyn-side"] {
        let d = t.side(side).join("clone");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("keep"), "x").unwrap();
    }
    let args = |url: &str| -> Vec<String> { vec!["clone".into(), url.into(), "clone".into()] };
    let g = out_of(
        git(&t.side("git-side"))
            .args(args(&t.url))
            .env("GIT_SSH_COMMAND", &t.shim)
            .output()
            .unwrap(),
    );
    let p = t.pyn(&t.side("pyn-side"), &["clone", &t.url, "clone"]);
    assert_eq!((g.code, p.code), (128, 128));
    assert_eq!(announce(&g.stderr), announce(&p.stderr));
    assert!(
        p.stderr.contains(
            "fatal: destination path 'clone' already exists and is not an empty directory."
        )
    );
    assert_eq!(
        fs::read_to_string(t.pyn_dir("clone").join("keep")).unwrap(),
        "x"
    );
    assert!(!t.pyn_dir("clone").join(".git").exists());

    for side in ["git-side", "pyn-side"] {
        fs::remove_file(t.side(side).join("clone/keep")).unwrap();
    }
    assert_ok(&t.both(&[]));
}

#[test]
fn failures_remove_what_the_clone_made() {
    let t = Rig::new("sha1", Via::Ssh);
    let missing = format!("ssh://git@example.invalid{}/nowhere.git", t.root.display());
    assert_eq!(t.both_at(&missing, &[]).code, 128);
    assert_eq!(t.both(&["-b", "nope"]).code, 128);
    let p = t.pyn_clone(&t.url, &["-b", "nope"], Some("nope-dir"));
    assert!(
        p.stderr
            .contains("Remote branch nope not found in upstream origin"),
        "{}",
        p.stderr
    );
    assert!(!t.pyn_dir("nope-dir").exists());
}

#[test]
fn a_failed_clone_into_an_existing_empty_folder_leaves_it_empty() {
    let t = Rig::new("sha1", Via::Ssh);
    let d = t.side("pyn-side").join("clone");
    fs::create_dir_all(&d).unwrap();
    let missing = format!("ssh://git@example.invalid{}/nowhere.git", t.root.display());
    let p = t.pyn_clone(&missing, &[], Some("clone"));
    assert_eq!(p.code, 128, "{}", p.stderr);
    assert!(d.is_dir());
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
}

#[test]
fn an_http_auth_wall_is_passed_with_a_token_url_and_stops_a_bad_one() {
    for format in FORMATS {
        let t = Rig::new(format, Via::HttpAuth);
        assert_ok(&t.both(&[]));
        let wrong = t.url.replace("s3cret", "wrong");
        let _ = fs::remove_dir_all(t.git_dir("clone"));
        let _ = fs::remove_dir_all(t.pyn_dir("clone"));
        let o = t.both_at(&wrong, &[]);
        assert_eq!(o.code, 128);
        assert!(
            !o.stderr.contains("wrong"),
            "credentials never reach the messages: {}",
            o.stderr
        );
    }
}

#[test]
fn settings_given_with_c_reach_the_new_repository_and_the_transport() {
    let t = Rig::new("sha1", Via::Http);
    let url = format!("{}/served.git", t.server.as_ref().unwrap().url);
    let p = t.pyn_clone(
        &url,
        &[
            "-c",
            "user.name=zed",
            "-c",
            "http.extraHeader=X-Clone-Test: yes",
        ],
        Some("d"),
    );
    let _ = p;
    let d = t.pyn_dir("d");
    assert_eq!(run(&d, &["config", "--local", "user.name"]).trim(), "zed");
    let seen = t.server.as_ref().unwrap().log();
    assert!(
        seen.iter()
            .any(|s| s.headers.get("x-clone-test").is_some_and(|v| v == "yes")),
        "{seen:?}"
    );
}

#[test]
fn every_address_form_reaches_the_same_repository() {
    let t = Rig::new("sha1", Via::Ssh);
    let path = t.bare.display().to_string();
    let reference = t.both(&[]);
    assert_ok(&reference);
    let want = state(&t.pyn_dir("clone"), false);
    for url in [
        format!("git@example.invalid:{path}"),
        format!("git+ssh://git@example.invalid{path}"),
        format!("ssh://git@example.invalid:22{path}"),
    ] {
        let _ = fs::remove_dir_all(t.pyn_dir("again"));
        let o = t.pyn_clone(&url, &[], Some("again"));
        assert_ok(&o);
        let got = state(&t.pyn_dir("again"), false);
        let norm = |s: &str| {
            s.lines()
                .filter(|l| !l.contains("remote.origin.url"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(norm(&want), norm(&got), "{url}");
        assert!(run(&t.pyn_dir("again"), &["config", "remote.origin.url"]).trim() == url);
    }
}

#[test]
fn unsupported_flags_are_usage_errors_and_leave_nothing_behind() {
    let t = Rig::new("sha1", Via::Ssh);
    for flag in [
        vec!["--mirror"],
        vec!["--depth", "1"],
        vec!["--shallow-since", "2020-01-01"],
        vec!["--shallow-exclude", "v1"],
        vec!["--filter", "blob:none"],
        vec!["--recurse-submodules"],
        vec!["--sparse"],
        vec!["--reference", "/x"],
        vec!["--separate-git-dir", "/x"],
        vec!["--template", "/x"],
        vec!["--local"],
        vec!["--no-hardlinks"],
    ] {
        let p = t.pyn_clone(&t.url, &flag, None);
        assert_eq!(p.code, 129, "{flag:?}: {}", p.stderr);
        assert!(
            p.stderr.contains(&format!("`pyn clone {}`", flag[0])),
            "{}",
            p.stderr
        );
        assert!(!t.pyn_dir("served").exists());
    }
}

#[test]
fn local_paths_and_file_urls_are_refused_with_a_clear_error() {
    let t = Rig::new("sha1", Via::Ssh);
    for src in [
        t.bare.display().to_string(),
        format!("file://{}", t.bare.display()),
    ] {
        let p = t.pyn_clone(&src, &[], Some("x"));
        assert_eq!(p.code, 128, "{}", p.stderr);
        assert!(p.stderr.contains("not supported"), "{}", p.stderr);
        assert!(!t.pyn_dir("x").exists());
    }
}

#[test]
fn pyn_works_in_a_fresh_clone_and_leaves_no_state_behind() {
    for format in FORMATS {
        let t = Rig::new(format, Via::Ssh);
        t.add_rich_files();
        assert_ok(&t.pyn_clone(&t.url, &[], Some("clone")));
        let d = t.pyn_dir("clone");
        assert!(!d.join(".pyn").exists());
        for args in [vec!["status"], vec!["log"], vec!["branch"], vec!["fetch"]] {
            let o = t.pyn(&d, &args);
            assert_eq!(o.code, 0, "{args:?}: {}", o.stderr);
        }
        t.work_commit("later.txt");
        let o = t.pyn(&d, &["fetch"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert!(o.stderr.contains("From "), "{}", o.stderr);
        fsck(&d);
    }
}

impl Rig {
    fn work_commit(&self, file: &str) {
        commit(&self.work, file, b"later\n", "later");
        run(
            &self.work,
            &["push", "-q", self.bare.to_str().unwrap(), "main"],
        );
    }
}

#[test]
fn a_git_host_is_cloned_as_git_after_the_probe_finds_no_pyn_server() {
    let t = Rig::new("sha1", Via::Http);
    let nested = t.root.join("team");
    fs::create_dir_all(&nested).unwrap();
    fs::rename(&t.bare, nested.join("served.git")).unwrap();
    let server = t.server.as_ref().unwrap();
    let url = format!("{}/team/served.git", server.url);
    server.clear_log();
    let o = t.pyn_clone(&url, &[], Some("probed"));
    assert_ok(&o);
    assert!(
        !o.stderr.contains("openapi"),
        "the probe prints nothing: {}",
        o.stderr
    );
    assert!(t.pyn_dir("probed").join(".git").is_dir());
    assert!(server.log().iter().any(|s| s.target == "/openapi.json"));
}

#[test]
fn a_pyn_server_answering_the_probe_is_cloned_as_a_pyn_server() {
    let env = common::start();
    let work = env.dir("ws");
    let out = env.run(
        env.dir("cwd").as_path(),
        "alice",
        &["clone", &env.source(common::REPO), work.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(work.join(".pyn").is_dir());
    assert!(!work.join(".git").exists());
    assert!(String::from_utf8_lossy(&out.stdout).contains("cloned"));
}

#[test]
fn a_silent_server_is_tried_as_git_and_the_failure_mentions_a_pyn_server() {
    let t = Rig::new("sha1", Via::Ssh);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/team/r.git");
    let p = t.pyn_clone(&url, &[], Some("never"));
    assert_eq!(p.code, 128, "{}", p.stderr);
    assert!(p.stderr.contains("pyn server"), "{}", p.stderr);
    assert!(!t.pyn_dir("never").exists());
}

#[test]
fn pyn_server_flags_are_refused_for_a_git_flag_and_git_flags_for_a_server() {
    let env = common::start();
    let o = env.run(
        env.dir("cwd").as_path(),
        "alice",
        &[
            "clone",
            "--bare",
            &env.source(common::REPO),
            env.dir("x").to_str().unwrap(),
        ],
    );
    assert_eq!(o.status.code(), Some(129));
    let t = Rig::new("sha1", Via::Ssh);
    let p = t.pyn_clone(&t.url, &["--revision", "HEAD~1"], Some("x"));
    assert_eq!(p.code, 129, "{}", p.stderr);
}

#[test]
fn an_empty_remote_over_protocol_v0_takes_the_configured_default_branch() {
    let mut t = Rig::empty("sha1", Via::Ssh);
    t.shim = ssh_shim(&t.root, true);
    assert_ok(&t.both(&[]));
    let d = t.pyn_dir("clone");
    assert_eq!(
        fs::read_to_string(d.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
    assert_ok(&t.pyn_clone(&t.url, &["-c", "init.defaultBranch=other"], Some("second")));
    assert_eq!(
        fs::read_to_string(t.pyn_dir("second").join(".git/HEAD")).unwrap(),
        "ref: refs/heads/other\n"
    );
}
