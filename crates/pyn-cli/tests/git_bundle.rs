//! `pyn bundle` and bundles as remotes for `clone`, `fetch` and `pull`, against real git in both directions: bundles
//! pyn writes are verified, cloned and fetched by git, and bundles git writes are read by pyn. Real git is the oracle
//! in these tests only; semantics are compared (refs, exit statuses, listings), never message wording.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
    format: &'static str,
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

impl Rig {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n[gc]\n\tauto = 0\n[maintenance]\n\tauto = false\n",
        )
        .unwrap();
        Rig {
            _tmp: tmp,
            root,
            home,
            format,
        }
    }

    fn envs(&self, c: &mut Command) {
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        if let Some(p) = std::env::var_os("GIT_EXEC_PATH") {
            c.env("GIT_EXEC_PATH", p);
        }
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
        c.current_dir(cwd).args(args);
        self.envs(&mut c);
        out_of(c.output().unwrap())
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Out {
        let mut c = Command::new("git");
        c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .current_dir(cwd)
            .args(args);
        self.envs(&mut c);
        out_of(c.output().unwrap())
    }

    fn git_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.git(cwd, args);
        assert_eq!(o.code, 0, "git {args:?}: {}", o.stderr);
        o.stdout
    }

    fn pyn_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.pyn(cwd, args);
        assert_eq!(o.code, 0, "pyn {args:?}: {}", o.stderr);
        o.stdout
    }

    fn init(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let format = format!("--object-format={}", self.format);
        self.git_ok(&dir, &["init", "-q", &format]);
        dir
    }

    fn commit(&self, dir: &Path, file: &str, content: &str, msg: &str) {
        fs::write(dir.join(file), content).unwrap();
        self.git_ok(dir, &["add", file]);
        self.git_ok(dir, &["commit", "-q", "-m", msg]);
    }

    /// Two commits on `main`, a tag, an annotated tag and a branch; `more` adds two commits and a branch.
    fn source(&self) -> PathBuf {
        let src = self.init("src");
        let big: String = (0..300)
            .map(|i| format!("line {i} of the big file\n"))
            .collect();
        fs::write(src.join("big.txt"), &big).unwrap();
        self.commit(&src, "a.txt", "one\n", "one");
        self.git_ok(&src, &["tag", "-a", "-m", "release", "v1"]);
        self.git_ok(&src, &["branch", "side"]);
        self.commit(&src, "b.txt", "two\n", "two");
        self.git_ok(&src, &["tag", "light"]);
        src
    }

    fn more(&self, src: &Path) {
        let big = fs::read_to_string(src.join("big.txt")).unwrap();
        self.commit(
            src,
            "big.txt",
            &big.replace("line 150 ", "LINE 150 "),
            "edit",
        );
        self.commit(src, "a.txt", "one\nthree\n", "three");
        self.commit(src, "c.txt", "four\n", "four");
        self.git_ok(src, &["branch", "extra"]);
    }

    fn refs(&self, repo: &Path) -> BTreeMap<String, String> {
        self.git_ok(repo, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .map(|l| {
                let (n, id) = l.split_once(' ').unwrap();
                (n.to_owned(), id.to_owned())
            })
            .collect()
    }

    fn fsck(&self, repo: &Path) {
        let o = self.git(repo, &["fsck", "--strict", "--no-dangling"]);
        assert_eq!(o.code, 0, "fsck: {}{}", o.stdout, o.stderr);
    }

    fn head(&self, repo: &Path) -> String {
        self.git_ok(repo, &["rev-parse", "HEAD"]).trim().to_owned()
    }
}

fn formats() -> [&'static str; 2] {
    ["sha1", "sha256"]
}

#[test]
fn pyn_bundles_verify_and_clone_in_git() {
    for format in formats() {
        let rig = Rig::new(format);
        let src = rig.source();
        let full = rig.root.join("full.bundle");
        rig.pyn_ok(&src, &["bundle", "create", full.to_str().unwrap(), "--all"]);
        rig.git_ok(&src, &["bundle", "verify", full.to_str().unwrap()]);
        let heads = rig.pyn_ok(&src, &["bundle", "list-heads", full.to_str().unwrap()]);
        assert_eq!(
            heads,
            rig.git_ok(&src, &["bundle", "list-heads", full.to_str().unwrap()]),
            "{format}"
        );
        let clone = rig.root.join("gitclone");
        rig.git_ok(
            &rig.root,
            &[
                "clone",
                "-q",
                full.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        rig.fsck(&clone);
        let tags = rig.git_ok(&clone, &["tag", "-l"]);
        assert_eq!(
            tags.lines().collect::<Vec<_>>(),
            ["light", "v1"],
            "{format}"
        );
        assert_eq!(rig.head(&clone), rig.head(&src), "{format}");
    }
}

#[test]
fn pyn_incremental_bundles_carry_prerequisites_git_accepts() {
    for format in formats() {
        let rig = Rig::new(format);
        let src = rig.source();
        let base = rig.root.join("base.bundle");
        rig.pyn_ok(&src, &["bundle", "create", base.to_str().unwrap(), "--all"]);
        let clone = rig.root.join("clone");
        rig.git_ok(
            &rig.root,
            &[
                "clone",
                "-q",
                base.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let before = rig.head(&src);
        rig.more(&src);
        let inc = rig.root.join("inc.bundle");
        let range = format!("{before}..main");
        rig.pyn_ok(&src, &["bundle", "create", inc.to_str().unwrap(), &range]);
        let text = fs::read(&inc).unwrap();
        let header = String::from_utf8_lossy(&text[..text.len().min(400)]).into_owned();
        assert!(
            header.contains(&format!("-{before} ")),
            "{format}: {header}"
        );
        let none = rig.git(
            &rig.init("empty"),
            &["bundle", "verify", inc.to_str().unwrap()],
        );
        assert_eq!(none.code, 1, "{format}");
        rig.git_ok(&clone, &["bundle", "verify", inc.to_str().unwrap()]);
        rig.git_ok(
            &clone,
            &["fetch", "-q", inc.to_str().unwrap(), "main:refs/heads/got"],
        );
        rig.fsck(&clone);
        assert_eq!(
            rig.refs(&clone)["refs/heads/got"],
            rig.head(&src),
            "{format}"
        );
    }
}

#[test]
fn git_bundles_clone_fetch_and_unbundle_in_pyn() {
    for format in formats() {
        let rig = Rig::new(format);
        let src = rig.source();
        let full = rig.root.join("full.bundle");
        rig.git_ok(&src, &["bundle", "create", full.to_str().unwrap(), "--all"]);
        let verify = rig.pyn(&src, &["bundle", "verify", full.to_str().unwrap()]);
        assert_eq!(verify.code, 0, "{format}: {}", verify.stderr);
        assert_eq!(
            verify.stdout,
            rig.git_ok(&src, &["bundle", "verify", full.to_str().unwrap()]),
            "{format}"
        );

        let ours = rig.root.join("pynclone");
        let theirs = rig.root.join("gitclone");
        rig.pyn_ok(
            &rig.root,
            &[
                "clone",
                "-q",
                full.to_str().unwrap(),
                ours.to_str().unwrap(),
            ],
        );
        rig.git_ok(
            &rig.root,
            &[
                "clone",
                "-q",
                full.to_str().unwrap(),
                theirs.to_str().unwrap(),
            ],
        );
        rig.fsck(&ours);
        assert_eq!(rig.refs(&ours), rig.refs(&theirs), "{format}");
        assert_eq!(
            rig.git_ok(&ours, &["config", "remote.origin.url"]),
            full.to_str().unwrap().to_owned() + "\n"
        );

        let before = rig.head(&src);
        rig.more(&src);
        let inc = rig.root.join("inc.bundle");
        let range = format!("{before}..main");
        rig.git_ok(
            &src,
            &["bundle", "create", inc.to_str().unwrap(), &range, "extra"],
        );
        rig.pyn_ok(
            &ours,
            &[
                "fetch",
                "-q",
                inc.to_str().unwrap(),
                "main:refs/heads/got",
                "extra:refs/heads/extra",
            ],
        );
        rig.git_ok(
            &theirs,
            &[
                "fetch",
                "-q",
                inc.to_str().unwrap(),
                "main:refs/heads/got",
                "extra:refs/heads/extra",
            ],
        );
        rig.fsck(&ours);
        assert_eq!(rig.refs(&ours), rig.refs(&theirs), "{format}");

        let loose = rig.init("loose");
        rig.pyn_ok(&loose, &["bundle", "unbundle", full.to_str().unwrap()]);
        let listed = rig.pyn_ok(&loose, &["bundle", "unbundle", full.to_str().unwrap()]);
        assert_eq!(
            listed,
            rig.git_ok(&src, &["bundle", "list-heads", full.to_str().unwrap()]),
            "{format}"
        );
        rig.git_ok(&loose, &["cat-file", "-e", &before]);
        rig.fsck(&loose);
    }
}

#[test]
fn pull_from_a_bundle_fast_forwards() {
    let rig = Rig::new("sha1");
    let src = rig.source();
    let full = rig.root.join("full.bundle");
    rig.git_ok(&src, &["bundle", "create", full.to_str().unwrap(), "--all"]);
    let ours = rig.root.join("ours");
    rig.pyn_ok(
        &rig.root,
        &[
            "clone",
            "-q",
            full.to_str().unwrap(),
            ours.to_str().unwrap(),
        ],
    );
    let before = rig.head(&src);
    rig.more(&src);
    let next = rig.root.join("next.bundle");
    let range = format!("{before}..main");
    rig.git_ok(&src, &["bundle", "create", next.to_str().unwrap(), &range]);
    rig.git_ok(
        &ours,
        &["remote", "set-url", "origin", next.to_str().unwrap()],
    );
    rig.pyn_ok(&ours, &["pull", "-q"]);
    assert_eq!(rig.head(&ours), rig.head(&src));
    assert_eq!(fs::read_to_string(ours.join("c.txt")).unwrap(), "four\n");
}

#[test]
fn missing_prerequisites_fail_and_leave_the_repository_alone() {
    let rig = Rig::new("sha1");
    let src = rig.source();
    let before = rig.head(&src);
    rig.more(&src);
    let inc = rig.root.join("inc.bundle");
    let range = format!("{before}..main");
    rig.pyn_ok(&src, &["bundle", "create", inc.to_str().unwrap(), &range]);

    let lacking = rig.init("lacking");
    rig.commit(&lacking, "z.txt", "z\n", "z");
    let refs = rig.refs(&lacking);
    for args in [
        vec!["bundle", "verify", inc.to_str().unwrap()],
        vec!["bundle", "unbundle", inc.to_str().unwrap()],
        vec!["fetch", inc.to_str().unwrap(), "main:refs/heads/got"],
    ] {
        let pyn = rig.pyn(&lacking, &args);
        assert_eq!(pyn.code, 1, "{args:?}: {}", pyn.stderr);
        assert!(pyn.stderr.contains(&before), "{args:?}: {}", pyn.stderr);
    }
    assert_eq!(rig.refs(&lacking), refs);
    assert_eq!(rig.git(&lacking, &["cat-file", "-e", &before]).code, 1);

    let into = rig.root.join("cloned");
    let clone = rig.pyn(
        &rig.root,
        &["clone", inc.to_str().unwrap(), into.to_str().unwrap()],
    );
    assert_ne!(clone.code, 0);
    assert!(!into.exists());
}

#[test]
fn creating_refuses_what_git_refuses() {
    let rig = Rig::new("sha1");
    let src = rig.source();
    let out = rig.root.join("x.bundle");
    let file = out.to_str().unwrap();
    for args in [
        vec!["bundle", "create", file],
        vec!["bundle", "create", file, "main", "^main"],
        vec!["bundle", "create", file, "main~1"],
        vec!["bundle", "create", file, "nosuch"],
        vec!["bundle", "create", "--version=4", file, "main"],
    ] {
        let ours = rig.pyn(&src, &args);
        let theirs = rig.git(&src, &args);
        assert_eq!(ours.code, theirs.code, "{args:?}: {}", ours.stderr);
        assert!(!out.exists(), "{args:?}");
    }
    let sha256 = Rig::new("sha256");
    let s = sha256.source();
    let ours = sha256.pyn(&s, &["bundle", "create", "--version=2", file, "main"]);
    let theirs = sha256.git(&s, &["bundle", "create", "--version=2", file, "main"]);
    assert_eq!((ours.code, theirs.code), (128, 128));
}

#[test]
fn a_damaged_or_foreign_file_is_not_a_bundle() {
    let rig = Rig::new("sha1");
    let src = rig.source();
    let full = rig.root.join("full.bundle");
    rig.pyn_ok(&src, &["bundle", "create", full.to_str().unwrap(), "main"]);
    let mut bytes = fs::read(&full).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 0xff;
    let bad = rig.root.join("bad.bundle");
    fs::write(&bad, &bytes).unwrap();
    assert_ne!(
        rig.pyn(&src, &["bundle", "verify", bad.to_str().unwrap()])
            .code,
        0
    );
    let text = rig.root.join("text.bundle");
    fs::write(&text, "not a bundle\n").unwrap();
    for args in [["bundle", "list-heads"], ["bundle", "verify"]] {
        let mut a = args.to_vec();
        a.push(text.to_str().unwrap());
        assert_ne!(rig.pyn(&src, &a).code, 0);
    }
    assert_eq!(
        rig.pyn(&src, &["bundle", "list-heads", "/nonexistent.bundle"])
            .code,
        1
    );
}

#[test]
fn list_heads_and_a_ranged_create_work_outside_a_repository() {
    let rig = Rig::new("sha1");
    let src = rig.source();
    let full = rig.root.join("full.bundle");
    rig.git_ok(&src, &["bundle", "create", full.to_str().unwrap(), "--all"]);
    let outside = rig.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let listed = rig.pyn_ok(&outside, &["bundle", "list-heads", full.to_str().unwrap()]);
    assert_eq!(
        listed,
        rig.git_ok(&outside, &["bundle", "list-heads", full.to_str().unwrap()])
    );
    let one = rig.pyn_ok(
        &outside,
        &["bundle", "list-heads", full.to_str().unwrap(), "side"],
    );
    assert_eq!(one.lines().count(), 1);
    assert_eq!(
        rig.pyn(&outside, &["bundle", "verify", full.to_str().unwrap()])
            .code,
        128
    );
}
