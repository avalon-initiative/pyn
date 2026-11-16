//! `pyn format-patch` of binary changes and of gitlink-only commits on a git repository, compared with the real git
//! on a twin: the text around the binary data is identical, and a patch pyn writes applies with git (`git am`) to
//! the same tree. The deflated data and the literal-or-delta choice are not compared. Real git is the oracle here only.

mod patchfix;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use patchfix::{FORMATS, Twin, commit, lines, run, write};

fn noise(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

fn put(dir: &Path, name: &str, data: &[u8]) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, data).unwrap();
}

/// Commits the index as it is, which `commit` would rewrite with `add -A`.
fn commit_index(dir: &Path, tick: &mut i64, message: &str) {
    *tick += 3600;
    let date = format!("{tick} +0200");
    let out = patchfix::git(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn history(dir: &Path) {
    let mut tick = 1_700_000_000;
    let who = "Alice Author <alice@example.com>";
    let big = noise(1, 6000);
    write(dir, "a.txt", &lines("line", 6));
    put(dir, "small.bin", b"a\0b\n");
    put(dir, "big.bin", &big);
    put(dir, "zero.bin", &[0u8; 5000]);
    put(dir, "tool.bin", &noise(2, 300));
    write(dir, "attr/x.dat", "one\ntwo\n");
    write(dir, ".gitattributes", "*.dat -diff\n*.txt diff\n");
    commit(dir, &mut tick, who, "first");
    run(dir, &["tag", "c1"]);
    let mut edited = big.clone();
    for b in &mut edited[3000..3010] {
        *b ^= 0xff;
    }
    put(dir, "big.bin", &edited);
    put(dir, "small.bin", b"a\0c\n");
    put(dir, "new.bin", &noise(3, 500));
    write(dir, "attr/x.dat", "one\ntwo\nthree\n");
    write(dir, "a.txt", &format!("{}\0nul\n", lines("line", 6)));
    commit(dir, &mut tick, who, "binary and text together");
    run(dir, &["tag", "c2"]);
    run(dir, &["rm", "-q", "small.bin"]);
    put(dir, "zero.bin", &[0u8; 5010]);
    put(dir, "tool.bin", &noise(2, 300));
    fs::set_permissions(dir.join("tool.bin"), fs::Permissions::from_mode(0o755)).unwrap();
    commit(dir, &mut tick, who, "delete, grow and mode change");
    run(dir, &["tag", "c3"]);
    put(dir, "tool.bin", &noise(9, 300));
    commit(dir, &mut tick, who, "mode kept, content replaced");
    run(dir, &["tag", "c4"]);
    let id = run(dir, &["rev-parse", "HEAD"]);
    let id = id.trim();
    run(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{id},sub"),
        ],
    );
    commit_index(dir, &mut tick, "add a gitlink");
    run(dir, &["tag", "c5"]);
    let other = run(dir, &["rev-parse", "c2"]);
    run(
        dir,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{},sub", other.trim()),
        ],
    );
    commit_index(dir, &mut tick, "move the gitlink");
    run(dir, &["tag", "c6"]);
    run(dir, &["update-index", "--force-remove", "sub"]);
    commit_index(dir, &mut tick, "remove the gitlink");
    run(dir, &["tag", "c7"]);
}

fn each(test: impl Fn(&Twin)) {
    for format in FORMATS {
        test(&Twin::new(format, history));
    }
}

/// Drops the base85 data and the sizes of the sections and masks the signature.
fn mask(text: &str) -> String {
    let mut out = Vec::new();
    let (mut blanks, mut inside, mut sig) = (0, false, false);
    for line in text.split('\n') {
        if sig {
            out.push(if line.is_empty() { line } else { "SIGNATURE" });
            sig = false;
        } else if line == "-- " {
            out.push(line);
            sig = true;
        } else if line == "GIT binary patch" {
            inside = true;
            blanks = 0;
            out.push(line);
        } else if inside && (line.starts_with("literal ") || line.starts_with("delta ")) {
            out.push("SECTION");
        } else if inside && line.is_empty() {
            blanks += 1;
            inside = blanks < 2;
            out.push(line);
        } else if !inside {
            out.push(line);
        }
    }
    out.join("\n")
}

fn same_stdout(t: &Twin, args: &[&str]) {
    let mut full = vec!["format-patch", "--stdout"];
    full.extend_from_slice(args);
    let want = t.run_git("", &full, b"");
    let got = t.run_pyn("", &full, b"");
    let what = format!("{} {args:?}", t.format);
    assert_eq!(
        mask(&got.out()),
        mask(&want.out()),
        "stdout of {what} (pyn stderr: {})",
        got.err()
    );
    assert_eq!(got.code, want.code, "exit status of {what}");
    assert_eq!(
        got.stderr.is_empty(),
        want.stderr.is_empty(),
        "stderr use of {what}: pyn {:?}",
        got.err()
    );
}

#[test]
fn writes_binary_patches_like_git() {
    each(|t| {
        for c in ["c2", "c3", "c4"] {
            same_stdout(t, &["-1", c]);
        }
        same_stdout(t, &["c1..c4"]);
        same_stdout(t, &["--no-stat", "-1", "c2"]);
        same_stdout(t, &["--cover-letter", "c1..c4"]);
    });
}

#[test]
fn writes_gitlink_only_commits_like_git() {
    each(|t| {
        for c in ["c5", "c6", "c7"] {
            same_stdout(t, &["-1", c]);
        }
        same_stdout(t, &["c4..c7"]);
    });
}

/// Applies pyn's patch for each commit with git onto the parent and demands the commit's own tree.
#[test]
fn writes_patches_git_applies_to_the_same_tree() {
    each(|t| {
        for (parent, c) in [
            ("c1", "c2"),
            ("c2", "c3"),
            ("c3", "c4"),
            ("c4", "c5"),
            ("c5", "c6"),
            ("c6", "c7"),
        ] {
            let patch = t.run_pyn("", &["format-patch", "--stdout", "-1", c], b"");
            assert_eq!(patch.code, 0, "{}", patch.err());
            if c == "c2" {
                assert!(
                    patch.out().contains("\ndelta "),
                    "a small edit of a big file is a delta"
                );
            }
            let tree = |rev: &str| {
                t.run_git("", &["rev-parse", &format!("{rev}^{{tree}}")], b"")
                    .out()
            };
            let reset = t.run_git("", &["checkout", "-q", "--detach", parent], b"");
            assert_eq!(reset.code, 0, "{}", reset.err());
            let am = t.run_git("", &["am", "-q"], &patch.stdout);
            assert_eq!(am.code, 0, "am of {c} ({}): {}", t.format, am.err());
            assert_eq!(tree("HEAD"), tree(c), "tree of {c} ({})", t.format);
        }
    });
}
