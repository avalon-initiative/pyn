#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use pyn_gitwire::process::{LocalGit, Shape};
use pyn_gitwire::{ProtocolVersion, Transport};
use pyn_object::{Algorithm, ObjectId};
use tempfile::TempDir;

pub fn git_cmd(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args([
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "advice.detachedHead=false",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2024-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2024-01-01T00:00:00Z");
    c
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_cmd(dir).args(args).output().expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

pub fn git_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = git_cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let t = std::thread::spawn(move || stdin.write_all(&data));
    let out = child.wait_with_output().unwrap();
    let _ = t.join();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Whether the local git can create repositories of this format; prints a skip note otherwise.
pub fn supported(alg: Algorithm) -> bool {
    if alg == Algorithm::Sha1 {
        return true;
    }
    let d = TempDir::new().unwrap();
    let ok = git_cmd(d.path())
        .args(["init", "--object-format=sha256", "-q"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipping: git without sha256 support");
    }
    ok
}

pub fn init(alg: Algorithm, bare: bool) -> TempDir {
    let d = TempDir::new().unwrap();
    let mut args = vec!["init", "-q", "--object-format", alg.name()];
    if bare {
        args.push("--bare");
    }
    git(d.path(), &args);
    d
}

pub fn commit(dir: &Path, file: &str, content: &str) -> ObjectId {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", content]);
    id(&git(dir, &["rev-parse", "HEAD"]))
}

pub fn id(hex: &str) -> ObjectId {
    ObjectId::parse(hex).unwrap()
}

/// A repository with commits on main and topic, a lightweight and an annotated tag.
pub fn origin(alg: Algorithm) -> TempDir {
    let d = init(alg, false);
    let p = d.path();
    commit(p, "a.txt", "one");
    commit(p, "b.txt", "two");
    git(p, &["branch", "topic"]);
    git(p, &["tag", "light"]);
    git(p, &["tag", "-a", "-m", "annotated", "v1"]);
    commit(p, "c.txt", "three");
    d
}

pub fn shapes(repo: &Path) -> Vec<(&'static str, Box<dyn Transport>)> {
    let mut v: Vec<(&'static str, Box<dyn Transport>)> = Vec::new();
    for (name, shape, ver) in [
        ("duplex-v2", Shape::Duplex, ProtocolVersion::V2),
        ("stateless-v2", Shape::StatelessRpc, ProtocolVersion::V2),
        ("duplex-v0", Shape::Duplex, ProtocolVersion::V0),
        ("stateless-v0", Shape::StatelessRpc, ProtocolVersion::V0),
    ] {
        v.push((
            name,
            Box::new(LocalGit::new(repo, shape).force_version(ver)),
        ));
    }
    v
}

/// Indexes `pack` into a fresh repository seeded from `base` (if any), points a ref at each tip, and fscks.
pub fn verify_pack(alg: Algorithm, base: Option<&Path>, pack: &[u8], tips: &[ObjectId]) {
    let scratch = match base {
        Some(b) => {
            let d = TempDir::new().unwrap();
            git(
                d.path(),
                &["clone", "-q", "--bare", b.to_str().unwrap(), "."],
            );
            d
        }
        None => init(alg, true),
    };
    git_stdin(
        scratch.path(),
        &["index-pack", "--stdin", "--fix-thin"],
        pack,
    );
    for (i, t) in tips.iter().enumerate() {
        git(
            scratch.path(),
            &["update-ref", &format!("refs/verify/{i}"), &t.to_hex()],
        );
    }
    git(scratch.path(), &["fsck", "--strict", "--no-dangling"]);
}
