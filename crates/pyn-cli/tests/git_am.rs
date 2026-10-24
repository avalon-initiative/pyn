//! `pyn am` and the round trip through `pyn format-patch` compared with real git on twin repositories: commit ids
//! (so authors, dates and messages), the exit status, standard output, whether standard error carries anything,
//! the session files in `.git/rebase-apply` and the resulting work tree and index. Real git is the oracle here only.

mod patchfix;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use patchfix::{FORMATS, Twin, commit, lines, run, write};

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
        &lines("line", 12)
            .replace("line9\n", "nine\n")
            .replace("line3\n", "LINE3\n"),
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
        "Doe, John <doe@example.com>",
        "line one of subject\nline two of subject\n\nbody\n\n\nafter blanks\n\ttabbed\nSigned-off-by: Alice <a@example.com>",
    );
    fs::set_permissions(dir.join("b.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::remove_file(dir.join("new.txt")).unwrap();
    symlink("a.txt", dir.join("link")).unwrap();
    write(dir, "s p.txt", &lines("sp", 8).replace("sp2\n", "SP2\n"));
    write(dir, "d/sub/deep.txt", "deep\n");
    commit(
        dir,
        &mut tick,
        "A. U. Thor <au@example.com>",
        "Fix: the (thing)... [now] ok..",
    );
    write(dir, "z.txt", "z\n");
    commit(dir, &mut tick, alice, "last: add z");
    run(dir, &["tag", "tip"]);
}

/// Twins at `c1`; `p/` holds git's patches of `c1..tip` and `q/` pyn's.
fn twin(format: &'static str) -> Twin {
    let t = Twin::new(format, history);
    for (dir, tool) in [("p", "git"), ("q", "pyn")] {
        let out = if tool == "git" {
            t.run_git(
                "",
                &["format-patch", "-o", &format!("../{dir}"), "c1..tip"],
                b"",
            )
        } else {
            t.run_pyn(
                "",
                &["format-patch", "-o", &format!("../{dir}"), "c1..tip"],
                b"",
            )
        };
        assert_eq!(out.code, 0, "{}", out.err());
    }
    t.both(&["reset", "-q", "--hard", "c1"]);
    t
}

fn each(test: impl Fn(&Twin)) {
    for format in FORMATS {
        test(&twin(format));
    }
}

fn patches(t: &Twin, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(t.root.join(dir))
        .unwrap()
        .map(|e| e.unwrap().path().display().to_string())
        .collect();
    names.sort();
    names
}

fn mbox(t: &Twin, dir: &str) -> Vec<u8> {
    patches(t, dir)
        .iter()
        .flat_map(|p| fs::read(p).unwrap())
        .collect()
}

fn am(t: &Twin, args: &[&str], stdin: &[u8]) {
    let mut full = vec!["am"];
    full.extend_from_slice(args);
    t.same("", &full, stdin);
}

#[test]
fn applies_git_patches_like_git() {
    for (dir, extra) in [
        ("p", vec![]),
        ("q", vec![]),
        ("p", vec!["--signoff"]),
        ("q", vec!["-q"]),
    ] {
        each(|t| {
            let names = patches(t, dir);
            let mut args = extra.clone();
            args.extend(names.iter().map(String::as_str));
            am(t, &args, b"");
        });
    }
}

#[test]
fn reads_a_mailbox_from_standard_input() {
    for dir in ["p", "q"] {
        each(|t| am(t, &[], &mbox(t, dir)));
    }
    each(|t| {
        let file = t.root.join("all.mbox");
        fs::write(&file, mbox(t, "p")).unwrap();
        am(t, &[file.to_str().unwrap()], b"");
    });
    each(|t| {
        let names = patches(t, "p");
        let one = fs::read(&names[1]).unwrap();
        am(t, &["-"], &one);
    });
}

#[test]
fn keeps_the_branch_and_reflog_like_git() {
    each(|t| {
        let names = patches(t, "p");
        let mut args = vec!["am"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
        let log = |dir: &Path| {
            run(
                dir,
                &[
                    "log",
                    "--format=%H|%an|%ae|%ad|%cn|%ce|%cd|%s|%b%n--",
                    "--date=iso-strict",
                ],
            )
        };
        assert_eq!(log(&t.pyn), log(&t.git));
        let reflog = |dir: &Path| run(dir, &["reflog", "--format=%gs"]);
        assert_eq!(reflog(&t.pyn), reflog(&t.git));
    });
}

#[test]
fn starts_on_an_unborn_branch_like_git() {
    for format in FORMATS {
        let t = Twin::new(format, |_| {});
        let src = Twin::new(format, history);
        let root = src.run_git("", &["format-patch", "--stdout", "--root", "-1", "c1"], b"");
        assert_eq!(root.code, 0);
        am(&t, &["-"], &root.stdout);
    }
}

#[test]
fn refuses_what_it_cannot_read() {
    let empty = "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001\nFrom: A <a@x.org>\nDate: Tue, 14 Nov 2023 23:13:20 +0100\nSubject: [PATCH] no diff\n\nbody\n";
    for (args, stdin) in [
        (vec![], ""),
        (vec![], "\n\n"),
        (vec![], "just some text\nwithout headers\n"),
        (vec![], empty),
        (vec!["nosuch.mbox"], ""),
        (vec!["--continue"], ""),
        (vec!["--skip"], ""),
        (vec!["--abort"], ""),
        (vec!["--show-current-patch"], ""),
    ] {
        each(|t| am(t, &args, stdin.as_bytes()));
    }
    each(|t| {
        let junk = t.root.join("junk.txt");
        fs::write(&junk, "just some text\nwithout headers\n").unwrap();
        am(t, &[junk.to_str().unwrap()], b"");
    });
}

#[test]
fn handles_a_session_stopped_on_an_empty_patch() {
    let empty = "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001\nFrom: A <a@x.org>\nDate: Tue, 14 Nov 2023 23:13:20 +0100\nSubject: [PATCH] no diff\n\nbody\n";
    for follow in [
        vec!["--continue"],
        vec!["--skip"],
        vec!["--abort"],
        vec!["--quit"],
        vec!["--show-current-patch=diff"],
    ] {
        each(|t| {
            am(t, &[], empty.as_bytes());
            am(t, &follow, b"");
        });
    }
    each(|t| {
        am(t, &[], b"just some text\nwithout headers\n");
        am(t, &["--continue"], b"");
        am(t, &["x.mbox"], b"");
    });
}

#[test]
fn refuses_a_dirty_index_and_a_second_session() {
    each(|t| {
        t.each_dir(|d| {
            write(d, "a.txt", "changed\n");
            run(d, &["add", "a.txt"]);
        });
        let names = patches(t, "p");
        am(t, &[names[0].as_str()], b"");
    });
}

/// `c1` plus a conflicting edit of `a.txt`, so the second patch stops the session.
fn conflicting(t: &Twin) {
    t.each_dir(|d| {
        write(
            d,
            "a.txt",
            &lines("line", 12)
                .replace("line9\n", "other\n")
                .replace("line3\n", "mine\n"),
        );
        run(d, &["add", "-A"]);
        let out = patchfix::git(d)
            .args(["commit", "-q", "-m", "diverge"])
            .env("GIT_AUTHOR_DATE", "1750000000 +0000")
            .env("GIT_COMMITTER_DATE", "1750000000 +0000")
            .output()
            .unwrap();
        assert!(out.status.success());
    });
}

fn session(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir.join(".git/rebase-apply")) else {
        return out;
    };
    for e in rd {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        let text = match name.as_str() {
            "patch" | "msg" | "final-commit" | "next" | "last" | "author-script" | "0001"
            | "0002" | "0003" | "0004" => String::from_utf8_lossy(&fs::read(e.path()).unwrap())
                .replace("2.43.0", "V")
                .replace("2.55.0", "V"),
            _ => String::new(),
        };
        out.push((name, text));
    }
    out.sort();
    out
}

fn stopped(t: &Twin, dir: &str) -> Vec<String> {
    conflicting(t);
    let names = patches(t, dir);
    let mut args = vec!["am"];
    args.extend(names.iter().map(String::as_str));
    let (want, _) = t.same("", &args, b"");
    assert_eq!(want.code, 128, "{}", want.err());
    names
}

#[test]
fn stops_on_a_conflict_like_git() {
    each(|t| {
        stopped(t, "p");
        let (g, p) = (session(&t.git), session(&t.pyn));
        let names = |s: &[(String, String)]| s.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        assert_eq!(names(&p), names(&g), "files of the session");
        for ((name, want), (_, got)) in g.iter().zip(&p) {
            if !want.is_empty() {
                assert_eq!(got, want, "session file {name}");
            }
        }
    });
}

#[test]
fn aborts_a_stopped_session_like_git() {
    each(|t| {
        stopped(t, "p");
        am(t, &["--abort"], b"");
    });
}

#[test]
fn skips_a_patch_like_git() {
    each(|t| {
        stopped(t, "p");
        am(t, &["--skip"], b"");
        am(t, &["--skip"], b"");
        am(t, &["--skip"], b"");
    });
}

#[test]
fn continues_after_resolving_like_git() {
    each(|t| {
        stopped(t, "p");
        am(t, &["--continue"], b"");
        t.each_dir(|d| {
            write(
                d,
                "a.txt",
                &lines("line", 12)
                    .replace("line3\n", "LINE3\n")
                    .replace("line9\n", "other\n"),
            );
            run(d, &["add", "a.txt"]);
        });
        am(t, &["--continue"], b"");
        am(t, &["--continue"], b"");
    });
}

#[test]
fn shows_the_current_patch_like_git() {
    let mask = |text: String| -> String {
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        if let Some(at) = lines.iter().position(|l| l == "-- ") {
            lines[at + 1] = "SIGNATURE".into();
        }
        lines.join("\n")
    };
    each(|t| {
        stopped(t, "p");
        for format in [
            "--show-current-patch=diff",
            "--show-current-patch=raw",
            "--show-current-patch",
        ] {
            let want = t.run_git("", &["am", format], b"");
            let got = t.run_pyn("", &["am", format], b"");
            assert_eq!(got.code, want.code, "{format}");
            assert_eq!(mask(got.out()), mask(want.out()), "{format}");
        }
    });
}

#[test]
fn quits_a_session_keeping_the_work() {
    each(|t| {
        stopped(t, "p");
        am(t, &["--quit"], b"");
    });
}

#[test]
fn falls_back_on_a_three_way_merge_like_git() {
    each(|t| {
        t.both(&["config", "merge.conflictStyle", "diff3"]);
        conflicting(t);
        let names = patches(t, "p");
        let mut args = vec!["am", "--3way"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
    });
}

const FAKE_SIGNER: &str = r#"#!/bin/sh
sum=$(cksum | cut -d' ' -f1)
echo "[GNUPG:] KEY_CONSIDERED 0 0" >&2
echo "[GNUPG:] SIG_CREATED D 1 8 00 1 ffff" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfake%s\n\n-----END PGP SIGNATURE-----\n' "$sum"
"#;

fn with_signer(t: &Twin) {
    let path = t.root.join("signer");
    fs::write(&path, FAKE_SIGNER).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    t.both(&["config", "gpg.program", path.to_str().unwrap()]);
}

#[test]
fn signs_the_commits_it_makes_like_git() {
    for (config, flags) in [
        (true, vec![]),
        (false, vec!["-S"]),
        (true, vec!["--no-gpg-sign"]),
        (false, vec!["-SABCDEF"]),
        (false, vec![]),
    ] {
        each(|t| {
            with_signer(t);
            if config {
                t.both(&["config", "commit.gpgsign", "true"]);
            }
            let names = patches(t, "p");
            let mut args = flags.clone();
            args.extend(names.iter().map(String::as_str));
            am(t, &args, b"");
            let raw = |d: &Path| run(d, &["cat-file", "commit", "HEAD"]);
            assert_eq!(raw(&t.pyn), raw(&t.git));
        });
    }
}

#[test]
fn a_stopped_session_signs_after_continue_like_git() {
    each(|t| {
        with_signer(t);
        conflicting(t);
        let names = patches(t, "p");
        let mut args = vec!["am", "-S"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
        t.each_dir(|d| {
            write(
                d,
                "a.txt",
                &lines("line", 12)
                    .replace("line3\n", "LINE3\n")
                    .replace("line9\n", "other\n"),
            );
            run(d, &["add", "a.txt"]);
        });
        am(t, &["--continue"], b"");
        let raw = |d: &Path| run(d, &["cat-file", "commit", "HEAD"]);
        assert_eq!(raw(&t.pyn), raw(&t.git));
    });
}

#[test]
fn git_carries_on_a_session_pyn_stopped_and_the_other_way_round() {
    each(|t| {
        conflicting(t);
        let names = patches(t, "p");
        let mut args = vec!["am"];
        args.extend(names.iter().map(String::as_str));
        let (g, p) = (t.run_git("", &args, b""), t.run_pyn("", &args, b""));
        assert_eq!((g.code, p.code), (128, 128));
        t.each_dir(|d| {
            write(
                d,
                "a.txt",
                &lines("line", 12)
                    .replace("line3\n", "LINE3\n")
                    .replace("line9\n", "other\n"),
            );
            run(d, &["add", "a.txt"]);
        });
        // pyn's directory is continued by git, git's by pyn.
        let by_git = t.exec("git", &t.pyn, &["am", "--continue"], b"");
        let by_pyn = t.exec(
            env!("CARGO_BIN_EXE_pyn"),
            &t.git,
            &["am", "--continue"],
            b"",
        );
        assert_eq!(
            by_git.code,
            by_pyn.code,
            "{} / {}",
            by_git.err(),
            by_pyn.err()
        );
        assert_eq!(patchfix::snapshot(&t.pyn), patchfix::snapshot(&t.git));
        assert_eq!(
            run(&t.pyn, &["log", "--format=%H %s"]),
            run(&t.git, &["log", "--format=%H %s"])
        );
    });
}

#[test]
fn three_way_continue_and_skip_like_git() {
    each(|t| {
        t.both(&["config", "merge.conflictStyle", "diff3"]);
        conflicting(t);
        let names = patches(t, "p");
        let mut args = vec!["am", "--3way"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
        am(t, &["--skip"], b"");
        am(t, &["--skip"], b"");
    });
    each(|t| {
        t.both(&["config", "merge.conflictStyle", "diff3"]);
        conflicting(t);
        let names = patches(t, "p");
        let mut args = vec!["am", "--3way"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
        t.each_dir(|d| {
            write(
                d,
                "a.txt",
                &lines("line", 12)
                    .replace("line3\n", "LINE3\n")
                    .replace("line9\n", "other\n"),
            );
            run(d, &["add", "a.txt"]);
        });
        am(t, &["--continue"], b"");
        am(t, &["--abort"], b"");
    });
}

#[test]
fn merges_a_deletion_and_a_mode_change_like_git() {
    each(|t| {
        t.both(&["config", "merge.conflictStyle", "diff3"]);
        t.each_dir(|d| {
            let text = fs::read_to_string(d.join("s p.txt")).unwrap();
            write(d, "s p.txt", &text.replace("sp4\n", "mine4\n"));
            run(d, &["add", "-A"]);
            let out = patchfix::git(d)
                .args(["commit", "-q", "-m", "diverge"])
                .env("GIT_AUTHOR_DATE", "1750000000 +0000")
                .env("GIT_COMMITTER_DATE", "1750000000 +0000")
                .output()
                .unwrap();
            assert!(out.status.success());
        });
        let names = patches(t, "p");
        let mut args = vec!["am", "--3way"];
        args.extend(names.iter().map(String::as_str));
        t.same("", &args, b"");
    });
}

#[test]
fn refuses_settings_it_does_not_read() {
    for (key, value) in [("am.threeWay", "true"), ("apply.whitespace", "fix")] {
        each(|t| {
            t.both(&["config", key, value]);
            let names = patches(t, "p");
            let out = t.run_pyn("", &["am", names[0].as_str()], b"");
            assert_eq!(out.code, 129, "{key}: {}", out.err());
            assert!(!t.pyn.join(".git/rebase-apply").exists());
        });
    }
}

#[test]
fn reports_whitespace_errors_like_git() {
    let mail = "From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001\nFrom: A <a@x.org>\nDate: Tue, 14 Nov 2023 23:13:20 +0100\nSubject: [PATCH] ws\n\n---\n a.txt | 1 +\n\ndiff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@\n line1\n+added  \n line2\n";
    for flags in [
        vec![],
        vec!["--whitespace=nowarn"],
        vec!["--whitespace=warn"],
    ] {
        each(|t| {
            let mut args = flags.clone();
            args.push("-");
            am(t, &args, mail.as_bytes());
        });
    }
}
