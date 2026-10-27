//! `pyn count-objects`, `fsck`, `prune`, `repack`, `gc` and `pack-refs` on a git repository. Output and exit statuses
//! are compared with the real git on the same repository, and what pyn writes is read back with `git fsck --full` and
//! `git verify-pack`. Real git is the oracle in these tests only; comparisons are of behaviour, not of git's exact bytes.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::{FORMATS, git, init, path_env};
use tempfile::{TempDir, tempdir};

struct Fx {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    format: &'static str,
    tick: Cell<i64>,
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn sorted(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

impl Fx {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        init(&repo, format, false);
        fs::create_dir_all(root.join("home")).unwrap();
        Self {
            _tmp: tmp,
            root,
            repo,
            format,
            tick: Cell::new(1_700_000_000),
        }
    }

    fn g(&self, args: &[&str]) -> String {
        self.tick.set(self.tick.get() + 86_400);
        let date = format!("{} +0000", self.tick.get());
        let out = git(&self.repo)
            .args(args)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, path: &str, content: &str) {
        let file = self.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn commit(&self, message: &str) {
        self.g(&["add", "-A"]);
        self.g(&["commit", "-q", "-m", message]);
    }

    fn rev(&self, spec: &str) -> String {
        self.g_ro(&["rev-parse", spec]).trim().to_string()
    }

    fn g_ro(&self, args: &[&str]) -> String {
        let out = git(&self.repo).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn run(&self, mut c: Command, args: &[&str]) -> Run {
        c.args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::null());
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn pyn(&self, args: &[&str]) -> Run {
        self.run(Command::new(env!("CARGO_BIN_EXE_pyn")), args)
    }

    fn oracle(&self, args: &[&str]) -> Run {
        let mut c = Command::new("git");
        c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
        self.run(c, args)
    }

    /// Git as configured, so `gc.auto` applies.
    fn oracle_auto(&self, args: &[&str]) -> Run {
        self.run(Command::new("git"), args)
    }

    /// Standard output byte for byte, the exit status, and whether standard error is used.
    fn same(&self, args: &[&str]) {
        let want = self.oracle(args);
        let got = self.pyn(args);
        assert_eq!(got.out, want.out, "{} stdout of {args:?}", self.format);
        assert_eq!(
            got.code, want.code,
            "{} status of {args:?}: {}",
            self.format, got.err
        );
        assert_eq!(
            got.err.is_empty(),
            want.err.is_empty(),
            "{} stderr use of {args:?}: pyn {:?}, git {:?}",
            self.format,
            got.err,
            want.err
        );
    }

    /// Like [`Fx::same`] for output whose line order is git's hash-table order: lines compare sorted.
    fn same_sorted(&self, args: &[&str]) {
        let want = self.oracle(args);
        let got = self.pyn(args);
        assert_eq!(
            sorted(&got.out),
            sorted(&want.out),
            "{} stdout of {args:?}",
            self.format
        );
        assert_eq!(
            got.code, want.code,
            "{} status of {args:?}: {}",
            self.format, got.err
        );
        let reason = |t: &str| {
            sorted(t)
                .iter()
                .map(|l| oracle::fsck_line(l))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            reason(&got.err),
            reason(&want.err),
            "{} stderr of {args:?}",
            self.format
        );
    }

    /// Status, the `missing` lines and the number of broken links must agree; which tree is named as the
    /// parent of a broken link depends on git's traversal order, and notices differ between git versions.
    fn same_connectivity(&self, args: &[&str]) {
        let want = self.oracle(args);
        let got = self.pyn(args);
        let keep = |t: &str, prefix: &str| {
            sorted(t)
                .into_iter()
                .filter(|l| l.starts_with(prefix))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keep(&got.out, "missing"),
            keep(&want.out, "missing"),
            "{} missing lines of {args:?}",
            self.format
        );
        assert_eq!(
            keep(&got.out, "broken link").len(),
            keep(&want.out, "broken link").len(),
            "{} broken links of {args:?}",
            self.format
        );
        assert_eq!(
            got.code, want.code,
            "{} status of {args:?}: {}",
            self.format, got.err
        );
        let errors = |t: &str| {
            sorted(t)
                .into_iter()
                .filter(|l| !l.starts_with("notice:"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            errors(&got.err),
            errors(&want.err),
            "{} stderr of {args:?}",
            self.format
        );
    }

    /// A copy of this repository to run the other implementation on.
    fn twin(&self, name: &str) -> Fx {
        let repo = self.root.join(name);
        let status = Command::new("cp")
            .args(["-a"])
            .arg(&self.repo)
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        Fx {
            _tmp: tempdir().unwrap(),
            root: self.root.clone(),
            repo,
            format: self.format,
            tick: Cell::new(self.tick.get()),
        }
    }

    /// Every object the repository can read, with its type.
    fn all_objects(&self) -> BTreeSet<String> {
        self.oracle(&[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ])
        .out
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn loose_ids(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for d in fs::read_dir(self.objects_dir()).unwrap().flatten() {
            let n = d.file_name().to_string_lossy().into_owned();
            if n.len() == 2 && d.path().is_dir() {
                for f in fs::read_dir(d.path()).unwrap().flatten() {
                    out.insert(format!("{n}{}", f.file_name().to_string_lossy()));
                }
            }
        }
        out
    }

    fn pack_bytes(&self) -> u64 {
        self.packs()
            .iter()
            .map(|p| fs::metadata(p.with_extension("pack")).unwrap().len())
            .sum()
    }

    fn deltas(&self) -> usize {
        self.packs()
            .iter()
            .map(|p| {
                let v = self.oracle(&["verify-pack", "-v", p.to_str().unwrap()]).out;
                v.lines()
                    .filter(|l| l.split_whitespace().count() == 7)
                    .count()
            })
            .sum()
    }

    fn assert_git_ok(&self) {
        let full = self.oracle(&["fsck", "--full", "--strict"]);
        assert_eq!(full.code, 0, "git fsck --full: {}{}", full.out, full.err);
        for pack in self.packs() {
            let v = self.oracle(&["verify-pack", pack.to_str().unwrap()]);
            assert_eq!(v.code, 0, "git verify-pack {}: {}", pack.display(), v.err);
        }
    }

    fn packs(&self) -> Vec<PathBuf> {
        let dir = self.repo.join(".git/objects/pack");
        let Ok(rd) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "idx"))
            .collect();
        v.sort();
        v
    }

    fn objects_dir(&self) -> PathBuf {
        self.repo.join(".git/objects")
    }

    fn loose_path(&self, id: &str) -> PathBuf {
        self.objects_dir().join(&id[..2]).join(&id[2..])
    }

    fn hash_object(&self, kind: &str, data: &[u8]) -> String {
        let mut c = git(&self.repo);
        c.args(["hash-object", "-t", kind, "--literally", "-w", "--stdin"]);
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), data).unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    }
}

fn force_remove(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).unwrap();
    fs::remove_file(path).unwrap();
}

/// A history with tags, a merge, a few versions of a larger file and some unreachable leftovers.
fn build(format: &'static str) -> Fx {
    let f = Fx::new(format);
    let big: String = (0..400)
        .map(|i| format!("line {i} of the shared file\n"))
        .collect();
    f.write("a.txt", "a\n");
    f.write("big.txt", &big);
    f.write("d/e.txt", "e\n");
    f.commit("initial commit");
    f.g(&["tag", "light"]);
    f.write("big.txt", &format!("{big}more one\n"));
    f.commit("second commit");
    f.g(&["tag", "-a", "-m", "release one", "v1"]);
    f.g(&["switch", "-q", "-c", "feature"]);
    f.write("feat.txt", "feature\n");
    f.write("big.txt", &format!("{big}more one\nfeature tail\n"));
    f.commit("feature work");
    f.g(&["switch", "-q", "main"]);
    f.write("d/e.txt", "e3\n");
    f.commit("third commit");
    f.g(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "Merge branch 'feature'",
        "feature",
    ]);
    f
}

#[test]
fn count_objects_matches_git() {
    for format in FORMATS {
        let f = build(format);
        for args in [
            &["count-objects"][..],
            &["count-objects", "-v"],
            &["count-objects", "-H"],
            &["count-objects", "-vH"],
        ] {
            f.same(args);
        }
        f.g(&["repack", "-q"]);
        f.write("loose.txt", "new\n");
        f.g(&["add", "loose.txt"]);
        f.g(&["repack", "-q"]);
        f.write("later.txt", "x\n");
        f.g(&["add", "later.txt"]);
        for args in [&["count-objects", "-v"][..], &["count-objects", "-vH"]] {
            f.same(args);
        }
        f.g(&["repack", "-q", "-a"]);
        f.same(&["count-objects", "-v"]);
        fs::write(f.objects_dir().join("pack/tmp_pack_junk"), "junk junk").unwrap();
        fs::write(f.objects_dir().join("info/stray"), "x").ok();
        f.same(&["count-objects", "-v"]);
        let alt = f.root.join("alt");
        init(&alt, format, true);
        fs::create_dir_all(f.objects_dir().join("info")).unwrap();
        fs::write(
            f.objects_dir().join("info/alternates"),
            format!("{}\n", alt.join("objects").display()),
        )
        .unwrap();
        let got = f.pyn(&["count-objects", "-v"]).out;
        assert!(got.contains("alternate: "), "{got}");
        f.same(&["count-objects", "-v"]);
    }
}

#[test]
fn humanised_sizes_round_like_git() {
    let f = build("sha1");
    f.write("blob.bin", &"x".repeat(5_000_000));
    f.g(&["add", "."]);
    f.g(&["repack", "-q", "-a", "-d"]);
    for args in [&["count-objects", "-vH"][..], &["count-objects", "-H"]] {
        f.same(args);
    }
}

#[test]
fn fsck_reports_dangling_objects_like_git() {
    for format in FORMATS {
        let f = build(format);
        f.hash_object("blob", b"orphan blob\n");
        f.g(&["reset", "-q", "--hard", "HEAD~1"]);
        for args in [
            &["fsck"][..],
            &["fsck", "--no-reflogs"],
            &["fsck", "--no-reflogs", "--unreachable"],
            &["fsck", "--no-reflogs", "--no-dangling"],
            &["fsck", "--no-reflogs", "--root"],
            &["fsck", "--tags"],
            &["fsck", "--strict", "--full"],
            &["fsck", "--connectivity-only"],
            &["fsck", "--no-full"],
        ] {
            f.same_sorted(args);
        }
        f.g(&["gc", "-q", "--prune=now"]);
        f.same_sorted(&["fsck", "--no-reflogs"]);
        f.assert_git_ok();
    }
}

#[test]
fn fsck_lost_found_writes_what_git_writes() {
    let f = build("sha1");
    f.hash_object("blob", b"orphan blob\n");
    f.g(&["reset", "-q", "--hard", "HEAD~1"]);
    let tree = |root: &Path| {
        let mut all = Vec::new();
        for kind in ["commit", "other"] {
            if let Ok(rd) = fs::read_dir(root.join(".git/lost-found").join(kind)) {
                for e in rd.flatten() {
                    all.push((
                        format!("{kind}/{}", e.file_name().to_string_lossy()),
                        fs::read(e.path()).unwrap(),
                    ));
                }
            }
        }
        all.sort();
        all
    };
    f.oracle(&["fsck", "--no-reflogs", "--lost-found"]);
    let want = tree(&f.repo);
    assert!(!want.is_empty());
    fs::remove_dir_all(f.repo.join(".git/lost-found")).unwrap();
    f.pyn(&["fsck", "--no-reflogs", "--lost-found"]);
    assert_eq!(tree(&f.repo), want);
}

#[test]
fn fsck_finds_missing_and_corrupt_objects_like_git() {
    for format in FORMATS {
        let f = build(format);
        let blob = f.rev("HEAD:a.txt");
        let sub = f.rev("HEAD:d");
        force_remove(&f.loose_path(&blob));
        f.same_connectivity(&["fsck"]);
        force_remove(&f.loose_path(&sub));
        f.same_connectivity(&["fsck"]);
        f.same_connectivity(&["fsck", "--connectivity-only"]);
    }
}

#[test]
fn fsck_flags_corrupt_loose_objects_and_bad_refs() {
    for format in FORMATS {
        let f = build(format);
        let blob = f.rev("HEAD:a.txt");
        let path = f.loose_path(&blob);
        force_remove(&path);
        fs::write(&path, "garbage").unwrap();
        let got = f.pyn(&["fsck"]);
        let want = f.oracle(&["fsck"]);
        assert_eq!(got.code, want.code, "{format}: {}", got.err);
        assert!(got.err.contains("object corrupt or missing"), "{}", got.err);
        force_remove(&path);

        fs::write(
            f.repo.join(".git/refs/heads/bad"),
            format!("{}\n", "1".repeat(f.rev("HEAD").len())),
        )
        .unwrap();
        f.same_connectivity(&["fsck"]);
    }
}

#[test]
fn fsck_checks_object_contents_like_git() {
    let f = build("sha1");
    let blob = f.rev("HEAD:a.txt");
    let bin: Vec<u8> = (0..blob.len() / 2)
        .map(|i| u8::from_str_radix(&blob[2 * i..2 * i + 2], 16).unwrap())
        .collect();
    let entry = |mode: &str, name: &str| {
        let mut v = format!("{mode} {name}\0").into_bytes();
        v.extend(&bin);
        v
    };
    let tree = |parts: &[(&str, &str)]| {
        let mut body = Vec::new();
        for (m, n) in parts {
            body.extend(entry(m, n));
        }
        body
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("tree", tree(&[("0100644", "a")])),
        ("tree", tree(&[("100644", "a"), ("100644", "a")])),
        ("tree", tree(&[("100644", "b"), ("100644", "a")])),
        ("tree", tree(&[("100644", ".git")])),
        ("tree", tree(&[("100644", ".")])),
        ("tree", tree(&[("100644", "x/y")])),
        ("tree", tree(&[("123456", "a")])),
        ("tree", tree(&[("100664", "a")])),
        (
            "tree",
            tree(&[("100644", "a"), ("100644", "a.b"), ("100644", "a0")]),
        ),
    ];
    for (n, (kind, body)) in cases.into_iter().enumerate() {
        eprintln!("case {n}");
        let id = f.hash_object(kind, &body);
        for args in [
            &["fsck", "--no-reflogs"][..],
            &["fsck", "--no-reflogs", "--strict"],
        ] {
            f.same_sorted(args);
        }
        force_remove(&f.loose_path(&id));
    }
}

#[test]
fn fsck_checks_commits_and_tags_like_git() {
    let f = build("sha1");
    let tree = f.rev("HEAD^{tree}");
    let ident = "A <a@b> 1700000000 +0000";
    let commit = |lines: &[String], body: &str| {
        let mut s = lines.join("\n");
        s.push_str("\n\n");
        s.push_str(body);
        s.into_bytes()
    };
    let t = format!("tree {tree}");
    let a = format!("author {ident}");
    let c = format!("committer {ident}");
    let cases: Vec<Vec<u8>> = vec![
        commit(&[a.clone(), c.clone()], "no tree\n"),
        commit(&[t.clone(), c.clone()], "no author\n"),
        commit(&[t.clone(), a.clone()], "no committer\n"),
        commit(&["tree xyz".into(), a.clone(), c.clone()], "bad tree\n"),
        commit(
            &[t.clone(), "parent xx".into(), a.clone(), c.clone()],
            "bad parent\n",
        ),
        commit(
            &[
                t.clone(),
                "author A a@b> 1700000000 +0000".into(),
                c.clone(),
            ],
            "name\n",
        ),
        commit(
            &[t.clone(), "author A <a@b> 17000x +0000".into(), c.clone()],
            "date\n",
        ),
        commit(
            &[
                t.clone(),
                a.clone(),
                "committer A <a@b> 1700000000 +0".into(),
            ],
            "tz\n",
        ),
        format!("{t}\n{a}\n{c}").into_bytes(),
        commit(&[t.clone(), a.clone(), c.clone()], "fine\n"),
    ];
    for body in cases {
        let id = f.hash_object("commit", &body);
        f.same_sorted(&["fsck", "--no-reflogs"]);
        force_remove(&f.loose_path(&id));
    }
    let head = f.rev("HEAD");
    let tags: Vec<String> = vec![
        format!("type commit\ntag t\ntagger {ident}\n\nm\n"),
        format!("object {head}\ntag t\ntagger {ident}\n\nm\n"),
        format!("object {head}\ntype commit\ntagger {ident}\n\nm\n"),
        format!("object {head}\ntype commit\ntag t\n\nm\n"),
        format!("object {head}\ntype commit\ntag t\ntagger {ident}\n\nm\n"),
    ];
    for body in tags {
        let id = f.hash_object("tag", body.as_bytes());
        f.same_sorted(&["fsck", "--no-reflogs"]);
        force_remove(&f.loose_path(&id));
    }
}

#[test]
fn fsck_notices_match_git() {
    let f = Fx::new("sha1");
    f.same_connectivity(&["fsck"]);
    f.write("a", "a\n");
    f.commit("one");
    f.g(&["checkout", "-q", "--detach"]);
    f.same_connectivity(&["fsck"]);
    f.g(&["switch", "-q", "-c", "other"]);
    f.g(&["branch", "-D", "main"]);
    f.same_connectivity(&["fsck"]);
}

#[test]
fn fsck_flags_a_damaged_pack() {
    let f = build("sha1");
    f.g(&["gc", "-q"]);
    let pack = f.packs()[0].with_extension("pack");
    let mut bytes = fs::read(&pack).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    force_remove(&pack);
    fs::write(&pack, bytes).unwrap();
    let got = f.pyn(&["fsck"]);
    let want = f.oracle(&["fsck"]);
    assert_ne!(want.code, 0);
    assert_ne!(got.code, 0, "{}", got.err);
    assert!(
        got.code & 4 != 0 || got.code & 1 != 0,
        "status {}",
        got.code
    );
}

/// Makes the leftovers every maintenance command works on: a dangling commit, an orphan blob and stale reflogs.
fn leftovers(f: &Fx) {
    f.hash_object("blob", b"orphan one\n");
    f.g(&["switch", "-q", "-c", "scratch"]);
    f.write("scratch.txt", "scratch\n");
    f.commit("scratch work");
    f.g(&["switch", "-q", "main"]);
    f.g(&["branch", "-q", "-D", "scratch"]);
    f.g(&["reflog", "expire", "--expire=all", "--all"]);
}

fn age(f: &Fx, id: &str, days: u32) {
    let path = f.loose_path(id);
    let status = Command::new("touch")
        .args(["-d", &format!("{days} days ago")])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn prune_removes_what_git_removes() {
    for format in FORMATS {
        let f = build(format);
        leftovers(&f);
        let old = f.hash_object("blob", b"old orphan\n");
        age(&f, &old, 30);
        let junk = f.objects_dir().join("info");
        fs::create_dir_all(&junk).unwrap();
        for args in [
            &["prune", "-n"][..],
            &["prune", "-n", "--expire", "2.weeks.ago"],
            &["prune", "-n", "-v", "--expire=now"],
            &["prune", "-n", "--expire=never"],
        ] {
            f.same_sorted(args);
        }
        let g = f.twin("git-side");
        let p = f.twin("pyn-side");
        let want = g.oracle(&["prune", "-v", "--expire", "2.weeks.ago"]);
        let got = p.pyn(&["prune", "-v", "--expire", "2.weeks.ago"]);
        assert_eq!(sorted(&got.out), sorted(&want.out), "{format}: {}", got.err);
        assert_eq!(got.code, want.code);
        assert_eq!(p.loose_ids(), g.loose_ids());
        assert!(p.loose_ids().len() < f.loose_ids().len());
        let want = g.oracle(&["prune"]);
        let got = p.pyn(&["prune"]);
        assert_eq!(got.code, want.code, "{}", got.err);
        assert_eq!(p.loose_ids(), g.loose_ids());
        assert_eq!(p.all_objects(), g.all_objects());
        p.assert_git_ok();
        // a head given on the command line keeps what only it reaches
        let orphan = f.hash_object("blob", b"orphan three\n");
        let kept = p.hash_object("blob", b"orphan three\n");
        assert_eq!(orphan, kept);
        let got = p.pyn(&["prune", &kept]);
        assert_eq!(got.code, 0, "{}", got.err);
        assert!(p.loose_path(&kept).exists());
        let got = p.pyn(&["prune"]);
        assert_eq!(got.code, 0, "{}", got.err);
        assert!(!p.loose_path(&kept).exists());
        p.assert_git_ok();
    }
}

#[test]
fn prune_keeps_what_recent_objects_need() {
    let f = build("sha1");
    let blob = f.hash_object("blob", b"referenced by a newer tree\n");
    let mut body = b"100644 x\0".to_vec();
    body.extend(
        (0..blob.len() / 2).map(|i| u8::from_str_radix(&blob[2 * i..2 * i + 2], 16).unwrap()),
    );
    let newer = f.hash_object("tree", &body);
    age(&f, &blob, 30);
    let got = f.pyn(&["prune", "--expire", "2.weeks.ago"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(f.loose_path(&blob).exists() && f.loose_path(&newer).exists());
    let got = f.pyn(&["prune", "--expire", "now"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(!f.loose_path(&blob).exists() && !f.loose_path(&newer).exists());
    f.assert_git_ok();
}

#[test]
fn prune_removes_stale_temporary_files() {
    let f = build("sha1");
    let tmp = f.objects_dir().join("pack/tmp_pack_stale");
    fs::write(&tmp, "partial").unwrap();
    let got = f.pyn(&["prune", "--expire", "now"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(!tmp.exists());
}

fn repack_cases() -> Vec<Vec<&'static str>> {
    vec![
        vec!["repack"],
        vec!["repack", "-d"],
        vec!["repack", "-q", "-a", "-d"],
        vec!["repack", "-a", "-d", "-l"],
        vec!["repack", "-A", "-d"],
        vec!["repack", "-a"],
        vec!["repack", "-a", "-d", "-f"],
        vec!["repack", "-a", "-d", "--window=0"],
        vec!["repack", "-a", "-d", "--window=20", "--depth=10"],
    ]
}

#[test]
fn repack_leaves_what_git_leaves_and_git_reads_it() {
    for format in FORMATS {
        let base = build(format);
        leftovers(&base);
        for args in repack_cases() {
            // an earlier pack and some loose objects, so -d and -a have something to replace
            let start = base.twin("start");
            start.g(&["repack", "-q"]);
            start.write("later.txt", "later\n");
            start.commit("later commit");
            let g = start.twin("git-side");
            let p = start.twin("pyn-side");
            let want = g.oracle(&args);
            let got = p.pyn(&args);
            assert_eq!(got.code, want.code, "{format} {args:?}: {}", got.err);
            assert_eq!(got.out, want.out, "{format} {args:?}");
            assert_eq!(p.loose_ids(), g.loose_ids(), "{format} {args:?} loose");
            assert_eq!(p.packs().len(), g.packs().len(), "{format} {args:?} packs");
            assert_eq!(
                p.all_objects(),
                g.all_objects(),
                "{format} {args:?} objects"
            );
            p.assert_git_ok();
            fs::remove_dir_all(&start.repo).unwrap();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
    }
}

#[test]
fn repack_writes_deltas_git_can_read() {
    let f = build("sha1");
    let big: String = (0..4000)
        .map(|i| format!("row {i} of a long table\n"))
        .collect();
    for n in 0..8 {
        f.write("table.txt", &format!("{big}revision {n}\n"));
        f.commit(&format!("table {n}"));
    }
    let g = f.twin("git-side");
    let p = f.twin("pyn-side");
    g.g(&["repack", "-q", "-a", "-d", "-f"]);
    let got = p.pyn(&["repack", "-a", "-d", "-f"]);
    assert_eq!(got.code, 0, "{}", got.err);
    p.assert_git_ok();
    assert!(p.deltas() > 0, "no deltas written");
    assert!(
        p.pack_bytes() < g.pack_bytes() * 2,
        "pyn {} bytes, git {} bytes",
        p.pack_bytes(),
        g.pack_bytes()
    );
    assert_eq!(p.all_objects(), g.all_objects());
    let loose = p.pyn(&["count-objects", "-v"]).out;
    assert!(loose.contains("count: 0"), "{loose}");
    let deltas = p.deltas();
    let reused = p.pyn(&["repack", "-a", "-d", "--window=0"]);
    assert_eq!(reused.code, 0);
    assert_eq!(p.deltas(), deltas, "existing deltas were not reused");
    let fresh = p.pyn(&["repack", "-a", "-d", "-f", "--window=0"]);
    assert_eq!(fresh.code, 0);
    assert_eq!(p.deltas(), 0, "-f kept old deltas");
    p.assert_git_ok();
}

#[test]
fn repack_keeps_the_deltas_git_wrote() {
    for format in FORMATS {
        let f = build(format);
        let big: String = (0..4000)
            .map(|i| format!("row {i} of a long table\n"))
            .collect();
        for n in 0..8 {
            f.write("table.txt", &format!("{big}revision {n}\n"));
            f.commit(&format!("table {n}"));
        }
        f.g(&["repack", "-q", "-a", "-d", "-f", "--window=50"]);
        let g = f.twin("git-side");
        let p = f.twin("pyn-side");
        g.g(&["repack", "-q", "-a", "-d", "--window=0"]);
        let got = p.pyn(&["repack", "-a", "-d", "--window=0"]);
        assert_eq!(got.code, 0, "{}", got.err);
        assert!(g.deltas() > 0);
        assert_eq!(p.deltas(), g.deltas(), "{format}");
        p.assert_git_ok();
        let sizes = p.oracle(&[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objectsize:disk)",
        ]);
        assert_eq!(sizes.code, 0, "{format}: {}", sizes.err);
        assert_eq!(sizes.out.lines().count(), p.all_objects().len());
    }
}

#[test]
fn repack_writes_the_cruft_pack_git_reads() {
    for format in FORMATS {
        let base = build(format);
        leftovers(&base);
        let old = base.hash_object("blob", b"ancient orphan\n");
        age(&base, &old, 30);
        base.g(&["repack", "-q", "-a", "-d"]);
        let recent = base.hash_object("blob", b"recent orphan\n");
        for args in [
            &["repack", "-a", "-d", "--cruft", "-q"][..],
            &[
                "repack",
                "-a",
                "-d",
                "--cruft",
                "--cruft-expiration=2.weeks.ago",
                "-q",
            ],
            &["repack", "-a", "-d", "-k", "-q"],
        ] {
            let g = base.twin("git-side");
            let p = base.twin("pyn-side");
            g.oracle(args);
            let got = p.pyn(args);
            assert_eq!(got.code, 0, "{format} {args:?}: {}", got.err);
            assert_eq!(p.all_objects(), g.all_objects(), "{format} {args:?}");
            assert_eq!(p.packs().len(), g.packs().len(), "{format} {args:?}");
            assert_eq!(
                p.loose_ids().is_empty(),
                g.loose_ids().is_empty(),
                "{format} {args:?}"
            );
            let mtimes = |x: &Fx| {
                x.packs()
                    .iter()
                    .filter(|i| i.with_extension("mtimes").exists())
                    .count()
            };
            assert_eq!(mtimes(&p), mtimes(&g), "{format} {args:?}");
            p.assert_git_ok();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
        assert!(base.loose_path(&recent).exists());
    }
}

#[test]
fn gc_packs_unreachable_objects_into_a_cruft_pack_by_default() {
    for format in FORMATS {
        let base = build(format);
        leftovers(&base);
        for args in [
            &["gc", "-q"][..],
            &["gc", "-q", "--no-cruft"],
            &["gc", "-q", "--prune=now"],
        ] {
            let g = base.twin("git-side");
            let p = base.twin("pyn-side");
            g.oracle(args);
            let got = p.pyn(args);
            assert_eq!(got.code, 0, "{format} {args:?}: {}", got.err);
            assert_eq!(p.all_objects(), g.all_objects(), "{format} {args:?}");
            assert_eq!(
                p.loose_ids().len(),
                g.loose_ids().len(),
                "{format} {args:?}"
            );
            assert_eq!(p.packs().len(), g.packs().len(), "{format} {args:?}");
            p.assert_git_ok();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
    }
}

#[test]
fn repack_loosens_unreachable_objects_with_a_capital_a() {
    let f = build("sha1");
    f.g(&["switch", "-q", "-c", "scratch"]);
    f.write("scratch.txt", "scratch\n");
    f.commit("scratch work");
    f.g(&["switch", "-q", "main"]);
    f.g(&["repack", "-q", "-a", "-d"]);
    f.g(&["branch", "-q", "-D", "scratch"]);
    f.g(&["reflog", "expire", "--expire=all", "--all"]);
    assert!(f.loose_ids().is_empty());
    let g = f.twin("git-side");
    let p = f.twin("pyn-side");
    g.g(&["repack", "-q", "-A", "-d"]);
    let got = p.pyn(&["repack", "-A", "-d"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert_eq!(p.loose_ids(), g.loose_ids());
    assert!(!p.loose_ids().is_empty());
    assert_eq!(p.all_objects(), g.all_objects());
    p.assert_git_ok();
    let dropped = f.twin("drop");
    let got = dropped.pyn(&["repack", "-a", "-d"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(dropped.loose_ids().is_empty());
    assert!(dropped.all_objects().len() < f.all_objects().len());
    dropped.assert_git_ok();
}

#[test]
fn repack_keeps_packs_marked_keep() {
    let f = build("sha1");
    f.g(&["repack", "-q", "-a", "-d"]);
    let pack = f.packs()[0].clone();
    fs::write(pack.with_extension("keep"), "").unwrap();
    f.write("more.txt", "more\n");
    f.commit("more");
    let got = f.pyn(&["repack", "-a", "-d"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(pack.exists(), "the kept pack went away");
    assert_eq!(f.packs().len(), 2);
    f.assert_git_ok();
}

#[test]
fn repack_local_leaves_alternate_objects_out() {
    let f = build("sha1");
    let clone = f.root.join("clone");
    let status = Command::new("git")
        .args(["clone", "-q", "--shared"])
        .arg(&f.repo)
        .arg(&clone)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    let c = Fx {
        _tmp: tempdir().unwrap(),
        root: f.root.clone(),
        repo: clone,
        format: "sha1",
        tick: Cell::new(0),
    };
    assert_eq!(c.pyn(&["repack", "-a", "-d", "-l"]).code, 0);
    assert!(c.packs().is_empty(), "local repack copied borrowed objects");
    assert_eq!(c.pyn(&["repack", "-a", "-d"]).code, 0);
    assert_eq!(c.packs().len(), 1);
    c.assert_git_ok();
}

#[test]
fn repack_reports_when_nothing_is_new() {
    let f = build("sha1");
    f.g(&["repack", "-q", "-a", "-d"]);
    f.same(&["repack"]);
    f.same(&["repack", "-q"]);
}

fn loose_refs(f: &Fx) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![f.repo.join(".git/refs")];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.insert(
                    e.path()
                        .strip_prefix(&f.repo)
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
    }
    out
}

fn refs_fixture(format: &'static str) -> Fx {
    let f = build(format);
    f.g(&["tag", "-a", "-m", "second", "v2"]);
    f.g(&["branch", "topic"]);
    f.g(&["branch", "deep/er/topic"]);
    f.g(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.g(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    f
}

#[test]
fn pack_refs_writes_the_file_git_writes() {
    for format in FORMATS {
        let base = refs_fixture(format);
        for args in [
            &["pack-refs"][..],
            &["pack-refs", "--all"],
            &["pack-refs", "--all", "--no-prune"],
            &["pack-refs", "--no-prune"],
        ] {
            let g = base.twin("git-side");
            let p = base.twin("pyn-side");
            let want = g.oracle(args);
            let got = p.pyn(args);
            assert_eq!(got.code, want.code, "{format} {args:?}: {}", got.err);
            assert_eq!(got.out, want.out);
            assert_eq!(
                fs::read(p.repo.join(".git/packed-refs")).unwrap(),
                fs::read(g.repo.join(".git/packed-refs")).unwrap(),
                "{format} {args:?} packed-refs"
            );
            assert_eq!(
                loose_refs(&p),
                loose_refs(&g),
                "{format} {args:?} loose refs"
            );
            for name in ["main", "topic", "deep/er/topic"] {
                let r = format!("refs/heads/{name}");
                assert_eq!(p.rev(&r), g.rev(&r));
            }
            p.assert_git_ok();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
    }
}

#[test]
fn pack_refs_keeps_ref_updates_working() {
    let f = refs_fixture("sha1");
    assert_eq!(f.pyn(&["pack-refs", "--all"]).code, 0);
    f.write("after.txt", "after\n");
    f.commit("after packing");
    assert_eq!(f.pyn(&["branch", "-d", "topic"]).code, 0);
    assert_eq!(f.pyn(&["tag", "-d", "v1"]).code, 0);
    assert_eq!(f.pyn(&["pack-refs", "--all"]).code, 0);
    f.assert_git_ok();
    let listed = f.g_ro(&["for-each-ref", "--format=%(refname)"]);
    assert!(!listed.contains("refs/heads/topic") && !listed.contains("refs/tags/v1\n"));
    assert!(listed.contains("refs/heads/main"));
}

#[test]
fn gc_results_match_git() {
    for format in FORMATS {
        let base = build(format);
        leftovers(&base);
        let old = base.hash_object("blob", b"old orphan\n");
        age(&base, &old, 30);
        for args in [
            &["gc"][..],
            &["gc", "--prune=now"],
            &["gc", "--no-prune"],
            &["gc", "--aggressive", "--prune=now"],
            &["gc", "-q", "--prune=2.weeks.ago"],
        ] {
            let g = base.twin("git-side");
            let p = base.twin("pyn-side");
            let want = g.oracle(args);
            let got = p.pyn(args);
            assert_eq!(got.code, want.code, "{format} {args:?}: {}", got.err);
            assert_eq!(got.out, want.out, "{format} {args:?}");
            assert_eq!(got.err, want.err, "{format} {args:?}");
            assert_eq!(
                p.all_objects(),
                g.all_objects(),
                "{format} {args:?} objects"
            );
            assert_eq!(
                fs::read(p.repo.join(".git/packed-refs")).unwrap(),
                fs::read(g.repo.join(".git/packed-refs")).unwrap(),
                "{format} {args:?} packed-refs"
            );
            assert_eq!(loose_refs(&p), loose_refs(&g));
            let live = p.g_ro(&["rev-list", "--objects", "--all"]);
            for line in live.lines() {
                let id = line.split_whitespace().next().unwrap();
                assert!(
                    !p.loose_path(id).exists(),
                    "{format} {args:?}: {id} still loose"
                );
            }
            p.assert_git_ok();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
    }
}

#[test]
fn gc_reads_the_cruft_pack_git_writes() {
    let f = build("sha1");
    leftovers(&f);
    f.g(&["gc", "-q"]);
    assert!(
        f.packs()
            .iter()
            .any(|p| p.with_extension("mtimes").exists())
    );
    let before = f.all_objects();
    assert_eq!(f.pyn(&["gc", "-q"]).code, 0);
    assert_eq!(
        f.all_objects(),
        before,
        "recent unreachable objects survive"
    );
    f.assert_git_ok();
    assert_eq!(f.pyn(&["gc", "--prune=now"]).code, 0);
    assert!(f.all_objects().len() < before.len());
    f.assert_git_ok();
}

#[test]
fn gc_auto_follows_the_thresholds() {
    let f = build("sha1");
    leftovers(&f);
    let before = f.loose_ids();
    // far below the default limits: nothing happens
    let got = f.pyn(&["gc", "--auto"]);
    assert_eq!((got.code, got.out.as_str(), got.err.as_str()), (0, "", ""));
    assert_eq!(f.loose_ids(), before);
    // enough loose objects in the sampled folder
    f.g(&["config", "gc.auto", "1"]);
    let make_17 = |x: &Fx| {
        // add loose objects until two land in the 17 folder
        let mut found = 0;
        for n in 0..8000 {
            let id = x.hash_object("blob", format!("filler {n}\n").as_bytes());
            if id.starts_with("17") {
                found += 1;
                if found == 2 {
                    return;
                }
            }
        }
        panic!("no objects hashed into 17");
    };
    make_17(&f);
    let g = f.twin("git-side");
    let p = f.twin("pyn-side");
    let want = g.oracle_auto(&["-c", "gc.autoDetach=false", "gc", "--auto"]);
    let got = p.pyn(&["gc", "--auto"]);
    assert_eq!(got.code, want.code, "{}", got.err);
    assert!(want.err.contains(&got.err), "{} / {}", got.err, want.err);
    assert!(got.err.starts_with("Auto packing"), "{}", got.err);
    assert_eq!(p.all_objects(), g.all_objects());
    assert_ne!(p.loose_ids(), f.loose_ids(), "pyn did not pack");
    assert_ne!(g.loose_ids(), f.loose_ids(), "git did not pack");
    assert_eq!(p.loose_ids().is_empty(), g.loose_ids().is_empty());
    p.assert_git_ok();
    // gc.auto = 0 turns it off
    let off = f.twin("off");
    off.g(&["config", "gc.auto", "0"]);
    let got = off.pyn(&["gc", "--auto"]);
    assert_eq!((got.code, got.err.as_str()), (0, ""));
    assert_eq!(off.loose_ids(), f.loose_ids());
}

#[test]
fn gc_auto_packs_when_there_are_too_many_packs() {
    let f = build("sha1");
    for n in 0..4 {
        f.write("more.txt", &format!("{n}\n"));
        f.commit(&format!("more {n}"));
        f.g(&["repack", "-q"]);
    }
    assert!(f.packs().len() > 3);
    f.g(&["config", "gc.autoPackLimit", "3"]);
    let got = f.pyn(&["gc", "--auto"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert_eq!(f.packs().len(), 1);
    f.assert_git_ok();
}

#[test]
fn commit_and_fetch_run_auto_gc() {
    let f = build("sha1");
    f.g(&["config", "gc.auto", "1"]);
    f.g(&["config", "user.name", "T"]);
    f.g(&["config", "user.email", "t@example.com"]);
    let mut found = 0;
    for n in 0..8000 {
        let id = f.hash_object("blob", format!("filler {n}\n").as_bytes());
        found += usize::from(id.starts_with("17"));
        if found == 2 {
            break;
        }
    }
    f.write("new.txt", "new\n");
    f.g(&["add", "new.txt"]);
    let got = f.pyn(&["commit", "-q", "-m", "triggers auto gc"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(!f.packs().is_empty(), "auto gc did not pack");
    f.assert_git_ok();
}

#[test]
fn maintenance_works_on_a_bare_repository() {
    let f = build("sha1");
    leftovers(&f);
    let bare = f.root.join("bare.git");
    let status = Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&f.repo)
        .arg(&bare)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    let b = Fx {
        _tmp: tempdir().unwrap(),
        root: f.root.clone(),
        repo: bare.clone(),
        format: "sha1",
        tick: Cell::new(0),
    };
    for args in [&["count-objects", "-v"][..], &["fsck"]] {
        b.same_sorted(args);
    }
    for args in [
        &["repack", "-a", "-d"][..],
        &["pack-refs", "--all"],
        &["gc"],
        &["prune"],
    ] {
        let got = b.pyn(args);
        assert_eq!(got.code, 0, "{args:?}: {}", got.err);
    }
    let full = b.oracle(&["fsck", "--full", "--strict"]);
    assert_eq!(full.code, 0, "{}{}", full.out, full.err);
    assert!(bare.join("packed-refs").exists());
}

fn fill_17(f: &Fx) {
    let mut found = 0;
    for n in 0..8000 {
        let id = f.hash_object("blob", format!("filler {n}\n").as_bytes());
        found += usize::from(id.starts_with("17"));
        if found == 2 {
            return;
        }
    }
    panic!("no objects hashed into 17");
}

#[test]
fn prune_packed_removes_what_git_removes() {
    for format in FORMATS {
        let base = build(format);
        base.g(&["repack", "-q"]);
        base.write("late.txt", "late\n");
        base.commit("late");
        base.g(&["repack", "-q", "-a"]);
        base.hash_object("blob", b"loose only\n");
        let g = base.twin("git-side");
        let p = base.twin("pyn-side");
        let want = g.oracle(&["prune-packed", "-n"]);
        let got = p.pyn(&["prune-packed", "-n"]);
        assert_eq!(sorted(&got.out), sorted(&want.out), "{format}");
        assert_eq!(got.code, want.code);
        assert_eq!(p.loose_ids(), base.loose_ids(), "dry run removed objects");
        g.oracle(&["prune-packed", "-q"]);
        let got = p.pyn(&["prune-packed", "-q"]);
        assert_eq!(got.code, 0, "{}", got.err);
        assert_eq!(p.loose_ids(), g.loose_ids(), "{format}");
        p.assert_git_ok();
        assert_eq!(p.pyn(&["prune-packed", "--bogus"]).code, 129);
    }
}

#[test]
fn pack_refs_include_and_exclude_match_git() {
    let base = build("sha1");
    base.g(&["tag", "t1"]);
    base.g(&["tag", "t2"]);
    base.g(&["tag", "u1"]);
    base.g(&["branch", "topic"]);
    base.g(&["branch", "other/deep"]);
    for args in [
        &[
            "pack-refs",
            "--include",
            "refs/tags/t*",
            "--exclude",
            "refs/tags/t2",
        ][..],
        &["pack-refs", "--all", "--exclude", "refs/heads/other/*"],
        &["pack-refs", "--include", "refs/heads/*"],
        &["pack-refs", "--exclude", "refs/tags/*"],
        &[
            "pack-refs",
            "--all",
            "--no-prune",
            "--exclude",
            "refs/tags/u?",
        ],
        &[
            "pack-refs",
            "--include",
            "refs/heads/[tm]*",
            "--include",
            "refs/tags/u*",
        ],
    ] {
        let g = base.twin("git-side");
        let p = base.twin("pyn-side");
        let want = g.oracle(args);
        let got = p.pyn(args);
        assert_eq!(got.code, want.code, "{args:?}: {}", got.err);
        assert_eq!(
            fs::read_to_string(p.repo.join(".git/packed-refs")).unwrap_or_default(),
            fs::read_to_string(g.repo.join(".git/packed-refs")).unwrap_or_default(),
            "{args:?}"
        );
        assert_eq!(loose_refs(&p), loose_refs(&g), "{args:?}");
        fs::remove_dir_all(&g.repo).unwrap();
        fs::remove_dir_all(&p.repo).unwrap();
    }
}

#[test]
fn gc_refuses_to_run_twice_at_once() {
    let f = build("sha1");
    let lock = f.repo.join(".git/gc.pid");
    fs::write(&lock, "12345 elsewhere\n").unwrap();
    let want = f.oracle(&["gc"]);
    let got = f.pyn(&["gc"]);
    assert_eq!(want.code, 128);
    assert_eq!((got.code, got.err.as_str()), (128, want.err.as_str()));
    assert!(lock.exists(), "the other gc's lock was removed");
    let got = f.pyn(&["gc", "--force"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(!lock.exists(), "gc left its lock behind");
    // a lock from this machine whose process is gone does not block
    let host = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    if !host.is_empty() {
        fs::write(&lock, format!("2000000000 {}\n", host.trim())).unwrap();
        let got = f.pyn(&["gc", "-q"]);
        assert_eq!(got.code, 0, "{}", got.err);
    }
}

#[test]
fn gc_auto_stops_at_a_recent_gc_log() {
    let f = build("sha1");
    f.g(&["config", "gc.auto", "1"]);
    fill_17(&f);
    let log = f.repo.join(".git/gc.log");
    fs::write(&log, "earlier failure\n").unwrap();
    let before = f.loose_ids();
    let got = f.pyn(&["gc", "--auto"]);
    assert_eq!(got.code, 0);
    assert!(
        got.err.contains("The last gc run reported the following"),
        "{}",
        got.err
    );
    assert!(got.err.contains("earlier failure"), "{}", got.err);
    assert_eq!(f.loose_ids(), before);
    let old = Command::new("touch")
        .args(["-d", "3 days ago"])
        .arg(&log)
        .status()
        .unwrap();
    assert!(old.success());
    let got = f.pyn(&["gc", "--auto"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert!(!log.exists());
    assert_ne!(f.loose_ids(), before);
    fs::write(&log, "again\n").unwrap();
    assert_eq!(f.pyn(&["gc", "-q"]).code, 0);
    assert!(!log.exists(), "a manual gc keeps the log");
}

#[test]
fn gc_keep_largest_pack_leaves_that_pack() {
    for format in FORMATS {
        let f = build(format);
        f.g(&["repack", "-q", "-a", "-d"]);
        let big = f.packs()[0].clone();
        f.write("more.txt", "more\n");
        f.commit("more");
        f.g(&["repack", "-q"]);
        assert_eq!(f.packs().len(), 2);
        let g = f.twin("git-side");
        let p = f.twin("pyn-side");
        g.oracle(&["gc", "-q", "--keep-largest-pack"]);
        let got = p.pyn(&["gc", "-q", "--keep-largest-pack"]);
        assert_eq!(got.code, 0, "{format}: {}", got.err);
        let name = big.file_name().unwrap().to_owned();
        assert!(
            p.repo.join(".git/objects/pack").join(&name).exists(),
            "{format}"
        );
        assert_eq!(p.packs().len(), g.packs().len(), "{format}");
        assert_eq!(p.all_objects(), g.all_objects());
        p.assert_git_ok();
    }
}

#[test]
fn pack_objects_and_unpack_objects_interoperate_with_git() {
    for format in FORMATS {
        let f = build(format);
        let listing = f.g_ro(&["rev-list", "--objects", "--all"]);
        let pack_with = |c: Command, input: &str, args: &[&str]| {
            let mut c = c;
            c.args(args)
                .current_dir(&f.repo)
                .env_clear()
                .env("PATH", path_env())
                .env("HOME", f.root.join("home"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = c.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let input = input.to_owned();
            let t =
                std::thread::spawn(move || std::io::Write::write_all(&mut stdin, input.as_bytes()));
            let out = child.wait_with_output().unwrap();
            let _ = t.join();
            out
        };
        // pyn's pack read by git
        let out = pack_with(
            Command::new(env!("CARGO_BIN_EXE_pyn")),
            &listing,
            &["pack-objects", "--stdout"],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let target = f.twin("target");
        let fed = {
            let mut c = Command::new("git");
            c.args(["unpack-objects", "-q"]).current_dir(&target.repo);
            c.env_clear().env("PATH", path_env()).stdin(Stdio::piped());
            let mut child = c.spawn().unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), &out.stdout).unwrap();
            child.wait().unwrap()
        };
        assert!(fed.success(), "{format}: git could not unpack pyn's pack");
        // revs on stdin, base-name output verified by git
        let rev = pack_with(
            Command::new(env!("CARGO_BIN_EXE_pyn")),
            "HEAD\n^HEAD~1\n",
            &["pack-objects", "--revs", ".git/objects/pack/extra"],
        );
        assert!(
            rev.status.success(),
            "{}",
            String::from_utf8_lossy(&rev.stderr)
        );
        let hash = String::from_utf8_lossy(&rev.stdout).trim().to_string();
        let packed = f.repo.join(format!(".git/objects/pack/extra-{hash}.pack"));
        assert!(packed.exists(), "{format}: no pack named {hash}");
        let v = f.oracle(&[
            "verify-pack",
            packed.with_extension("idx").to_str().unwrap(),
        ]);
        assert_eq!(v.code, 0, "{format}: {}", v.err);
        let want = f.oracle(&["rev-list", "--objects", "HEAD", "^HEAD~1"]);
        let want_ids: BTreeSet<String> = want
            .out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap().to_owned())
            .collect();
        let listed = f.oracle(&[
            "verify-pack",
            "-v",
            packed.with_extension("idx").to_str().unwrap(),
        ]);
        let hex_len = want_ids.iter().next().unwrap().len();
        let got_set: BTreeSet<String> = listed
            .out
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|t| t.len() == hex_len && t.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_owned)
            .collect();
        assert_eq!(got_set, want_ids, "{format}");
        // git's pack read by pyn unpack-objects
        let git_pack = {
            let o = pack_with(
                Command::new("git"),
                "",
                &["pack-objects", "--stdout", "--revs", "--all"],
            );
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            o.stdout
        };
        let empty = Fx::new(format);
        let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
        c.arg("unpack-objects")
            .current_dir(&empty.repo)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", empty.root.join("home"))
            .stdin(Stdio::piped());
        let mut child = c.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), &git_pack).unwrap();
        assert!(child.wait().unwrap().success(), "{format}");
        assert_eq!(empty.loose_ids().len(), f.all_objects().len(), "{format}");
        assert!(empty.packs().is_empty());
        let fsck = empty.oracle(&["fsck", "--no-dangling"]);
        assert_eq!(fsck.code, 0, "{format}: {}{}", fsck.out, fsck.err);
    }
}

#[test]
fn maintenance_registers_like_git() {
    let f = build("sha1");
    let global = f.root.join("home/.gitconfig");
    let local = f.repo.join(".git/config");
    let tail = |p: &Path| fs::read_to_string(p).unwrap_or_default();
    let with_global = |bin: Option<&str>, args: &[&str]| {
        let mut c = match bin {
            Some(_) => Command::new("git"),
            None => Command::new(env!("CARGO_BIN_EXE_pyn")),
        };
        c.args(args)
            .current_dir(&f.repo)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", f.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C");
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    };
    let oracle = |args: &[&str]| with_global(Some("git"), args);
    let pyn = |args: &[&str]| with_global(None, args);
    let want = oracle(&["maintenance", "register"]);
    assert_eq!(want.code, 0, "{}", want.err);
    let (want_global, want_local) = (tail(&global), tail(&local));
    assert!(want_global.contains("repo = "), "{want_global}");
    assert_eq!(oracle(&["maintenance", "unregister"]).code, 0);
    fs::write(
        &local,
        tail(&local).replace(
            "[maintenance]\n\tauto = false\n\tstrategy = incremental\n",
            "",
        ),
    )
    .unwrap();
    let got = pyn(&["maintenance", "register"]);
    assert_eq!(got.code, 0, "{}", got.err);
    assert_eq!(tail(&global), want_global);
    assert_eq!(tail(&local), want_local);
    assert_eq!(pyn(&["maintenance", "register"]).code, 0);
    assert_eq!(
        tail(&global),
        want_global,
        "registering twice duplicated the entry"
    );
    assert_eq!(pyn(&["maintenance", "unregister"]).code, 0);
    assert!(!tail(&global).contains("repo = "));
    let want = oracle(&["maintenance", "unregister"]);
    let got = pyn(&["maintenance", "unregister"]);
    assert_eq!((got.code, got.err), (want.code, want.err));
    assert_eq!(pyn(&["maintenance", "unregister", "--force"]).code, 0);
    assert_eq!(pyn(&["maintenance", "run", "--task=bogus"]).code, 129);
}

#[test]
fn maintenance_runs_the_tasks_git_runs() {
    for format in FORMATS {
        let base = build(format);
        leftovers(&base);
        base.g(&["tag", "t1"]);
        for task in ["gc", "loose-objects", "pack-refs"] {
            let g = base.twin("git-side");
            let p = base.twin("pyn-side");
            let arg = format!("--task={task}");
            let want = g.oracle(&["maintenance", "run", &arg]);
            let got = p.pyn(&["maintenance", "run", &arg]);
            assert_eq!(
                got.code, want.code,
                "{format} {task}: {} / {}",
                got.out, got.err
            );
            assert_eq!(p.all_objects(), g.all_objects(), "{format} {task}");
            if task != "gc" {
                assert_eq!(p.loose_ids().len(), g.loose_ids().len(), "{format} {task}");
            }
            assert_eq!(loose_refs(&p), loose_refs(&g), "{format} {task}");
            if task == "loose-objects" {
                assert!(
                    p.packs().iter().any(|x| x
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("loose-")),
                    "{format}: no loose- pack"
                );
            }
            p.assert_git_ok();
            fs::remove_dir_all(&g.repo).unwrap();
            fs::remove_dir_all(&p.repo).unwrap();
        }
    }
}

#[test]
fn merge_rebase_and_am_run_auto_gc() {
    let f = build("sha1");
    f.g(&["config", "user.name", "T"]);
    f.g(&["config", "user.email", "t@example.com"]);
    f.g(&["switch", "-q", "-c", "work", "main"]);
    f.write("work.txt", "work\n");
    f.commit("work");
    let patch = f.g(&["format-patch", "--stdout", "-1"]);
    f.g(&["switch", "-q", "main"]);
    f.g(&["config", "gc.auto", "1"]);
    let base = f.rev("main");
    for (n, args) in [vec!["merge", "work"], vec!["rebase", "work"], vec!["am"]]
        .into_iter()
        .enumerate()
    {
        f.g(&["switch", "-q", "main"]);
        f.g(&["reset", "-q", "--hard", &base]);
        f.oracle(&["branch", "-q", "-D", "side"]);
        f.g(&["gc", "-q", "--prune=now"]);
        if n == 1 {
            f.g(&["switch", "-q", "-c", "side", "main"]);
            f.write("side.txt", "side\n");
            f.commit("side");
        }
        if n == 2 {
            fs::write(f.root.join("p.mbox"), &patch).unwrap();
        }
        fill_17(&f);
        let mut full = args.clone();
        let mbox = f.root.join("p.mbox");
        if n == 2 {
            full.push(mbox.to_str().unwrap());
        }
        let got = f.pyn(&full);
        assert_eq!(got.code, 0, "{args:?}: {}", got.err);
        assert!(got.err.contains("Auto packing"), "{args:?}: {}", got.err);
        f.assert_git_ok();
    }
}
