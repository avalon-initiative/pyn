//! `pyn format-patch` on a git repository compared with the real `git format-patch` on a twin of the same
//! repository: standard output, the files written and their names, the exit status and whether standard error
//! carries anything. Only the signature line (git's version, pyn's own) and the date of a cover letter are masked.
//! Real git is the oracle in these tests only.

mod patchfix;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use patchfix::{FORMATS, Twin, commit, lines, run, write};

/// A history with plain, multi-paragraph, long, non-ASCII and awkward-author commits, a merge and an empty commit.
fn history(dir: &Path) {
    let mut tick = 1_700_000_000;
    let alice = "Alice Author <alice@example.com>";
    write(dir, "a.txt", &lines("line", 12));
    write(dir, "b.sh", "#!/bin/sh\necho b\n");
    fs::set_permissions(dir.join("b.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    write(dir, "d/e.txt", "e1\ne2\ne3\n");
    write(dir, "s p.txt", &lines("sp", 8));
    commit(dir, &mut tick, alice, "first");
    run(dir, &["tag", "c1"]);
    write(
        dir,
        "a.txt",
        &lines("line", 12).replace("line3\n", "LINE3\n"),
    );
    write(dir, "new.txt", "brand\nnew\n");
    commit(
        dir,
        &mut tick,
        alice,
        "second subject\n\nbody line one\nbody line two",
    );
    run(dir, &["tag", "c2"]);
    write(dir, "d/e.txt", "e1\nE2\ne3\n");
    commit(
        dir,
        &mut tick,
        "René Ünï <rene@example.com>",
        "Café ünï subject\n\nÜnïcode body é\nsecond line",
    );
    write(
        dir,
        "a.txt",
        &lines("line", 12).replace("line9\n", "nine\n"),
    );
    commit(
        dir,
        &mut tick,
        alice,
        "A very long subject line that goes on and on and on and on and on and on and on forever and ever",
    );
    write(
        dir,
        "a.txt",
        &lines("line", 12).replace("line5\n", "five\n"),
    );
    commit(
        dir,
        &mut tick,
        alice,
        "line one of subject\nline two of subject\n\nbody\n\n\nafter blanks   \n\ttabbed",
    );
    run(dir, &["checkout", "-q", "-b", "side"]);
    write(dir, "side.txt", "side\n");
    commit(dir, &mut tick, "Bob <bob@example.com>", "side work");
    run(dir, &["checkout", "-q", "main"]);
    write(dir, "m.txt", "main\n");
    commit(dir, &mut tick, alice, "main work");
    run(
        dir,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge branch 'side'",
            "side",
        ],
    );
    fs::set_permissions(dir.join("b.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::remove_file(dir.join("new.txt")).unwrap();
    symlink("a.txt", dir.join("link")).unwrap();
    write(dir, "s p.txt", &lines("sp", 8).replace("sp2\n", "SP2\n"));
    write(dir, "d/sub/deep.txt", "deep\n");
    commit(
        dir,
        &mut tick,
        "Doe, John <doe@example.com>",
        "mode, delete and link",
    );
    write(dir, "a.txt", &format!("{}trailing  \n", lines("line", 12)));
    commit(
        dir,
        &mut tick,
        "A. U. Thor <au@example.com>",
        "Fix: the (thing)... [now] ok..\n\nSigned-off-by: Alice Author <alice@example.com>",
    );
    commit(dir, &mut tick, alice, "empty one");
    write(dir, "z.txt", "z\n");
    commit(
        dir,
        &mut tick,
        alice,
        "Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode Ünïcode",
    );
    run(dir, &["tag", "tip"]);
}

fn each(test: impl Fn(&Twin)) {
    for format in FORMATS {
        test(&Twin::new(format, history));
    }
}

/// The signature after `-- ` and the date line of a cover letter differ by design.
fn mask(text: &str, cover: bool) -> String {
    let mut out = Vec::new();
    let mut masked_date = !cover;
    let mut lines = text.split('\n');
    while let Some(line) = lines.next() {
        if line == "-- " {
            out.push(line.to_string());
            if let Some(next) = lines.next() {
                out.push(if next.is_empty() { next } else { "SIGNATURE" }.to_string());
            }
        } else if !masked_date && line.starts_with("Date: ") {
            masked_date = true;
            out.push("Date: NOW".to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

fn same_stdout(t: &Twin, args: &[&str]) {
    let mut full = vec!["format-patch", "--stdout"];
    full.extend_from_slice(args);
    let want = t.run_git("", &full, b"");
    let got = t.run_pyn("", &full, b"");
    let cover = args.contains(&"--cover-letter");
    let what = format!("{} {args:?}", t.format);
    assert_eq!(
        mask(&got.out(), cover),
        mask(&want.out(), cover),
        "stdout of {what} (pyn stderr: {})",
        got.err()
    );
    assert_eq!(
        got.code,
        want.code,
        "exit status of {what} (pyn stderr: {})",
        got.err()
    );
    assert_eq!(
        got.stderr.is_empty(),
        want.stderr.is_empty(),
        "stderr use of {what}: pyn {:?}, git {:?}",
        got.err(),
        want.err()
    );
}

#[test]
fn prints_patches_like_git() {
    each(|t| {
        for args in [
            vec!["-1"],
            vec!["-2"],
            vec!["-3"],
            vec!["-1", "c2"],
            vec!["-2", "c2"],
            vec!["c1"],
            vec!["c1..c2"],
            vec!["c2..tip"],
            vec!["tip~2"],
            vec!["--root", "c2"],
            vec!["--root", "-1", "c1"],
            vec!["-12"],
            vec!["tip"],
            vec!["-n", "-1"],
            vec!["-N", "-3"],
            vec!["-N", "c1..c2"],
            vec!["--subject-prefix=RFC", "-3"],
            vec!["--subject-prefix=RFC PATCH v2", "-1"],
            vec!["--no-signature", "-3"],
            vec!["--signature=hello there", "-2"],
            vec!["--no-stat", "-3"],
            vec!["-p", "-3"],
            vec!["-U1", "-2"],
            vec!["-U0", "-2"],
            vec!["--signoff", "-3"],
            vec!["-s", "c1..c2"],
            vec!["--no-renames", "-2"],
        ] {
            same_stdout(t, &args);
        }
    });
}

#[test]
fn prints_every_awkward_commit_like_git() {
    each(|t| {
        let list = run(&t.git, &["rev-list", "--no-merges", "--reverse", "HEAD"]);
        for id in list.lines() {
            same_stdout(t, &["-1", id]);
        }
    });
}

#[test]
fn writes_a_cover_letter_like_git() {
    each(|t| {
        for args in [
            vec!["--cover-letter", "-1"],
            vec!["--cover-letter", "tip~4..tip"],
            vec!["--cover-letter", "-4"],
            vec!["--cover-letter", "-3"],
            vec!["--cover-letter", "tip~2"],
            vec!["--cover-letter", "-N", "-1"],
            vec!["--cover-letter", "--root", "c2"],
            vec!["--cover-letter", "--root", "-1", "c1"],
            vec!["--cover-letter", "--subject-prefix=RFC", "-n", "-2"],
            vec!["--cover-letter", "--signoff", "-1"],
        ] {
            same_stdout(t, &args);
        }
    });
}

#[test]
fn refuses_a_cover_letter_it_cannot_make_exactly() {
    each(|t| {
        let out = t.run_pyn(
            "",
            &["format-patch", "--stdout", "--cover-letter", "-5"],
            b"",
        );
        assert_eq!(out.code, 129, "{}", out.err());
    });
}

#[test]
fn says_nothing_for_an_empty_range() {
    each(|t| {
        for args in [vec!["HEAD"], vec![], vec!["tip..tip"]] {
            same_stdout(t, &args);
        }
    });
}

#[test]
fn fails_like_git_on_bad_input() {
    each(|t| {
        for args in [
            vec!["nosuchrev"],
            vec!["nosuch..tip"],
            vec!["--stdout", "-o", "out", "-1"],
        ] {
            let mut full = vec!["format-patch"];
            full.extend_from_slice(&args);
            let want = t.run_git("", &full, b"");
            let got = t.run_pyn("", &full, b"");
            assert_eq!(got.code, want.code, "{args:?}: {}", got.err());
            assert_eq!(got.stderr.is_empty(), want.stderr.is_empty(), "{args:?}");
            assert_eq!(got.out(), want.out(), "{args:?}");
        }
    });
}

fn listing(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| {
                let p = e.unwrap().path();
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    mask(&fs::read_to_string(&p).unwrap(), true),
                )
            })
            .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn writes_files_like_git() {
    each(|t| {
        for (i, args) in [
            vec!["-o", "out", "-3"],
            vec!["-o", "out/", "-5"],
            vec!["-o", "deep/er/out", "--suffix=.txt", "c1"],
            vec!["-o", "out", "-q", "-2"],
            vec!["-o", "out", "--cover-letter", "-2"],
            vec!["-o", "out", "-N", "-12", "--subject-prefix=X"],
            vec!["-o", "out", "tip~1"],
        ]
        .iter()
        .enumerate()
        {
            let mut full = vec!["format-patch"];
            full.extend_from_slice(args);
            let want = t.run_git("d", &full, b"");
            let got = t.run_pyn("d", &full, b"");
            let what = format!("{} {args:?}", t.format);
            assert_eq!(got.out(), want.out(), "stdout of {what}: {}", got.err());
            assert_eq!(got.code, want.code, "status of {what}: {}", got.err());
            assert_eq!(got.stderr.is_empty(), want.stderr.is_empty(), "{what}");
            let dir = |root: &Path| {
                let sub = if args.contains(&"deep/er/out") {
                    "deep/er/out"
                } else {
                    "out"
                };
                root.join("d").join(sub)
            };
            assert_eq!(
                listing(&dir(&t.pyn)),
                listing(&dir(&t.git)),
                "files of {what} (run {i})"
            );
            for root in [&t.git, &t.pyn] {
                let _ = fs::remove_dir_all(root.join("d/out"));
                let _ = fs::remove_dir_all(root.join("d/deep"));
            }
        }
    });
}

#[test]
fn refuses_format_settings_it_does_not_read() {
    each(|t| {
        t.both(&["config", "format.numbered", "true"]);
        let out = t.run_pyn("", &["format-patch", "--stdout", "-1"], b"");
        assert_eq!(out.code, 129, "{}", out.err());
        assert!(out.stdout.is_empty());
    });
}
