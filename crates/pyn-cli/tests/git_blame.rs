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
    tz: Cell<&'static str>,
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
            tz: Cell::new("UTC"),
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
            .env("TZ", self.tz.get())
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
            .env("TZ", self.tz.get())
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

    /// Porcelain without the per-line `ignored` and `unblamable` lines, which git 2.43 does not print.
    fn same_unmarked(&self, args: &[&str]) {
        let drop = |out: Vec<u8>| {
            String::from_utf8_lossy(&out)
                .lines()
                .filter(|l| *l != "ignored" && *l != "unblamable")
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (want_code, want) = self.oracle(&self.dir, "blame", args);
        let (code, got, err) = self.pyn(&self.dir, "blame", args);
        assert_eq!(code, want_code, "{} blame {args:?}: {err}", self.format);
        assert_eq!(drop(got), drop(want), "{} blame {args:?}", self.format);
    }

    fn rev(&self, spec: &str) -> String {
        let out = git(&self.dir).args(["rev-parse", spec]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
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

/// A seeded choice of commits to ignore: every second one of the history, newest first.
fn some_commits(r: &Repo, skip: usize) -> Vec<String> {
    let out = git(&r.dir).args(["rev-list", "HEAD"]).output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .skip(skip)
        .step_by(2)
        .map(String::from)
        .collect()
}

#[test]
fn ignored_revisions_are_blamed_past() {
    each_format(|r| {
        linear(r);
        for ignored in ["HEAD", "HEAD~1", "HEAD~2", "HEAD~3", "HEAD~4"] {
            r.same(&["--ignore-rev", ignored, "a.txt"]);
            r.same(&[&format!("--ignore-rev={ignored}"), "-f", "-n", "a.txt"]);
            r.same_unmarked(&["--ignore-rev", ignored, "-p", "a.txt"]);
        }
        r.same(&["--ignore-rev", "HEAD", "--ignore-rev", "HEAD~2", "a.txt"]);
        r.same(&["--ignore-rev", "HEAD~1", "-w", "a.txt"]);
        r.same(&["--ignore-rev", "HEAD~3", "-L", "3,9", "a.txt"]);
        r.same(&["--ignore-rev", "HEAD~1", "HEAD~2", "a.txt"]);
        r.same_annotate(&["--ignore-rev", "HEAD~1", "a.txt"]);
        let ids = format!("{}\n{}\n", r.rev("HEAD~1"), r.rev("HEAD~3"));
        r.write(
            "../ignore",
            format!(
                "# a comment\n\n  {}  # trailing\n{}\n",
                r.rev("HEAD~2"),
                ids
            )
            .as_bytes(),
        );
        let file = r.dir.parent().unwrap().join("ignore");
        let file = file.to_str().unwrap();
        r.same(&["--ignore-revs-file", file, "a.txt"]);
        r.same(&[&format!("--ignore-revs-file={file}"), "a.txt"]);
        r.same(&[
            "--ignore-revs-file",
            file,
            "--ignore-revs-file",
            "",
            "a.txt",
        ]);
        r.git_ok(ANN, &["config", "blame.ignoreRevsFile", file]);
        r.same(&["a.txt"]);
        r.same(&["--ignore-revs-file", "", "a.txt"]);
        r.git_ok(ANN, &["config", "blame.markIgnoredLines", "true"]);
        r.same(&["a.txt"]);
        r.same(&["-s", "a.txt"]);
        r.same_unmarked(&["-p", "a.txt"]);
        r.git_ok(ANN, &["config", "blame.markUnblamableLines", "true"]);
        r.same(&["a.txt"]);
        r.same(&["-l", "-b", "a.txt"]);
        r.same_annotate(&["a.txt"]);
        r.same_unmarked(&["--line-porcelain", "a.txt"]);
        r.git_ok(ANN, &["config", "--unset", "blame.ignoreRevsFile"]);
        r.git_ok(ANN, &["config", "blame.markIgnoredLines", "false"]);
        r.same(&["--ignore-rev", "HEAD~1", "a.txt"]);
    });
}

#[test]
fn ignore_errors_match_git() {
    each_format(|r| {
        linear(r);
        r.write("../bad", b"not-a-hash\n");
        r.write("../short", format!("{}\n", &r.rev("HEAD")[..10]).as_bytes());
        r.write("../tree", format!("{}\n", r.rev("HEAD^{tree}")).as_bytes());
        let dir = r.dir.parent().unwrap();
        for args in [
            vec!["--ignore-rev", "nonsense", "a.txt"],
            vec!["--ignore-rev", "HEAD^{tree}", "a.txt"],
            vec!["--ignore-revs-file", "missing", "a.txt"],
            vec![
                "--ignore-revs-file",
                dir.join("bad").to_str().unwrap(),
                "a.txt",
            ],
            vec![
                "--ignore-revs-file",
                dir.join("short").to_str().unwrap(),
                "a.txt",
            ],
            vec![
                "--ignore-revs-file",
                dir.join("tree").to_str().unwrap(),
                "a.txt",
            ],
            vec!["--ignore-rev"],
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", &args);
            let (code, _, err) = r.pyn(&r.dir, "blame", &args);
            assert_eq!(
                oracle::status_class(code),
                oracle::status_class(want),
                "{args:?}: {err}"
            );
        }
    });
}

#[test]
fn ignoring_revisions_in_merged_and_random_histories_matches_git() {
    each_format(|r| {
        merged(r);
        for skip in [0, 1] {
            let mut args = some_commits(r, skip)
                .into_iter()
                .flat_map(|c| ["--ignore-rev".to_string(), c])
                .collect::<Vec<_>>();
            args.push("m.txt".into());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            r.same(&refs);
            r.same_unmarked(&[&refs[..refs.len() - 1], &["-p", "m.txt"]].concat());
        }
        r.git_ok(ANN, &["config", "blame.markIgnoredLines", "true"]);
        r.git_ok(ANN, &["config", "blame.markUnblamableLines", "true"]);
        for single in some_commits(r, 0) {
            r.same(&["--ignore-rev", &single, "m.txt"]);
        }
    });
    for format in FORMATS {
        for seed in 0..40u64 {
            let r = Repo::new(format);
            let path = random_history(&r, seed);
            r.git_ok(ANN, &["config", "blame.markIgnoredLines", "true"]);
            r.git_ok(ANN, &["config", "blame.markUnblamableLines", "true"]);
            for skip in [0, 1] {
                let ids = some_commits(&r, skip);
                let mut args: Vec<String> = ids
                    .iter()
                    .flat_map(|c| ["--ignore-rev".to_string(), c.clone()])
                    .collect();
                args.push(path.clone());
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                r.same(&refs);
            }
        }
    }
}

/// Commits at fixed ages before now, each changing another line, so `human` and `relative` dates cover every band.
fn recent(r: &Repo) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut l = body(8);
    r.tick.set(now - 3 * 365 * 86_400);
    r.lines("t.txt", &refs(&l));
    r.commit(ANN, "start");
    let ages = [
        400 * 86_400,
        100 * 86_400,
        40 * 86_400,
        12 * 86_400,
        4 * 86_400 + 3_000,
        2 * 86_400 + 3_000,
        7_300,
    ];
    for (i, age) in ages.iter().enumerate() {
        r.tick.set(now - age - 100);
        l[i] = format!("changed {i}");
        r.lines("t.txt", &refs(&l));
        r.commit([ANN, BOB, CY, ZOE][i % 4], &format!("c{i}"));
    }
}

#[test]
fn every_date_mode_matches_git() {
    each_format(|r| {
        linear(r);
        recent(r);
        let modes = [
            "human",
            "local",
            "default-local",
            "iso-local",
            "iso8601-local",
            "rfc-local",
            "rfc2822-local",
            "short-local",
            "raw-local",
            "unix-local",
            "relative-local",
            "human-local",
            "format:%Y-%m-%d %H:%M",
            "format:%a %b %e %s %z|%Z|%%",
            "format-local:%Y-%m-%d %H:%M %z",
            "format:",
            "auto:human",
            "auto:iso",
        ];
        for tz in ["UTC", "Asia/Kolkata", "America/New_York"] {
            r.tz.set(tz);
            for mode in modes {
                let flag = format!("--date={mode}");
                for file in ["a.txt", "t.txt"] {
                    r.same(&[&flag, file]);
                    r.same(&[&flag, "-e", file]);
                }
            }
            for mode in ["iso-strict-local", "iso8601-strict-local"] {
                r.same_loose(&[&format!("--date={mode}"), "t.txt"]);
            }
            r.git_ok(ANN, &["config", "blame.date", "human"]);
            r.same(&["t.txt"]);
            r.git_ok(ANN, &["config", "blame.date", "format-local:%d/%m %H"]);
            r.same(&["t.txt"]);
            r.same_annotate(&["t.txt"]);
            r.git_ok(ANN, &["config", "--unset", "blame.date"]);
        }
        r.tz.set("UTC");
        for bad in [
            "--date=nonsense",
            "--date=format",
            "--date=isoish",
            "--date=human-localx",
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", &[bad, "a.txt"]);
            let (code, out, err) = r.pyn(&r.dir, "blame", &[bad, "a.txt"]);
            assert_eq!(code, want, "{bad}: {err}");
            assert!(out.is_empty(), "{bad}");
        }
    });
}

#[test]
fn reverse_finds_where_lines_last_existed() {
    each_format(|r| {
        linear(r);
        for args in [
            &["--reverse", "HEAD~4..HEAD", "a.txt"][..],
            &["--reverse", "HEAD~3", "a.txt"],
            &["--reverse", "^HEAD~2", "HEAD", "a.txt"],
            &["--reverse", "HEAD~4..HEAD~1", "a.txt"],
            &["--reverse", "HEAD~2..HEAD", "-f", "-n", "a.txt"],
            &["--reverse", "HEAD~3", "-e", "-t", "a.txt"],
            &["--reverse", "--root", "HEAD~4..HEAD", "a.txt"],
            &["--reverse", "-w", "HEAD~4..HEAD", "a.txt"],
            &["--reverse", "-L", "2,6", "HEAD~4..HEAD", "a.txt"],
            &["--reverse", "HEAD~4..HEAD", "--", "a.txt"],
            &["--reverse", "HEAD~4", "--", "a.txt"],
            &["--reverse", "--", "a.txt", "HEAD~3"],
        ] {
            r.same(args);
        }
        r.same_annotate(&["--reverse", "HEAD~3", "a.txt"]);
        r.same(&["--reverse", "-p", "HEAD~4..HEAD", "a.txt"]);
        r.same(&["--reverse", "--line-porcelain", "HEAD~3", "a.txt"]);
        for args in [
            &["--reverse", "a.txt"][..],
            &["--reverse", "HEAD~1", "HEAD~2", "a.txt"],
            &["--reverse", "^HEAD~1", "^HEAD~2", "HEAD", "a.txt"],
            &["--reverse", "HEAD~1", "nonexistent.txt"],
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", args);
            let (code, _, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(code, want, "{args:?}: {err}");
        }
        let (code, _, _) = r.pyn(
            &r.dir,
            "blame",
            &["--reverse", "--contents", "a.txt", "HEAD~1", "a.txt"],
        );
        assert_eq!(code, 128);
    });
}

#[test]
fn reverse_across_merges_and_random_histories_matches_git() {
    each_format(|r| {
        merged(r);
        for args in [
            &["--reverse", "HEAD~6..HEAD", "m.txt"][..],
            &["--reverse", "HEAD~4..HEAD", "m.txt"],
            &["--reverse", "HEAD~3", "m.txt"],
            &["--reverse", "HEAD~2", "m.txt"],
            &["--reverse", "side~1..HEAD", "m.txt"],
            &["--reverse", "side~1..late", "m.txt"],
            &["--reverse", "side~1..side", "m.txt"],
            &["--reverse", "-w", "HEAD~4", "m.txt"],
        ] {
            r.same(args);
        }
    });
    for format in FORMATS {
        for seed in 0..40u64 {
            let r = Repo::new(format);
            let path = random_history(&r, seed);
            let out = git(&r.dir)
                .args(["rev-list", "--max-parents=0", "HEAD"])
                .output()
                .unwrap();
            let root = String::from_utf8(out.stdout).unwrap().trim().to_string();
            r.same(&["--reverse", &format!("{root}..HEAD"), "f.txt"]);
            r.same(&["--reverse", &format!("{root}~0"), "-f", "f.txt"]);
            let mid = r.rev("HEAD~4");
            r.same(&["--reverse", &format!("{mid}..HEAD"), &path]);
        }
    }
}

#[test]
fn symmetric_ranges_match_git() {
    each_format(|r| {
        merged(r);
        for args in [
            &["HEAD...HEAD~1", "m.txt"][..],
            &["HEAD~1...HEAD", "m.txt"],
            &["HEAD...HEAD", "m.txt"],
            &["...HEAD~2", "m.txt"],
            &["side...main", "m.txt"],
            &["HEAD~2...HEAD~4", "m.txt"],
            &["HEAD~3...HEAD", "--", "m.txt"],
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", args);
            let (code, _, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(code, want, "{args:?}: {err}");
            if want == 0 {
                r.same(args);
            }
        }
    });
}

#[test]
fn minimal_and_progress_match_git() {
    each_format(|r| {
        linear(r);
        r.same(&["--minimal", "a.txt"]);
        r.same(&["--minimal", "-w", "-f", "a.txt"]);
        r.same(&["--progress", "a.txt"]);
        r.same(&["--no-progress", "a.txt"]);
        for args in [
            &["--progress", "-p", "a.txt"][..],
            &["--progress", "--line-porcelain", "a.txt"],
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", args);
            let (code, _, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(code, want, "{args:?}: {err}");
        }
    });
    for format in FORMATS {
        for seed in 0..20u64 {
            let r = Repo::new(format);
            let path = random_history(&r, seed);
            r.same(&["--minimal", &path]);
        }
    }
}

/// Each name of the exit status and the part of the message that git fixes: the commits named, in order.
fn dug_from(err: &str) -> Option<String> {
    err.split("dig from ")
        .nth(1)
        .or_else(|| err.split("dig up from, ").nth(1))
        .map(|t| t.trim().to_string())
}

#[test]
fn reverse_with_first_parent_follows_the_first_parent_chain() {
    each_format(|r| {
        merged(r);
        let root = r.rev("HEAD~4");
        let mut candidates: Vec<Vec<String>> = Vec::new();
        for range in [
            "HEAD~1..HEAD",
            "HEAD~2..HEAD",
            "HEAD~3..HEAD",
            "HEAD~4..HEAD",
            "HEAD~3..HEAD~1",
            "HEAD~4..HEAD~2",
            "HEAD~1..late",
            "side..HEAD",
            "side~1..side",
            "side~1..HEAD",
            "late..HEAD",
            "HEAD..HEAD~1",
            "HEAD~2",
            "HEAD~4",
            "side",
            "side~1",
            "HEAD",
        ] {
            candidates.push(vec![range.to_string()]);
        }
        candidates.push(vec![format!("^{root}"), "HEAD".into()]);
        candidates.push(vec!["HEAD~2..HEAD".into(), "HEAD~1".into()]);
        candidates.push(vec!["HEAD~1".into(), "HEAD~2..HEAD".into()]);
        candidates.push(vec!["HEAD~2..HEAD".into(), "late".into(), "side".into()]);
        for revs in candidates {
            let mut args: Vec<&str> = vec!["--reverse", "--first-parent"];
            args.extend(revs.iter().map(String::as_str));
            args.push("m.txt");
            let (want, _) = r.oracle(&r.dir, "blame", &args);
            let (code, _, err) = r.pyn(&r.dir, "blame", &args);
            assert_eq!(
                oracle::status_class(code),
                oracle::status_class(want),
                "{args:?}: {err}"
            );
            if want == 0 {
                r.same(&args);
                let mut with = args.clone();
                with.splice(2..2, ["-f", "-n", "-e"]);
                r.same(&with);
                r.same(&[&["-p"][..], &args[..]].concat());
            }
            let git_err = git(&r.dir)
                .args(["blame"])
                .args(&args)
                .output()
                .unwrap()
                .stderr;
            let theirs = dug_from(&String::from_utf8_lossy(&git_err));
            if theirs.is_some() {
                assert_eq!(dug_from(&err), theirs, "{args:?}");
            }
        }
    });
    for format in FORMATS {
        for seed in 0..40u64 {
            let r = Repo::new(format);
            let path = random_history(&r, seed);
            let out = git(&r.dir)
                .args(["rev-list", "--max-parents=0", "HEAD"])
                .output()
                .unwrap();
            let root = String::from_utf8(out.stdout).unwrap().trim().to_string();
            r.same(&[
                "--reverse",
                "--first-parent",
                &format!("{root}..HEAD"),
                "f.txt",
            ]);
            r.same(&["--reverse", "--first-parent", &root, &path]);
            let mid = r.rev("HEAD~4");
            let args = [
                "--reverse",
                "--first-parent",
                &format!("{mid}..HEAD"),
                &path,
            ];
            let (want, _) = r.oracle(&r.dir, "blame", &args);
            let (code, _, err) = r.pyn(&r.dir, "blame", &args);
            assert_eq!(code, want, "{args:?}: {err}");
            if want == 0 {
                r.same(&args);
            }
        }
    }
}

/// `c.txt` keeps same-commit lines contiguous in the source (an edit, never a deletion between them): git releases
/// differ on whether lines split by a deleted source line are one entry, so `a.txt` is not compared for repeated lines.
fn colored(r: &Repo) {
    linear(r);
    recent(r);
    let mut l = body(10);
    r.lines("c.txt", &refs(&l));
    r.commit(ANN, "c one");
    l[4] = "c changed".into();
    l[5] = "c changed too".into();
    r.lines("c.txt", &refs(&l));
    r.commit(BOB, "c two");
}

#[test]
fn colors_match_git() {
    each_format(|r| {
        colored(r);
        for file in ["a.txt", "t.txt", "c.txt"] {
            for flags in [
                &["--color-lines"][..],
                &["--color-by-age"],
                &["--color-lines", "--color-by-age"],
                &["--color-by-age", "--color-lines"],
                &["--no-color-lines"],
                &["--color-by-age", "--no-color-by-age"],
                &["--color-lines", "-s"],
                &["--color-by-age", "-e", "-f", "-n"],
                &["--color-lines", "--color-by-age", "-l", "-t", "-b"],
                &["--color-lines", "--color-by-age", "--root"],
                &["--color-by-age", "-c"],
                &["--color-lines", "-p"],
                &["--color-by-age", "--line-porcelain"],
                &["--color-lines", "--color-by-age", "-L", "2,6"],
                &["--color-lines", "--color-by-age", "--date=short"],
            ] {
                if file == "a.txt" && flags.contains(&"--color-lines") && !flags.contains(&"-c") {
                    continue;
                }
                let args: Vec<&str> = flags.iter().copied().chain([file]).collect();
                r.same(&args);
            }
        }
        r.same_annotate(&["--color-by-age", "--color-lines", "c.txt"]);
        r.same_annotate(&["--color-lines", "a.txt"]);
        for (key, value) in [
            ("blame.coloring", "repeatedLines"),
            ("blame.coloring", "highlightRecent"),
            ("blame.coloring", "none"),
            ("blame.coloring", "bogus"),
            ("color.blame.repeatedLines", "bold green"),
            ("color.blame.repeatedLines", "nonsense"),
            ("color.blame.repeatedLines", ""),
            (
                "color.blame.highlightRecent",
                "red,6 months ago,bold green,3 days ago,yellow",
            ),
            ("color.blame.highlightRecent", "magenta"),
            ("color.blame.highlightRecent", ""),
            ("color.blame.highlightRecent", "red,1999-01-01,blue"),
            ("color.blame.highlightRecent", "red,6 months ago"),
            ("color.blame.highlightRecent", "nonsense"),
        ] {
            r.git_ok(ANN, &["config", key, value]);
            for flags in [
                &[][..],
                &["--color-lines"],
                &["--color-by-age"],
                &["--no-color-lines"],
                &["--no-color-by-age"],
                &["--color-lines", "--color-by-age"],
            ] {
                for file in ["a.txt", "t.txt", "c.txt"] {
                    let repeated = flags.contains(&"--color-lines")
                        || (key == "blame.coloring" && value == "repeatedLines");
                    if file == "a.txt" && repeated {
                        continue;
                    }
                    let args: Vec<&str> = flags.iter().copied().chain([file]).collect();
                    let (want_code, want) = r.oracle(&r.dir, "blame", &args);
                    let (code, got, err) = r.pyn(&r.dir, "blame", &args);
                    assert_eq!(
                        oracle::status_class(code),
                        oracle::status_class(want_code),
                        "{key}={value} {args:?}: {err}"
                    );
                    if want_code == 0 {
                        assert_eq!(
                            oracle::blame_lines(&got),
                            oracle::blame_lines(&want),
                            "{key}={value} {args:?}"
                        );
                    }
                }
            }
            r.git_ok(ANN, &["config", "--unset", key]);
        }
    });
}

#[test]
fn graft_files_replace_parents() {
    each_format(|r| {
        merged(r);
        let ids: Vec<String> = [
            "HEAD", "HEAD~1", "HEAD~2", "HEAD~3", "HEAD~4", "side", "side~1", "late",
        ]
        .iter()
        .map(|n| r.rev(n))
        .collect();
        let (head, m1, m2, m3, root, side, side1, late) = (
            &ids[0], &ids[1], &ids[2], &ids[3], &ids[4], &ids[5], &ids[6], &ids[7],
        );
        let full = {
            let out = git(&r.dir)
                .args(["rev-list", "--parents", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let upper = format!("{} {}\n", m1.to_uppercase(), root);
        let cases: Vec<(&str, String)> = vec![
            ("the real graph", full),
            ("a commit given a new parent", format!("{m1} {root}\n")),
            ("two parents", format!("{head} {m1} {side}\n")),
            ("a root", format!("{m1}\n")),
            (
                "several lines",
                format!("{m1} {root}\n{late} {side1}\n{side} {root}\n"),
            ),
            (
                "the last of two for a commit wins",
                format!("{m1} {root}\n{m1} {side}\n"),
            ),
            ("no final newline", format!("{m1} {root}")),
            (
                "blank lines and comments",
                format!("\n# note\n{m1} {root}\n\n"),
            ),
            ("trailing space", format!("{m1} {root} \n")),
            ("a tab between", format!("{m1}\t{root}\n")),
            ("capitals", upper),
            (
                "an unknown commit",
                format!("{} {root}\n{m2} {m3}\n", "1".repeat(root.len())),
            ),
            ("a short id", format!("{m1} {}\n", &root[..10])),
            ("a double space", format!("{m1}  {root}\n")),
            ("an id with junk", format!("{m1} {root}x\n")),
            ("only an id and a short one", format!("{}\n", &m1[..10])),
            ("nothing", String::new()),
        ];
        for (name, text) in cases {
            let file = r.dir.parent().unwrap().join("grafts");
            fs::write(&file, text).unwrap();
            let file = file.to_str().unwrap();
            for args in [
                &["-S", file, "m.txt"][..],
                &["-S", file, "-f", "-n", "m.txt"],
                &["-S", file, "--root", "m.txt"],
                &["-S", file, "--first-parent", "m.txt"],
                &["-S", file, "-p", "m.txt"],
                &["-S", file, "HEAD~1", "m.txt"],
                &["-S", file, "late", "m.txt"],
            ] {
                let (want, _) = r.oracle(&r.dir, "blame", args);
                let (code, _, err) = r.pyn(&r.dir, "blame", args);
                assert_eq!(code, want, "{name} {args:?}: {err}");
                r.same(args);
            }
            r.same(&[&format!("-S{file}"), "m.txt"]);
        }
        let (want, _) = r.oracle(&r.dir, "blame", &["-S", "nonexistent", "m.txt"]);
        let (code, out, _) = r.pyn(&r.dir, "blame", &["-S", "nonexistent", "m.txt"]);
        assert_eq!((code, want), (128, 128));
        assert!(out.is_empty());
    });
}

#[test]
fn graft_files_reach_ranges_and_reverse() {
    each_format(|r| {
        merged(r);
        let id = |n: &str| r.rev(n);
        let cases = [
            format!("{} {}\n", id("HEAD~1"), id("HEAD~4")),
            format!("{}\n", id("HEAD~3")),
            format!("{}\n", id("side~1")),
            format!("{} {}\n", id("HEAD~2"), id("HEAD~4")),
            format!("{} {}\n{}\n", id("HEAD~1"), id("HEAD~3"), id("side")),
            format!("{} {}\n", id("late"), id("HEAD~4")),
            String::new(),
        ];
        let file = r.dir.parent().unwrap().join("grafts");
        for text in cases {
            fs::write(&file, &text).unwrap();
            let file = file.to_str().unwrap();
            for tail in [
                &["HEAD~2..HEAD"][..],
                &["^HEAD~2", "HEAD"],
                &["HEAD~4..HEAD"],
                &["HEAD~1...late"],
                &["HEAD~4...late"],
                &["main...late"],
                &["side..HEAD"],
                &["--reverse", "HEAD~4..HEAD"],
                &["--reverse", "HEAD~2..HEAD"],
                &["--reverse", "HEAD~4"],
                &["--reverse", "HEAD~4..late"],
                &["--reverse", "--first-parent", "HEAD~4..HEAD"],
                &["--reverse", "--first-parent", "HEAD~3..HEAD"],
                &["--reverse", "-p", "HEAD~4..HEAD"],
            ] {
                let mut args = vec!["-S", file];
                args.extend_from_slice(tail);
                args.push("m.txt");
                r.same(&args);
            }
        }
    });
}

#[test]
fn incremental_prints_entries_as_settled_like_git() {
    each_format(|r| {
        merged(r);
        for args in [
            &["--incremental", "m.txt"][..],
            &["--incremental", "--root", "m.txt"],
            &["--incremental", "-w", "m.txt"],
            &["--incremental", "-L", "3,9", "m.txt"],
            &["--incremental", "--first-parent", "m.txt"],
            &["--incremental", "HEAD~2", "m.txt"],
            &["--incremental", "HEAD~4..", "m.txt"],
            &["--incremental", "side...late", "m.txt"],
            &["--incremental", "--reverse", "HEAD~4..HEAD", "m.txt"],
            &["--incremental", "--reverse", "HEAD~3", "m.txt"],
            &["--incremental", "-p", "m.txt"],
            &["--incremental", "-s", "-e", "m.txt"],
            &["--incremental", "--progress", "m.txt"],
        ] {
            r.same(args);
        }
        r.same_masked(&["--incremental", "m.txt"]);
        let mut text = fs::read_to_string(r.dir.join("m.txt")).unwrap();
        text = text.replace("line 4", "work tree four");
        r.write("m.txt", text.as_bytes());
        r.same_masked(&["--incremental", "m.txt"]);
        let (code, out, _) = r.pyn(&r.dir, "blame", &["--incremental=x", "m.txt"]);
        assert_eq!(code, 129);
        assert!(out.is_empty());
    });
    each_format(|r| {
        r.lines("a.txt", &["one", "two", "three"]);
        r.commit(ANN, "one");
        r.git_ok(BOB, &["mv", "a.txt", "b.txt"]);
        r.lines("b.txt", &["one", "two", "three", "four"]);
        r.git_ok(BOB, &["add", "-A"]);
        r.git_ok(BOB, &["commit", "-q", "-m", "rename and add"]);
        r.git_ok(BOB, &["mv", "b.txt", "a.txt"]);
        r.git_ok(BOB, &["commit", "-q", "-m", "back"]);
        r.same(&["--incremental", "a.txt"]);
        r.same(&["--incremental", "-f", "a.txt"]);
    });
    for format in FORMATS {
        for seed in 1..=12u64 {
            let r = Repo::new(format);
            let cur = random_history(&r, seed);
            r.same(&["--incremental", &cur]);
            r.same(&["--incremental", "-w", &cur]);
            r.same(&["--incremental", "-L", "2,5", "-L", "6,9", &cur]);
            r.same(&["--incremental", "-L", "3,12", &cur]);
            r.same(&["--incremental", "HEAD~4..", "--", &cur]);
            r.same(&["--incremental", "--reverse", "HEAD~4..HEAD", &cur]);
        }
    }
}

const FUNCS: &str = "#include <x>\n\nint alpha(void)\n{\n\treturn 1;\n}\n\nstatic int beta(int x)\n{\n\tif (x)\n\t\treturn 2;\n\n\treturn 3;\n}\n$weird() {\n}\n indented()\n\nlast line\n";

#[test]
fn function_ranges_match_git() {
    each_format(|r| {
        r.write("f.c", FUNCS.as_bytes());
        r.commit(ANN, "one");
        let edited = FUNCS
            .replace("return 2;", "return 22;")
            .replace("alpha", "alpha2");
        r.write("f.c", edited.as_bytes());
        r.commit(BOB, "two");
        r.write("empty.c", b"");
        r.commit(CY, "empty");
        let compare = |args: &[&str]| {
            let (want, _) = r.oracle(&r.dir, "blame", args);
            let (code, out, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(
                oracle::status_class(code),
                oracle::status_class(want),
                "{args:?}: {err}"
            );
            if want == 0 {
                r.same(args);
            } else {
                assert!(out.is_empty(), "{args:?}");
            }
        };
        for range in [
            ":alpha",
            ":alpha2",
            ":beta",
            ":int",
            "^:int",
            ":nothing",
            ":weird",
            ":include",
            ":last",
            ":x",
            ":alpha,+2",
            "3,:beta",
            ":",
            ":ind",
            ":\\(",
            ":a\\+",
            ":alpha:",
            ":[[:alpha:]]*ta",
            ":nothing:x",
            ":alpha:x",
            ":a:b",
            "^:beta",
            ":[",
            "1,:beta",
            "/alpha/,:beta",
            ":^int",
            ":void$",
            ":(x)",
            ":e",
        ] {
            compare(&["-L", range, "f.c"]);
            compare(&["-L", range, "-s", "-f", "f.c"]);
        }
        compare(&["-L", ":alpha", "-L", ":beta", "f.c"]);
        compare(&["-L", ":beta", "-L", ":alpha", "f.c"]);
        compare(&["-L", ":alpha", "-L", "^:alpha", "f.c"]);
        compare(&["-L", "1,2", "-L", ":beta", "f.c"]);
        compare(&["-L", "8,9", "-L", ":alpha", "f.c"]);
        compare(&["-L", "/alpha/", "-L", ":beta", "f.c"]);
        compare(&["-L", ":alpha", "HEAD~1", "f.c"]);
        compare(&["-L:alpha", "f.c"]);
        compare(&["-L", ":a", "empty.c"]);
        compare(&["-L", ":a", "--reverse", "HEAD~2", "f.c"]);
    });
}

#[test]
fn function_ranges_follow_the_diff_driver() {
    each_format(|r| {
        r.write("f.c", FUNCS.as_bytes());
        r.write("g.txt", b"alpha\n  one\n  two\nbeta\n  three\n");
        r.write(
            ".gitattributes",
            b"g.txt diff=mine\nf.c diff=mydefault\nx.rs diff=rust\n",
        );
        r.commit(ANN, "one");
        r.git_ok(ANN, &["config", "diff.mine.funcname", "^[a-z]"]);
        r.git_ok(ANN, &["config", "diff.mine.xfuncname", "^(beta)"]);
        r.git_ok(ANN, &["config", "diff.mydefault.binary", "false"]);
        for args in [
            &["-L", ":alpha", "g.txt"][..],
            &["-L", ":beta", "g.txt"],
            &["-L", ":a", "g.txt"],
            &["-L", ":alpha", "f.c"],
            &["-L", ":beta", "f.c"],
        ] {
            let (want, _) = r.oracle(&r.dir, "blame", args);
            let (code, _, err) = r.pyn(&r.dir, "blame", args);
            assert_eq!(
                oracle::status_class(code),
                oracle::status_class(want),
                "{args:?}: {err}"
            );
            if want == 0 {
                r.same(args);
            }
        }
        r.write(
            "x.rs",
            b"use x;\npub fn one() {}\nlet y = 1;\nfn two() {}\n",
        );
        r.commit(ANN, "rust");
        r.same(&["-L", ":one", "x.rs"]);
        r.same(&["-L", ":two", "x.rs"]);
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
            &["--show-stats", "a.txt"],
            &["--score-debug", "a.txt"],
            &["--diff-algorithm=patience", "a.txt"],
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
