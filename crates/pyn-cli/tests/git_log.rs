//! `pyn log` on a git repository compared with the real `git log` on the same repository: formats, revision
//! ranges, filters and diff output, on SHA-1 and SHA-256 repositories with merges, several authors and fixed dates.
//! Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    format: &'static str,
}

fn identity(c: &mut Command, who: (&str, &str), date: &str) {
    c.env("GIT_AUTHOR_NAME", who.0)
        .env("GIT_AUTHOR_EMAIL", who.1)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", format!("C {}", who.0))
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", date)
        .env("TZ", "UTC");
}

impl Fixture {
    fn new(format: &'static str) -> Self {
        let f = Self::empty(format);
        f.build();
        f
    }

    fn empty(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        init(&repo, format, false);
        Self {
            _tmp: tmp,
            repo,
            format,
        }
    }

    /// A random history of `n` commits built with `commit-tree`: forks, 2-way and octopus merges, equal dates.
    fn with_dag(format: &'static str, seed: u64, n: usize) -> Self {
        Self::dag(format, seed, n, false)
    }

    /// Like [`Fixture::with_dag`], with two files that change at random and two authors, for path limits and filters.
    fn with_dag_files(format: &'static str, seed: u64, n: usize) -> Self {
        Self::dag(format, seed, n, true)
    }

    fn dag(format: &'static str, seed: u64, n: usize, files: bool) -> Self {
        let f = Self::empty(format);
        let mut state = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut next = move |m: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % m
        };
        let tree_of = |entries: &[(&str, String)]| -> String {
            let mut listing = String::new();
            for (name, text) in entries {
                let mut h = git(&f.repo);
                h.args(["hash-object", "-w", "--stdin"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped());
                let mut child = h.spawn().unwrap();
                std::io::Write::write_all(&mut child.stdin.take().unwrap(), text.as_bytes())
                    .unwrap();
                let blob = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
                listing.push_str(&format!("100644 blob {}\t{name}\n", blob.trim()));
            }
            let mut mk = git(&f.repo);
            mk.arg("mktree")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
            let mut child = mk.spawn().unwrap();
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), listing.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let tree = tree_of(&[]);
        let mut contents: Vec<(u64, u64)> = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut time = 1_700_000_000u64;
        for i in 0..n {
            let count = if i == 0 || next(12) == 0 {
                0
            } else if i < 4 {
                1
            } else {
                [1, 1, 1, 1, 2, 2, 3][next(7) as usize]
            };
            let recent = &ids[ids.len().saturating_sub(4)..];
            let mut pool: Vec<String> = if next(10) < 7 {
                recent.to_vec()
            } else {
                ids.clone()
            };
            let mut parents = Vec::new();
            while parents.len() < count && !pool.is_empty() {
                parents.push(pool.remove(next(pool.len() as u64) as usize));
            }
            time += [0, 10, 10, 50][next(4) as usize];
            let mut c = git(&f.repo);
            let (who, tree, message) = if files {
                let (mut a, mut b) = parents.first().map_or((0, 0), |p| {
                    contents[ids.iter().position(|x| x == p).unwrap()]
                });
                if next(3) == 0 {
                    a = next(4);
                }
                if next(3) == 0 {
                    b = next(4);
                }
                contents.push((a, b));
                let who =
                    [("Ann", "ann@example.com"), ("Bob", "bob@example.com")][next(2) as usize];
                let word = ["fix", "feat", "chore"][next(3) as usize];
                let tree = tree_of(&[("a.txt", format!("a{a}\n")), ("b.txt", format!("b{b}\n"))]);
                (who, tree, format!("{word} c{i}"))
            } else {
                (("Dag", "dag@example.com"), tree.clone(), format!("c{i}"))
            };
            identity(&mut c, who, &format!("{time} +0000"));
            c.args(["commit-tree", &tree, "-m", &message]);
            for p in &parents {
                c.args(["-p", p]);
            }
            let out = c.output().unwrap();
            ids.push(String::from_utf8(out.stdout).unwrap().trim().to_string());
        }
        f.git(
            ("t", "t@example.com"),
            "1700000000 +0000",
            &["update-ref", "refs/heads/main", ids.last().unwrap()],
        );
        for j in 0..next(4) {
            let at = &ids[next(ids.len() as u64) as usize];
            f.git(
                ("t", "t@example.com"),
                "1700000000 +0000",
                &["update-ref", &format!("refs/heads/b{j}"), at],
            );
        }
        f
    }

    /// History with renames: an exact one, an edited one into a folder, a merge, and a path that is deleted and added again.
    fn with_renames(format: &'static str) -> Self {
        let f = Self::empty(format);
        let (ann, bob) = (
            ("Ann Author", "ann@example.com"),
            ("Bob Builder", "bob@example.com"),
        );
        let body: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        f.write("a.txt", body.as_bytes());
        f.commit(ann, "1700000000 +0000", "add a");
        f.write("other.txt", b"other\n");
        f.commit(bob, "1700000100 +0000", "add other");
        f.git(ann, "1700000200 +0000", &["mv", "a.txt", "b.txt"]);
        f.commit(ann, "1700000200 +0000", "rename a to b");
        f.write("b.txt", format!("{body}more\n").as_bytes());
        f.commit(bob, "1700000300 +0000", "edit b");
        f.git(bob, "1700000400 +0000", &["switch", "-q", "-c", "side"]);
        f.write("other.txt", b"other\nside\n");
        f.commit(bob, "1700000500 +0000", "side edits other");
        f.git(bob, "1700000600 +0000", &["switch", "-q", "main"]);
        f.write("b.txt", format!("{body}more\nmain\n").as_bytes());
        f.commit(ann, "1700000700 +0000", "main edits b");
        f.git(
            ann,
            "1700000800 +0000",
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        let edited = body
            .replace("line 5\n", "line five\n")
            .replace("line 6\n", "line six\n");
        f.git(bob, "1700000900 +0000", &["mv", "b.txt", "sub_c.txt"]);
        f.write("sub_c.txt", format!("{edited}more\nmain\n").as_bytes());
        fs::create_dir_all(f.repo.join("sub")).unwrap();
        f.git(bob, "1700000900 +0000", &["mv", "sub_c.txt", "sub/c.txt"]);
        f.commit(bob, "1700000900 +0000", "move b into sub and edit");
        f.write(
            "sub/c.txt",
            format!("{edited}more\nmain\nlast\n").as_bytes(),
        );
        f.commit(ann, "1700001000 +0000", "edit c");
        f.git(ann, "1700001100 +0000", &["rm", "-q", "sub/c.txt"]);
        f.commit(ann, "1700001100 +0000", "remove c");
        f.write("sub/c.txt", b"fresh\n");
        f.commit(bob, "1700001200 +0000", "new c");
        f
    }

    fn write(&self, path: &str, content: &[u8]) {
        let file = self.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn git(&self, who: (&str, &str), date: &str, args: &[&str]) {
        let mut c = git(&self.repo);
        identity(&mut c, who, date);
        let out = c.args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, who: (&str, &str), date: &str, message: &str) {
        self.git(who, date, &["add", "-A"]);
        self.git(who, date, &["commit", "-q", "-m", message]);
    }

    /// History: a root, a side branch merged with `--no-ff`, a second merge, then work in a folder and a removal.
    fn build(&self) {
        let (ann, bob, cy, dee) = (
            ("Ann Author", "ann@example.com"),
            ("Bob Builder", "bob@example.com"),
            ("Cy Coder", "cy@example.com"),
            ("Dee Dev", "dee@example.com"),
        );
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        self.write("a.txt", format!("fn start\n{body}").as_bytes());
        self.commit(ann, "1700000000 +0100", "one");
        self.write("b.txt", b"b\n");
        self.write("bin.dat", b"x\0y");
        self.commit(
            bob,
            "1700001000 -0500",
            "two\n\nbody line\n\nsecond paragraph\nof the body",
        );
        self.git(cy, "1700002000 +0000", &["switch", "-q", "-c", "side"]);
        self.write("s.txt", b"side\n");
        self.commit(cy, "1700002000 +0000", "side one");
        self.write("s.txt", b"side\nmore\n");
        self.commit(cy, "1700002500 +0000", "side two: more");
        self.git(cy, "1700002600 +0000", &["switch", "-q", "main"]);
        self.write("a.txt", format!("fn start\n{body}tail\n").as_bytes());
        self.commit(ann, "1700003000 +0530", "three");
        self.git(
            dee,
            "1700004000 +0000",
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        );
        self.git(dee, "1700004100 +0000", &["tag", "v1"]);
        self.git(
            dee,
            "1700004200 +0000",
            &["tag", "-a", "v2", "-m", "second", "HEAD~1"],
        );
        self.write("b.txt", b"b\nq\n");
        self.commit(bob, "1700005000 +0000", "four");
        self.git(bob, "1700005100 +0000", &["branch", "feature"]);
        self.git(bob, "1700005100 +0000", &["branch", "alpha"]);
        self.git(bob, "1700005100 +0000", &["tag", "t1"]);
        self.git(
            bob,
            "1700005100 +0000",
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        self.write("sub/z.txt", b"z\n");
        self.commit(dee, "1700006000 +0000", "subdir\n\nwith a body");
        self.git(dee, "1700006100 +0000", &["switch", "-q", "feature"]);
        self.write("f.txt", b"feature\n");
        self.commit(dee, "1700006200 +0000", "feature work");
        self.git(dee, "1700006300 +0000", &["switch", "-q", "main"]);
        self.git(
            ann,
            "1700007000 +0000",
            &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
        );
        self.git(ann, "1700007100 +0000", &["rm", "-q", "a.txt"]);
        self.commit(ann, "1700007200 +0000", "remove a");
    }

    fn oracle(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        self.oracle_cmd("log", cwd, args)
    }

    fn oracle_cmd(&self, cmd: &str, cwd: &Path, args: &[&str]) -> (i32, String) {
        let mut c = git(cwd);
        c.env("TZ", "UTC").arg(cmd).args(args);
        let out = c.output().unwrap();
        (
            out.status.code().unwrap(),
            // git before 2.45 writes a zero offset as +00:00 in strict ISO dates
            String::from_utf8_lossy(&out.stdout).replace("+00:00", "Z"),
        )
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.pyn_cmd("log", cwd, args)
    }

    fn pyn_cmd(&self, cmd: &str, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .arg(cmd)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.repo.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Both tools must exit alike and print the same text.
    fn same(&self, args: &[&str]) {
        self.same_in(&self.repo, args);
    }

    /// Like [`Fixture::same`], but git 2.56 may draw extra connector rows: then only the commit rows must agree.
    fn same_graph(&self, args: &[&str]) {
        let (want_code, want) = self.oracle(&self.repo, args);
        let (code, got, err) = self.pyn(&self.repo, args);
        assert_eq!(code, want_code, "{} log {args:?}: {err}", self.format);
        if got != want {
            assert_eq!(
                oracle::graph_commits(&got),
                oracle::graph_commits(&want),
                "{} log {args:?}",
                self.format
            );
        }
    }

    fn same_in(&self, cwd: &Path, args: &[&str]) {
        let (want_code, want) = self.oracle(cwd, args);
        let (code, got, err) = self.pyn(cwd, args);
        assert_eq!(code, want_code, "{} log {args:?}: {err}", self.format);
        assert_eq!(got, want, "{} log {args:?}", self.format);
    }
}

impl Fixture {
    fn same_show(&self, args: &[&str]) {
        let (want_code, want) = self.oracle_cmd("show", &self.repo, args);
        let (code, got, err) = self.pyn_cmd("show", &self.repo, args);
        assert_eq!(code, want_code, "{} show {args:?}: {err}", self.format);
        assert_eq!(got, want, "{} show {args:?}", self.format);
    }
}

fn each_format(check: impl Fn(&Fixture)) {
    for format in FORMATS {
        check(&Fixture::new(format));
    }
}

#[test]
fn named_formats_match_git() {
    each_format(|f| {
        for args in [
            &[][..],
            &["--oneline"],
            &["--pretty=oneline"],
            &["--pretty=short"],
            &["--pretty=medium"],
            &["--pretty=full"],
            &["--pretty=fuller"],
            &["--pretty=raw"],
            &["--format=short"],
            &["--pretty"],
            &["-n", "3", "--pretty=fuller"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn templates_match_git() {
    each_format(|f| {
        for args in [
            &["--format=%H|%h|%T|%t|%P|%p"][..],
            &["--format=%an|%ae|%ad|%aD|%ai|%at|%cn|%ce|%cd|%cI|%ct"],
            &["--format=%aI|%as|%cD|%ci|%cs|%ar"],
            &["--format=%s|%%|%n%b"],
            &["--format=[%B]"],
            &["--pretty=format:%h %s"],
            &["--pretty=tformat:%h %s"],
            &["--format=%h %d"],
            &["--format=%D"],
            &["--format=%h%d", "v2", "-n", "2"],
            &["--format=%unknown %x %"],
            &["--format="],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn decorations_follow_gits_order() {
    each_format(|f| {
        f.same(&["--format=%h%d"]);
        f.git(
            ("t", "t@example.com"),
            "1700009000 +0000",
            &["switch", "-q", "--detach", "v1"],
        );
        f.same(&["--format=%h%d", "HEAD", "main"]);
    });
}

#[test]
fn date_styles_match_git() {
    each_format(|f| {
        for style in [
            "default",
            "iso",
            "iso8601",
            "iso-strict",
            "rfc",
            "rfc2822",
            "short",
            "unix",
            "raw",
        ] {
            let date = format!("--date={style}");
            f.same(&[date.as_str()]);
            f.same(&[date.as_str(), "--pretty=fuller"]);
            f.same(&[date.as_str(), "--format=%ad|%cd"]);
        }
        f.same(&["--date=relative", "--format=%ad"]);
        f.same(&["--format=%ar|%cr"]);
    });
}

#[test]
fn revision_ranges_match_git() {
    each_format(|f| {
        for args in [
            &["v1"][..],
            &["v2"],
            &["feature"],
            &["HEAD~2"],
            &["HEAD^2"],
            &["v1..main"],
            &["main~3..main"],
            &["side..main"],
            &["main..side"],
            &["main..feature"],
            &["feature..main"],
            &["main...side"],
            &["side...main"],
            &["feature...side"],
            &["v2..", "--oneline"],
            &["..v2", "--oneline"],
            &["^side", "main"],
            &["main", "^side"],
            &["main", "^side", "^feature"],
            &["^HEAD"],
            &["main", "feature", "^v1"],
            &["--oneline", "main~1", "side"],
            &["--oneline", "side", "--", "s.txt"],
            &["--oneline", "main", "--", "a.txt"],
            &["--oneline", "--", "sub"],
            &["--oneline", "v1", "--", "b.txt", "s.txt"],
            &["--oneline", "a.txt"],
            &["--oneline", "b.txt", "s.txt"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn ranges_with_a_path_match_git_without_merges_on_the_excluded_side() {
    each_format(|f| {
        for args in [
            &["--oneline", "v1..main", "--", "b.txt"][..],
            &["--oneline", "main~3..main", "--", "sub"],
            &["--oneline", "side~1..side", "--", "s.txt"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn filters_match_git() {
    each_format(|f| {
        for args in [
            &["--author=Bob"][..],
            &["--author=bob"],
            &["--author=bob", "-i"],
            &["--author=Bob", "--author=Ann"],
            &["--author=<cy@"],
            &["--author=^Dee Dev <dee@example.com>$"],
            &["--committer=C Ann"],
            &["--committer=C"],
            &["--author=Ann", "--committer=C Bob"],
            &["--grep=side"],
            &["--grep=SIDE"],
            &["--grep=SIDE", "-i"],
            &["--grep=SIDE", "--regexp-ignore-case"],
            &["--grep=one", "--grep=side"],
            &["--grep=o", "--grep=e", "--all-match"],
            &["--grep=o", "--invert-grep"],
            &["--grep=side", "--grep=one", "--invert-grep"],
            &["--grep=^second"],
            &["--grep=body$"],
            &["--grep=thr\\|fo"],
            &["--grep=t.r"],
            &["--grep=[[:digit:]]"],
            &["--grep=a\\{2\\}"],
            &["--grep=(one)"],
            &["--grep=wo\\+"],
            &["--grep=side", "--author=Cy"],
            &["--grep=side", "--author=Bob"],
            &["--since=1700003500"],
            &["--until=1700003500"],
            &["--since=1700001500", "--until=1700004500"],
            &["--after=1700003500"],
            &["--before=1700003500"],
            &["--since=@1700003500"],
            &["--since=2023-11-14 23:10:00 +0100"],
            &["--since=2023-11-14T23:10:00Z"],
            &["--until=2023-11-14T23:10:00Z"],
            &["--since=Tue, 14 Nov 2023 23:10:00 +0000"],
            &["--since=2 years ago", "--until=1 year ago"],
            &["--no-merges"],
            &["--merges"],
            &["--merges", "--oneline", "v1"],
            &["--skip=2"],
            &["--skip", "1", "-n", "2"],
            &["--skip=100"],
            &["--reverse"],
            &["--reverse", "-n", "3"],
            &["--reverse", "--skip=1", "-n", "2"],
            &["--reverse", "--no-merges", "--author=Ann"],
            &["-n", "0"],
            &["--max-count=1"],
            &[
                "--no-merges",
                "--author=Dee",
                "--since=1700005500",
                "--reverse",
            ],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn diff_output_matches_git() {
    each_format(|f| {
        for args in [
            &["--stat"][..],
            &["--shortstat"],
            &["--name-only"],
            &["--name-status"],
            &["-p"],
            &["--patch"],
            &["-u"],
            &["-U1"],
            &["-U0"],
            &["-U5"],
            &["--unified=2"],
            &["-p", "--stat"],
            &["--stat", "-p"],
            &["-p", "--shortstat"],
            &["--oneline", "--stat"],
            &["--oneline", "--shortstat"],
            &["--oneline", "-p"],
            &["--oneline", "--name-only"],
            &["--oneline", "--name-status"],
            &["--format=%s", "--stat"],
            &["--format=%s", "-p"],
            &["--format=%s", "--name-status"],
            &["--format=", "--stat"],
            &["--pretty=format:%s", "-p"],
            &["--pretty=format:%s", "--stat"],
            &["--pretty=format:%s", "--name-only"],
            &["--pretty=short", "--stat"],
            &["--pretty=full", "-p"],
            &["--pretty=fuller", "--shortstat"],
            &["--pretty=raw", "-p", "--stat"],
            &["--stat", "--", "a.txt"],
            &["-p", "--", "sub"],
            &["-p", "b.txt"],
            &["--name-status", "--", "a.txt", "sub"],
            &["-p", "--no-merges", "--author=Ann"],
            &["-p", "-n", "1", "side"],
            &["-p", "v1..main"],
            &["--stat", "--merges"],
            &["-p", "--reverse", "-n", "4"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn paths_resolve_from_the_folder_you_stand_in() {
    each_format(|f| {
        let sub = f.repo.join("sub");
        for args in [
            &["--oneline", "z.txt"][..],
            &["--oneline", "--", "z.txt"],
            &["--oneline", "."],
            &["--oneline", "-p", ".."],
            &["--oneline", "--stat", "--", "../b.txt"],
            &["--oneline", "main", "--", "."],
        ] {
            f.same_in(&sub, args);
        }
    });
}

#[test]
fn bad_arguments_fail_as_git_does() {
    each_format(|f| {
        for args in [
            &["nonexistent"][..],
            &["nonexistent", "--"],
            &["--format=nonsense"],
            &["--pretty=nonsense"],
            &["nope..main"],
            &["--", "nonexistent-path"],
        ] {
            let (want, _) = f.oracle(&f.repo, args);
            let (code, out, err) = f.pyn(&f.repo, args);
            assert_eq!(code, want, "{args:?}: {err}");
            if want != 0 {
                assert!(out.is_empty() && !err.is_empty(), "{args:?}");
            }
        }
        fs::write(f.repo.join("v1"), "x").unwrap();
        let (want, _) = f.oracle(&f.repo, &["v1"]);
        let (code, _, err) = f.pyn(&f.repo, &["v1"]);
        assert_eq!((want, code), (128, 128), "{err}");
        assert!(err.contains("ambiguous"), "{err}");
        f.same(&["v1", "--", "b.txt"]);
        for bad in [
            "--date=nonsense",
            "--since=gibberish",
            "--author=[",
            "--grep=\\(",
        ] {
            let (code, out, err) = f.pyn(&f.repo, &[bad]);
            assert_ne!(code, 0, "{bad}");
            assert!(out.is_empty() && !err.is_empty(), "{bad}");
        }
    });
}

#[test]
fn counts_all_refs_and_first_parent_match_git() {
    each_format(|f| {
        for args in [
            &["-1"][..],
            &["-3"],
            &["-2", "--oneline"],
            &["-0"],
            &["-2", "--reverse"],
            &["-2", "--", "b.txt"],
            &["-1", "-p"],
            &["--all"],
            &["--all", "--oneline"],
            &["--all", "--format=%h%d"],
            &["--all", "--", "b.txt"],
            &["--all", "-2"],
            &["--all", "--reverse", "--oneline"],
            &["--all", "^v1", "--oneline"],
            &["--all", "--first-parent", "--oneline"],
            &["--first-parent"],
            &["--first-parent", "--oneline"],
            &["--first-parent", "--", "b.txt"],
            &["--first-parent", "--", "s.txt"],
            &["--first-parent", "a.txt"],
            &["--first-parent", "v1..HEAD", "--oneline"],
            &["--first-parent", "^v1", "HEAD", "--oneline"],
            &["--first-parent", "side", "--oneline"],
            &["--first-parent", "--stat"],
            &["--first-parent", "--name-only"],
            &["--first-parent", "-p", "-2"],
            &["--first-parent", "--merges", "--oneline"],
            &["--first-parent", "--no-merges", "--oneline"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn decorate_matches_git() {
    each_format(|f| {
        for args in [
            &["--decorate"][..],
            &["--decorate=short"],
            &["--decorate=full"],
            &["--decorate=no"],
            &["--decorate=auto"],
            &["--decorate=yes"],
            &["--decorate=off"],
            &["--no-decorate"],
            &["--decorate", "--no-decorate"],
            &["--no-decorate", "--decorate"],
            &["--decorate", "--oneline"],
            &["--decorate=full", "--oneline"],
            &["--decorate", "--pretty=oneline"],
            &["--decorate", "--pretty=short"],
            &["--decorate", "--pretty=full"],
            &["--decorate", "--pretty=fuller"],
            &["--decorate", "--pretty=raw"],
            &["--decorate", "--format=%h"],
            &["--decorate", "--format=%h%d"],
            &["--decorate=full", "--format=%h%d"],
            &["--decorate=full", "--format=%D"],
            &["--no-decorate", "--format=%h%d"],
            &["--decorate", "--all", "--oneline"],
            &["--decorate=full", "--all", "--oneline"],
            &["--decorate", "--stat"],
            &["--decorate", "--oneline", "-p", "-3"],
            &["--decorate=bogus"],
            &["--decorate", "v1"],
        ] {
            f.same(args);
        }
        f.git(
            ("t", "t@example.com"),
            "1700009000 +0000",
            &["switch", "-q", "--detach", "v1"],
        );
        f.same(&["--decorate", "--all", "--oneline"]);
        f.same(&["--decorate=full", "--oneline", "HEAD", "main"]);
    });
}

const DECORATE_VIEWS: &[&[&str]] = &[
    &[],
    &["--oneline"],
    &["--oneline", "--all"],
    &["--oneline", "--decorate"],
    &["--oneline", "--decorate=full"],
    &["--oneline", "--decorate=auto"],
    &["--oneline", "--decorate=no"],
    &["--oneline", "--no-decorate"],
    &["--oneline", "--no-decorate", "--decorate"],
    &["--pretty=short", "-3"],
    &["--pretty=raw", "-2"],
    &["--format=%h%d"],
    &["--format=%h %D"],
    &["--oneline", "--graph"],
    &["--oneline", "--graph", "--decorate"],
    &["--oneline", "--decorate", "--decorate-refs=refs/tags"],
    &[
        "--oneline",
        "--decorate",
        "--decorate-refs-exclude=refs/tags",
    ],
    &["--oneline", "--decorate", "--decorate-refs=HEAD"],
    &["--oneline", "--decorate", "--decorate-refs=bogus"],
];

const SHOW_VIEWS: &[&[&str]] = &[
    &["-s"],
    &["-s", "--oneline"],
    &["-s", "--oneline", "--decorate"],
    &["-s", "--oneline", "--decorate=full"],
    &["-s", "--oneline", "--decorate=auto"],
    &["-s", "--oneline", "--no-decorate"],
    &["-s", "--decorate", "--no-decorate"],
    &["-s", "--no-decorate", "--decorate=short"],
    &["-s", "--decorate", "--pretty=fuller"],
    &["-s", "--decorate", "--pretty=raw", "HEAD~2"],
    &["-s", "--decorate", "--format=%h%d"],
    &["-s", "--format=%h%d"],
    &["--decorate", "--oneline", "--stat"],
    &["--decorate", "--oneline", "HEAD~2", "side"],
    &[
        "-s",
        "--decorate",
        "--decorate-refs=refs/tags",
        "--oneline",
        "v1",
    ],
    &[
        "-s",
        "--decorate",
        "--decorate-refs-exclude=refs/heads/*",
        "--oneline",
    ],
    &["-s", "--decorate=bogus"],
];

impl Fixture {
    fn set_config(&self, args: &[&str]) {
        let mut all = vec!["config"];
        all.extend_from_slice(args);
        self.git(("t", "t@example.com"), "1700009000 +0000", &all);
    }

    fn views_match(&self) {
        for args in DECORATE_VIEWS {
            self.same(args);
        }
        for args in SHOW_VIEWS {
            self.same_show(args);
        }
    }
}

/// Extra refs: a nested branch, a notes ref and a tag on the oldest commit.
fn with_odd_refs(f: &Fixture) {
    let who = ("t", "t@example.com");
    let date = "1700009000 +0000";
    f.git(who, date, &["branch", "other/deep", "HEAD~3"]);
    f.git(who, date, &["update-ref", "refs/notes/x", "HEAD~3"]);
    f.git(who, date, &["tag", "-a", "-m", "old", "old", "HEAD~5"]);
}

#[test]
fn log_decorate_config_matches_git() {
    each_format(|f| {
        for value in [
            "short", "full", "no", "false", "true", "yes", "on", "off", "0", "1", "2", "auto",
            "bogus", "Full", "",
        ] {
            f.set_config(&["log.decorate", value]);
            f.views_match();
        }
        f.set_config(&["--unset", "log.decorate"]);
        let config = f.repo.join(".git/config");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str("[log]\n\tdecorate\n");
        fs::write(&config, text).unwrap();
        f.views_match();
    });
}

#[test]
fn decorate_refs_patterns_match_git() {
    each_format(|f| {
        with_odd_refs(f);
        for pattern in [
            "refs/tags",
            "refs/tags/",
            "refs/tags/*",
            "tags/*",
            "tags/v*",
            "refs/tags/v1",
            "refs/heads/*",
            "heads/*",
            "refs/heads/other",
            "refs/heads/other/deep",
            "refs/heads/mai",
            "refs/heads/ma*",
            "refs/heads/[ms]*",
            "refs/heads/?ain",
            "refs/*/main",
            "refs/remotes",
            "refs/notes/x",
            "refs",
            "",
            "HEAD",
            "main",
            "refs/main",
        ] {
            for style in ["--decorate", "--decorate=full"] {
                for flag in ["--decorate-refs", "--decorate-refs-exclude"] {
                    let arg = format!("{flag}={pattern}");
                    f.same(&["--oneline", "--all", style, &arg]);
                    f.same(&["--format=%h%d", "--all", &arg]);
                    f.same(&["--oneline", "--graph", "--all", style, &arg]);
                    f.same_show(&["-s", "--oneline", style, &arg, "HEAD~3"]);
                }
            }
        }
        for combo in [
            &[
                "--decorate-refs=refs/heads/*",
                "--decorate-refs-exclude=refs/heads/side",
            ][..],
            &[
                "--decorate-refs=refs/heads/side",
                "--decorate-refs-exclude=refs/heads/*",
            ],
            &["--decorate-refs=refs/tags", "--decorate-refs=refs/remotes"],
            &["--decorate-refs=HEAD", "--decorate-refs=refs/heads/main"],
            &[
                "--decorate-refs-exclude=HEAD",
                "--decorate-refs-exclude=refs/tags",
            ],
            &["--decorate-refs-exclude=refs/heads/main"],
            &[
                "--decorate-refs=refs/heads/main",
                "--decorate-refs-exclude=HEAD",
            ],
        ] {
            let mut args = vec!["--oneline", "--all", "--decorate"];
            args.extend_from_slice(combo);
            f.same(&args);
            let mut shown = vec!["-s", "--oneline", "--decorate"];
            shown.extend_from_slice(combo);
            f.same_show(&shown);
        }
        f.git(
            ("t", "t@example.com"),
            "1700009100 +0000",
            &["switch", "-q", "--detach", "v1"],
        );
        f.same(&["--oneline", "--all", "--decorate", "--decorate-refs=HEAD"]);
        f.same(&[
            "--oneline",
            "--all",
            "--decorate",
            "--decorate-refs-exclude=HEAD",
        ]);
    });
}

#[test]
fn exclude_decoration_config_matches_git() {
    each_format(|f| {
        with_odd_refs(f);
        let config = f.repo.join(".git/config");
        for values in [
            &["refs/tags"][..],
            &["tags/*"],
            &["refs/heads/main"],
            &["HEAD"],
            &["refs/tags", "refs/remotes/origin"],
            &["refs/heads/*", "HEAD"],
            &[""],
        ] {
            let mut text = String::from("[log]\n");
            for v in values {
                text.push_str(&format!("\texcludeDecoration = {v}\n"));
            }
            let saved = fs::read_to_string(&config).unwrap();
            fs::write(&config, format!("{saved}{text}")).unwrap();
            for args in [
                &["--oneline", "--all", "--decorate"][..],
                &["--format=%h%d", "--all"],
                &[
                    "--oneline",
                    "--all",
                    "--decorate",
                    "--decorate-refs=refs/heads/*",
                ],
                &[
                    "--oneline",
                    "--all",
                    "--decorate",
                    "--decorate-refs-exclude=refs/remotes",
                ],
                &[
                    "--oneline",
                    "--all",
                    "--decorate",
                    "--decorate-refs-exclude=refs/heads/*",
                ],
                &["--oneline", "--all", "--graph", "--decorate"],
            ] {
                f.same(args);
            }
            f.same_show(&["-s", "--oneline", "--decorate", "HEAD~3"]);
            f.same_show(&["-s", "--format=%d", "HEAD"]);
            fs::write(&config, saved).unwrap();
        }
    });
}

#[test]
fn follow_matches_git() {
    for format in FORMATS {
        let f = Fixture::with_renames(format);
        for args in [
            &["--follow", "--", "sub/c.txt"][..],
            &["--follow", "--oneline", "--", "sub/c.txt"],
            &["--follow", "--oneline", "sub/c.txt"],
            &["--follow", "--oneline", "--", "b.txt"],
            &["--follow", "--oneline", "--", "a.txt"],
            &["--follow", "--oneline", "--", "other.txt"],
            &["--follow", "--oneline", "--", "nothing.txt"],
            &["--follow", "--oneline", "-n", "2", "--", "sub/c.txt"],
            &["--follow", "--oneline", "-1", "--", "other.txt"],
            &["--follow", "--oneline", "--skip=1", "--", "sub/c.txt"],
            &["--follow", "--oneline", "--reverse", "--", "sub/c.txt"],
            &["--follow", "--oneline", "--first-parent", "--", "sub/c.txt"],
            &["--follow", "--oneline", "--author=Bob", "--", "sub/c.txt"],
            &["--follow", "--oneline", "--decorate", "--", "sub/c.txt"],
            &["--follow", "--format=%h %an", "--", "sub/c.txt"],
            &["--follow", "--oneline", "HEAD~3", "--", "b.txt"],
            &["--follow", "--oneline", "--all", "--", "sub/c.txt"],
            &["--follow", "--oneline", "--merges", "--", "sub/c.txt"],
            &["--oneline", "--follow", "--", "sub"],
            &["--follow", "--oneline"],
            &["--follow", "--oneline", "--", "a.txt", "b.txt"],
        ] {
            f.same(args);
        }
    }
}

#[test]
fn follow_with_diff_output_matches_git() {
    for format in FORMATS {
        let f = Fixture::with_renames(format);
        for path in ["sub/c.txt", "b.txt", "a.txt", "other.txt", "nothing.txt"] {
            for opt in [
                &["-p"][..],
                &["--stat"],
                &["--stat", "-p"],
                &["--shortstat"],
                &["--name-only"],
                &["--name-status"],
                &["--raw"],
                &["--summary"],
                &["-p", "-M"],
                &["-p", "-M90%"],
                &["--name-status", "-M95%"],
                &["--name-status", "-M30%"],
                &["-U1"],
                &["-p", "--first-parent"],
                &["-p", "--decorate"],
                &["-p", "--skip=1", "-3"],
                &["-p", "--author=Bob"],
            ] {
                let mut args = vec!["--follow", "--oneline"];
                args.extend_from_slice(opt);
                args.extend(["--", path]);
                f.same(&args);
            }
        }
        f.same(&["--follow", "-p", "--", "sub/c.txt"]);
        f.same(&["--follow", "--stat", "--pretty=short", "--", "sub/c.txt"]);
        f.same(&["--follow", "-p", "-w", "--all", "--", "sub/c.txt"]);
        f.same(&["--follow", "-p", "--merges", "--", "sub/c.txt"]);
    }
}

#[test]
fn pickaxe_matches_git() {
    each_format(|f| {
        for args in [
            &["-S", "tail", "--oneline"][..],
            &["-Stail", "--oneline"],
            &["-S", "line 3", "--oneline"],
            &["-S", "fn start", "--oneline"],
            &["-S", "b", "--oneline"],
            &["-S", "side", "--oneline"],
            &["-S", "x", "--oneline"],
            &["-S", "nothing here", "--oneline"],
            &["-S", "TAIL", "-i", "--oneline"],
            &["-S", "TAIL", "--oneline"],
            &["-G", "line [0-9]+", "--oneline"],
            &["-G", "^q$", "--oneline"],
            &["-G", "tail|more", "--oneline"],
            &["-G", "tail\\|more", "--oneline"],
            &["-G", "TAIL", "-i", "--oneline"],
            &["-Gx", "--oneline"],
            &["-G", "side", "-E", "--oneline"],
            &["-S", "tail", "--format=%h%d"],
            &["-S", "tail"],
            &["-S", "tail", "--stat"],
            &["-S", "b", "--name-only"],
            &["-S", "b", "--name-status"],
            &["-S", "b", "--shortstat"],
            &["-S", "line", "-p"],
            &["-G", "line", "-p", "-U1"],
            &["-S", "more", "--oneline", "--", "s.txt"],
            &["-S", "more", "--oneline", "s.txt", "b.txt"],
            &["-S", "more", "--first-parent", "--oneline"],
            &["-S", "feature", "--first-parent", "-p"],
            &["-S", "b", "--reverse", "--oneline"],
            &["-S", "b", "-n1", "--oneline"],
            &["-S", "b", "--skip=1", "--oneline"],
            &["-S", "b", "--all", "--oneline"],
            &["-S", "b", "--merges", "--oneline"],
            &["-S", "q", "--author=Bob", "--oneline"],
            &["-S", "q", "v1..HEAD", "--oneline"],
            &["-S", "b", "-G", "b"],
        ] {
            f.same(args);
        }
    });
    for format in FORMATS {
        let f = Fixture::with_renames(format);
        for args in [
            &["-S", "line 3", "--oneline"][..],
            &["-S", "more", "--oneline"],
            &["-S", "last", "--oneline"],
            &["-S", "fresh", "--oneline"],
            &["-S", "line five", "--oneline"],
            &["-G", "line", "--oneline"],
            &["-G", "five", "--oneline"],
            &["-G", "side", "--oneline"],
            &["-S", "line", "--oneline", "--", "sub"],
        ] {
            f.same(args);
        }
    }
}

#[test]
fn regex_dialects_match_git() {
    each_format(|f| {
        for args in [
            &["-E", "--grep=o(n|u)e"][..],
            &["-E", "--grep=^t(wo|hree)$"],
            &["-E", "--grep=s.d+e"],
            &["-E", "--grep=s.{1,2}e one"],
            &["-E", "--grep=side one|merge"],
            &["--extended-regexp", "--grep=fo+ur"],
            &["-E", "--grep=fo\\+ur"],
            &["--grep=fo\\+ur"],
            &["-E", "--author=Bob|Ann"],
            &["-E", "--committer=^C (Cy|Dee)"],
            &["-E", "--author=Bob|Ann", "-i"],
            &["-E", "--grep=a{2"],
            &["-E", "--grep=\\bone\\b"],
            &["-E", "--grep=(", "--oneline"],
            &["-E", "--grep=one", "--grep=two", "--all-match"],
            &["-F", "--grep=side"],
            &["-F", "--grep=a.b"],
            &["-F", "--grep=s.de"],
            &["-F", "--grep=SIDE", "-i"],
            &["--fixed-strings", "--grep=two"],
            &["-F", "--author=<cy@"],
            &["-F", "--author=Ann.Author"],
            &["-F", "--grep=side: more"],
            &["-F", "--grep=^one"],
            &["-F", "--grep=one$"],
            &["-F", "--grep=(", "--oneline"],
            &["-E", "-F", "--grep=s.de"],
            &["-F", "-E", "--grep=s.de"],
            &["-F", "--basic-regexp", "--grep=s.de"],
            &["--basic-regexp", "-E", "--grep=o(n|u)e"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn graph_matches_git() {
    each_format(|f| {
        for args in [
            &["--graph", "--oneline"][..],
            &["--graph"],
            &["--graph", "--format=%h %s"],
            &["--graph", "--format=format:%h"],
            &["--graph", "--pretty=short"],
            &["--graph", "--pretty=fuller", "-4"],
            &["--graph", "--pretty=oneline", "--decorate"],
            &["--graph", "--oneline", "--all"],
            &["--graph", "--oneline", "--first-parent"],
            &["--graph", "--oneline", "--skip=2", "-3"],
            &["--graph", "--oneline", "v1..HEAD"],
            &["--graph", "--oneline", "side", "feature"],
        ] {
            f.same_graph(args);
        }
    });
    for format in FORMATS {
        for seed in 0..40 {
            let f = Fixture::with_dag(format, seed, 6 + (seed as usize % 12));
            for args in [
                &["--graph", "--oneline"][..],
                &["--graph"],
                &["--graph", "--format=%h%n%s"],
                &["--graph", "--oneline", "--all"],
                &["--graph", "--oneline", "--first-parent"],
                &["--graph", "--oneline", "-5"],
            ] {
                f.same_graph(args);
            }
        }
    }
}

#[test]
fn graph_refusals() {
    let f = Fixture::new("sha1");
    f.same(&["--graph", "--reverse"]);
    for args in [
        &["--graph", "--follow", "--", "b.txt"][..],
        &["--graph", "-m", "-p"],
        &["--graph", "--cc"],
    ] {
        let (code, out, _) = f.pyn(&f.repo, args);
        assert_eq!(code, 129, "{args:?}");
        assert!(out.is_empty());
    }
}

const GRAPH_LIMITED: &[&[&str]] = &[
    &["--graph", "--oneline", "--", "a.txt"],
    &["--graph", "--oneline", "a.txt"],
    &["--graph", "--oneline", "--", "b.txt"],
    &["--graph", "--oneline", "--", "sub"],
    &["--graph", "--oneline", "--", "a.txt", "s.txt"],
    &["--graph", "--oneline", "--all", "--", "s.txt"],
    &["--graph", "--oneline", "--first-parent", "--", "a.txt"],
    &["--graph", "--oneline", "-2", "--", "a.txt"],
    &["--graph", "--", "b.txt"],
    &["--graph", "--oneline", "--", "nothing.txt"],
    &["--graph", "-p"],
    &["--graph", "-p", "--oneline"],
    &["--graph", "--stat"],
    &["--graph", "--stat", "--oneline"],
    &["--graph", "--stat", "-p"],
    &["--graph", "--name-only", "--oneline"],
    &["--graph", "--name-status"],
    &["--graph", "--shortstat", "--oneline"],
    &["--graph", "--raw", "--oneline"],
    &["--graph", "--summary", "--oneline"],
    &["--graph", "-p", "--format=%h %s"],
    &["--graph", "-p", "--format=format:%h%n%s"],
    &["--graph", "-p", "--pretty=short"],
    &["--graph", "-p", "--oneline", "--first-parent"],
    &["--graph", "-p", "--oneline", "--", "a.txt"],
    &["--graph", "--stat", "--", "a.txt", "b.txt"],
    &["--graph", "-p", "--all", "--oneline"],
    &["--graph", "--oneline", "--author=Bob"],
    &["--graph", "--oneline", "--author=Ann", "--author=Cy"],
    &["--graph", "--oneline", "--committer=Dee"],
    &["--graph", "--oneline", "--grep=side"],
    &["--graph", "--oneline", "--grep=two", "--grep=four"],
    &["--graph", "--oneline", "-i", "--grep=MERGE"],
    &["--graph", "--oneline", "--grep=merge", "--invert-grep"],
    &["--graph", "--oneline", "--merges"],
    &["--graph", "--oneline", "--no-merges"],
    &["--graph", "--oneline", "--no-merges", "--all"],
    &["--graph", "--oneline", "--since=2023-11-14 22:30:00 +0000"],
    &["--graph", "--oneline", "--until=2023-11-14 22:30:00 +0000"],
    &[
        "--graph",
        "--oneline",
        "--since=2023-11-14 22:20:00 +0000",
        "--until=2023-11-14 23:20:00 +0000",
    ],
    &["--graph", "--oneline", "-S", "tail"],
    &["--graph", "--oneline", "-S", "side"],
    &["--graph", "--oneline", "-G", "^side"],
    &["--graph", "-p", "-S", "side"],
    &["--graph", "--oneline", "-S", "line 3", "-3"],
    &["--graph", "--oneline", "--author=Bob", "--", "b.txt"],
    &["--graph", "--oneline", "--no-merges", "--author=Ann", "-p"],
    &["--graph", "--oneline", "--skip=1", "--grep=e"],
    &["--graph", "--oneline", "-3", "--author=Dee", "--stat"],
    &["--graph", "--oneline", "-E", "--grep=(one|two)"],
];

#[test]
fn graph_with_paths_diffs_and_filters_matches_git() {
    each_format(|f| {
        for args in GRAPH_LIMITED {
            f.same_graph(args);
        }
    });
}

#[test]
fn graph_on_random_histories_with_paths_and_filters_matches_git() {
    for format in FORMATS {
        for seed in 0..30 {
            let f = Fixture::with_dag_files(format, seed, 8 + (seed as usize % 10));
            for args in [
                &["--graph", "--oneline", "--", "a.txt"][..],
                &["--graph", "--oneline", "--", "b.txt"],
                &["--graph", "--oneline", "--all", "--", "a.txt", "b.txt"],
                &["--graph", "--oneline", "--first-parent", "--", "b.txt"],
                &["--graph", "--oneline", "--author=Bob"],
                &["--graph", "--oneline", "--all", "--grep=fix"],
                &["--graph", "--oneline", "--merges"],
                &["--graph", "--oneline", "--no-merges"],
                &["--graph", "--oneline", "--since=@1700000100"],
                &["--graph", "--oneline", "--until=@1700000100"],
                &["--graph", "--oneline", "-S", "a1"],
                &["--graph", "--oneline", "-G", "^b2"],
                &["--graph", "--oneline", "-p", "--", "a.txt"],
                &["--graph", "--oneline", "--stat", "--author=Ann"],
                &["--graph", "--oneline", "--author=Ann", "--", "b.txt"],
            ] {
                f.same_graph(args);
            }
        }
    }
}

#[test]
fn unsupported_variants_are_usage_errors() {
    let f = Fixture::new("sha1");
    for args in [
        &["-P", "--grep=x"][..],
        &["--perl-regexp", "--grep=x"],
        &["--follow", "-S", "x", "--", "b.txt"],
        &["--follow", "-C", "--", "b.txt"],
        &["--notes"],
        &["--format"],
        &["--stat", "--shortstat"],
        &["--name-only", "-p"],
        &["--merges", "--no-merges"],
    ] {
        let (code, out, err) = f.pyn(&f.repo, args);
        assert_eq!(code, 129, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?}");
    }
}

#[test]
fn the_commits_table_takes_no_git_log_options() {
    let f = Fixture::new("sha1");
    let (code, out, _) = f.pyn(&f.repo, &["--commits"]);
    assert_eq!(code, 0);
    assert!(out.contains("COMMIT") && out.contains("remove a"), "{out}");
    let (code, _, err) = f.pyn(&f.repo, &["--commits", "--oneline"]);
    assert_ne!(code, 0, "{err}");
}

#[test]
fn show_formats_match_git() {
    each_format(|f| {
        for args in [
            &["-s"][..],
            &["--no-patch"],
            &["-s", "HEAD~1"],
            &["-s", "--oneline"],
            &["--oneline", "HEAD~3"],
            &["--oneline", "--stat", "HEAD~3"],
            &["-s", "--pretty=short", "HEAD~4"],
            &["-s", "--pretty=full", "HEAD~4"],
            &["-s", "--pretty=fuller", "HEAD~4"],
            &["-s", "--pretty=raw", "HEAD~4"],
            &["-s", "--format=%H|%h|%T|%P|%p|%an|%ae|%cn|%s"],
            &["-s", "--format=%b|%B|%%"],
            &["-s", "--format=%ad|%cd", "--date=iso"],
            &["-s", "--format=%ad", "--date=unix", "HEAD~4"],
            &["--format=%h %s", "HEAD~3"],
            &["--format=format:%h", "HEAD~3"],
            &["--format=tformat:%an", "HEAD~3"],
            &["-s", "--date=rfc", "--pretty=fuller", "HEAD~6"],
            &["-s", "--format="],
        ] {
            f.same_show(args);
        }
    });
}

#[test]
fn show_limits_to_paths_and_takes_several_commits_like_git() {
    each_format(|f| {
        for args in [
            &["HEAD~3", "--", "b.txt"][..],
            &["HEAD~2", "--", "sub"],
            &["HEAD~3", "b.txt"],
            &["HEAD~2", "--", "sub", "b.txt"],
            &["--stat", "HEAD~2", "--", "sub"],
            &["HEAD~3", "HEAD~2"],
            &["--oneline", "HEAD~3", "HEAD~2", "HEAD"],
            &["HEAD~3", "HEAD~2", "nosuchrev"],
        ] {
            f.same_show(args);
        }
        for args in [
            &["HEAD~3", "--", "a.txt"][..],
            &["HEAD~3", "HEAD~2", "b.txt"],
            &["HEAD~3", "--", "nosuchpath"],
            &["HEAD~3", "HEAD~2", "--", "b.txt"],
            &["HEAD~1", "--", "f.txt"],
            &["HEAD~1", "--", "b.txt"],
            &["HEAD~1", "--", "a.txt", "f.txt"],
        ] {
            // git 2.55 prints a commit that changes none of the paths; 2.43 and 2.56 leave it out
            let (want_code, want) = f.oracle_cmd("show", &f.repo, args);
            let (code, out, err) = f.pyn_cmd("show", &f.repo, args);
            assert_eq!(code, want_code, "{} show {args:?}: {err}", f.format);
            if out != want {
                let diffed: String = format!("\n{out}")
                    .split("\ncommit ")
                    .skip(1)
                    .filter(|b| b.contains("\ndiff --git"))
                    .map(|b| format!("commit {b}"))
                    .collect();
                assert_eq!(
                    diffed.trim_end(),
                    want.trim_end(),
                    "{} show {args:?}",
                    f.format
                );
            }
        }
    });
}

#[test]
fn show_format_errors_match_git() {
    let f = Fixture::new("sha1");
    for args in [&["--format=%h", "nope"][..], &["-s", "--date=bogus"]] {
        let (code, _) = f.oracle_cmd("show", &f.repo, args);
        let (got, out, _) = f.pyn_cmd("show", &f.repo, args);
        assert_eq!(got != 0, code != 0, "{args:?}");
        assert!(out.is_empty(), "{args:?}");
    }
}
