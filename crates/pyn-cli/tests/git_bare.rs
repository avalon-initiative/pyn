//! Bare repositories, mirrors and separate git directories on a git home, compared with real git on the same
//! operations. Real git is the oracle in these tests only; semantics are compared (refs, config values, exit
//! statuses, listings), never message wording.

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
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

impl Rig {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n",
        )
        .unwrap();
        Rig {
            _tmp: tmp,
            root,
            home,
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
        c.current_dir(cwd).args(args);
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

    /// A work repository with two commits, a branch, a tag and a note ref.
    fn source(&self) -> PathBuf {
        let src = self.root.join("src");
        fs::create_dir_all(&src).unwrap();
        self.git_ok(&src, &["init", "-q", "-b", "main"]);
        fs::write(src.join("a.txt"), "one\n").unwrap();
        self.git_ok(&src, &["add", "."]);
        self.git_ok(&src, &["commit", "-q", "-m", "one"]);
        self.git_ok(&src, &["tag", "v1"]);
        self.git_ok(&src, &["branch", "side"]);
        self.git_ok(&src, &["update-ref", "refs/notes/n", "HEAD"]);
        fs::write(src.join("a.txt"), "one\ntwo\n").unwrap();
        self.git_ok(&src, &["commit", "-q", "-a", "-m", "two"]);
        src
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

    fn config(&self, repo: &Path, key: &str) -> Vec<String> {
        let o = self.git(repo, &["config", "--local", "--get-all", key]);
        o.stdout.lines().map(str::to_owned).collect()
    }
}

#[test]
fn init_bare_makes_what_git_makes() {
    let rig = Rig::new();
    let (p, g) = (rig.root.join("p.git"), rig.root.join("g.git"));
    rig.pyn_ok(&rig.root, &["init-git", "--bare", "p.git"]);
    rig.git_ok(&rig.root, &["init", "-q", "--bare", "g.git"]);
    for repo in [&p, &g] {
        assert_eq!(
            rig.git_ok(repo, &["rev-parse", "--is-bare-repository"]),
            "true\n"
        );
        assert_eq!(rig.config(repo, "core.bare"), ["true"]);
        assert!(rig.config(repo, "core.logallrefupdates").is_empty());
        assert!(!repo.join(".git").exists());
        for d in ["objects", "refs/heads", "refs/tags"] {
            assert!(repo.join(d).is_dir(), "{d}");
        }
    }
    assert!(!p.join(".pyn").exists());
}

#[test]
fn init_bare_in_the_current_folder_and_in_sha256() {
    let rig = Rig::new();
    let here = rig.root.join("here");
    fs::create_dir_all(&here).unwrap();
    rig.pyn_ok(&here, &["init-git", "--bare", "--object-format", "sha256"]);
    assert_eq!(
        rig.git_ok(&here, &["rev-parse", "--show-object-format"]),
        "sha256\n"
    );
    assert_eq!(rig.config(&here, "core.bare"), ["true"]);
}

#[test]
fn init_with_a_separate_git_dir_leaves_a_gitdir_file() {
    let rig = Rig::new();
    let work = rig.root.join("work");
    rig.pyn_ok(&rig.root, &["init-git", "--separate-git-dir", "g", "work"]);
    let g = rig.root.join("g");
    assert_eq!(
        fs::read_to_string(work.join(".git")).unwrap(),
        format!("gitdir: {}\n", g.display())
    );
    assert!(g.join("HEAD").is_file() && !work.join(".git").is_dir());
    assert_eq!(rig.config(&g, "core.bare"), ["false"]);
    fs::write(work.join("f.txt"), "hi\n").unwrap();
    rig.pyn_ok(&work, &["add", "f.txt"]);
    rig.pyn_ok(&work, &["commit", "-m", "first"]);
    assert_eq!(rig.git_ok(&work, &["log", "--format=%s"]), "first\n");
    assert_eq!(rig.git_ok(&work, &["status", "--short"]), "");
    assert_eq!(rig.pyn_ok(&work, &["log", "--format=%s"]), "first\n");
}

#[test]
fn separate_git_dir_conflicts_and_bad_parents_fail_as_git_does() {
    let rig = Rig::new();
    for args in [
        vec!["--bare", "--separate-git-dir", "g", "w"],
        vec!["--separate-git-dir", "missing/g", "w"],
    ] {
        let mut p = vec!["init-git"];
        p.extend(&args);
        let mut g = vec!["init"];
        g.extend(&args);
        let (po, go) = (rig.pyn(&rig.root, &p), rig.git(&rig.root, &g));
        assert_eq!((po.code, go.code), (128, 128), "{args:?}: {}", po.stderr);
        assert!(!rig.root.join("g").exists() && !rig.root.join("missing").exists());
    }
}

#[test]
fn clone_bare_and_mirror_match_git() {
    let rig = Rig::new();
    let src = rig.source();
    let url = src.to_str().unwrap();
    for flags in [vec!["--bare"], vec!["--mirror"]] {
        let (pd, gd) = (format!("p{}.git", flags[0]), format!("g{}.git", flags[0]));
        let mut p = vec!["clone"];
        p.extend(&flags);
        let mut g = p.clone();
        p.extend([url, pd.as_str()]);
        g.extend([url, gd.as_str()]);
        let po = rig.pyn(&rig.root, &p);
        assert_eq!(po.code, 0, "{flags:?}: {}", po.stderr);
        rig.git_ok(&rig.root, &g);
        let (pr, gr) = (rig.root.join(&pd), rig.root.join(&gd));
        assert_eq!(rig.refs(&pr), rig.refs(&gr), "{flags:?}");
        for key in [
            "core.bare",
            "remote.origin.fetch",
            "remote.origin.mirror",
            "remote.origin.url",
        ] {
            assert_eq!(
                rig.config(&pr, key),
                rig.config(&gr, key),
                "{flags:?} {key}"
            );
        }
        assert_eq!(
            fs::read_to_string(pr.join("HEAD")).unwrap(),
            fs::read_to_string(gr.join("HEAD")).unwrap()
        );
        assert!(!pr.join("logs").exists() || !gr.join("logs").exists());
    }
}

#[test]
fn a_mirror_clone_carries_every_ref_and_fetches_the_new_ones() {
    let rig = Rig::new();
    let src = rig.source();
    rig.pyn_ok(
        &rig.root,
        &["clone", "--mirror", src.to_str().unwrap(), "m"],
    );
    let m = rig.root.join("m");
    let refs = rig.refs(&m);
    for name in ["refs/heads/side", "refs/notes/n", "refs/tags/v1"] {
        assert!(refs.contains_key(name), "{name}");
    }
    fs::write(src.join("b.txt"), "b\n").unwrap();
    rig.git_ok(&src, &["add", "."]);
    rig.git_ok(&src, &["commit", "-q", "-m", "three"]);
    rig.git_ok(&src, &["tag", "v2"]);
    rig.pyn_ok(&m, &["fetch"]);
    assert_eq!(rig.refs(&m), rig.refs(&src.join(".git")));
}

#[test]
fn clone_with_a_separate_git_dir_matches_git() {
    let rig = Rig::new();
    let src = rig.source();
    let url = src.to_str().unwrap();
    rig.pyn_ok(&rig.root, &["clone", "--separate-git-dir", "pg", url, "pw"]);
    rig.git_ok(
        &rig.root,
        &["clone", "-q", "--separate-git-dir", "gg", url, "gw"],
    );
    let (pw, gw) = (rig.root.join("pw"), rig.root.join("gw"));
    assert_eq!(
        fs::read_to_string(pw.join(".git")).unwrap(),
        format!("gitdir: {}\n", rig.root.join("pg").display())
    );
    assert_eq!(rig.refs(&pw), rig.refs(&gw));
    assert_eq!(
        fs::read(pw.join("a.txt")).unwrap(),
        fs::read(gw.join("a.txt")).unwrap()
    );
    assert_eq!(rig.git_ok(&pw, &["status", "--short"]), "");
    assert_eq!(rig.pyn_ok(&pw, &["log", "--format=%s"]), "two\none\n");
    for key in ["core.bare", "remote.origin.fetch", "branch.main.remote"] {
        assert_eq!(
            rig.config(&rig.root.join("pg"), key),
            rig.config(&rig.root.join("gg"), key),
            "{key}"
        );
    }
}

#[test]
fn clone_separate_git_dir_errors_leave_nothing_behind() {
    let rig = Rig::new();
    let src = rig.source();
    let url = src.to_str().unwrap();
    let cases: [&[&str]; 2] = [
        &["--bare", "--separate-git-dir", "g", url, "w"],
        &["--separate-git-dir", "missing/g", url, "w"],
    ];
    for args in cases {
        let mut p = vec!["clone"];
        p.extend(args);
        let po = rig.pyn(&rig.root, &p);
        let go = rig.git(&rig.root, &p);
        assert_eq!((po.code, go.code), (128, 128), "{args:?}: {}", po.stderr);
        assert!(!rig.root.join("w").exists() && !rig.root.join("g").exists());
    }
}

/// A bare clone of the source, made by pyn.
fn bare_clone(rig: &Rig) -> PathBuf {
    let src = rig.source();
    rig.pyn_ok(
        &rig.root,
        &["clone", "--bare", src.to_str().unwrap(), "b.git"],
    );
    rig.root.join("b.git")
}

#[test]
fn commands_that_need_a_work_tree_fail_as_git_does() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    let cases = [
        "status",
        "status -s",
        "add -A",
        "add a.txt",
        "commit -m x",
        "stash",
        "stash list",
        "rm a.txt",
        "reset --hard",
        "reset",
        "reset --mixed",
        "diff",
        "diff HEAD",
        "clean -n",
        "grep one",
        "merge side",
        "pull",
        "mv a.txt b.txt",
        "restore a.txt",
        "switch side",
        "rebase side",
        "cherry-pick side",
        "revert side",
        "am x",
        "ls-files -o",
    ];
    for c in cases {
        let args: Vec<&str> = c.split(' ').collect();
        let (po, go) = (rig.pyn(&b, &args), rig.git(&b, &args));
        assert_eq!(go.code, 128, "git {c}");
        assert_eq!(po.code, go.code, "{c}: {}", po.stderr);
        assert!(po.stdout.is_empty() && !po.stderr.is_empty(), "{c}");
    }
    assert!(!b.join("index").exists() && !b.join("refs/heads/side2").exists());
}

#[test]
fn the_reset_that_needs_no_work_tree_is_the_soft_one() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    let before = rig.refs(&b)["refs/heads/main"].clone();
    rig.pyn_ok(&b, &["reset", "--soft", "HEAD~1"]);
    assert_ne!(rig.refs(&b)["refs/heads/main"], before);
    assert_eq!(
        rig.refs(&b)["refs/heads/main"],
        rig.git_ok(&b, &["rev-parse", "HEAD"]).trim()
    );
    assert!(
        rig.pyn(&b, &["reset", "--soft", "HEAD", "--", "a.txt"])
            .code
            == 128
    );
}

#[test]
fn read_commands_work_in_a_bare_repository_like_git() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    let same: &[&[&str]] = &[
        &["log", "--oneline"],
        &["log", "--format=%H %s", "side"],
        &["show", "--stat", "--format=%s"],
        &["branch"],
        &["branch", "-v"],
        &["tag"],
        &["show-ref"],
        &["ls-tree", "HEAD"],
        &["ls-files"],
        &["cat-file", "-p", "HEAD:a.txt"],
        &["rev-parse", "HEAD", "main~1"],
        &["rev-parse", "--is-bare-repository"],
        &["rev-parse", "--is-inside-work-tree"],
        &["rev-parse", "--is-inside-git-dir"],
        &["rev-parse", "--git-dir"],
        &["diff", "side", "main"],
        &["diff", "--stat", "side..main"],
        &["diff", "--cached"],
        &["grep", "-e", "two", "HEAD"],
        &["blame", "a.txt"],
        &["shortlog", "-s", "HEAD"],
        &["describe", "--tags"],
        &["name-rev", "HEAD"],
    ];
    for args in same {
        let (po, go) = (rig.pyn(&b, args), rig.git(&b, args));
        assert_eq!(po.code, go.code, "{args:?}: {}", po.stderr);
        let norm = |s: &str| {
            s.lines()
                .map(|l| l.split(" (T ").next().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(norm(&po.stdout), norm(&go.stdout), "{args:?}");
    }
}

#[test]
fn a_bare_repository_is_found_from_a_folder_inside_it() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    let out = rig.pyn_ok(
        &b.join("refs/heads"),
        &["rev-parse", "--is-bare-repository"],
    );
    assert_eq!(out, "true\n");
    assert_eq!(
        rig.pyn_ok(&b.join("refs"), &["branch"]),
        rig.git_ok(&b, &["branch"])
    );
}

#[test]
fn branches_are_managed_and_bare_repositories_keep_no_reflog() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    rig.pyn_ok(&b, &["branch", "extra"]);
    rig.pyn_ok(&b, &["branch", "-m", "extra", "extra2"]);
    rig.pyn_ok(&b, &["tag", "t1"]);
    assert!(rig.refs(&b).contains_key("refs/heads/extra2"));
    rig.pyn_ok(&b, &["branch", "-d", "extra2"]);
    assert!(!rig.refs(&b).contains_key("refs/heads/extra2"));
    assert!(!b.join("logs").exists());
}

#[test]
fn bisect_in_a_bare_repository_checks_nothing_out() {
    let rig = Rig::new();
    let b = bare_clone(&rig);
    rig.pyn_ok(&b, &["bisect", "start", "main", "main~1"]);
    rig.pyn_ok(&b, &["bisect", "reset"]);
    assert_eq!(
        rig.git_ok(&b, &["symbolic-ref", "HEAD"]),
        "refs/heads/main\n"
    );
}

#[test]
fn remote_add_mirror_writes_what_git_writes() {
    let rig = Rig::new();
    let (p, g) = (rig.root.join("p"), rig.root.join("g"));
    for d in [&p, &g] {
        fs::create_dir_all(d).unwrap();
        rig.git_ok(d, &["init", "-q"]);
    }
    for (i, mode) in ["push", "fetch"].iter().enumerate() {
        let flag = format!("--mirror={mode}");
        let name = format!("r{i}");
        rig.pyn_ok(&p, &["remote", "add", &flag, &name, "../x"]);
        rig.git_ok(&g, &["remote", "add", &flag, &name, "../x"]);
    }
    rig.pyn_ok(&p, &["remote", "add", "--mirror", "plain", "../x"]);
    rig.git_ok(&g, &["remote", "add", "--mirror", "plain", "../x"]);
    for name in ["r0", "r1", "plain"] {
        for key in ["url", "fetch", "mirror"] {
            let k = format!("remote.{name}.{key}");
            assert_eq!(rig.config(&p, &k), rig.config(&g, &k), "{k}");
        }
    }
    let po = rig.pyn(&p, &["remote", "add", "--mirror=bogus", "bad", "../x"]);
    let go = rig.git(&g, &["remote", "add", "--mirror=bogus", "bad", "../x"]);
    assert_eq!((po.code, go.code), (129, 129));
    assert!(rig.config(&p, "remote.bad.url").is_empty());
}

#[test]
fn push_mirror_sends_every_ref_and_deletes_the_ones_that_are_gone() {
    let rig = Rig::new();
    let src = rig.source();
    for tool in ["pyn", "git"] {
        let dst = rig.root.join(format!("{tool}-dst.git"));
        rig.git_ok(&rig.root, &["init", "-q", "--bare", dst.to_str().unwrap()]);
        rig.git_ok(&src, &["push", "-q", dst.to_str().unwrap(), "main"]);
        rig.git_ok(&dst, &["update-ref", "refs/stale/x", "main"]);
        rig.git_ok(&dst, &["update-ref", "refs/heads/gone", "main"]);
        let args = ["push", "--mirror", dst.to_str().unwrap()];
        let o = if tool == "pyn" {
            rig.pyn(&src, &args)
        } else {
            rig.git(&src, &args)
        };
        assert_eq!(o.code, 0, "{tool}: {}", o.stderr);
    }
    let want = rig.refs(&rig.root.join("git-dst.git"));
    assert_eq!(rig.refs(&rig.root.join("pyn-dst.git")), want);
    assert!(!want.contains_key("refs/stale/x") && want.contains_key("refs/notes/n"));
    assert!(want.contains_key("refs/tags/v1") && want.contains_key("refs/heads/side"));
}

#[test]
fn a_mirror_remote_pushes_everything_and_refuses_refspecs() {
    let rig = Rig::new();
    let src = rig.source();
    let dst = rig.root.join("dst.git");
    rig.git_ok(&rig.root, &["init", "-q", "--bare", dst.to_str().unwrap()]);
    rig.pyn_ok(
        &src,
        &[
            "remote",
            "add",
            "--mirror=push",
            "mir",
            dst.to_str().unwrap(),
        ],
    );
    rig.pyn_ok(&src, &["push", "mir"]);
    assert_eq!(rig.refs(&dst), rig.refs(&src.join(".git")));
    let go = rig.git(&src, &["push", "mir", "main"]);
    let po = rig.pyn(&src, &["push", "mir", "main"]);
    assert_eq!((po.code, go.code), (128, 128), "{}", po.stderr);
}

#[test]
fn push_mirror_option_conflicts_exit_like_git() {
    let rig = Rig::new();
    let src = rig.source();
    let dst = rig.root.join("dst.git");
    rig.git_ok(&rig.root, &["init", "-q", "--bare", dst.to_str().unwrap()]);
    let d = dst.to_str().unwrap();
    let cases: [&[&str]; 4] = [
        &["push", "--mirror", "--all", d],
        &["push", "--mirror", "--tags", d],
        &["push", "--mirror", "--delete", d, "main"],
        &["push", "--mirror", d, "main"],
    ];
    for args in cases {
        let (po, go) = (rig.pyn(&src, args), rig.git(&src, args));
        assert_eq!((po.code, go.code), (128, 128), "{args:?}: {}", po.stderr);
    }
    assert!(rig.refs(&dst).is_empty());
}

#[test]
fn shallow_clones_compose_with_bare_mirror_and_separate_git_dir() {
    let rig = Rig::new();
    let src = rig.source();
    let url = format!("file://{}", src.display());
    let shallow = |dir: &Path| fs::read_to_string(dir.join("shallow")).ok();
    for flags in [
        vec!["--bare"],
        vec!["--mirror"],
        vec!["--separate-git-dir", "SEP"],
    ] {
        let tag = flags[0].trim_start_matches('-').to_owned();
        let sep = |who: &str| format!("{who}-{tag}-gitdir");
        let args: Vec<String> = flags
            .iter()
            .map(|f| {
                if *f == "SEP" {
                    sep("X")
                } else {
                    (*f).to_owned()
                }
            })
            .collect();
        let run_clone = |who: &str, dest: &str| {
            let mut a = vec!["clone".to_owned(), "--depth".into(), "1".into()];
            a.extend(args.iter().map(|x| x.replace("X-", &format!("{who}-"))));
            a.extend([url.clone(), dest.to_owned()]);
            a
        };
        let pa = run_clone("p", &format!("p-{tag}"));
        let ga = run_clone("g", &format!("g-{tag}"));
        let po = rig.pyn(
            &rig.root,
            &pa.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(po.code, 0, "{flags:?}: {}", po.stderr);
        rig.git_ok(
            &rig.root,
            &ga.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (pr, gr) = if flags[0] == "--separate-git-dir" {
            (rig.root.join(sep("p")), rig.root.join(sep("g")))
        } else {
            (
                rig.root.join(format!("p-{tag}")),
                rig.root.join(format!("g-{tag}")),
            )
        };
        assert!(shallow(&pr).is_some(), "{flags:?}: no shallow file");
        assert_eq!(shallow(&pr).is_some(), shallow(&gr).is_some());
        assert_eq!(rig.refs(&pr), rig.refs(&gr), "{flags:?}");
        let work = if flags[0] == "--separate-git-dir" {
            rig.root.join(format!("p-{tag}"))
        } else {
            pr.clone()
        };
        assert_eq!(
            rig.pyn_ok(&work, &["log", "--format=%H", "main"]),
            rig.git_ok(&work, &["log", "--format=%H", "main"]),
            "{flags:?}"
        );
    }
}

#[test]
fn a_shallow_bare_clone_can_be_deepened() {
    let rig = Rig::new();
    let src = rig.source();
    let url = format!("file://{}", src.display());
    rig.pyn_ok(
        &rig.root,
        &["clone", "--bare", "--depth", "1", &url, "s.git"],
    );
    let s = rig.root.join("s.git");
    assert_eq!(rig.pyn_ok(&s, &["log", "--format=%s", "main"]), "two\n");
    rig.pyn_ok(&s, &["fetch", "--unshallow"]);
    assert_eq!(
        rig.pyn_ok(&s, &["log", "--format=%s", "main"]),
        "two\none\n"
    );
    assert!(!s.join("shallow").exists());
}

/// A work repository cloned from `src` with a linked work tree `lw`, under `root/<name>`.
fn twin(rig: &Rig, src: &Path, name: &str) -> PathBuf {
    let dir = rig.root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let work = dir.join("work");
    rig.git_ok(
        &dir,
        &["clone", "-q", src.to_str().unwrap(), work.to_str().unwrap()],
    );
    rig.git_ok(&work, &["worktree", "add", "-q", "../lw", "-b", "lw"]);
    work
}

fn mentions(text: &str, root: &Path) -> String {
    text.replace(root.to_str().unwrap(), "<root>")
}

#[test]
fn init_separate_git_dir_moves_an_existing_git_directory_as_git_does() {
    let rig = Rig::new();
    let src = rig.source();
    for (dest, setup) in [("sep", ""), ("deep/er/sep", "mkdir"), ("empty", "empty")] {
        let (g, p) = (twin(&rig, &src, "g"), twin(&rig, &src, "p"));
        let (groot, proot) = (
            g.parent().unwrap().to_owned(),
            p.parent().unwrap().to_owned(),
        );
        for root in [&groot, &proot] {
            match setup {
                "mkdir" => fs::create_dir_all(root.join("deep/er")).unwrap(),
                "empty" => fs::create_dir_all(root.join(dest)).unwrap(),
                _ => {}
            }
        }
        let target = format!("../{dest}");
        let go = rig.git(&g, &["init", "--separate-git-dir", &target]);
        let po = rig.pyn(&p, &["init-git", "--separate-git-dir", &target]);
        assert_eq!((po.code, go.code), (0, 0), "{dest}: {}", po.stderr);
        assert_eq!(
            mentions(&po.stdout, &proot),
            mentions(&go.stdout, &groot),
            "{dest}"
        );
        for (root, work) in [(&groot, &g), (&proot, &p)] {
            let gitdir = root.join(dest);
            assert_eq!(
                fs::read_to_string(work.join(".git")).unwrap(),
                format!("gitdir: {}\n", gitdir.display())
            );
            assert!(gitdir.join("HEAD").is_file() && gitdir.join("worktrees/lw").is_dir());
            assert_eq!(
                fs::read_to_string(root.join("lw/.git")).unwrap(),
                format!("gitdir: {}\n", gitdir.join("worktrees/lw").display())
            );
            assert_eq!(rig.git_ok(work, &["status", "--short"]), "");
            assert_eq!(rig.git_ok(&root.join("lw"), &["status", "--short"]), "");
        }
        let listed = rig.pyn_ok(&p, &["worktree", "list"]);
        assert!(
            listed.contains("[main]") && listed.contains("[lw]"),
            "{listed}"
        );
        assert_eq!(
            fs::read_to_string(groot.join(dest).join("config")).unwrap(),
            fs::read_to_string(proot.join(dest).join("config")).unwrap()
        );
        fs::remove_dir_all(&groot).unwrap();
        fs::remove_dir_all(&proot).unwrap();
    }
}

#[test]
fn init_separate_git_dir_on_a_repository_refuses_as_git_does() {
    let rig = Rig::new();
    let src = rig.source();
    for args in [
        vec!["--separate-git-dir", "../full"],
        vec!["--separate-git-dir", "../missing/sep"],
        vec!["--separate-git-dir", "../file"],
        vec!["--object-format", "sha256", "--separate-git-dir", "../sep"],
        vec!["--separate-git-dir", ".git/inside"],
    ] {
        let (g, p) = (twin(&rig, &src, "g"), twin(&rig, &src, "p"));
        for work in [&g, &p] {
            let root = work.parent().unwrap();
            fs::create_dir_all(root.join("full")).unwrap();
            fs::write(root.join("full/x"), "x").unwrap();
            fs::write(root.join("file"), "x").unwrap();
        }
        let mut pa = vec!["init-git"];
        pa.extend(&args);
        let mut ga = vec!["init"];
        ga.extend(&args);
        let (po, go) = (rig.pyn(&p, &pa), rig.git(&g, &ga));
        assert_eq!((po.code, go.code), (128, 128), "{args:?}: {}", po.stderr);
        fs::remove_dir_all(g.parent().unwrap()).unwrap();
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}

#[test]
fn init_separate_git_dir_again_is_quiet_about_branches_and_keeps_core_worktree() {
    let rig = Rig::new();
    let src = rig.source();
    let (g, p) = (twin(&rig, &src, "g"), twin(&rig, &src, "p"));
    for (tool, work) in [("git", &g), ("pyn", &p)] {
        rig.git_ok(work, &["config", "core.worktree", work.to_str().unwrap()]);
        let args: Vec<&str> = if tool == "git" {
            vec!["init", "-q", "-b", "other", "--separate-git-dir", "../s"]
        } else {
            vec![
                "init-git",
                "-q",
                "-b",
                "other",
                "--separate-git-dir",
                "../s",
            ]
        };
        let o = if tool == "git" {
            rig.git(work, &args)
        } else {
            rig.pyn(work, &args)
        };
        assert_eq!(o.code, 0, "{tool}: {}", o.stderr);
        assert!(o.stdout.is_empty(), "{tool}: {}", o.stdout);
        assert!(
            o.stderr.contains("re-init: ignored --initial-branch=other"),
            "{tool}: {}",
            o.stderr
        );
        let s = work.parent().unwrap().join("s");
        assert_eq!(
            rig.config(&s, "core.worktree"),
            [work.to_str().unwrap().to_owned()]
        );
        assert_eq!(rig.git_ok(work, &["branch", "--show-current"]), "main\n");
        let again = if tool == "git" {
            rig.git(work, &["init", "-q", "--separate-git-dir", "../s"])
        } else {
            rig.pyn(work, &["init-git", "-q", "--separate-git-dir", "../s"])
        };
        assert_eq!(again.code, 0, "{tool}: {}", again.stderr);
    }
}

#[test]
fn log_paths_in_a_bare_repository_ignore_the_folder_it_runs_in() {
    let rig = Rig::new();
    let src = rig.source();
    let bare = rig.root.join("b.git");
    rig.git_ok(
        &rig.root,
        &["clone", "-q", "--bare", src.to_str().unwrap(), "b.git"],
    );
    for cwd in [bare.clone(), bare.join("refs"), bare.join("refs/heads")] {
        for args in [
            vec!["log", "--oneline", "--", "a.txt"],
            vec!["log", "--oneline", "-p", "--", "a.txt"],
            vec!["log", "--oneline", "--", "nonexistent"],
            vec!["log", "--oneline", "a.txt"],
            vec!["log", "-p", "a.txt"],
            vec!["log", "--oneline", "main", "--", "a.txt"],
            vec!["log", "--oneline", "--", "../a.txt"],
            vec!["log", "--oneline", "--", "../../a.txt"],
            vec!["log", "--oneline", "--", "/elsewhere/a.txt"],
        ] {
            let (p, g) = (rig.pyn(&cwd, &args), rig.git(&cwd, &args));
            let ctx = format!("{args:?} in {}", cwd.display());
            assert_eq!(p.code, g.code, "{ctx}: {}", p.stderr);
            assert_eq!(p.stdout, g.stdout, "{ctx}");
        }
    }
}
