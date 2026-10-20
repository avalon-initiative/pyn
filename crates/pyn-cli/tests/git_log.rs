//! `pyn log` on a git repository compared with the real `git log` on the same repository: formats, revision
//! ranges, filters and diff output, on SHA-1 and SHA-256 repositories with merges, several authors and fixed dates.
//! Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

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
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        init(&repo, format, false);
        let f = Self {
            _tmp: tmp,
            repo,
            format,
        };
        f.build();
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
fn unsupported_variants_are_usage_errors() {
    let f = Fixture::new("sha1");
    for args in [
        &["--graph"][..],
        &["--all"],
        &["--follow", "--", "b.txt"],
        &["--decorate"],
        &["-S", "x"],
        &["-G", "x"],
        &["--first-parent"],
        &["--show-signature"],
        &["--notes"],
        &["-E", "--grep=x"],
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
fn show_format_errors_match_git() {
    let f = Fixture::new("sha1");
    for args in [&["--format=%h", "nope"][..], &["-s", "--date=bogus"]] {
        let (code, _) = f.oracle_cmd("show", &f.repo, args);
        let (got, out, _) = f.pyn_cmd("show", &f.repo, args);
        assert_eq!(got != 0, code != 0, "{args:?}");
        assert!(out.is_empty(), "{args:?}");
    }
}
