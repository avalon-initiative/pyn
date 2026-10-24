//! `pyn apply` on a git repository, compared with the real `git apply` on a twin of the same repository: the exit
//! status, standard output, whether standard error carries anything, and the resulting work tree, index and
//! `HEAD`. Real git is the oracle in these tests only.

mod patchfix;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use patchfix::{FORMATS, Twin, commit, lines, run, write};

/// The history every scenario starts from: `c1`, then `c2` changing most kinds of path.
fn history(dir: &Path) {
    let mut tick = 1_700_000_000;
    let who = "T <t@example.com>";
    write(dir, "a.txt", &lines("line", 12));
    write(dir, "b.sh", "#!/bin/sh\necho b\n");
    fs::set_permissions(dir.join("b.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    write(dir, "d/e.txt", "e1\ne2\ne3\n");
    write(dir, "s p.txt", &lines("sp", 8));
    write(dir, "gone.txt", "bye\n");
    write(dir, "r1.txt", &lines("rr", 20));
    write(dir, "r2.txt", &lines("cc", 20));
    fs::write(dir.join("bin.dat"), [0u8, 1, 2, 3, 0, 5]).unwrap();
    write(
        dir,
        "docs/a/very/deeply/nested/folder/with/a/rather/long/name/readme.txt",
        "x\n",
    );
    commit(dir, &mut tick, who, "c1");
    run(dir, &["tag", "c1"]);
    let a = lines("line", 12)
        .replace("line3\n", "LINE3\n")
        .replace("line10\n", "line10\nextra\n");
    write(dir, "a.txt", &a);
    write(dir, "new.txt", "brand\nnew\n");
    fs::set_permissions(dir.join("b.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    write(dir, "d/e.txt", "e1\nE2\ne3\n");
    fs::remove_file(dir.join("gone.txt")).unwrap();
    fs::remove_file(dir.join("s p.txt")).unwrap();
    write(dir, "t q.txt", &lines("sp", 8).replace("sp4\n", "SP4\n"));
    symlink("a.txt", dir.join("link")).unwrap();
    commit(dir, &mut tick, who, "c2");
    run(dir, &["tag", "c2"]);
    fs::create_dir_all(dir.join("dir")).unwrap();
    run(dir, &["mv", "r1.txt", "dir/renamed.txt"]);
    write(
        dir,
        "dir/renamed.txt",
        &lines("rr", 20).replace("rr7\n", "RR7\n"),
    );
    write(dir, "copy.txt", &lines("cc", 20).replace("cc3\n", "CC3\n"));
    fs::write(dir.join("bin.dat"), [0u8, 1, 2, 9, 0, 5]).unwrap();
    let long = "docs/a/very/deeply/nested/folder/with/a/rather/long/name/readme.txt";
    write(dir, long, &lines("big", 150));
    write(dir, "med.txt", &lines("med", 30));
    commit(dir, &mut tick, who, "c3");
    run(dir, &["tag", "c3"]);
}

/// Twins at `base`, with patches made by real git in `p/`.
fn twin(format: &'static str, base: &str) -> Twin {
    let t = Twin::new(format, history);
    fs::create_dir_all(t.root.join("p")).unwrap();
    let patches: [(&str, Vec<&str>); 12] = [
        (
            "ext",
            vec![
                "diff",
                "-M",
                "-C",
                "--find-copies-harder",
                "c2",
                "c3",
                "--",
                ":!bin.dat",
            ],
        ),
        (
            "ext-text",
            vec!["diff", "-M", "c2", "c3", "--", "dir", "r1.txt"],
        ),
        ("bin", vec!["diff", "c2", "c3", "--", "bin.dat"]),
        ("big", vec!["diff", "c2", "c3", "--", "docs", "med.txt"]),
        (
            "copy",
            vec![
                "diff",
                "-C",
                "--find-copies-harder",
                "c2",
                "c3",
                "--",
                "copy.txt",
                "r2.txt",
            ],
        ),
        ("fwd", vec!["diff", "c1", "c2"]),
        ("fwd-p0", vec!["diff", "--no-prefix", "c1", "c2"]),
        (
            "fwd-p2",
            vec!["diff", "--src-prefix=x/a/", "--dst-prefix=x/b/", "c1", "c2"],
        ),
        ("fwd-nocopies", vec!["diff", "--no-renames", "c1", "c2"]),
        ("a-only", vec!["diff", "c1", "c2", "--", "a.txt"]),
        ("d-only", vec!["diff", "c1", "c2", "--", "d"]),
        ("mode", vec!["diff", "c1", "c2", "--", "b.sh"]),
    ];
    for (name, args) in patches {
        let text = run(&t.git, &args);
        fs::write(t.root.join("p").join(format!("{name}.patch")), text).unwrap();
    }
    t.both(&["reset", "-q", "--hard", base]);
    t
}

fn patch(t: &Twin, name: &str) -> String {
    t.root
        .join("p")
        .join(format!("{name}.patch"))
        .display()
        .to_string()
}

/// `apply <args>` with both tools in `sub`.
fn same(t: &Twin, sub: &str, args: &[&str], stdin: &[u8]) {
    let mut full = vec!["apply"];
    full.extend_from_slice(args);
    t.same(sub, &full, stdin);
}

fn each(base: &str, test: impl Fn(&Twin)) {
    for format in FORMATS {
        test(&twin(format, base));
    }
}

#[test]
fn applies_a_git_diff_like_git() {
    for args in [
        vec![],
        vec!["--check"],
        vec!["--stat"],
        vec!["--numstat"],
        vec!["--summary"],
        vec!["--stat", "--numstat", "--summary"],
        vec!["--check", "--stat", "--summary"],
        vec!["--apply", "--stat", "--numstat", "--summary"],
        vec!["--index"],
        vec!["--cached"],
        vec!["--whitespace=nowarn"],
    ] {
        each("c1", |t| {
            let patch = patch(t, "fwd");
            let mut full = args.clone();
            full.push(&patch);
            same(t, "", &full, b"");
        });
    }
}

#[test]
fn reads_the_patch_from_standard_input() {
    each("c1", |t| {
        let text = fs::read(patch(t, "fwd")).unwrap();
        same(t, "", &["--check"], &text);
        same(t, "", &["-"], &text);
    });
    each("c1", |t| {
        let text = fs::read(patch(t, "fwd")).unwrap();
        same(t, "", &[], &text);
    });
}

#[test]
fn reverses_a_patch() {
    for args in [
        vec!["-R"],
        vec!["-R", "--index"],
        vec!["-R", "--cached"],
        vec!["-R", "--stat", "--summary"],
    ] {
        each("c2", |t| {
            let patch = patch(t, "fwd");
            let mut full = args.clone();
            full.push(&patch);
            same(t, "", &full, b"");
        });
    }
}

#[test]
fn strips_leading_directories() {
    each("c1", |t| {
        same(t, "", &["-p0", &patch(t, "fwd-p0")], b"");
    });
    each("c1", |t| {
        same(t, "", &["-p3", &patch(t, "fwd-p2")], b"");
    });
    each("c1", |t| {
        same(t, "", &["-p4", &patch(t, "fwd")], b"");
    });
}

#[test]
fn finds_a_hunk_that_moved() {
    each("c1", |t| {
        let a = fs::read_to_string(t.git.join("a.txt")).unwrap();
        for dir in [&t.git, &t.pyn] {
            write(dir, "a.txt", &format!("top1\ntop2\ntop3\n{a}"));
        }
        same(t, "", &[&patch(t, "a-only")], b"");
    });
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(dir, "a.txt", &format!("{}tail\n", lines("line", 12)));
        }
        same(t, "", &["--check", &patch(t, "a-only")], b"");
    });
}

#[test]
fn refuses_what_does_not_apply() {
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(
                dir,
                "a.txt",
                &lines("line", 12).replace("line3\n", "other\n"),
            );
        }
        same(t, "", &[&patch(t, "fwd")], b"");
        same(t, "", &["--check", &patch(t, "fwd")], b"");
        same(t, "", &["--stat", &patch(t, "fwd")], b"");
    });
    each("c2", |t| {
        same(t, "", &[&patch(t, "fwd")], b"");
        same(t, "", &["--cached", &patch(t, "fwd")], b"");
        same(t, "", &["--index", &patch(t, "fwd")], b"");
    });
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            fs::remove_file(dir.join("a.txt")).unwrap();
        }
        same(t, "", &[&patch(t, "a-only")], b"");
        same(t, "", &["--cached", &patch(t, "a-only")], b"");
        same(t, "", &["--index", &patch(t, "a-only")], b"");
    });
}

#[test]
fn an_unstaged_edit_blocks_index_but_not_cached() {
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(dir, "a.txt", &format!("{}more\n", lines("line", 12)));
        }
        same(t, "", &["--index", &patch(t, "a-only")], b"");
        same(t, "", &["--cached", &patch(t, "a-only")], b"");
    });
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(dir, "a.txt", &format!("{}more\n", lines("line", 12)));
            run(dir, &["add", "a.txt"]);
        }
        same(t, "", &["--index", &patch(t, "a-only")], b"");
    });
}

#[test]
fn applies_mode_changes_and_empty_files() {
    each("c1", |t| {
        same(t, "", &["--summary", &patch(t, "mode")], b"");
        same(t, "", &[&patch(t, "mode")], b"");
    });
    each("c1", |t| {
        let text = "diff --git a/empty b/empty\nnew file mode 100644\nindex 0000000..e69de29\n";
        same(t, "", &["--summary", "--numstat"], text.as_bytes());
        same(t, "", &[], text.as_bytes());
    });
    each("c1", |t| {
        let text = "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\nindex 3abcdef..0000000\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        same(t, "", &["--summary", "--stat"], text.as_bytes());
        same(t, "", &["--index"], text.as_bytes());
    });
}

#[test]
fn works_from_a_subfolder() {
    each("c1", |t| {
        same(t, "d", &["--check", &patch(t, "fwd")], b"");
        same(t, "d", &["--stat", &patch(t, "fwd")], b"");
        same(t, "d", &[&patch(t, "fwd")], b"");
    });
}

#[test]
fn reports_whitespace_errors() {
    let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n line1\n+added  \n line2\n";
    each("c1", |t| {
        same(t, "", &["--check"], patch.as_bytes());
        same(t, "", &["--whitespace=nowarn", "--check"], patch.as_bytes());
        same(t, "", &[], patch.as_bytes());
    });
    let tab = "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n line1\n+ \tindent\n line2\n";
    each("c1", |t| {
        same(t, "", &[], tab.as_bytes());
    });
}

#[test]
fn rejects_broken_input_like_git() {
    each("c1", |t| {
        same(t, "", &[], b"");
        same(t, "", &[], b"nothing to see\nhere\n");
        same(t, "", &[], b"@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
        same(
            t,
            "",
            &[],
            b"--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n line1\n-line2\n",
        );
        same(
            t,
            "",
            &[],
            b"diff --git a/a.txt b/a.txt\nindex 111..222 100644\n",
        );
        same(t, "", &["--allow-empty"], b"");
        same(t, "", &["nosuch.patch"], b"");
        same(
            t,
            "",
            &[],
            b"--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n line1\n line2\n+x\n+y\n",
        );
    });
}

#[test]
fn applies_several_patches_in_one_input() {
    each("c1", |t| {
        let both = format!(
            "{}{}",
            fs::read_to_string(patch(t, "a-only")).unwrap(),
            fs::read_to_string(patch(t, "d-only")).unwrap()
        );
        same(t, "", &["--stat"], both.as_bytes());
        same(t, "", &[], both.as_bytes());
    });
    each("c1", |t| {
        same(t, "", &[&patch(t, "a-only"), &patch(t, "d-only")], b"");
    });
    each("c1", |t| {
        t.both(&["status", "--short"]);
        let text = format!(
            "{}{}",
            fs::read_to_string(patch(t, "a-only")).unwrap(),
            fs::read_to_string(patch(t, "a-only")).unwrap()
        );
        same(t, "", &[], text.as_bytes());
    });
}

#[test]
fn input_with_mail_headers_and_trailers() {
    each("c1", |t| {
        let body = fs::read_to_string(patch(t, "a-only")).unwrap();
        let mail = format!(
            "From: someone\nSubject: [PATCH] hi\n\nSome words.\n---\n a.txt | 3 ++-\n\n{body}-- \n2.0\n"
        );
        same(t, "", &["--check"], mail.as_bytes());
        same(t, "", &[], mail.as_bytes());
    });
}

#[test]
fn lists_renames_copies_and_big_changes_like_git() {
    for args in [
        vec!["--stat"],
        vec!["--numstat"],
        vec!["--summary"],
        vec!["--stat", "--numstat", "--summary"],
        vec![],
        vec!["--check"],
        vec!["--index"],
        vec!["--cached"],
        vec!["-R", "--stat"],
    ] {
        for name in ["ext", "ext-text", "big", "copy"] {
            let base = if args.contains(&"-R") { "c3" } else { "c2" };
            each(base, |t| {
                let file = patch(t, name);
                let mut full = args.clone();
                full.push(&file);
                same(t, "", &full, b"");
            });
        }
    }
}

#[test]
fn reverses_a_rename_like_git() {
    each("c3", |t| {
        same(t, "", &["-R", &patch(t, "ext-text")], b"");
    });
    each("c3", |t| {
        same(t, "", &["-R", "--index", &patch(t, "ext-text")], b"");
    });
}

#[test]
fn handles_a_binary_diff_without_data_like_git() {
    each("c2", |t| {
        same(t, "", &["--stat", &patch(t, "bin")], b"");
        same(t, "", &["--numstat", &patch(t, "bin")], b"");
        same(t, "", &["--check", &patch(t, "bin")], b"");
        same(t, "", &[&patch(t, "bin")], b"");
    });
}

#[test]
fn refuses_binary_data_and_unsupported_options_with_a_usage_status() {
    each("c2", |t| {
        let binary = t
            .run_git("", &["diff", "--binary", "c2", "c3", "--", "bin.dat"], b"")
            .stdout;
        let out = t.run_pyn("", &["apply", "--check"], &binary);
        assert_eq!(out.code, 129, "{}", out.err());
        for flag in [
            "--reject",
            "-C1",
            "--ignore-whitespace",
            "--directory=x",
            "--unidiff-zero",
            "-z",
        ] {
            let out = t.run_pyn("", &["apply", flag, &patch(t, "fwd")], b"");
            assert_eq!(out.code, 129, "{flag}: {}", out.err());
        }
        let out = t.run_pyn("", &["apply", "--whitespace=fix", &patch(t, "fwd")], b"");
        assert_ne!(out.code, 0);
    });
}

#[test]
fn merges_with_three_way_like_git() {
    // A change within the hunk's context, away from the changed lines: the plain patch fails, the merge is clean.
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(
                dir,
                "a.txt",
                &lines("line", 12).replace("line5\n", "mine5\n"),
            );
            run(dir, &["add", "a.txt"]);
        }
        same(t, "", &["--3way", &patch(t, "a-only")], b"");
    });
    // A change on the same line: conflict markers and the three stages.
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(
                dir,
                "a.txt",
                &lines("line", 12).replace("line3\n", "mine3\n"),
            );
            run(dir, &["add", "a.txt"]);
        }
        same(t, "", &["--3way", &patch(t, "a-only")], b"");
    });
    // Plain application still works and stages the result.
    each("c1", |t| {
        same(t, "", &["--3way", &patch(t, "fwd")], b"");
    });
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(
                dir,
                "a.txt",
                &lines("line", 12).replace("line5\n", "mine5\n"),
            );
            run(dir, &["add", "a.txt"]);
        }
        same(t, "", &["--3way", "--cached", &patch(t, "a-only")], b"");
    });
    // Without the blob the patch names there is nothing to merge from.
    each("c1", |t| {
        for dir in [&t.git, &t.pyn] {
            write(
                dir,
                "a.txt",
                &lines("line", 12).replace("line5\n", "mine5\n"),
            );
            run(dir, &["add", "a.txt"]);
        }
        let text = fs::read_to_string(patch(t, "a-only")).unwrap();
        let from = text.find("index ").unwrap() + "index ".len();
        let to = from + text[from..].find("..").unwrap();
        let broken = format!("{}1234567{}", &text[..from], &text[to..]);
        same(t, "", &["--3way"], broken.as_bytes());
    });
}

#[test]
fn refuses_whitespace_settings_it_does_not_read() {
    for (key, value) in [
        ("apply.whitespace", "fix"),
        ("core.whitespace", "tab-in-indent"),
    ] {
        each("c1", |t| {
            t.both(&["config", key, value]);
            let out = t.run_pyn("", &["apply", "--check", &patch(t, "fwd")], b"");
            assert_eq!(out.code, 129, "{key}: {}", out.err());
        });
    }
}
