//! Commands that write the work tree of a git repository refuse a tree holding a path that is not safe to
//! materialize, and write nothing. The served trees are built with the repository's own object writers.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use pyn_git::Repository;
use pyn_object::{Kind, ObjectId};
use tempfile::{TempDir, tempdir};

/// Name parts of the offending path, and whether the reference tree writer accepts them.
const CASES: [(&[&[u8]], bool); 7] = [
    (&[b".git", b"config"], true),
    (&[b"sub", b".GIT", b"hooks"], true),
    (&[b".pyn", b"local_only", b"config.toml"], true),
    (&[b"sub", b"a\\b"], true),
    (&[b"sub", b"x\x1b[2Jy"], true),
    (&[b"sub", b".."], false),
    (&[b"git~1", b"x"], true),
];

struct Out {
    code: i32,
    stderr: String,
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = sample_bare(&root, "sha1");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let url = format!("ssh://git@example.invalid{}", bare.display());
        Rig {
            shim: ssh_shim(&root, false),
            _tmp: tmp,
            root,
            bare,
            home,
            url,
        }
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
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .output()
            .unwrap();
        Out {
            code: o.status.code().unwrap(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn head_of(&self, branch: &str) -> String {
        run(&self.bare, &["rev-parse", &format!("refs/heads/{branch}")])
            .trim()
            .to_owned()
    }

    /// Publishes `branch` as a child of `parent` whose tree adds the file at `parts`.
    fn plant(&self, branch: &str, parent: &str, parts: &[&[u8]]) {
        let repo = Repository::open(&self.bare).unwrap();
        let db = repo.objects().unwrap();
        let algorithm = db.algorithm();
        let blob = db.write(Kind::Blob, b"payload\n").unwrap();
        let mut node = blob;
        let mut mode = "100644";
        for part in parts[1..].iter().rev() {
            let body = raw_tree(&[(mode, *part, &node)]);
            node = db.write(Kind::Tree, &body).unwrap();
            mode = "40000";
        }
        let keep = ObjectId::from_hex(
            algorithm,
            run(&self.bare, &["rev-parse", &format!("{parent}^{{tree}}")]).trim(),
        )
        .unwrap();
        let top = db.read(&keep).unwrap();
        let mut body = top.data.clone();
        body = merge_entry(body, parts[0], &node);
        let tree = db.write(Kind::Tree, &body).unwrap();
        let commit = format!(
            "tree {}\nparent {parent}\nauthor t <t@example.com> 1600000000 +0000\ncommitter t <t@example.com> 1600000000 +0000\n\nplanted\n",
            tree.to_hex()
        );
        let id = db.write(Kind::Commit, commit.as_bytes()).unwrap();
        fs::write(
            self.bare.join("refs/heads").join(branch),
            format!("{}\n", id.to_hex()),
        )
        .unwrap();
    }
}

/// Entries of one tree, in git's order for the names used here.
fn raw_tree(entries: &[(&str, &[u8], &ObjectId)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (mode, name, id) in entries {
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(name);
        out.push(0);
        out.extend_from_slice(id.as_bytes());
    }
    out
}

/// Adds the directory `name` to a raw tree body, keeping git's order.
fn merge_entry(body: Vec<u8>, name: &[u8], id: &ObjectId) -> Vec<u8> {
    let mut parsed: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut rest = &body[..];
    while !rest.is_empty() {
        let nul = rest.iter().position(|b| *b == 0).unwrap();
        let end = nul + 1 + 20;
        let head = &rest[..nul];
        let sp = head.iter().position(|b| *b == b' ').unwrap();
        parsed.push((head[sp + 1..].to_vec(), rest[..end].to_vec()));
        rest = &rest[end..];
    }
    let mut entry = b"40000 ".to_vec();
    entry.extend_from_slice(name);
    entry.push(0);
    entry.extend_from_slice(id.as_bytes());
    let key = |n: &[u8], dir: bool| {
        let mut k = n.to_vec();
        if dir {
            k.push(b'/');
        }
        k
    };
    let mut all: Vec<(Vec<u8>, Vec<u8>)> = parsed
        .into_iter()
        .map(|(n, raw)| (key(&n, raw.starts_with(b"40000 ")), raw))
        .collect();
    all.push((key(name, true), entry));
    all.sort();
    all.into_iter().flat_map(|(_, raw)| raw).collect()
}

fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            let meta = fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                stack.push(p);
            } else {
                out.insert(rel, fs::read(&p).unwrap_or_default());
            }
        }
    }
    out
}

fn refused(out: &Out, parses: bool) {
    assert_ne!(out.code, 0, "{}", out.stderr);
    if parses {
        assert!(out.stderr.contains("refusing path"), "{}", out.stderr);
    }
}

#[test]
fn clone_refuses_a_tree_with_an_unsafe_path_and_leaves_nothing() {
    let rig = Rig::new();
    let main = rig.head_of("main");
    for (i, (parts, parses)) in CASES.iter().enumerate() {
        let branch = format!("evil{i}");
        rig.plant(&branch, &main, parts);
        let dest = rig.root.join(format!("clone{i}"));
        let out = rig.pyn(
            &rig.root,
            &[
                "clone",
                "-b",
                &branch,
                "--",
                &rig.url,
                dest.to_str().unwrap(),
            ],
        );
        refused(&out, *parses);
        if *parses {
            assert!(!dest.exists(), "case {i}: a refused clone is cleaned up");
        } else if dest.exists() {
            assert!(files(&dest).is_empty(), "case {i}");
        }
    }
}

#[test]
fn pull_and_switch_refuse_an_unsafe_path_and_change_nothing() {
    let rig = Rig::new();
    let main = rig.head_of("main");
    let work = rig.root.join("work");
    let cloned = rig.pyn(
        &rig.root,
        &["clone", "--", &rig.url, work.to_str().unwrap()],
    );
    assert_eq!(cloned.code, 0, "{}", cloned.stderr);
    for (i, (parts, parses)) in CASES.iter().enumerate() {
        if *parses {
            rig.plant(&format!("evil{i}"), &main, parts);
        }
    }
    let fetched = rig.pyn(&work, &["fetch"]);
    assert_eq!(fetched.code, 0, "{}", fetched.stderr);
    let before = files(&work);
    let head = fs::read_to_string(work.join(".git/HEAD")).unwrap();
    let tip = run(&work, &["rev-parse", "HEAD"]);
    for (i, (_, parses)) in CASES.iter().enumerate().filter(|(_, (_, p))| *p) {
        let branch = format!("evil{i}");
        let remote = format!("origin/{branch}");
        for args in [
            vec!["switch", "--detach", remote.as_str()],
            vec!["switch", "-c", "local", remote.as_str()],
            vec!["checkout", remote.as_str()],
            vec!["merge", remote.as_str()],
            vec!["merge", "--ff-only", remote.as_str()],
        ] {
            let out = rig.pyn(&work, &args);
            refused(&out, *parses);
            assert_eq!(files(&work), before, "case {i} {args:?}");
            assert_eq!(fs::read_to_string(work.join(".git/HEAD")).unwrap(), head);
            assert_eq!(run(&work, &["rev-parse", "HEAD"]), tip, "case {i} {args:?}");
        }
    }
    // The pull path: the remote's main moves to a commit with an unsafe path.
    rig.plant("main", &main, CASES[0].0);
    let out = rig.pyn(&work, &["pull"]);
    refused(&out, true);
    assert_eq!(files(&work), before);
    assert_eq!(run(&work, &["rev-parse", "HEAD"]), tip);
}
