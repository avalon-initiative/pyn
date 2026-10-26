//! `pyn worktree` on a git repository, compared with the real `git worktree` on a twin repository: the same
//! operations leave the same `worktrees/<name>` folders, work trees, lists and exit statuses, and each tool
//! works on the work trees the other made. Needs `git` on the PATH (tests only).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Git,
    Pyn,
}

struct Twin {
    tmp: TempDir,
}

impl Twin {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let twin = Twin { tmp };
        for tool in ["git", "pyn"] {
            let r = twin.root(tool).join("r");
            fs::create_dir_all(&r).unwrap();
            twin.git(&r, &["init", "-q", "-b", "main"]);
            fs::create_dir_all(r.join("d")).unwrap();
            fs::write(r.join("d/f"), "one\n").unwrap();
            fs::write(r.join("g"), "g\n").unwrap();
            twin.git(&r, &["add", "."]);
            twin.git(&r, &["commit", "-qm", "one"]);
            fs::write(r.join("g"), "two\n").unwrap();
            twin.git(&r, &["commit", "-qam", "two"]);
            twin.git(&r, &["branch", "side", "HEAD~1"]);
        }
        twin
    }

    fn root(&self, tool: &str) -> PathBuf {
        let root = fs::canonicalize(self.tmp.path()).unwrap().join(tool);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        let out = self.cmd(Tool::Git, cwd, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn cmd(&self, tool: Tool, cwd: &Path, args: &[&str]) -> Output {
        let home = self.tmp.path().join("home");
        let mut cmd = match tool {
            Tool::Git => {
                let mut c = Command::new("git");
                c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
                c.env("GIT_CONFIG_NOSYSTEM", "1");
                c
            }
            Tool::Pyn => {
                let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
                c.env("PYN_CONFIG_DIR", home.join("pyn-config"))
                    .env_remove("PYN_SERVER")
                    .env_remove("PYN_REPO");
                c
            }
        };
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("xdg"))
            .envs(IDENTITY);
        cmd.output().unwrap()
    }

    /// Runs the arguments with `tool` in the repository of its twin side.
    fn run(&self, tool: Tool, side: &str, args: &[&str]) -> Output {
        let cwd = self.root(side).join("r");
        self.cmd(tool, &cwd, args)
    }

    /// The porcelain list from real git, with the twin's root replaced.
    fn list(&self, side: &str) -> String {
        let out = self.run(Tool::Git, side, &["worktree", "list", "--porcelain"]);
        String::from_utf8_lossy(&out.stdout).replace(self.root(side).to_str().unwrap(), "ROOT")
    }

    /// Every file under the work tree and the admin folder, with its contents, root replaced.
    fn dump(&self, side: &str, name: &str) -> String {
        let root = self.root(side);
        let mut out = String::new();
        for dir in [root.join(name), root.join("r/.git/worktrees").join(name)] {
            let mut files = Vec::new();
            walk(&dir, &dir, &mut files);
            files.sort();
            for f in files {
                if f.ends_with("index") || f.contains("logs/") || f.ends_with("ORIG_HEAD") {
                    continue;
                }
                let text = fs::read_to_string(dir.join(&f)).unwrap_or_default();
                out.push_str(&format!(
                    "{f}: {}\n",
                    text.replace(root.to_str().unwrap(), "ROOT")
                ));
            }
        }
        out
    }
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && p.file_name().is_none_or(|n| n != ".git") {
            walk(base, &p, out);
        } else {
            out.push(p.strip_prefix(base).unwrap().display().to_string());
        }
    }
}

fn status(out: &Output) -> i32 {
    out.status.code().unwrap()
}

#[test]
fn add_variants_match_git() {
    let cases: [&[&str]; 9] = [
        &["worktree", "add", "../a"],
        &["worktree", "add", "../b", "side"],
        &["worktree", "add", "--detach", "../c", "HEAD~1"],
        &["worktree", "add", "-b", "nb", "../d"],
        &["worktree", "add", "-b", "nb2", "../e", "side"],
        &["worktree", "add", "-B", "side", "../f", "main"],
        &[
            "worktree", "add", "--lock", "--reason", "why", "../g", "main~1",
        ],
        &["worktree", "add", "--no-checkout", "../h", "main"],
        &["worktree", "add", "--orphan", "-b", "orph", "../i"],
    ];
    let twin = Twin::new();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for (case, name) in cases.iter().zip(names) {
        let args = case.to_vec();
        let g = twin.run(Tool::Git, "git", &args);
        let p = twin.run(Tool::Pyn, "pyn", &args);
        let (gs, ps) = (status(&g), status(&p));
        assert_eq!(gs, ps, "{case:?}: {}", String::from_utf8_lossy(&p.stderr));
        assert_eq!(twin.dump("git", name), twin.dump("pyn", name), "{case:?}");
    }
    assert_eq!(twin.list("git"), twin.list("pyn"));
    for side in ["git", "pyn"] {
        for name in ["a", "c", "d", "e"] {
            let cwd = twin.root(side).join(name);
            let out = twin.cmd(Tool::Git, &cwd, &["status", "--porcelain"]);
            assert!(out.stdout.is_empty(), "{side}/{name} should be clean");
        }
        let fsck = twin.cmd(
            Tool::Git,
            &twin.root(side).join("r"),
            &["fsck", "--no-progress"],
        );
        let report = String::from_utf8_lossy(&fsck.stderr);
        assert!(
            !report.contains("error") && !report.contains("broken"),
            "{side}: {report}"
        );
    }
}

#[test]
fn add_refusals_match_git() {
    let twin = Twin::new();
    for side in ["git", "pyn"] {
        let tool = if side == "git" { Tool::Git } else { Tool::Pyn };
        let r = twin.root(side).join("r");
        let fails = |args: &[&str]| status(&twin.cmd(tool, &r, args));
        assert_eq!(fails(&["worktree", "add", "../x", "main"]), 128);
        assert_ne!(fails(&["worktree", "add", "-b", "side", "../x"]), 0);
        assert_eq!(fails(&["worktree", "add", "../x", "nope"]), 128);
        assert_eq!(fails(&["worktree", "add", "../x"]), 0);
        assert_eq!(fails(&["worktree", "add", "../x"]), 128);
        assert!(!r.join("../y").exists());
    }
}

#[test]
fn list_lock_move_prune_remove_match_git() {
    let twin = Twin::new();
    for side in ["git", "pyn"] {
        let tool = if side == "git" { Tool::Git } else { Tool::Pyn };
        let r = twin.root(side).join("r");
        let run = |args: &[&str]| twin.cmd(tool, &r, args);
        for args in [
            &["worktree", "add", "../a"][..],
            &["worktree", "add", "--detach", "../b"],
            &["worktree", "add", "../c", "side"],
        ] {
            assert_eq!(status(&run(args)), 0);
        }
        assert_eq!(
            status(&run(&["worktree", "lock", "--reason", "busy", "../a"])),
            0
        );
        assert_eq!(status(&run(&["worktree", "lock", "../a"])), 128);
        assert_eq!(status(&run(&["worktree", "remove", "../a"])), 128);
        assert_eq!(status(&run(&["worktree", "move", "../a", "../z"])), 128);
        assert_eq!(status(&run(&["worktree", "unlock", "../b"])), 128);
        assert_eq!(status(&run(&["worktree", "unlock", "../a"])), 0);
        assert_eq!(status(&run(&["worktree", "move", "../a", "../z"])), 0);
        fs::write(twin.root(side).join("b/new"), "x").unwrap();
        assert_eq!(status(&run(&["worktree", "remove", "../b"])), 128);
        assert_eq!(status(&run(&["worktree", "remove", "--force", "../b"])), 0);
        fs::remove_dir_all(twin.root(side).join("c")).unwrap();
        assert_eq!(status(&run(&["worktree", "prune", "-n"])), 0);
        assert!(r.join(".git/worktrees/c").exists());
        assert_eq!(status(&run(&["worktree", "prune"])), 0);
        assert!(!r.join(".git/worktrees/c").exists());
        assert_eq!(status(&run(&["worktree", "remove", "."])), 128);
    }
    assert_eq!(twin.list("git"), twin.list("pyn"));
    assert_eq!(twin.dump("git", "z"), twin.dump("pyn", "z"));
}

#[test]
fn list_output_matches_git() {
    let twin = Twin::new();
    let r = twin.root("pyn").join("r");
    let rg = twin.root("git").join("r");
    for (tool, dir) in [(Tool::Git, &rg), (Tool::Pyn, &r)] {
        for args in [
            &["worktree", "add", "../a"][..],
            &["worktree", "add", "--detach", "../bb"],
            &[
                "worktree", "add", "--lock", "--reason", "why", "../ccc", "side",
            ],
            &["worktree", "add", "--orphan", "-b", "o", "../d"],
        ] {
            assert_eq!(status(&twin.cmd(tool, dir, args)), 0);
        }
        fs::remove_dir_all(dir.parent().unwrap().join("bb")).unwrap();
    }
    for flags in [
        &["worktree", "list"][..],
        &["worktree", "list", "-v"],
        &["worktree", "list", "--porcelain"],
        &["worktree", "list", "--porcelain", "-z"],
    ] {
        let g = twin.cmd(Tool::Git, &rg, flags);
        let p = twin.cmd(Tool::Pyn, &r, flags);
        // the column gap is one space from git 2.4x on and two before; the text is compared without runs of spaces
        let norm = |o: &Output, side: &str| {
            let text = String::from_utf8_lossy(&o.stdout)
                .replace(twin.root(side).to_str().unwrap(), "ROOT");
            text.split(' ')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(norm(&g, "git"), norm(&p, "pyn"), "{flags:?}");
    }
}

#[test]
fn each_tool_works_on_the_others_work_trees() {
    let twin = Twin::new();
    let r = twin.root("git").join("r");
    twin.git(&r, &["worktree", "add", "../fromgit"]);
    assert_eq!(
        status(&twin.cmd(Tool::Pyn, &r, &["worktree", "add", "../frompyn"])),
        0
    );
    let list = twin.cmd(Tool::Pyn, &r, &["worktree", "list"]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(text.contains("fromgit") && text.contains("frompyn"));
    // the work trees are usable by git, and by pyn from inside
    twin.git(&twin.root("git").join("frompyn"), &["status"]);
    let inside = twin.cmd(
        Tool::Pyn,
        &twin.root("git").join("fromgit"),
        &["worktree", "list"],
    );
    assert_eq!(status(&inside), 0);
    twin.git(&r, &["worktree", "lock", "../frompyn"]);
    assert_eq!(
        status(&twin.cmd(Tool::Pyn, &r, &["worktree", "remove", "../frompyn"])),
        128
    );
    assert_eq!(
        status(&twin.cmd(Tool::Pyn, &r, &["worktree", "remove", "../fromgit"])),
        0
    );
    twin.git(&r, &["worktree", "remove", "-f", "-f", "../frompyn"]);
    let after = twin.git(&r, &["worktree", "list"]);
    assert_eq!(String::from_utf8_lossy(&after.stdout).lines().count(), 1);
}

#[test]
fn repair_restores_links_after_a_manual_move() {
    let twin = Twin::new();
    let r = twin.root("pyn").join("r");
    assert_eq!(
        status(&twin.cmd(Tool::Pyn, &r, &["worktree", "add", "../a"])),
        0
    );
    let root = twin.root("pyn");
    fs::rename(root.join("a"), root.join("moved")).unwrap();
    assert_eq!(
        status(&twin.cmd(Tool::Pyn, &r, &["worktree", "repair", "../moved"])),
        0
    );
    let out = twin.git(&r, &["worktree", "list", "--porcelain"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("moved") && !text.contains("prunable"));
    twin.git(&root.join("moved"), &["status"]);
}
