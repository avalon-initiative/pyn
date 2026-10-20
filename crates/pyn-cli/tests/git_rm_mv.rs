//! `pyn rm` and `pyn mv` on a git repository, compared with the real `git rm` and `git mv` on a copy of the same
//! repository: the exit status, the index entries and modes, the work tree and what `status` says afterwards
//! (never the messages). Needs `git` on the PATH (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::copy_tree;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Env {
    home: PathBuf,
}

impl Env {
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git(cwd, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, text).unwrap();
}

fn set_exec(dir: &Path, name: &str, on: bool) {
    let file = dir.join(name);
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_mode(if on { 0o755 } else { 0o644 });
    fs::set_permissions(&file, perms).unwrap();
}

fn tree(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        if rel == ".git" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            out.insert(
                rel,
                format!("-> {}", fs::read_link(&path).unwrap().display()),
            );
        } else if meta.is_dir() {
            out.insert(rel.clone(), "dir".into());
            tree(root, &path, out);
        } else {
            let exec = meta.permissions().mode() & 0o111 != 0;
            out.insert(rel, format!("{exec} {:?}", fs::read(&path).unwrap()));
        }
    }
}

/// What `rm` and `mv` may change: the index entries, the work tree and what `status` reports about both.
fn state(env: &Env, dir: &Path) -> Vec<String> {
    let mut files = BTreeMap::new();
    tree(dir, dir, &mut files);
    vec![
        env.ok(dir, &["ls-files", "-s"]),
        format!("{files:?}"),
        env.ok(dir, &["status", "--porcelain=v1", "--ignored"]),
    ]
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
}

impl Pair {
    fn root() -> (tempfile::TempDir, Env, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        (tmp, Env { home }, git)
    }

    fn copied(tmp: tempfile::TempDir, env: Env, git: PathBuf) -> Self {
        let pyn = git.parent().unwrap().join("pyn");
        let _ = fs::remove_dir_all(&pyn);
        copy_tree(&git, &pyn);
        let _ = env.git(&git, &["update-index", "-q", "--refresh"]);
        Self {
            _tmp: tmp,
            env,
            git,
            pyn,
        }
    }

    /// Runs `<verb> <args>` in both from `sub`; the exit status and the resulting state must agree. Returns
    /// whether the command succeeded.
    fn same_in(&self, sub: &str, verb: &str, args: &[&str], label: &str) -> bool {
        let mut full = vec![verb];
        full.extend(args);
        let g = self.env.git(&self.git.join(sub), &full);
        let p = self.env.pyn(&self.pyn.join(sub), &full);
        let what = format!("{label}: {verb} {args:?} in '{sub}'");
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            String::from_utf8_lossy(&p.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        let (gs, ps) = (state(&self.env, &self.git), state(&self.env, &self.pyn));
        for (i, (a, b)) in ps.iter().zip(&gs).enumerate() {
            assert_eq!(a, b, "{what}: state part {i} differs (pyn, then git)");
        }
        let fsck = self.env.git(&self.pyn, &["fsck", "--strict"]);
        assert!(
            fsck.status.success(),
            "{what}: fsck: {}",
            String::from_utf8_lossy(&fsck.stderr)
        );
        g.status.success()
    }

    fn rm(&self, args: &[&str], label: &str) -> bool {
        self.same_in("", "rm", args, label)
    }

    fn mv(&self, args: &[&str], label: &str) -> bool {
        self.same_in("", "mv", args, label)
    }
}

fn init(env: &Env, dir: &Path, format: &str) {
    env.ok(
        dir,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
}

fn commit_all(env: &Env, dir: &Path, message: &str) {
    env.ok(dir, &["add", "-A"]);
    env.ok(dir, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// A committed repository with files, folders, an executable, a link, plus untracked and ignored files; `extra`
/// then changes the index and work tree before the copy for pyn is made.
fn repo(format: &str, extra: impl Fn(&Env, &Path)) -> Pair {
    let (tmp, env, git) = Pair::root();
    init(&env, &git, format);
    write(&git, ".gitignore", "*.log\n");
    write(&git, "a", "one\n");
    write(&git, "dir/b", "b1\n");
    write(&git, "dir/sub/c", "c1\n");
    write(&git, "other/d", "d1\n");
    write(&git, "run.sh", "#!/bin/sh\n");
    set_exec(&git, "run.sh", true);
    std::os::unix::fs::symlink("a", git.join("link")).unwrap();
    commit_all(&env, &git, "first");
    write(&git, "u", "untracked\n");
    write(&git, "x.log", "ignored\n");
    write(&git, "dir/u2", "untracked in dir\n");
    extra(&env, &git);
    Pair::copied(tmp, env, git)
}

fn clean(format: &str) -> Pair {
    repo(format, |_, _| {})
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

type Case = (&'static str, fn(&Env, &Path));

#[test]
fn rm_of_clean_files_and_folders() {
    for format in FORMATS {
        for args in [
            &["a"][..],
            &["dir/b"],
            &["dir/b", "other/d"],
            &["run.sh"],
            &["link"],
            &["--cached", "a"],
            &["-f", "a"],
            &["-n", "a"],
            &["-q", "a"],
            &["--", "a"],
            &["dir"],
            &["-r", "dir"],
            &["-r", "--cached", "dir"],
            &["-r", "-n", "dir"],
            &["-r", "dir/sub"],
            &["-r", "."],
            &["."],
            &["-r", "dir", "other"],
            &["a", "a"],
            &["-r", "dir", "dir/b"],
        ] {
            clean(format).rm(args, &format!("{format} clean"));
        }
    }
}

#[test]
fn rm_of_paths_that_are_not_tracked() {
    for format in FORMATS {
        for args in [
            &["u"][..],
            &["x.log"],
            &["nothing"],
            &["--ignore-unmatch", "nothing"],
            &["--ignore-unmatch", "u"],
            &["--ignore-unmatch", "a", "nothing"],
            &["a", "nothing"],
            &["-r", "dir/u2"],
            &["--cached", "u"],
        ] {
            clean(format).rm(args, &format!("{format} untracked"));
        }
    }
}

#[test]
fn rm_refuses_what_has_changes_unless_forced() {
    for format in FORMATS {
        for flag in [&[][..], &["--cached"], &["-f"], &["-f", "--cached"]] {
            let cases: [Case; 8] = [
                ("local edit", |_, g| write(g, "a", "edited\n")),
                ("staged edit", |e, g| {
                    write(g, "a", "staged\n");
                    e.ok(g, &["add", "a"]);
                }),
                ("staged and edited again", |e, g| {
                    write(g, "a", "staged\n");
                    e.ok(g, &["add", "a"]);
                    write(g, "a", "worse\n");
                }),
                ("staged then restored on disk", |e, g| {
                    write(g, "a", "staged\n");
                    e.ok(g, &["add", "a"]);
                    write(g, "a", "one\n");
                }),
                ("new file staged", |e, g| {
                    write(g, "new", "n\n");
                    e.ok(g, &["add", "new"]);
                }),
                ("mode change", |_, g| set_exec(g, "a", true)),
                ("deleted on disk", |_, g| {
                    fs::remove_file(g.join("a")).unwrap()
                }),
                ("staged mode change", |e, g| {
                    set_exec(g, "a", true);
                    e.ok(g, &["add", "a"]);
                }),
            ];
            for (name, make) in cases {
                for target in ["a", "new"] {
                    let p = repo(format, make);
                    let mut args = flag.to_vec();
                    args.push(target);
                    p.rm(&args, &format!("{format} {name} {flag:?}"));
                }
            }
        }
    }
}

#[test]
fn rm_over_a_folder_with_one_changed_file_removes_nothing() {
    for format in FORMATS {
        let p = repo(format, |_, g| write(g, "dir/sub/c", "edited\n"));
        assert!(!p.rm(&["-r", "dir"], "folder"));
        assert!(p.rm(&["-r", "-f", "dir"], "folder forced"));
        let p = repo(format, |_, g| write(g, "dir/sub/c", "edited\n"));
        assert!(!p.rm(&["-r", "dir", "other"], "two folders"));
        p.rm(&["-r", "--cached", "dir", "other"], "two folders cached");
    }
}

#[test]
fn rm_with_a_symlink_and_an_executable_changed() {
    for format in FORMATS {
        let make = |e: &Env, g: &Path| {
            fs::remove_file(g.join("link")).unwrap();
            std::os::unix::fs::symlink("elsewhere", g.join("link")).unwrap();
            fs::remove_file(g.join("run.sh")).unwrap();
            write(g, "run.sh", "#!/bin/sh\necho\n");
            set_exec(g, "run.sh", true);
            let _ = e;
        };
        for args in [&["link"][..], &["run.sh"], &["-f", "link", "run.sh"]] {
            repo(format, make).rm(args, &format!("{format} changed link and exec"));
        }
        let make = |_: &Env, g: &Path| {
            fs::remove_file(g.join("link")).unwrap();
            write(g, "link", "now a file\n");
        };
        repo(format, make).rm(&["link"], &format!("{format} link became a file"));
        let make = |_: &Env, g: &Path| {
            fs::remove_file(g.join("a")).unwrap();
            fs::create_dir(g.join("a")).unwrap();
        };
        repo(format, make).rm(&["a"], &format!("{format} file became a folder"));
    }
}

#[test]
fn rm_from_a_subfolder() {
    for format in FORMATS {
        for (sub, args) in [
            ("dir", &["b"][..]),
            ("dir", &["../a"]),
            ("dir", &["-r", "sub"]),
            ("dir", &["-r", "."]),
            ("dir/sub", &["c", "../b"]),
            ("dir", &["-r", ".."]),
            ("dir", &["sub"]),
        ] {
            clean(format).same_in(sub, "rm", args, &format!("{format} from {sub}"));
        }
    }
}

#[test]
fn rm_on_an_unborn_branch_and_with_intent_to_add() {
    for format in FORMATS {
        let make = |e: &Env, d: &Path| {
            init(e, d, format);
            write(d, "a", "one\n");
            e.ok(d, &["add", "a"]);
        };
        for args in [&["a"][..], &["-f", "a"], &["--cached", "a"]] {
            let (tmp, env, git) = Pair::root();
            make(&env, &git);
            Pair::copied(tmp, env, git).rm(args, &format!("{format} unborn"));
        }
        for args in [&["u"][..], &["--cached", "u"], &["-f", "u"]] {
            repo(format, |e, g| {
                e.ok(g, &["add", "-N", "u"]);
            })
            .rm(args, &format!("{format} intent to add"));
        }
    }
}

fn conflicted(format: &str) -> Pair {
    repo(format, |e, g| {
        e.ok(g, &["switch", "-q", "-c", "side"]);
        write(g, "a", "side\n");
        write(g, "dir/b", "side b\n");
        e.ok(g, &["commit", "-q", "-am", "side"]);
        e.ok(g, &["switch", "-q", "main"]);
        write(g, "a", "main\n");
        e.ok(g, &["commit", "-q", "-am", "main"]);
        let merged = e.git(g, &["merge", "side"]);
        assert!(!merged.status.success());
    })
}

#[test]
fn rm_of_unmerged_paths() {
    for format in FORMATS {
        for args in [
            &["a"][..],
            &["--cached", "a"],
            &["-f", "a"],
            &["dir/b"],
            &["a", "dir/b"],
            &["-r", "."],
        ] {
            conflicted(format).rm(args, &format!("{format} unmerged"));
        }
    }
}

#[test]
fn mv_of_files_and_folders() {
    for format in FORMATS {
        for args in [
            &["a", "a2"][..],
            &["dir/b", "dir/b2"],
            &["dir/b", "other"],
            &["dir/b", "other/"],
            &["dir/b", "other/d"],
            &["-f", "dir/b", "other/d"],
            &["a", "dir/b"],
            &["-f", "a", "dir/b"],
            &["a", "u"],
            &["-f", "a", "u"],
            &["a", "x.log"],
            &["-f", "a", "x.log"],
            &["a", "a"],
            &["-f", "a", "a"],
            &["run.sh", "other"],
            &["link", "other"],
            &["link", "link2"],
            &["a", "dir/b", "other"],
            &["a", "link", "run.sh", "dir/sub"],
            &["a", "dir/b", "other/d"],
            &["a", "nodir/a"],
            &["a", "nodir/"],
            &["-k", "a", "nodir/a"],
            &["dir", "dir2"],
            &["dir", "other"],
            &["dir", "dir/sub"],
            &["dir", "dir/new"],
            &["dir/sub", "other"],
            &["dir/sub", "."],
            &["dir/sub", "sub2"],
            &["dir", "a"],
            &["-f", "dir", "a"],
            &["other", "dir"],
            &["dir", "other", "dir/sub"],
            &["u", "u2"],
            &["x.log", "y.log"],
            &["nothing", "n2"],
            &["dir/u2", "other"],
            &["a", "."],
            &[".", "dir"],
            &["a"],
            &["-n", "a", "a2"],
            &["-v", "a", "a2"],
            &["-n", "-v", "dir", "dir2"],
            &["-n", "a", "dir/b"],
            &["a", "dir/b", "other/d", "dir/sub"],
        ] {
            clean(format).mv(args, &format!("{format} clean"));
        }
    }
}

#[test]
fn mv_with_k_skips_what_cannot_move() {
    for format in FORMATS {
        for args in [
            &["-k", "u", "a", "other"][..],
            &["-k", "nothing", "a", "other"],
            &["-k", "a", "dir/b", "u"],
            &["-k", "a", "link", "run.sh", "other/d"],
            &["-k", "other/d", "dir/b", "other"],
            &["-k", "u", "x.log", "other"],
            &["-k", "dir", "other"],
            &["-k", "-f", "a", "dir/b", "other"],
            &["-k", "-n", "u", "a", "other"],
            &["-kv", "u", "a", "other"],
        ] {
            clean(format).mv(args, &format!("{format} -k"));
        }
    }
}

#[test]
fn mv_keeps_the_staged_content_and_the_modes() {
    for format in FORMATS {
        let cases: [Case; 6] = [
            ("edited", |_, g| write(g, "a", "edited\n")),
            ("staged edit", |e, g| {
                write(g, "a", "staged\n");
                e.ok(g, &["add", "a"]);
                write(g, "a", "again\n");
            }),
            ("exec toggled", |_, g| set_exec(g, "a", true)),
            ("exec removed", |_, g| set_exec(g, "run.sh", false)),
            ("new staged", |e, g| {
                write(g, "dir/new", "n\n");
                e.ok(g, &["add", "dir/new"]);
            }),
            ("deleted on disk", |_, g| {
                fs::remove_file(g.join("a")).unwrap()
            }),
        ];
        for (name, make) in cases {
            for args in [
                &["a", "moved"][..],
                &["run.sh", "other"],
                &["dir/new", "other"],
                &["dir", "other"],
            ] {
                repo(format, make).mv(args, &format!("{format} {name}"));
            }
        }
    }
}

#[test]
fn mv_from_a_subfolder() {
    for format in FORMATS {
        for (sub, args) in [
            ("dir", &["b", "b2"][..]),
            ("dir", &["b", "../other"]),
            ("dir", &["../a", "."]),
            ("dir", &["sub", "../other"]),
            ("dir/sub", &["c", ".."]),
            ("dir/sub", &["c", "../../other"]),
            ("dir", &["../a", "../dir/b"]),
            ("dir", &["u2", "b"]),
        ] {
            clean(format).same_in(sub, "mv", args, &format!("{format} from {sub}"));
        }
    }
}

#[test]
fn mv_of_unmerged_paths() {
    for format in FORMATS {
        for args in [
            &["a", "a2"][..],
            &["dir/b", "b2"],
            &["dir", "dir2"],
            &["-k", "a", "dir/b", "other"],
            &["run.sh", "a"],
            &["-f", "run.sh", "a"],
            &["-f", "run.sh", "other"],
        ] {
            conflicted(format).mv(args, &format!("{format} unmerged"));
        }
    }
}

#[test]
fn mv_moves_untracked_files_inside_a_folder_with_it() {
    for format in FORMATS {
        let p = clean(format);
        assert!(p.mv(&["dir", "moved"], "folder with an untracked file"));
        assert_eq!(
            fs::read_to_string(p.pyn.join("moved/u2")).unwrap(),
            "untracked in dir\n"
        );
        assert!(p.pyn.join("moved/sub/c").exists() && !p.pyn.join("dir").exists());
    }
}

#[test]
fn rm_then_mv_then_rm_in_a_row() {
    for format in FORMATS {
        let p = clean(format);
        p.mv(&["dir", "moved"], "step 1");
        p.rm(&["-r", "moved/sub"], "step 2");
        p.mv(&["a", "moved"], "step 3");
        p.rm(&["--cached", "moved/a"], "step 4");
        p.mv(&["moved/b", "back"], "step 5");
        p.rm(&["-f", "back", "other/d"], "step 6");
    }
}
