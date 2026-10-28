//! `pyn blame` and `pyn annotate` on a git repository compared with the real `git blame` and `git annotate` on the same
//! repository: every layout, line ranges, renames, merges, boundaries, a dirty work tree and seeded random histories, on
//! SHA-1 and SHA-256 repositories. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const ANN: (&str, &str) = ("Ann Author", "ann@example.com");
const BOB: (&str, &str) = ("Bob Builder", "bob@example.com");
const CY: (&str, &str) = ("Cy Coder", "cy@example.com");
const ZOE: (&str, &str) = ("Zo\u{eb} \u{4e2d}\u{6587}", "zoe@example.com");

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    format: &'static str,
    tick: Cell<i64>,
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
            tick: Cell::new(1_700_000_000),
        }
    }

    fn write(&self, path: &str, content: &[u8]) {
        let file = self.dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn lines(&self, path: &str, lines: &[&str]) {
        let mut text = lines.join("\n");
        if !lines.is_empty() {
            text.push('\n');
        }
        self.write(path, text.as_bytes());
    }

    fn git_as(&self, who: (&str, &str), args: &[&str]) -> bool {
        self.tick.set(self.tick.get() + 100);
        let zone = ["+0000", "+0100", "-0500", "+0530"][(self.tick.get() / 100 % 4) as usize];
        let date = format!("{} {zone}", self.tick.get());
        let out = git(&self.dir)
            .env("GIT_AUTHOR_NAME", who.0)
            .env("GIT_AUTHOR_EMAIL", who.1)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_NAME", format!("C {}", who.0))
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .unwrap();
        out.status.success()
    }

    fn git_ok(&self, who: (&str, &str), args: &[&str]) {
        assert!(self.git_as(who, args), "git {args:?} failed");
    }

    fn commit(&self, who: (&str, &str), message: &str) {
        self.git_ok(who, &["add", "-A"]);
        self.git_ok(who, &["commit", "-q", "--allow-empty", "-m", message]);
    }

    fn oracle(&self, cwd: &Path, cmd: &str, args: &[&str]) -> (i32, Vec<u8>) {
        let out = git(cwd)
            .env("TZ", "UTC")
            .arg(cmd)
            .args(args)
            .output()
            .unwrap();
        (out.status.code().unwrap(), fix_strict_zero(out.stdout))
    }

    fn pyn(&self, cwd: &Path, cmd: &str, args: &[&str]) -> (i32, Vec<u8>, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .arg(cmd)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.dir.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn compare(&self, cwd: &Path, cmd: &str, args: &[&str], mask: bool, loose: bool) {
        let (want_code, mut want) = self.oracle(cwd, cmd, args);
        let (code, mut got, err) = self.pyn(cwd, cmd, args);
        if mask {
            want = mask_times(&want);
            got = mask_times(&got);
        }
        if loose {
            want = squeeze(&want);
            got = squeeze(&got);
        }
        assert_eq!(code, want_code, "{} {cmd} {args:?}: {err}", self.format);
        assert_eq!(
            oracle::blame_lines(&got),
            oracle::blame_lines(&want),
            "{} {cmd} {args:?}",
            self.format
        );
    }

    fn same(&self, args: &[&str]) {
        self.compare(&self.dir, "blame", args, false, false);
    }

    fn same_annotate(&self, args: &[&str]) {
        self.compare(&self.dir, "annotate", args, false, false);
    }

    /// For a dirty work tree: times of the uncommitted pseudo-commit differ between the two runs.
    fn same_masked(&self, args: &[&str]) {
        self.compare(&self.dir, "blame", args, true, false);
    }

    /// For strict ISO dates, where git before 2.45 prints `+00:00` and pads to the longer width.
    fn same_loose(&self, args: &[&str]) {
        self.compare(&self.dir, "blame", args, false, true);
    }

    fn same_in(&self, cwd: &str, args: &[&str]) {
        self.compare(&self.dir.join(cwd), "blame", args, false, false);
    }
}

/// git before 2.45 writes a zero offset as `+00:00` in strict ISO dates, later ones as `Z`.
fn fix_strict_zero(out: Vec<u8>) -> Vec<u8> {
    String::from_utf8_lossy(&out)
        .replace("+00:00", "Z")
        .into_bytes()
}

/// Collapses runs of spaces, so output that differs only in padding compares equal.
fn squeeze(out: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(out)
        .lines()
        .map(|l| {
            l.split(' ')
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// Blanks the time fields of porcelain output and the date of `Not Committed Yet` lines.
fn mask_times(out: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(out);
    let mut result = String::new();
    for line in text.split_inclusive('\n') {
        let mut line = line.to_string();
        for key in ["author-time ", "committer-time "] {
            if line.starts_with(key) {
                line = format!("{key}T\n");
            }
        }
        if let Some(at) = line.find("Not Committed Yet ") {
            let start = at + "Not Committed Yet ".len();
            if line.len() >= start + 25 {
                line.replace_range(start..start + 25, "DATE");
            }
        }
        result.push_str(&line);
    }
    result.into_bytes()
}

fn each_format(check: impl Fn(&Repo)) {
    for format in FORMATS {
        check(&Repo::new(format));
    }
}

fn body(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("line {i}")).collect()
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// A history with edits, insertions and deletions by several authors in several zones.
fn linear(r: &Repo) {
    let mut l = body(12);
    r.lines("a.txt", &refs(&l));
    r.commit(ANN, "one");
    l.insert(3, "inserted by bob".into());
    l[8] = "changed by bob".into();
    r.lines("a.txt", &refs(&l));
    r.commit(BOB, "two\n\nwith a body");
    l.remove(0);
    l.remove(5);
    l.push("tail by cy".into());
    r.lines("a.txt", &refs(&l));
    r.commit(CY, "three");
    l[2] = "  indented\tand\ttabbed ".into();
    r.lines("a.txt", &refs(&l));
    r.commit(ZOE, "four");
    l[2] = "indented and tabbed".into();
    l.insert(7, "    ".into());
    r.lines("a.txt", &refs(&l));
    r.commit(ANN, "five");
}

#[test]
fn layouts_match_git() {
    each_format(|r| {
        linear(r);
        for args in [
            &["a.txt"][..],
            &["-e", "a.txt"],
            &["-s", "a.txt"],
            &["-f", "a.txt"],
            &["-n", "a.txt"],
            &["-f", "-n", "a.txt"],
            &["-fn", "a.txt"],
            &["-l", "a.txt"],
            &["-t", "a.txt"],
            &["-b", "a.txt"],
            &["-es", "a.txt"],
            &["--root", "a.txt"],
            &["--no-root", "a.txt"],
            &["-w", "a.txt"],
            &["--show-email", "a.txt"],
            &["--show-name", "--show-number", "a.txt"],
            &["--abbrev=5", "a.txt"],
            &["--abbrev=12", "a.txt"],
            &["--abbrev=2", "a.txt"],
            &["--abbrev=0", "a.txt"],
            &["--abbrev=40", "a.txt"],
            &["--abbrev", "a.txt"],
            &["--no-abbrev", "a.txt"],
            &["-l", "--root", "a.txt"],
            &["--", "a.txt"],
            &["a.txt", "HEAD"],
            &["HEAD", "a.txt"],
            &["HEAD", "--", "a.txt"],
            &["--", "a.txt", "HEAD"],
            &["HEAD~2", "--", "a.txt"],
            &["HEAD~1", "a.txt"],
            &["-w", "HEAD~2", "a.txt"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn date_styles_match_git() {
    each_format(|r| {
        linear(r);
        for style in [
            "default",
            "iso",
            "iso8601",
            "iso-strict",
            "iso8601-strict",
            "rfc",
            "rfc2822",
            "short",
            "unix",
            "raw",
            "relative",
        ] {
            let flag = format!("--date={style}");
            if style.contains("strict") {
                r.same_loose(&[&flag, "a.txt"]);
                r.same_loose(&[&flag, "-e", "a.txt"]);
                continue;
            }
            r.same(&[&flag, "a.txt"]);
            r.same(&[&flag, "-e", "a.txt"]);
        }
        r.git_ok(ANN, &["config", "blame.date", "short"]);
        r.same(&["a.txt"]);
        r.same(&["--date=rfc", "a.txt"]);
        r.git_ok(ANN, &["config", "blame.date", "default"]);
        r.same(&["a.txt"]);
    });
}

#[test]
fn porcelain_matches_git() {
    each_format(|r| {
        linear(r);
        for args in [
            &["--porcelain", "a.txt"][..],
            &["-p", "a.txt"],
            &["--line-porcelain", "a.txt"],
            &["-p", "-w", "a.txt"],
            &["-p", "--root", "a.txt"],
            &["-p", "-L", "3,6", "a.txt"],
            &["-p", "HEAD~2", "a.txt"],
            &["-p", "-e", "-s", "a.txt"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn annotate_matches_git() {
    each_format(|r| {
        linear(r);
        for args in [
            &["a.txt"][..],
            &["-e", "a.txt"],
            &["-l", "a.txt"],
            &["-t", "a.txt"],
            &["--root", "a.txt"],
            &["-w", "a.txt"],
            &["-L", "2,5", "a.txt"],
            &["HEAD~1", "a.txt"],
            &["--date=short", "a.txt"],
        ] {
            r.same_annotate(args);
        }
        r.same(&["-c", "a.txt"]);
        r.same(&["-c", "--root", "a.txt"]);
    });
}

#[test]
fn line_ranges_match_git() {
    each_format(|r| {
        linear(r);
        for args in [
            &["-L", "3,6", "a.txt"][..],
            &["-L3,6", "a.txt"],
            &["-L", "3,", "a.txt"],
            &["-L", ",4", "a.txt"],
            &["-L", "3", "a.txt"],
            &["-L", "5,+3", "a.txt"],
            &["-L", "5,-3", "a.txt"],
            &["-L", "6,2", "a.txt"],
            &["-L", "2,3", "-L", "6,8", "a.txt"],
            &["-L", "2,6", "-L", "4,9", "a.txt"],
            &["-L", "7,9", "-L", "2,3", "a.txt"],
            &["-L", "2,3", "-L", "+2", "a.txt"],
            &["-L", "/line 3/", "a.txt"],
            &["-L", "/line 3/,/line 6/", "a.txt"],
            &["-L", "/line 3/,+2", "a.txt"],
            &["-L", "^/line 3/,/line 6/", "a.txt"],
            &["-L", "/^line [58]$/,/nothing-matches/", "a.txt"],
            &["-L", "/changed/,+1", "a.txt"],
            &["-L", "/line .*8/,$", "a.txt"],
            &["-L", "1,1", "a.txt"],
            &["-L", "0,3", "a.txt"],
            &["-L", "3,99", "a.txt"],
            &["-L", "99", "a.txt"],
            &["-L", "99,100", "a.txt"],
            &["-L", "/zzz/", "a.txt"],
            &["-L", "x", "a.txt"],
            &["-L", "3,x", "a.txt"],
            &["-L", "3,+0", "a.txt"],
            &["-L", "-2,3", "a.txt"],
            &["-L", "1,3x", "a.txt"],
            &["-L", "3,6", "-w", "--porcelain", "a.txt"],
            &["-L", "2,4", "HEAD~2", "a.txt"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn renames_are_followed_like_git() {
    each_format(|r| {
        let mut l = body(20);
        r.lines("old.txt", &refs(&l));
        r.lines("keep/other.txt", &["x", "y", "z"]);
        r.commit(ANN, "one");
        l[4] = "edit before rename".into();
        r.lines("old.txt", &refs(&l));
        r.commit(BOB, "two");
        fs::create_dir_all(r.dir.join("sub")).unwrap();
        r.git_ok(CY, &["mv", "old.txt", "sub/new.txt"]);
        r.git_ok(CY, &["commit", "-q", "-m", "exact rename"]);
        l.insert(10, "added after rename".into());
        r.lines("sub/new.txt", &refs(&l));
        r.commit(ZOE, "three");
        r.git_ok(ANN, &["mv", "sub/new.txt", "renamed.txt"]);
        l[0] = "line one changed".into();
        l[1] = "line two changed".into();
        l[19] = "tail changed".into();
        r.lines("renamed.txt", &refs(&l));
        r.git_ok(ANN, &["add", "-A"]);
        r.git_ok(ANN, &["commit", "-q", "-m", "rename with edits"]);
        for args in [
            &["renamed.txt"][..],
            &["-f", "renamed.txt"],
            &["-f", "-n", "renamed.txt"],
            &["-p", "renamed.txt"],
            &["--line-porcelain", "renamed.txt"],
            &["-w", "-f", "renamed.txt"],
            &["-f", "HEAD~1", "sub/new.txt"],
            &["-p", "HEAD~1", "sub/new.txt"],
            &["-f", "HEAD~2", "sub/new.txt"],
            &["-f", "HEAD~3", "old.txt"],
            &["-f", "HEAD~2", "--", "old.txt"],
            &["-L", "3,12", "-f", "renamed.txt"],
        ] {
            r.same(args);
        }
        r.same_annotate(&["renamed.txt"]);
    });
}

#[test]
fn one_commit_touching_two_paths_shows_filename_lines_in_porcelain() {
    each_format(|r| {
        r.lines("a.txt", &["one", "two", "three"]);
        r.commit(ANN, "one");
        r.git_ok(BOB, &["mv", "a.txt", "b.txt"]);
        r.lines("b.txt", &["one", "two", "three", "four"]);
        r.git_ok(BOB, &["add", "-A"]);
        r.git_ok(BOB, &["commit", "-q", "-m", "rename and add"]);
        r.lines("c.txt", &["x"]);
        r.git_ok(BOB, &["add", "-A"]);
        r.git_ok(BOB, &["commit", "-q", "-m", "other"]);
        r.git_ok(BOB, &["mv", "b.txt", "a.txt"]);
        r.git_ok(BOB, &["commit", "-q", "-m", "back"]);
        for args in [
            &["-p", "a.txt"][..],
            &["-f", "a.txt"],
            &["--line-porcelain", "a.txt"],
        ] {
            r.same(args);
        }
    });
}

/// A merge whose result takes lines from both sides, and a second merge that changes lines itself.
fn merged(r: &Repo) {
    let l = body(16);
    r.lines("m.txt", &refs(&l));
    r.commit(ANN, "root");
    r.git_ok(CY, &["switch", "-q", "-c", "side"]);
    let mut s = l.clone();
    s[2] = "side change".into();
    s.insert(7, "side insert".into());
    s[15] = "side tail".into();
    r.lines("m.txt", &refs(&s));
    r.commit(CY, "side one");
    s.push("side more".into());
    r.lines("m.txt", &refs(&s));
    r.commit(CY, "side two");
    r.git_ok(CY, &["switch", "-q", "main"]);
    let mut m = l.clone();
    m[0] = "main change".into();
    m[5] = "main five".into();
    m[10] = "main twelve".into();
    r.lines("m.txt", &refs(&m));
    r.commit(BOB, "main one");
    r.git_ok(ANN, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let mut t = fs::read_to_string(r.dir.join("m.txt")).unwrap();
    t.push_str("after merge\n");
    t = t.replace("main five", "evil merge");
    r.write("m.txt", t.as_bytes());
    r.commit(BOB, "after merge");
    r.git_ok(ANN, &["switch", "-q", "-c", "late", "HEAD~1"]);
    let mut late = fs::read_to_string(r.dir.join("m.txt")).unwrap();
    late = late.replace("line 13", "late thirteen");
    r.write("m.txt", late.as_bytes());
    r.commit(ZOE, "late");
    r.git_ok(ANN, &["switch", "-q", "main"]);
    r.git_ok(ANN, &["merge", "-q", "--no-ff", "-m", "merge late", "late"]);
}

#[test]
fn merges_pass_blame_to_the_right_parent() {
    each_format(|r| {
        merged(r);
        for args in [
            &["m.txt"][..],
            &["-p", "m.txt"],
            &["--line-porcelain", "m.txt"],
            &["-w", "m.txt"],
            &["-f", "-n", "m.txt"],
            &["--root", "m.txt"],
            &["HEAD~1", "m.txt"],
            &["HEAD^2", "m.txt"],
            &["HEAD^1", "m.txt"],
            &["side", "m.txt"],
            &["HEAD~3", "m.txt"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn first_parent_follows_only_the_first_parent_of_merges() {
    each_format(|r| {
        merged(r);
        for args in [
            &["--first-parent", "m.txt"][..],
            &["--first-parent", "-p", "m.txt"],
            &["--first-parent", "--line-porcelain", "m.txt"],
            &["--first-parent", "-f", "-n", "m.txt"],
            &["--first-parent", "-w", "m.txt"],
            &["--first-parent", "--root", "m.txt"],
            &["--first-parent", "HEAD^2", "m.txt"],
            &["--first-parent", "HEAD~1", "m.txt"],
            &["--first-parent", "side", "m.txt"],
            &["--first-parent", "HEAD~2..", "m.txt"],
            &["--first-parent", "side..", "m.txt"],
            &["--first-parent", "^side", "HEAD", "m.txt"],
            &["--first-parent", "^HEAD^2", "HEAD", "m.txt"],
            &["--first-parent", "HEAD^2..HEAD", "m.txt"],
        ] {
            r.same(args);
        }
        r.same_annotate(&["--first-parent", "m.txt"]);
        r.git_ok(CY, &["switch", "-q", "-c", "x", "side"]);
        r.git_ok(
            CY,
            &["merge", "-q", "--no-ff", "-X", "ours", "-m", "x", "main"],
        );
        r.git_ok(CY, &["switch", "-q", "main"]);
        for args in [
            &["--first-parent", "^x", "main", "m.txt"][..],
            &["--first-parent", "main~1..main", "m.txt"],
            &["--first-parent", "x..main", "m.txt"],
            &["--first-parent", "main..x", "m.txt"],
            &["^x", "main", "m.txt"],
        ] {
            r.same(args);
        }
    });
}

#[test]
fn first_parent_on_random_histories_matches_git() {
    for format in FORMATS {
        for seed in 1..=6u64 {
            let r = Repo::new(format);
            let cur = random_history(&r, seed);
            for args in [
                vec!["--first-parent", cur.as_str()],
                vec!["--first-parent", "--line-porcelain", &cur],
                vec!["--first-parent", "HEAD~4..", "--", &cur],
            ] {
                r.same(&args);
            }
        }
    }
}

#[test]
fn ranges_of_revisions_mark_boundaries() {
    each_format(|r| {
        merged(r);
        for args in [
            &["HEAD~2..", "m.txt"][..],
            &["HEAD~2..HEAD", "--", "m.txt"],
            &["^HEAD~2", "m.txt"],
            &["^HEAD~2", "HEAD", "m.txt"],
            &["side..", "m.txt"],
            &["side..HEAD", "m.txt"],
            &["HEAD^1..HEAD", "-p", "m.txt"],
            &["..HEAD~1", "m.txt"],
            &["HEAD~1..HEAD~1", "m.txt"],
            &["HEAD~3..", "-b", "m.txt"],
            &["HEAD~3..", "-l", "m.txt"],
            &["HEAD~3..", "--root", "m.txt"],
            &["HEAD~3..", "-f", "m.txt"],
        ] {
            r.same(args);
        }
        r.same_annotate(&["HEAD~3..", "m.txt"]);
    });
}

#[test]
fn blame_configuration_is_read() {
    each_format(|r| {
        linear(r);
        r.git_ok(ANN, &["config", "blame.showRoot", "true"]);
        r.same(&["a.txt"]);
        r.git_ok(ANN, &["config", "blame.showRoot", "false"]);
        r.git_ok(ANN, &["config", "blame.blankBoundary", "true"]);
        r.same(&["a.txt"]);
        r.same(&["-c", "a.txt"]);
        r.git_ok(ANN, &["config", "blame.blankBoundary", "false"]);
        r.git_ok(ANN, &["config", "blame.showEmail", "true"]);
        r.same(&["a.txt"]);
        r.git_ok(ANN, &["config", "blame.showEmail", "false"]);
        r.git_ok(ANN, &["config", "core.abbrev", "10"]);
        r.same(&["a.txt"]);
        r.same(&["--abbrev=6", "a.txt"]);
        r.git_ok(ANN, &["config", "core.abbrev", "4"]);
        r.same(&["a.txt"]);
        r.git_ok(ANN, &["config", "--unset", "core.abbrev"]);
        r.git_ok(ANN, &["config", "diff.indentHeuristic", "false"]);
        r.same(&["a.txt"]);
    });
}

#[test]
fn awkward_files_match_git() {
    each_format(|r| {
        r.write("empty.txt", b"");
        r.write("no-eol.txt", b"first\nsecond\nlast without newline");
        r.write("crlf.txt", b"one\r\ntwo\r\nthree\r\n");
        r.write("bin.dat", b"text\n\0binary\nmore\n");
        r.write("blank.txt", b"\n\n\n");
        r.commit(ANN, "one");
        r.write("no-eol.txt", b"first\nsecond\nlast with a newline\n");
        r.write("crlf.txt", b"one\r\nTWO\r\nthree\r\n");
        r.write("bin.dat", b"text\n\0binary\nmore\nmore\n");
        r.write("blank.txt", b"\n\nx\n\n");
        r.commit(BOB, "two");
        r.write(
            "no-eol.txt",
            b"first\nsecond\nlast with a newline\nno newline again",
        );
        r.write("empty.txt", b"");
        r.commit(CY, "three");
        for file in [
            "empty.txt",
            "no-eol.txt",
            "crlf.txt",
            "bin.dat",
            "blank.txt",
        ] {
            for opts in [
                &[][..],
                &["-w"],
                &["-p"],
                &["-s"],
                &["--line-porcelain"],
                &["-f", "-n"],
            ] {
                let mut args = opts.to_vec();
                args.push(file);
                r.same(&args);
            }
        }
        r.same(&["-L", "1,1", "empty.txt"]);
        r.same(&["-L", "1", "empty.txt"]);
        r.same(&["-L", "/x/", "blank.txt"]);
    });
}

#[test]
fn symlinks_and_unusual_paths_match_git() {
    each_format(|r| {
        r.write("na\u{ef}ve dir/f\u{fc}r \"q\".txt", b"one\ntwo\n");
        std::os::unix::fs::symlink("target-one", r.dir.join("link")).unwrap();
        r.commit(ANN, "one");
        r.write("na\u{ef}ve dir/f\u{fc}r \"q\".txt", b"one\ntwo\nthree\n");
        fs::remove_file(r.dir.join("link")).unwrap();
        std::os::unix::fs::symlink("target-two", r.dir.join("link")).unwrap();
        r.commit(BOB, "two");
        r.git_ok(
            CY,
            &["mv", "na\u{ef}ve dir/f\u{fc}r \"q\".txt", "plain.txt"],
        );
        r.git_ok(CY, &["commit", "-q", "-m", "plain"]);
        r.same(&["link"]);
        r.same(&["-p", "link"]);
        r.same(&["-f", "plain.txt"]);
        r.same(&["-p", "plain.txt"]);
        r.same(&["-f", "HEAD~1", "na\u{ef}ve dir/f\u{fc}r \"q\".txt"]);
        r.same(&["-p", "HEAD~1", "na\u{ef}ve dir/f\u{fc}r \"q\".txt"]);
        r.git_ok(ANN, &["config", "core.quotePath", "false"]);
        r.same(&["-p", "plain.txt"]);
    });
}

#[test]
fn paths_resolve_from_the_folder_you_stand_in() {
    each_format(|r| {
        r.lines("top.txt", &["a", "b"]);
        r.lines("sub/in.txt", &["c", "d", "e"]);
        r.commit(ANN, "one");
        r.lines("sub/in.txt", &["c", "D", "e"]);
        r.commit(BOB, "two");
        r.same_in("sub", &["in.txt"]);
        r.same_in("sub", &["../top.txt"]);
        r.same_in("sub", &["-f", "./in.txt"]);
        r.same_in("sub", &["HEAD~1", "in.txt"]);
        r.same_in("sub", &["sub/in.txt"]);
        r.same_in("sub", &["../sub/in.txt"]);
        r.same_in("sub", &["../../nowhere"]);
    });
}

#[test]
fn a_dirty_work_tree_is_blamed_as_not_committed_yet() {
    each_format(|r| {
        linear(r);
        r.git_ok(ANN, &["config", "user.name", "Local"]);
        let mut text = fs::read_to_string(r.dir.join("a.txt")).unwrap();
        text = text.replace("line 4", "work tree four");
        text.push_str("work tree tail\n");
        r.write("a.txt", text.as_bytes());
        for args in [
            &["-s", "a.txt"][..],
            &["-s", "-f", "-n", "a.txt"],
            &["-p", "a.txt"],
            &["--line-porcelain", "a.txt"],
            &["-s", "-w", "a.txt"],
            &["-s", "-L", "2,4", "a.txt"],
            &["-s", "^HEAD~2", "a.txt"],
            &["-s", "--root", "a.txt"],
            &["-e", "-s", "a.txt"],
            &["-s", "HEAD", "a.txt"],
        ] {
            r.same_masked(args);
        }
        // a committed revision ignores the work tree
        r.same(&["HEAD", "a.txt"]);
        r.same(&["-p", "HEAD~1", "a.txt"]);
        // the file removed from the work tree
        fs::remove_file(r.dir.join("a.txt")).unwrap();
        r.same_masked(&["-s", "a.txt"]);
        r.same(&["HEAD", "a.txt"]);
    });
}

#[test]
fn contents_replace_the_newest_version() {
    each_format(|r| {
        linear(r);
        r.write("other.txt", b"line 1\nbrand new\nline 3\n");
        for args in [
            &["--contents", "other.txt", "-s", "a.txt"][..],
            &["--contents=other.txt", "-p", "a.txt"],
            &["--contents", "other.txt", "-s", "HEAD~2", "a.txt"],
        ] {
            r.same_masked(args);
        }
        r.same_masked(&["--contents", "missing.txt", "-s", "a.txt"]);
    });
}

#[test]
fn staged_and_renamed_work_tree_files() {
    each_format(|r| {
        let l = body(15);
        r.lines("a.txt", &refs(&l));
        r.commit(ANN, "one");
        r.git_ok(BOB, &["mv", "a.txt", "b.txt"]);
        r.same_masked(&["-s", "-f", "b.txt"]);
        let mut l2 = l.clone();
        l2[3] = "edited".into();
        r.lines("b.txt", &refs(&l2));
        r.same_masked(&["-s", "-f", "b.txt"]);
        r.git_ok(BOB, &["add", "b.txt"]);
        l2.push("unstaged".into());
        r.lines("b.txt", &refs(&l2));
        r.same_masked(&["-s", "-f", "b.txt"]);
        r.lines("new.txt", &["untracked"]);
        r.same_masked(&["-s", "new.txt"]);
        r.same(&["HEAD", "new.txt"]);
    });
}

#[test]
fn errors_match_git() {
    each_format(|r| {
        linear(r);
        r.lines("sub/x.txt", &["x"]);
        r.commit(ANN, "sub");
        for args in [
            &[][..],
            &["nosuchfile"],
            &["HEAD", "nosuchfile"],
            &["HEAD", "--", "nosuchfile"],
            &["nosuchrev", "--", "a.txt"],
            &["a.txt", "nosuchrev"],
            &["--", "a.txt", "nosuchrev"],
            &["HEAD"],
            &["HEAD~1", "HEAD~2", "a.txt"],
            &["HEAD~1", "HEAD~2", "--", "a.txt"],
            &["sub", "--"],
            &["sub"],
            &["HEAD", "sub"],
            &["--bogus", "a.txt"],
            &["-Q", "a.txt"],
            &["--date=bogus", "a.txt"],
            &["--abbrev=x", "a.txt"],
            &["--", "a.txt", "HEAD", "HEAD"],
            &["-L"],
            &["--contents"],
            &["^HEAD~1", "HEAD~3", "a.txt"],
            &["HEAD~99", "a.txt"],
            &["a.txt", "--"],
            &["a.txt", "a.txt"],
        ] {
            r.same(args);
        }
        r.same_annotate(&["nosuchfile"]);
        r.same_annotate(&[]);
    });
}

#[test]
fn an_unborn_branch_has_nothing_to_blame() {
    each_format(|r| {
        r.lines("a.txt", &["x"]);
        r.git_ok(ANN, &["add", "a.txt"]);
        r.same(&["a.txt"]);
        r.same_masked(&["-s", "a.txt"]);
    });
}

#[test]
fn what_is_not_built_is_a_usage_error() {
    each_format(|r| {
        linear(r);
        for args in [
            &["-M", "a.txt"][..],
            &["-C", "a.txt"],
            &["-C", "-C", "a.txt"],
            &["-M20", "a.txt"],
            &["--incremental", "a.txt"],
            &["-L", ":main", "a.txt"],
            &["--ignore-rev", "HEAD", "a.txt"],
            &["--ignore-revs-file", "x", "a.txt"],
            &["--reverse", "a.txt"],
            &["--show-stats", "a.txt"],
            &["--score-debug", "a.txt"],
            &["--color-lines", "a.txt"],
            &["--color-by-age", "a.txt"],
            &["--minimal", "a.txt"],
            &["--diff-algorithm=patience", "a.txt"],
            &["-S", "revs", "a.txt"],
            &["--date=human", "a.txt"],
            &["--date=local", "a.txt"],
            &["--date=format:%Y", "a.txt"],
            &["HEAD...HEAD~1", "a.txt"],
        ] {
            let (code, out, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(code, 129, "{} blame {args:?}: {err}", r.format);
            assert!(out.is_empty(), "{args:?}");
        }
        r.git_ok(ANN, &["config", "diff.algorithm", "patience"]);
        let (code, _, err) = r.pyn(&r.dir, "blame", &["a.txt"]);
        assert_eq!(code, 128, "{err}");
    });
}

#[test]
fn a_mailmap_is_refused_rather_than_ignored() {
    each_format(|r| {
        linear(r);
        r.write(
            ".mailmap",
            b"Proper <proper@example.com> <ann@example.com>\n",
        );
        let (code, _, err) = r.pyn(&r.dir, "blame", &["a.txt"]);
        assert_eq!(code, 128, "{err}");
        assert!(err.contains("mailmap"), "{err}");
    });
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const VOCAB: [&str; 14] = [
    "alpha",
    "beta",
    "gamma",
    "}",
    "{",
    "",
    "  indent",
    "    deep",
    "return x;",
    "if (a) {",
    "else {",
    "// note",
    "x = y + z;",
    "foo(bar);",
];

fn random_lines(rng: &mut Rng, n: usize) -> Vec<String> {
    (0..n).map(|_| random_line(rng)).collect()
}

fn random_line(rng: &mut Rng) -> String {
    if rng.chance(50) {
        VOCAB[rng.below(VOCAB.len())].to_string()
    } else {
        format!("line {}", rng.below(100_000))
    }
}

fn edited(rng: &mut Rng, mut l: Vec<String>, min: usize, spread: usize) -> Vec<String> {
    let edits = min + rng.below(spread);
    for _ in 0..edits {
        let x = rng.below(10);
        if x < 4 || l.is_empty() {
            let at = rng.below(l.len() + 1);
            l.insert(at, random_line(rng));
        } else if x < 7 {
            let at = rng.below(l.len());
            l.remove(at);
        } else {
            let at = rng.below(l.len());
            l[at] = format!("changed {}", rng.below(100_000));
        }
    }
    l
}

/// A seeded history of edits, renames with edits, copies, side branches merged with `-X ours` and unrelated files.
fn random_history(r: &Repo, seed: u64) -> String {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let who = [ANN, BOB, CY, ZOE];
    let mut cur = "f.txt".to_string();
    let n = 5 + rng.below(30);
    let first = random_lines(&mut rng, n);
    r.lines(&cur, &refs(&first));
    r.lines("sub/g.txt", &refs(&random_lines(&mut rng, 12)));
    r.commit(ANN, "c0");
    let read = |r: &Repo, p: &str| -> Vec<String> {
        fs::read_to_string(r.dir.join(p))
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    };
    for step in 1..16 {
        let author = who[rng.below(who.len())];
        let roll = rng.below(100);
        if roll < 35 {
            let l = edited(&mut rng, read(r, &cur), 1, 5);
            r.lines(&cur, &refs(&l));
            r.commit(author, &format!("edit {step}"));
        } else if roll < 55 {
            let new = [
                "f.txt".to_string(),
                format!("n{step}.txt"),
                format!("sub/m{step}.txt"),
            ][rng.below(3)]
            .clone();
            if new != cur && !r.dir.join(&new).exists() {
                fs::create_dir_all(r.dir.join(&new).parent().unwrap()).unwrap();
                fs::rename(r.dir.join(&cur), r.dir.join(&new)).unwrap();
                let l = edited(&mut rng, read(r, &new), 0, 7);
                r.lines(&new, &refs(&l));
                cur = new;
                r.commit(author, &format!("rename {step}"));
            }
        } else if roll < 70 {
            let l = edited(&mut rng, read(r, &cur), 0, 4);
            r.lines(&format!("sub/copy{step}.txt"), &refs(&l));
            r.commit(author, &format!("copy {step}"));
        } else if roll < 85 {
            let branch = format!("b{step}");
            r.git_ok(author, &["switch", "-q", "-c", &branch]);
            let l = edited(&mut rng, read(r, &cur), 1, 4);
            r.lines(&cur, &refs(&l));
            r.commit(author, &format!("branch {step}"));
            r.git_ok(author, &["switch", "-q", "main"]);
            let l = edited(&mut rng, read(r, &cur), 1, 4);
            r.lines(&cur, &refs(&l));
            r.commit(author, &format!("main {step}"));
            if !r.git_as(
                author,
                &["merge", "-q", "--no-ff", "--no-edit", "-X", "ours", &branch],
            ) {
                r.git_ok(author, &["merge", "--abort"]);
            }
        } else {
            r.lines(
                &format!("other{step}.txt"),
                &refs(&random_lines(&mut rng, 5)),
            );
            r.commit(author, &format!("other {step}"));
        }
    }
    cur
}

#[test]
fn seeded_random_histories_match_git() {
    for format in FORMATS {
        for seed in 1..=6u64 {
            let r = Repo::new(format);
            let cur = random_history(&r, seed);
            for args in [
                vec![cur.as_str()],
                vec!["-w", &cur],
                vec!["--root", &cur],
                vec!["--line-porcelain", &cur],
                vec!["-f", "-n", &cur],
                vec!["HEAD~4..", "--", &cur],
                vec!["-L", "3,9", &cur],
                vec!["-e", "-s", &cur],
                vec!["HEAD~3", &cur],
            ] {
                r.same(&args);
            }
        }
    }
}

#[test]
fn large_repetitive_files_diff_like_git() {
    for format in FORMATS {
        for seed in 1..=3u64 {
            let r = Repo::new(format);
            let mut rng = Rng(seed * 7919 + 13);
            let n = [300, 1500, 2500][seed as usize - 1];
            let mut l: Vec<String> = (0..n).map(|_| VOCAB[rng.below(6)].to_string()).collect();
            r.lines("big.txt", &refs(&l));
            r.commit(ANN, "c0");
            for step in 0..5 {
                for _ in 0..[3, 40, 200][step % 3] {
                    let x = rng.below(10);
                    let at = rng.below(l.len() + 1);
                    if x < 4 {
                        l.insert(at, VOCAB[rng.below(6)].to_string());
                    } else if x < 7 && !l.is_empty() {
                        l.remove(at.min(l.len() - 1));
                    } else if !l.is_empty() {
                        let at = at.min(l.len() - 1);
                        l[at] = VOCAB[rng.below(6)].to_string();
                    }
                }
                r.lines("big.txt", &refs(&l));
                r.commit([BOB, CY][step % 2], &format!("edit {step}"));
            }
            r.same(&["big.txt"]);
            r.same(&["-w", "big.txt"]);
            r.same(&["-p", "big.txt"]);
        }
    }
}

#[test]
fn long_common_tails_are_trimmed_like_git() {
    each_format(|r| {
        let tail: Vec<String> = (0..400).map(|i| format!("common tail line {i}")).collect();
        let mut l = vec!["head".to_string()];
        l.extend(tail.iter().cloned());
        r.lines("t.txt", &refs(&l));
        r.commit(ANN, "one");
        l[0] = "head changed".into();
        l.insert(1, "inserted".into());
        r.lines("t.txt", &refs(&l));
        r.commit(BOB, "two");
        l.push("tail".into());
        l.insert(5, "dup".into());
        r.lines("t.txt", &refs(&l));
        r.commit(CY, "three");
        r.same(&["t.txt"]);
        r.same(&["-w", "t.txt"]);
    });
}

#[test]
fn whitespace_changes_are_ignored_with_w() {
    each_format(|r| {
        r.lines("w.txt", &["a b", "c  d", "  e", "f", "g"]);
        r.commit(ANN, "one");
        r.lines("w.txt", &["a  b", "c d", "e", "f ", "g", "new"]);
        r.commit(BOB, "ws only");
        r.lines("w.txt", &["a b", "c d", "x", "f", "g", "new"]);
        r.commit(CY, "real");
        r.same(&["w.txt"]);
        r.same(&["-w", "w.txt"]);
        r.same(&["-w", "-p", "w.txt"]);
    });
}

#[test]
fn indent_heuristic_places_hunks_like_git() {
    each_format(|r| {
        let before = [
            "fn a() {", "    x();", "}", "", "fn b() {", "    y();", "}", "", "fn c() {",
            "    z();", "}",
        ];
        r.lines("c.txt", &before);
        r.commit(ANN, "one");
        let after = [
            "fn a() {",
            "    x();",
            "}",
            "",
            "fn new() {",
            "    y();",
            "}",
            "",
            "fn b() {",
            "    y();",
            "}",
            "",
            "fn c() {",
            "    z();",
            "}",
        ];
        r.lines("c.txt", &after);
        r.commit(BOB, "insert a duplicate-looking block");
        r.lines(
            "c.txt",
            &[
                "fn a() {",
                "    x();",
                "}",
                "",
                "}",
                "",
                "fn new() {",
                "    y();",
                "}",
                "",
                "fn b() {",
                "    y();",
                "}",
                "",
                "fn c() {",
                "    z();",
                "}",
            ],
        );
        r.commit(CY, "ambiguous insert");
        r.same(&["c.txt"]);
        r.git_ok(ANN, &["config", "diff.indentHeuristic", "false"]);
        r.same(&["c.txt"]);
    });
}

#[test]
fn many_authors_align_like_git() {
    each_format(|r| {
        let who = [
            ("A", "a@x"),
            ("Beatrice Longname-Longername", "b@x"),
            ("\u{4e2d}\u{6587}\u{540d}", "c@x"),
            ("\u{e9}\u{e8}\u{ea}", "d@x"),
        ];
        let mut l = vec![];
        for (i, w) in who.iter().enumerate() {
            l.push(format!("line by {i}"));
            r.lines("a.txt", &refs(&l));
            r.commit(*w, &format!("by {i}"));
        }
        for opts in [&[][..], &["-e"], &["-s"], &["-p"], &["-f"]] {
            let mut args = opts.to_vec();
            args.push("a.txt");
            r.same(&args);
            r.same_annotate(&args);
        }
    });
}
