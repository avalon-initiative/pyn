//! Rename and copy detection on a git repository compared with real git on the same repository: patches, stats,
//! name-status, summaries and the short status. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    format: &'static str,
}

impl Repo {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap().join("repo");
        init(&dir, format, false);
        Self {
            _tmp: tmp,
            dir,
            format,
        }
    }

    fn write(&self, path: &str, content: &[u8]) {
        let file = self.dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn mv(&self, from: &str, to: &str) {
        fs::create_dir_all(self.dir.join(to).parent().unwrap()).unwrap();
        self.git(&["mv", from, to]);
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.dir.join(path)).unwrap();
    }

    fn chmod_x(&self, path: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(self.dir.join(path), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let out = git(&self.dir).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Runs the first commit's files through `before`, then `after` as the second commit.
    fn two_commits(&self, before: impl Fn(&Self), after: impl Fn(&Self)) {
        before(self);
        self.commit("one");
        after(self);
        self.commit("two");
    }

    fn pyn(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.dir.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    /// Same standard output and exit status as git; returns that output.
    fn same(&self, args: &[&str]) -> String {
        let want = git(&self.dir).env("TZ", "UTC").args(args).output().unwrap();
        let got = self.pyn(args);
        assert_eq!(
            got.status.code(),
            want.status.code(),
            "{} {args:?}: {}",
            self.format,
            String::from_utf8_lossy(&got.stderr)
        );
        let (got, want) = (
            String::from_utf8_lossy(&got.stdout).into_owned(),
            String::from_utf8_lossy(&want.stdout).into_owned(),
        );
        assert_eq!(got, want, "{} {args:?}", self.format);
        got
    }

    /// Every view of the last commit and of the history, with `flags` added to each.
    fn views(&self, flags: &[&str]) -> Vec<String> {
        let mut seen = Vec::new();
        for base in [
            &["diff", "HEAD~1", "HEAD"][..],
            &["diff", "HEAD~1", "HEAD", "--stat"],
            &["diff", "HEAD~1", "HEAD", "--numstat"],
            &["diff", "HEAD~1", "HEAD", "--shortstat"],
            &["diff", "HEAD~1", "HEAD", "--name-status"],
            &["diff", "HEAD~1", "HEAD", "--name-only"],
            &["diff", "HEAD~1", "HEAD", "--summary"],
            &["diff", "HEAD~1", "HEAD", "--stat", "--summary"],
            &["show"],
            &["show", "--stat"],
            &["show", "--summary", "--format="],
            &["show", "--name-status", "--format="],
            &["log", "-p", "--format=%s"],
            &["log", "--stat", "--summary", "--format=%s"],
            &["log", "--name-status", "--format=%s"],
        ] {
            let mut args = base.to_vec();
            args.extend_from_slice(flags);
            seen.push(self.same(&args));
        }
        seen
    }
}

fn each_format(check: impl Fn(&Repo)) {
    for format in FORMATS {
        check(&Repo::new(format));
    }
}

/// `n` distinct 40-byte lines tagged `tag`.
fn lines(tag: &str, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| format!("{tag}-{i:03}-abcdefghijklmnopqrstuvwxy\n"))
        .collect::<String>()
        .into_bytes()
}

/// `lines` with the first `edited` lines replaced.
fn edited(tag: &str, n: usize, edited: usize) -> Vec<u8> {
    let mut text = String::from_utf8(lines(tag, n)).unwrap();
    for i in 0..edited {
        text = text.replacen(
            &format!("{tag}-{i:03}-abcdefghijklmnopqrstuvwxy"),
            &format!("{tag}-{i:03}-CHANGED-qrstuvwxyzabcdefgh"),
            1,
        );
    }
    text.into_bytes()
}

#[test]
fn an_exact_rename_is_a_rename_in_every_view() {
    each_format(|r| {
        r.two_commits(
            |r| {
                r.write("a.txt", &lines("a", 12));
                r.write("keep.txt", &lines("k", 3));
            },
            |r| {
                r.mv("a.txt", "moved.txt");
                r.write("moved/other.txt", &lines("o", 2));
            },
        );
        let views = r.views(&[]);
        assert!(views[0].contains("rename from a.txt\nrename to moved.txt\n"));
        assert!(views[0].contains("similarity index 100%\n"));
        assert!(views[4].contains("R100\ta.txt\tmoved.txt\n"));
        assert!(views[6].contains(" rename a.txt => moved.txt (100%)\n"));
        // A path filter applies before pairing, so half a rename is a deletion or an addition.
        for path in ["a.txt", "moved.txt", "keep.txt"] {
            for flags in [&[][..], &["--name-status"], &["--stat", "--summary"]] {
                let mut args = vec!["diff", "HEAD~1", "HEAD"];
                args.extend_from_slice(flags);
                args.extend(["--", path]);
                r.same(&args);
            }
        }
        r.same(&["log", "--format=%s", "--name-status", "--", "moved.txt"]);
        r.same(&["log", "-p", "--format=%s", "-S", "abcdefghij", "-M"]);
    });
}

#[test]
fn renames_around_the_similarity_threshold_score_as_git_does() {
    each_format(|r| {
        r.two_commits(
            |r| {
                for k in 0..=20 {
                    r.write(&format!("p{k}/old.txt"), &lines(&format!("f{k}"), 20));
                }
            },
            |r| {
                for k in 0..=20 {
                    r.remove(&format!("p{k}/old.txt"));
                    r.write(&format!("p{k}/new.txt"), &edited(&format!("f{k}"), 20, k));
                }
            },
        );
        for flags in [
            &[][..],
            &["-M10%"],
            &["-M40%"],
            &["-M50%"],
            &["-M51%"],
            &["-M75%"],
            &["-M90%"],
            &["-M99%"],
            &["-M100%"],
            &["-M5"],
            &["-M0"],
            &["-C0%"],
            &["--find-renames=60%"],
            &["--no-renames"],
        ] {
            let mut args = vec!["diff", "HEAD~1", "HEAD", "--name-status"];
            args.extend_from_slice(flags);
            r.same(&args);
        }
        r.views(&[]);
    });
}

#[test]
fn growth_and_shrinkage_are_judged_by_size_like_git() {
    each_format(|r| {
        r.two_commits(
            |r| {
                for k in 0..8 {
                    r.write(&format!("d{k}/in.txt"), &lines(&format!("s{k}"), 10));
                }
            },
            |r| {
                for k in 0..8 {
                    r.remove(&format!("d{k}/in.txt"));
                    let mut text = lines(&format!("s{k}"), 10);
                    text.extend(lines(&format!("extra{k}"), k * 3));
                    r.write(&format!("d{k}/out.txt"), &text);
                }
            },
        );
        for flags in [&[][..], &["-M30%"], &["-M70%"]] {
            r.views(flags);
        }
    });
}

#[test]
fn a_rename_with_a_mode_change_prints_modes_before_the_rename_lines() {
    each_format(|r| {
        r.two_commits(
            |r| {
                r.write("run.sh", &lines("sh", 8));
                r.write("tool.sh", &lines("tl", 8));
            },
            |r| {
                r.mv("run.sh", "bin/run.sh");
                r.chmod_x("bin/run.sh");
                r.mv("tool.sh", "bin/tool.sh");
                r.write("bin/tool.sh", &edited("tl", 8, 1));
                r.chmod_x("bin/tool.sh");
            },
        );
        let views = r.views(&[]);
        assert!(views[0].contains("old mode 100644\nnew mode 100755\nsimilarity index 100%\n"));
    });
}

#[test]
fn a_copy_and_a_rename_of_one_source_follow_gits_pairing() {
    each_format(|r| {
        r.two_commits(
            |r| {
                r.write("src.txt", &lines("s", 15));
                r.write("stay.txt", &lines("t", 15));
                r.write("gone.txt", &lines("g", 15));
            },
            |r| {
                // A copy of an unchanged file, of a modified one, and two takers of a deleted one.
                r.write("copy_of_stay.txt", &lines("t", 15));
                r.write("src.txt", &edited("s", 15, 2));
                r.write("copy_of_src.txt", &lines("s", 15));
                r.remove("gone.txt");
                r.write("gone_a.txt", &lines("g", 15));
                r.write("gone_b.txt", &edited("g", 15, 1));
            },
        );
        for flags in [
            &[][..],
            &["-C"],
            &["-C", "-M90%"],
            &["-C100%"],
            &["--find-copies=60%"],
        ] {
            r.views(flags);
        }
        let out = r.same(&["diff", "HEAD~1", "HEAD", "-C", "--name-status"]);
        assert!(out.contains("C"), "{out}");
    });
}

/// Copies of unchanged files, of modified ones and a rename, with whole and edited content.
fn harder_repo(r: &Repo) {
    r.two_commits(
        |r| {
            r.write("stay.txt", &lines("t", 15));
            r.write("dir/stay2.txt", &lines("u", 15));
            r.write("src.txt", &lines("s", 15));
            r.write("gone.txt", &lines("g", 15));
        },
        |r| {
            r.write("copy_of_stay.txt", &lines("t", 15));
            r.write("near/stay_edit.txt", &edited("t", 15, 2));
            r.write("dir/stay2_copy.txt", &lines("u", 15));
            r.write("src.txt", &edited("s", 15, 2));
            r.write("copy_of_src.txt", &lines("s", 15));
            r.remove("gone.txt");
            r.write("gone_a.txt", &lines("g", 15));
            r.write("gone_b.txt", &edited("g", 15, 1));
        },
    );
}

#[test]
fn find_copies_harder_takes_unmodified_files_as_sources() {
    each_format(|r| {
        harder_repo(r);
        let out = r.same(&["diff", "HEAD~1", "HEAD", "-C", "-C", "--name-status"]);
        assert!(out.contains("C100\tstay.txt\tcopy_of_stay.txt\n"), "{out}");
        for flags in [
            &["-C", "-C"][..],
            &["--find-copies-harder"],
            &["-C", "--find-copies-harder"],
            &["--find-copies-harder", "--no-renames"],
            &["--no-renames", "--find-copies-harder", "-M90%"],
            &["-C", "-C", "-M90%"],
            &["-C90%", "-C"],
            &["-C", "-C60%"],
            &["--find-copies=70%", "--find-copies"],
            &["-C", "-M", "-C"],
            &["-M", "-C", "-C"],
            &["--no-renames", "-C", "-C"],
            &["-C", "-C", "--no-renames"],
            &["-C", "-C", "-l1"],
            &["-C", "-C", "-l3"],
            &["-C", "-C", "-l0"],
        ] {
            r.views(flags);
        }
        for path in ["copy_of_stay.txt", "near", "dir"] {
            r.same(&[
                "diff",
                "HEAD~1",
                "HEAD",
                "-C",
                "-C",
                "--name-status",
                "--",
                path,
            ]);
        }
        r.same(&["log", "-p", "--format=%s", "-C", "-C", "--", "near"]);
        r.same(&["show", "--name-status", "--format=", "--find-copies-harder"]);
        r.same(&["diff", "HEAD~1", "HEAD", "-C", "-C", "--raw"]);
        r.same(&["diff", "HEAD~1", "HEAD", "-C", "-C", "--dirstat=files"]);
        r.same(&[
            "log",
            "--name-status",
            "--format=%s",
            "-C",
            "-C",
            "-S",
            "t-001",
        ]);
    });
}

#[test]
fn a_bare_c_over_copies_config_is_find_copies_harder() {
    each_format(|r| {
        harder_repo(r);
        r.git(&["config", "diff.renames", "copies"]);
        for flags in [
            &[][..],
            &["-C"],
            &["-C", "-C"],
            &["-M", "-C"],
            &["--no-renames", "-C"],
            &["--find-copies"],
            &["-C", "-M"],
        ] {
            r.views(flags);
        }
    });
}

#[test]
fn find_copies_harder_in_the_work_tree_and_index() {
    each_format(|r| {
        r.write("stay.txt", &lines("t", 15));
        r.write("src.txt", &lines("s", 15));
        r.write("skip.txt", &lines("k", 15));
        r.commit("one");
        r.write("src.txt", &edited("s", 15, 2));
        r.write("copy.txt", &lines("t", 15));
        r.write("copy_k.txt", &lines("k", 15));
        r.git(&["add", "-N", "copy.txt", "copy_k.txt"]);
        for setup in [
            None,
            Some(&["update-index", "--skip-worktree", "skip.txt"][..]),
        ] {
            if let Some(args) = setup {
                r.git(args);
            }
            for args in [
                &["diff", "-C", "-C", "--name-status"][..],
                &["diff", "-C", "-C"],
                &["diff", "-C", "-C", "--stat", "--summary"],
                &["diff", "HEAD", "-C", "-C", "--name-status"],
                &["diff", "HEAD", "--find-copies-harder", "--stat"],
                &["diff", "-C", "-C", "--name-status", "--", "copy.txt"],
            ] {
                r.same(args);
            }
        }
        r.git(&["add", "-A"]);
        for args in [
            &["diff", "--cached", "-C", "-C", "--name-status"][..],
            &["diff", "--cached", "-C", "-C"],
            &["diff", "--cached", "--find-copies-harder", "--summary"],
            &[
                "diff",
                "--cached",
                "-C",
                "-C",
                "--name-status",
                "--",
                "copy.txt",
            ],
        ] {
            r.same(args);
        }
        // Git's stash cannot push in a sha256 repository.
        if r.format != "sha1" {
            return;
        }
        r.git(&["update-index", "--no-skip-worktree", "skip.txt"]);
        r.git(&["stash", "push", "-q"]);
        for args in [
            &["stash", "show", "-C", "-C", "--name-status"][..],
            &["stash", "show", "--find-copies-harder", "-p"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn find_copies_harder_in_merge_views() {
    let r = Repo::new("sha1");
    r.write("stay.txt", &lines("t", 15));
    r.write("src.txt", &lines("s", 15));
    r.commit("base");
    r.git(&["checkout", "-q", "-b", "side"]);
    r.write("copy.txt", &lines("t", 15));
    r.write("src.txt", &edited("s", 15, 2));
    r.commit("side");
    r.git(&["checkout", "-q", "main"]);
    r.write("stay.txt", &edited("t", 15, 1));
    r.commit("main");
    r.git(&["merge", "-q", "side", "-m", "merge"]);
    for flags in [&["-C", "-C"][..], &["--find-copies-harder", "-M"]] {
        for base in [
            &["show", "-m", "--name-status", "--format=%s"][..],
            &["show", "-c", "--format=%s"],
            &["show", "--cc", "--format=%s"],
            &["show", "--first-parent", "--stat", "--format=%s"],
            &["show", "--remerge-diff", "--format=%s"],
            &["log", "-m", "-p", "--format=%s"],
        ] {
            let mut args = base.to_vec();
            args.extend_from_slice(flags);
            r.same(&args);
        }
    }
}

#[test]
fn pickaxe_pairs_with_the_detection_the_command_line_asks_for() {
    each_format(|r| {
        r.two_commits(
            |r| {
                r.write("stay.txt", &lines("t", 15));
                r.write("src.txt", &lines("s", 15));
                r.write("gone.txt", &lines("g", 15));
                r.write("old/mv.txt", &lines("m", 15));
            },
            |r| {
                r.write("copy_of_stay.txt", &lines("t", 15));
                r.write("src.txt", &edited("s", 15, 2));
                r.write("copy_of_src.txt", &edited("s", 15, 1));
                r.remove("gone.txt");
                r.write("gone_edit.txt", &edited("g", 15, 3));
                r.mv("old/mv.txt", "new/mv.txt");
            },
        );
        for config in [None, Some("false"), Some("copies")] {
            if let Some(value) = config {
                r.git(&["config", "diff.renames", value]);
            }
            for flags in [
                &[][..],
                &["-M"],
                &["-M90%"],
                &["-C"],
                &["-C", "-C"],
                &["--find-copies-harder"],
                &["--no-renames"],
                &["--no-renames", "-C", "-C"],
                &["-l1"],
            ] {
                for pick in [
                    &["-S", "t-001"][..],
                    &["-S", "CHANGED"],
                    &["-S", "g-005"],
                    &["-G", "s-00"],
                    &["-G", "CHANGED"],
                    &["-S", "m-003"],
                ] {
                    let mut args = vec!["log", "--format=%s", "--name-status"];
                    args.extend_from_slice(pick);
                    args.extend_from_slice(flags);
                    r.same(&args);
                    r.same(
                        &["log", "--format=%s", "-p", pick[0], pick[1]]
                            .iter()
                            .chain(flags)
                            .copied()
                            .collect::<Vec<_>>(),
                    );
                }
            }
        }
    });
}

#[test]
fn unusual_path_characters_are_quoted_in_rename_and_copy_lines() {
    each_format(|r| {
        let names = [
            ("plain old.txt", "tab\there.txt"),
            ("quote\"d.txt", "back\\slash.txt"),
            ("caf\u{e9}.txt", "na\u{ef}ve dir/\u{fc}ber.txt"),
            ("a\nb.txt", "c\u{1}d.txt"),
            ("del\u{7f}.txt", "plain new.txt"),
        ];
        r.two_commits(
            |r| {
                for (i, (from, _)) in names.iter().enumerate() {
                    r.write(from, &lines(&format!("n{i}"), 12));
                }
                r.write("stay \u{e9}.txt", &lines("stay", 12));
                r.write("bin \u{e9}.bin", b"\0binary content");
                r.write("bin2 \"x\".bin", b"\0other binary content here");
            },
            |r| {
                for (i, (from, to)) in names.iter().enumerate() {
                    r.remove(from);
                    r.write(to, &edited(&format!("n{i}"), 12, i));
                }
                r.write("copy \"q\".txt", &lines("stay", 12));
                r.write("bin \u{e9}.bin", b"\0binary changed content");
                r.write("bin2 \"x\".bin", b"\0other binary content here too");
            },
        );
        for quote in [None, Some("false")] {
            if let Some(value) = quote {
                r.git(&["config", "core.quotePath", value]);
            }
            for flags in [&[][..], &["-C", "-C"], &["-M60%"], &["--no-renames"]] {
                r.views(flags);
                let mut raw = vec!["diff", "HEAD~1", "HEAD", "--raw", "-z"];
                raw.extend_from_slice(flags);
                r.same(&raw);
                let mut dirstat = vec!["diff", "HEAD~1", "HEAD", "--numstat"];
                dirstat.extend_from_slice(flags);
                r.same(&dirstat);
            }
        }
    });
}

#[test]
fn several_candidates_are_paired_in_gits_order() {
    each_format(|r| {
        r.two_commits(
            |r| {
                // Identical sources in different folders, one sharing the new file's name.
                r.write("one/same.txt", &lines("x", 6));
                r.write("two/other.txt", &lines("x", 6));
                r.write("three/third.txt", &lines("x", 6));
                // One source that two new files resemble to different degrees.
                r.write("src/base.rs", &lines("b", 20));
                // Two sources and two destinations that cross.
                r.write("m/left.txt", &lines("l", 20));
                r.write("m/right.txt", &lines("r", 20));
            },
            |r| {
                r.remove("one/same.txt");
                r.remove("two/other.txt");
                r.remove("three/third.txt");
                r.write("new/same.txt", &lines("x", 6));
                r.write("new/a.txt", &lines("x", 6));
                r.write("new/b.txt", &lines("x", 6));
                r.remove("src/base.rs");
                r.write("dst/closer.rs", &edited("b", 20, 3));
                r.write("dst/far.rs", &edited("b", 20, 8));
                r.remove("m/left.txt");
                r.remove("m/right.txt");
                let mut mixed = lines("l", 12);
                mixed.extend(lines("r", 8));
                r.write("n/first.txt", &mixed);
                let mut other = lines("r", 14);
                other.extend(lines("l", 6));
                r.write("n/second.txt", &other);
            },
        );
        for flags in [
            &[][..],
            &["-M30%"],
            &["-C"],
            &["--no-renames"],
            &["-C", "-C"],
        ] {
            r.views(flags);
        }
    });
}

#[test]
fn binary_files_are_paired_by_chunk_similarity() {
    each_format(|r| {
        // Whole 64-byte spans with no newline: git 2.43 and 2.55 differ on a final partial span.
        let blob = |seed: u8, len: usize| -> Vec<u8> {
            (0..len)
                .map(|i| match (i as u8).wrapping_mul(31).wrapping_add(seed) {
                    0 | 10 => 11,
                    b => b,
                })
                .collect()
        };
        let mut a = blob(1, 2048);
        a[10] = 0;
        let mut b = blob(7, 2048);
        b[10] = 0;
        let mut b_edited = b.clone();
        b_edited[1500] = 99;
        r.two_commits(
            |r| {
                r.write("img/a.bin", &a);
                r.write("img/b.bin", &b);
                r.write("img/small.bin", b"\0ab");
            },
            |r| {
                r.remove("img/a.bin");
                r.write("pics/a.bin", &a);
                r.remove("img/b.bin");
                r.write("pics/b2.bin", &b_edited);
                r.remove("img/small.bin");
                r.write("pics/small2.bin", b"\0abc");
            },
        );
        let views = r.views(&[]);
        assert!(views[0].contains("rename from img/a.bin"));
        r.views(&["-M99%"]);
        r.views(&["--no-renames"]);
    });
}

#[test]
fn moving_a_directory_pairs_every_file() {
    each_format(|r| {
        r.two_commits(
            |r| {
                for i in 0..6 {
                    r.write(&format!("lib/mod/f{i}.rs"), &lines(&format!("m{i}"), 9));
                }
                r.write("lib/mod/deep/g.rs", &lines("deep", 9));
            },
            |r| {
                r.mv("lib/mod", "src/module");
                r.write("src/module/f2.rs", &edited("m2", 9, 2));
                r.write("src/module/f3.rs", &edited("m3", 9, 6));
            },
        );
        let views = r.views(&[]);
        assert!(views[1].contains(" => "), "{}", views[1]);
        r.views(&["-M90%"]);
    });
}

#[test]
fn flags_and_settings_turn_detection_on_and_off() {
    each_format(|r| {
        r.two_commits(
            |r| {
                r.write("a.txt", &lines("a", 10));
                r.write("b.txt", &lines("b", 10));
            },
            |r| {
                r.mv("a.txt", "a2.txt");
                r.remove("b.txt");
                r.write("b2.txt", &edited("b", 10, 3));
            },
        );
        for setting in [
            None,
            Some(("diff.renames", "false")),
            Some(("diff.renames", "true")),
            Some(("diff.renames", "copies")),
            Some(("diff.renameLimit", "1")),
            Some(("diff.renames", "false")),
        ] {
            if let Some((key, value)) = setting {
                r.git(&["config", key, value]);
            }
            for flags in [
                &[][..],
                &["-M"],
                &["-C"],
                &["--no-renames"],
                &["--no-renames", "-M"],
                &["-M", "--no-renames"],
                &["-M70%", "-C"],
                &["--find-renames"],
                &["--find-copies"],
                &["-l1"],
                &["-l0"],
                &["-M90%", "-l1"],
            ] {
                r.views(flags);
            }
        }
    });
}

#[test]
fn unsupported_and_invalid_switches_are_usage_errors() {
    let r = Repo::new("sha1");
    r.two_commits(
        |r| r.write("a", &lines("a", 5)),
        |r| r.write("b", &lines("a", 5)),
    );
    for args in [
        &["diff", "-Mnot-a-number"][..],
        &["diff", "-Cx"],
        &["diff", "-C", "-C", "-Mx"],
    ] {
        let out = r.pyn(args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn status_shows_staged_renames_and_copies() {
    each_format(|r| {
        r.write("a.txt", &lines("a", 20));
        r.write("b.txt", &lines("b", 20));
        r.write("c.txt", &lines("c", 20));
        r.write("d.txt", &lines("d", 20));
        r.commit("one");
        r.mv("a.txt", "dir/a.txt");
        r.mv("b.txt", "b2.txt");
        r.write("b2.txt", &edited("b", 20, 4));
        r.write("c_copy.txt", &lines("c", 20));
        r.write("d.txt", &edited("d", 20, 1));
        r.write("d_copy.txt", &edited("d", 20, 2));
        r.write("untracked.txt", b"u\n");
        r.git(&["add", "dir", "b2.txt", "c_copy.txt", "d.txt", "d_copy.txt"]);
        r.write("dir/a.txt", &edited("a", 20, 1));
        for setting in [
            None,
            Some(("status.renames", "false")),
            Some(("status.renames", "copies")),
            Some(("status.renames", "true")),
            Some(("diff.renames", "false")),
            Some(("status.renames", "false")),
            Some(("diff.renames", "copies")),
        ] {
            if let Some((key, value)) = setting {
                r.git(&["config", key, value]);
            }
            for args in [
                &["status", "-s"][..],
                &["status", "--porcelain"],
                &["status", "-sb"],
                &["status", "--porcelain", "dir"],
                &["status", "-s", "--", "b2.txt"],
                &["diff", "--cached", "--name-status"],
                &["diff", "--cached", "--stat"],
                &["diff", "--cached"],
                &["diff", "--cached", "-C", "--name-status"],
                &["diff", "--name-status"],
                &["diff", "HEAD", "--name-status"],
                &["diff", "HEAD", "--stat", "--summary"],
            ] {
                r.same(args);
            }
        }
    });
}

#[test]
fn status_renames_flags_follow_gits_option_order() {
    each_format(|r| {
        r.write("a.txt", &lines("a", 20));
        r.write("b.txt", &lines("b", 20));
        r.write("c.txt", &lines("c", 20));
        r.write("d.txt", &lines("d", 20));
        r.commit("one");
        r.mv("a.txt", "a2.txt");
        r.mv("b.txt", "b2.txt");
        r.write("b2.txt", &edited("b", 20, 8));
        r.write("c_copy.txt", &lines("c", 20));
        r.write("d.txt", &edited("d", 20, 1));
        r.write("d_copy.txt", &edited("d", 20, 2));
        r.git(&["add", "-A"]);
        for setting in [None, Some("false"), Some("true"), Some("copies")] {
            if let Some(value) = setting {
                r.git(&["config", "status.renames", value]);
            }
            for flags in [
                &[][..],
                &["--renames"],
                &["--no-renames"],
                &["--renames", "--no-renames"],
                &["--no-renames", "--renames"],
                &["-M"],
                &["-M90%"],
                &["-M30%"],
                &["-M", "--no-renames"],
                &["--no-renames", "-M"],
                &["--no-renames", "-M30%"],
                &["--renames", "--no-renames", "-M30%"],
                &["--find-renames"],
                &["--find-renames=30%"],
                &["--find-renames=95%"],
                &["--find-renames=100%"],
                &["-M40%", "-M90%"],
                &["-M=30%"],
                &["-Mx"],
                &["-M9x"],
                &["--find-renames="],
                &["-M.3"],
                &["--renames", "-M30%"],
            ] {
                for base in [
                    &["status", "-s"][..],
                    &["status", "--porcelain"],
                    &["status", "-sb", "--", "b2.txt", "d_copy.txt"],
                ] {
                    let mut args = base.to_vec();
                    args.splice(1..1, flags.iter().copied());
                    r.same(&args);
                }
            }
        }
        for bad in [&["status", "--renames=1"][..], &["status", "-C"]] {
            let want = git(&r.dir).args(bad).output().unwrap().status.code();
            assert_eq!(r.pyn(bad).status.code(), want, "{bad:?}");
        }
    });
}

#[test]
fn status_pairs_an_intent_to_add_file_with_the_deleted_one() {
    each_format(|r| {
        r.write("a.txt", &lines("a", 20));
        r.commit("one");
        r.mv("a.txt", "z.txt");
        r.git(&["reset", "-q"]);
        r.git(&["add", "-N", "z.txt"]);
        for setting in [
            None,
            Some(("status.renames", "false")),
            Some(("status.renames", "copies")),
        ] {
            if let Some((key, value)) = setting {
                r.git(&["config", key, value]);
            }
            for args in [
                &["status", "-s"][..],
                &["status", "--porcelain"],
                &["diff", "--name-status"],
                &["diff", "--stat"],
                &["diff"],
                &["diff", "HEAD", "--name-status"],
            ] {
                r.same(args);
            }
        }
    });
}

#[test]
fn stash_show_pairs_renames() {
    let r = Repo::new("sha1");
    r.two_commits(
        |r| r.write("a.txt", &lines("a", 20)),
        |r| r.write("keep.txt", b"k\n"),
    );
    r.mv("a.txt", "moved.txt");
    r.write("moved.txt", &edited("a", 20, 2));
    r.git(&["add", "moved.txt"]);
    r.git(&["stash", "push", "-q"]);
    for args in [
        &["stash", "show"][..],
        &["stash", "show", "--name-status"],
        &["stash", "show", "-p"],
        &["stash", "show", "--summary"],
        &["stash", "show", "--no-renames", "--name-status"],
        &["stash", "show", "-M90%", "--stat"],
    ] {
        r.same(args);
    }
}

/// A small deterministic generator, so a failing seed can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % below as u64) as usize
    }
}

fn random_repo(r: &Repo, seed: u64) {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let words = [
        "alpha", "beta", "gamma", "delta", "eps", "zeta", "eta", "theta",
    ];
    let dirs = ["", "x/", "y/", "x/z/"];
    let names = ["a", "b", "c", "d", "e", "f"];
    let exts = [".txt", ".c", ""];
    let text = |rng: &mut Rng, tag: &str, n: usize| -> Vec<u8> {
        (0..n)
            .map(|i| {
                format!(
                    "{} {} {tag}{i}\n",
                    words[rng.next(words.len())],
                    words[rng.next(words.len())]
                )
            })
            .collect::<String>()
            .into_bytes()
    };
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for _ in 0..(3 + rng.next(8)) {
        let path = format!(
            "{}{}{}",
            dirs[rng.next(4)],
            names[rng.next(6)],
            exts[rng.next(3)]
        );
        if files.iter().any(|(p, _)| *p == path) {
            continue;
        }
        let content = if rng.next(8) == 0 {
            let mut b = vec![0u8];
            b.extend((0..(20 + rng.next(120))).map(|_| rng.next(256) as u8));
            b
        } else if rng.next(5) == 0 && !files.is_empty() {
            files[rng.next(files.len())].1.clone()
        } else {
            let n = 1 + rng.next(30);
            text(&mut rng, "t", n)
        };
        files.push((path, content));
    }
    for (p, c) in &files {
        r.write(p, c);
    }
    r.commit("one");
    let mut next: Vec<(String, Vec<u8>)> = Vec::new();
    for (p, c) in &files {
        r.remove(p);
        match rng.next(10) {
            0 => {}
            1..=5 => {
                let mut q = format!(
                    "{}{}{}",
                    dirs[rng.next(4)],
                    names[rng.next(6)],
                    exts[rng.next(3)]
                );
                if next.iter().any(|(n, _)| *n == q) || files.iter().any(|(n, _)| *n == q) {
                    q = format!("{p}.moved");
                }
                let mut c2 = c.clone();
                if !c.contains(&0) && rng.next(10) < 7 {
                    let mut ls: Vec<Vec<u8>> =
                        c.split(|b| *b == b'\n').map(<[u8]>::to_vec).collect();
                    for _ in 0..rng.next(ls.len() / 2 + 2) {
                        let k = rng.next(ls.len() - 1);
                        ls[k] = format!("edit{}", rng.next(99)).into_bytes();
                    }
                    c2 = ls.join(&b'\n');
                }
                next.push((q, c2));
                if rng.next(10) < 3 {
                    next.push((p.clone(), c.clone()));
                }
            }
            6 | 7 => {
                let mut c2 = c.clone();
                c2.extend_from_slice(b"tail\n");
                next.push((p.clone(), c2));
            }
            _ => next.push((p.clone(), c.clone())),
        }
    }
    if rng.next(2) == 0 {
        next.push((
            format!("copy{}", rng.next(5)),
            files[rng.next(files.len())].1.clone(),
        ));
    }
    for (p, c) in &next {
        r.write(p, c);
    }
    r.commit("two");
}

#[test]
fn seeded_random_histories_match_git() {
    for seed in 1..=14u64 {
        let format = FORMATS[(seed % 2) as usize];
        let r = Repo::new(format);
        random_repo(&r, seed);
        for flags in [
            &[][..],
            &["-M20%"],
            &["-M80%"],
            &["-C"],
            &["-C60%"],
            &["-C", "-C"],
            &["-C", "-C", "-M80%"],
            &["--no-renames"],
            &["-l2"],
        ] {
            for args in [
                &["diff", "HEAD~1", "HEAD"][..],
                &["diff", "HEAD~1", "HEAD", "--stat", "--summary"],
                &["diff", "HEAD~1", "HEAD", "--numstat"],
                &["diff", "HEAD~1", "HEAD", "--name-status"],
                &["show", "--summary", "--stat", "--format="],
                &["log", "-p", "--format=%s"],
            ] {
                let mut all = args.to_vec();
                all.extend_from_slice(flags);
                r.same(&all);
            }
        }
    }
}
