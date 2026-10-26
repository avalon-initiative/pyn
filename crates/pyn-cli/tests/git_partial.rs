//! Partial clones over ssh, smart HTTP, `file://` and local paths: `pyn clone --filter` and `pyn fetch --filter`
//! against real git on twin clones, comparing config, refs, the objects each side holds and the ones it lacks, and the
//! output of the commands that fetch missing objects on demand. Real git is the oracle in these tests only; semantics
//! are compared, never message wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    _server: Option<Server>,
}

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| b'a' + ((i as u8).wrapping_mul(7).wrapping_add(seed)) % 26)
        .collect()
}

fn write(dir: &Path, path: &str, content: &[u8]) {
    let full = dir.join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, content).unwrap();
}

fn commit_all(dir: &Path, msg: &str) {
    run(dir, &["add", "-A"]);
    run(dir, &["commit", "-q", "-m", msg]);
}

impl Rig {
    /// Three commits on `main` touching blobs of several sizes in nested directories, and `feature` with one more.
    fn new(format: &str, via: Via, allow_filter: bool) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("seed");
        init(&work, format, false);
        write(&work, "small.txt", b"small\n");
        write(&work, "d/mid.txt", &bytes(1, 600));
        write(&work, "d/sub/deep.txt", b"deep needle\n");
        write(&work, "big.bin", &bytes(2, 5000));
        commit_all(&work, "c1");
        write(&work, "small.txt", b"small\nmore\n");
        write(&work, "d/sub/deeper/x.txt", b"x\n");
        commit_all(&work, "c2");
        write(&work, "big.bin", &bytes(3, 5200));
        write(&work, "d/mid.txt", &bytes(4, 650));
        commit_all(&work, "c3");
        run(&work, &["branch", "feature"]);
        run(&work, &["switch", "-q", "feature"]);
        write(&work, "feature.txt", b"feature\n");
        write(&work, "d/sub/deep.txt", b"deep needle\nfeature\n");
        commit_all(&work, "f1");
        run(&work, &["switch", "-q", "main"]);
        let bare = root.join("served.git");
        init(&bare, format, true);
        run(&work, &["push", "-q", bare.to_str().unwrap(), "--all"]);
        run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        if allow_filter {
            run(&bare, &["config", "uploadpack.allowFilter", "true"]);
        }
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
        let server = via.serve(&root);
        let url = via.url(&server, "served.git", &bare);
        Rig {
            shim: ssh_shim(&root, false),
            _tmp: tmp,
            root,
            bare,
            home,
            url,
            _server: server,
        }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Out {
        out_of(
            git(cwd)
                .args(args)
                .env("HOME", &self.home)
                .env("GIT_SSH_COMMAND", &self.shim)
                .env("GIT_SSH_VARIANT", "ssh")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap(),
        )
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        out_of(
            Command::new(env!("CARGO_BIN_EXE_pyn"))
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
                .unwrap(),
        )
    }

    fn side(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fresh(&self) {
        let _ = fs::remove_dir_all(self.root.join("git-side"));
        let _ = fs::remove_dir_all(self.root.join("pyn-side"));
    }

    /// Clones with real git into `git-side/c` and with pyn into `pyn-side/c`; both must agree.
    fn clone_both(&self, flags: &[&str]) -> (PathBuf, PathBuf) {
        self.fresh();
        let mut args = vec!["clone"];
        args.extend_from_slice(flags);
        args.extend(["--", self.url.as_str(), "c"]);
        let g = self.git(&self.side("git-side"), &args);
        let p = self.pyn(&self.side("pyn-side"), &args);
        assert_eq!(
            g.code, p.code,
            "{flags:?}\ngit: {}\npyn: {}",
            g.stderr, p.stderr
        );
        let dirs = (self.root.join("git-side/c"), self.root.join("pyn-side/c"));
        if p.code == 0 {
            self.same(&dirs.0, &dirs.1, &format!("clone {flags:?}"));
        }
        dirs
    }

    /// Runs the same git subcommand with git in `g` and pyn in `p`; the results must agree.
    fn both(&self, g: &Path, p: &Path, args: &[&str]) -> Out {
        let go = self.git(g, args);
        let po = self.pyn(p, args);
        let ctx = format!("{args:?}\ngit: {}\npyn: {}", go.stderr, po.stderr);
        assert_eq!(go.code, po.code, "{ctx}");
        assert_eq!(go.stdout, po.stdout, "{ctx}");
        po
    }

    fn fetch_both(&self, g: &Path, p: &Path, flags: &[&str]) -> Out {
        let mut args = vec!["fetch"];
        args.extend_from_slice(flags);
        let go = self.git(g, &args);
        let po = self.pyn(p, &args);
        let ctx = format!("{args:?}\ngit: {}\npyn: {}", go.stderr, po.stderr);
        assert_eq!(go.code, po.code, "{ctx}");
        if po.code == 0 {
            self.same(g, p, &format!("{args:?}"));
        }
        po
    }

    /// Every object a repository holds, as `id type`, sorted.
    fn held(&self, d: &Path) -> Vec<String> {
        let o = self.git(
            d,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname) %(objecttype)",
            ],
        );
        assert_eq!(o.code, 0, "{}", o.stderr);
        let mut v: Vec<String> = o.stdout.lines().map(str::to_owned).collect();
        v.sort();
        v
    }

    /// The objects reachable from refs that the repository lacks and expects its promisor remote to have.
    fn missing(&self, d: &Path) -> Vec<String> {
        let o = self.git(d, &["rev-list", "--objects", "--missing=print", "--all"]);
        assert_eq!(o.code, 0, "{}", o.stderr);
        let mut v: Vec<String> = o
            .stdout
            .lines()
            .filter(|l| l.starts_with('?'))
            .map(str::to_owned)
            .collect();
        v.sort();
        v
    }

    fn same(&self, g: &Path, p: &Path, ctx: &str) {
        for args in [
            &["for-each-ref", "--format=%(refname) %(objectname)"][..],
            &["rev-list", "--all"],
            &["config", "--local", "--list"],
        ] {
            let (a, b) = (self.git(g, args), self.git(p, args));
            assert_eq!(a.stdout, b.stdout, "{ctx}: {args:?}");
        }
        assert_eq!(self.held(g), self.held(p), "{ctx}: objects held");
        assert_eq!(self.missing(g), self.missing(p), "{ctx}: objects missing");
        let packs = |d: &Path| {
            fs::read_dir(d.join(".git/objects/pack")).is_ok_and(|rd| {
                rd.flatten()
                    .any(|e| e.path().extension().is_some_and(|x| x == "promisor"))
            })
        };
        assert_eq!(packs(g), packs(p), "{ctx}: promisor marks");
        fsck(p);
    }
}

fn has_http_client() -> bool {
    let out = Command::new("git").arg("--exec-path").output().unwrap();
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Path::new(&dir).join("git-remote-http").exists()
}

fn ways() -> Vec<(&'static str, Via)> {
    let mut v = vec![
        ("sha1", Via::Ssh),
        ("sha1", Via::FileUrl),
        ("sha256", Via::FileUrl),
    ];
    if has_http_client() {
        v.push(("sha1", Via::Http));
    }
    v
}

const FILTERS: [&str; 7] = [
    "--filter=blob:none",
    "--filter=blob:limit=1k",
    "--filter=blob:limit=0",
    "--filter=tree:0",
    "--filter=tree:1",
    "--filter=tree:2",
    "--filter=combine:blob:limit=1k+tree:3",
];

#[test]
fn clone_with_each_filter_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        for filter in FILTERS {
            for extra in [&[][..], &["--no-checkout"], &["--bare"]] {
                let mut flags = vec![filter];
                flags.extend_from_slice(extra);
                let (_, p) = rig.clone_both(&flags);
                assert!(p.exists(), "{flags:?}");
            }
        }
    }
}

#[test]
fn several_filters_combine_and_other_clone_flags_compose() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        for flags in [
            &["--filter=blob:limit=1k", "--filter=tree:2", "-n"][..],
            &["--filter=blob:none", "--depth=1"],
            &["--filter=blob:none", "--branch", "feature"],
            &[
                "--filter=blob:none",
                "--no-single-branch",
                "--depth=2",
                "-n",
            ],
            &["--filter=blob:none", "--single-branch", "--no-tags"],
            &["--filter=blob:none", "--origin", "up", "-n"],
        ] {
            rig.clone_both(flags);
        }
    }
}

#[test]
fn a_bad_filter_fails_before_anything_is_made() {
    let rig = Rig::new("sha1", Via::FileUrl, true);
    for spec in [
        "bogus",
        "blob:limit=",
        "blob:limit=1x",
        "tree:x",
        "combine:blob:none+bogus",
    ] {
        rig.fresh();
        let flag = format!("--filter={spec}");
        let args = ["clone", flag.as_str(), "--", rig.url.as_str(), "c"];
        let g = rig.git(&rig.side("git-side"), &args);
        let p = rig.pyn(&rig.side("pyn-side"), &args);
        assert_eq!(g.code, p.code, "{spec}\n{}\n{}", g.stderr, p.stderr);
        assert_ne!(p.code, 0, "{spec}");
        assert!(!rig.root.join("pyn-side/c").exists(), "{spec}");
    }
    let p = rig.pyn(
        &rig.side("pyn-side"),
        &[
            "clone",
            "--filter=sparse:oid=HEAD:x",
            "--",
            rig.url.as_str(),
            "c",
        ],
    );
    assert_ne!(p.code, 0);
    assert!(p.stderr.contains("not built"), "{}", p.stderr);
}

#[test]
fn a_server_without_filters_gives_a_full_clone_and_a_warning() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, false);
        rig.fresh();
        let args = ["clone", "--filter=blob:none", "--", rig.url.as_str(), "c"];
        let g = rig.git(&rig.side("git-side"), &args);
        let p = rig.pyn(&rig.side("pyn-side"), &args);
        assert_eq!(g.code, p.code, "{}\n{}", g.stderr, p.stderr);
        assert!(p.stderr.contains("filtering not recognized by server"));
        assert!(g.stderr.contains("filtering not recognized by server"));
        rig.same(
            &rig.root.join("git-side/c"),
            &rig.root.join("pyn-side/c"),
            "unfiltered",
        );
    }
}

#[test]
fn a_local_path_ignores_the_filter_but_records_it() {
    let rig = Rig::new("sha1", Via::Local, true);
    let (g, p) = rig.clone_both(&["--filter=blob:none"]);
    let o = rig.git(&p, &["config", "remote.origin.partialclonefilter"]);
    assert_eq!(o.stdout.trim(), "blob:none");
    assert_eq!(rig.pyn(&p, &["fsck"]).code, 0);
    assert_eq!(rig.pyn(&g, &["fsck"]).code, 0);
}

#[test]
fn commands_fetch_missing_objects_on_demand_like_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        for filter in ["--filter=blob:none", "--filter=tree:0"] {
            let (g, p) = rig.clone_both(&[filter, "--no-checkout"]);
            let before = rig.missing(&p);
            assert!(!before.is_empty(), "{filter}");
            for args in [
                &["show", "HEAD"][..],
                &["show", "HEAD~1", "--stat"],
                &["cat-file", "-p", "HEAD:d/mid.txt"],
                &["cat-file", "-s", "HEAD:big.bin"],
                &["diff", "HEAD~2", "HEAD"],
                &["diff", "--stat", "HEAD~1", "origin/feature"],
                &["log", "-p", "--", "d/mid.txt"],
                &["blame", "HEAD", "--", "small.txt"],
                &["grep", "needle", "origin/feature"],
                &["ls-tree", "-r", "-l", "HEAD"],
            ] {
                rig.both(&g, &p, args);
            }
            assert_eq!(rig.held(&g), rig.held(&p), "{filter}: objects held");
            assert!(rig.missing(&p).len() < before.len(), "{filter}");
            fsck(&p);
        }
    }
}

#[test]
fn checkout_merge_and_restore_use_the_remote_for_blobs() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        let (g, p) = rig.clone_both(&["--filter=blob:none"]);
        let args = ["merge", "origin/feature", "-m", "m"];
        let go = rig.git(&g, &args);
        let po = rig.pyn(&p, &args);
        assert_eq!(go.code, po.code, "{args:?}\n{}\n{}", go.stderr, po.stderr);
        for d in [&g, &p] {
            assert_eq!(
                fs::read_to_string(d.join("d/sub/deep.txt")).unwrap(),
                "deep needle\nfeature\n"
            );
        }
        rig.both(&g, &p, &["status", "--porcelain"]);
        rig.both(&g, &p, &["log", "--oneline", "--all"]);
        assert_eq!(rig.held(&g), rig.held(&p));
        fsck(&p);
    }
}

#[test]
fn fetch_uses_the_stored_filter_and_the_flags_change_it() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        let (g, p) = rig.clone_both(&["--filter=blob:limit=1k", "--no-checkout"]);
        let work = rig.root.join("seed");
        write(&work, "new/n.txt", &bytes(9, 3000));
        write(&work, "tiny.txt", b"t\n");
        commit_all(&work, "c4");
        run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
        rig.fetch_both(&g, &p, &[]);
        rig.fetch_both(&g, &p, &["--filter=blob:none"]);
        write(&work, "new/m.txt", &bytes(8, 2000));
        write(&work, "tiny2.txt", b"t2\n");
        commit_all(&work, "c5");
        run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
        rig.fetch_both(&g, &p, &[]);
        write(&work, "new/o.txt", &bytes(7, 2000));
        commit_all(&work, "c6");
        run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
        rig.fetch_both(&g, &p, &["--no-filter"]);
        rig.both(&g, &p, &["show", "origin/main", "--stat"]);
    }
}

#[test]
fn fetch_with_a_filter_turns_a_full_clone_into_a_partial_one() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        let (g, p) = rig.clone_both(&["--no-checkout"]);
        let work = rig.root.join("seed");
        write(&work, "later.bin", &bytes(5, 4000));
        commit_all(&work, "c4");
        run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
        rig.fetch_both(&g, &p, &["--filter=blob:none"]);
        assert_eq!(
            rig.git(&p, &["config", "remote.origin.promisor"]).stdout,
            "true\n"
        );
        rig.both(&g, &p, &["show", "origin/main", "--stat"]);
    }
}

#[test]
fn refetch_reapplies_the_filter() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        let (g, p) = rig.clone_both(&["--filter=blob:none", "--no-checkout"]);
        rig.fetch_both(&g, &p, &["--refetch"]);
        rig.fetch_both(&g, &p, &["--refetch", "--filter=tree:1"]);
    }
}

#[test]
fn partial_clones_made_by_git_work_in_pyn() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        for filter in [
            "--filter=blob:none",
            "--filter=blob:limit=1k",
            "--filter=tree:0",
        ] {
            rig.fresh();
            let made = rig.git(
                &rig.side("git-side"),
                &["clone", filter, "--", rig.url.as_str(), "c"],
            );
            assert_eq!(made.code, 0, "{}", made.stderr);
            let g = rig.root.join("git-side/c");
            let twin = rig.side("twin");
            let _ = fs::remove_dir_all(twin.join("c"));
            assert_eq!(
                rig.git(&twin, &["clone", filter, "--", rig.url.as_str(), "c"])
                    .code,
                0
            );
            let t = twin.join("c");
            for args in [
                &["show", "HEAD", "--stat"][..],
                &["diff", "HEAD~2", "HEAD"],
                &["log", "-p", "-n", "2"],
                &["cat-file", "-p", "HEAD:big.bin"],
            ] {
                rig.both(&t, &g, args);
            }
            let sw = ["checkout", "-b", "feature", "origin/feature"];
            assert_eq!((rig.git(&t, &sw).code, rig.pyn(&g, &sw).code), (0, 0));
            rig.both(&t, &g, &["status", "--porcelain"]);
            assert_eq!(rig.held(&t), rig.held(&g), "{filter}");
            let work = rig.root.join("seed");
            write(&work, "after.txt", b"after\n");
            commit_all(&work, "after");
            run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
            let go = rig.git(&t, &["fetch"]);
            let po = rig.pyn(&g, &["fetch"]);
            assert_eq!((go.code, po.code), (0, 0), "{}", po.stderr);
            let (ht, hg) = (rig.held(&t), rig.held(&g));
            let only_t: Vec<_> = ht.iter().filter(|x| !hg.contains(x)).collect();
            let only_g: Vec<_> = hg.iter().filter(|x| !ht.contains(x)).collect();
            assert!(
                only_t.is_empty() && only_g.is_empty(),
                "{filter}: after fetch git-only {only_t:?} pyn-only {only_g:?}"
            );
            assert_eq!(rig.missing(&t), rig.missing(&g), "{filter}: missing");
            run(&work, &["reset", "-q", "--hard", "HEAD~1"]);
            run(
                &work,
                &["push", "-q", "-f", rig.bare.to_str().unwrap(), "main"],
            );
        }
    }
}

#[test]
fn fsck_gc_and_repack_treat_promised_objects_as_present() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via, true);
        for filter in ["--filter=blob:none", "--filter=tree:1"] {
            let (g, p) = rig.clone_both(&[filter, "--no-checkout"]);
            let want = rig.missing(&p);
            let fsck = rig.pyn(&p, &["fsck"]);
            assert_eq!(fsck.code, 0, "{filter}: {}{}", fsck.stdout, fsck.stderr);
            assert_eq!(rig.git(&g, &["fsck"]).code, 0);
            for args in [&["gc"][..], &["repack", "-a", "-d"], &["prune"]] {
                let o = rig.pyn(&p, args);
                assert_eq!(o.code, 0, "{filter} {args:?}: {}", o.stderr);
                assert_eq!(rig.missing(&p), want, "{filter} {args:?}");
                assert_eq!(rig.pyn(&p, &["fsck"]).code, 0, "{filter} {args:?}");
                assert_eq!(rig.git(&p, &["fsck"]).code, 0, "{filter} {args:?}");
            }
            let packs: Vec<_> = fs::read_dir(p.join(".git/objects/pack"))
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
                .collect();
            assert!(!packs.is_empty());
            for e in packs {
                assert!(e.path().with_extension("promisor").exists(), "{filter}");
            }
            let shown = rig.git(&p, &["show", "origin/main:small.txt"]);
            assert_eq!(shown.code, 0, "{}", shown.stderr);
        }
    }
}
