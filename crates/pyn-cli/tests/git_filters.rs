//! Filter drivers (`filter=<name>` with `clean`, `smudge`, `process` and `required`) on a git repository, compared with
//! real git on identical repositories: the staged blob ids, work tree bytes, `status` and `diff`. Real git is the
//! oracle in these tests only; exit statuses are compared by class and never wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const ROT: &str = "tr 'a-zA-Z' 'n-za-mN-ZA-M'";

/// A long-running filter speaking the pkt-line protocol: rot13 both ways, `status=error` for `*.fail`.
const PROCESS: &str = r#"
import sys
log = open(sys.argv[1], "a")
inp, out = sys.stdin.buffer, sys.stdout.buffer
def rd():
    h = inp.read(4)
    if len(h) < 4:
        sys.exit(0)
    n = int(h, 16)
    return None if n == 0 else inp.read(n - 4)
def wr(b):
    out.write(b"%04x" % (len(b) + 4) + b)
def fl():
    out.write(b"0000"); out.flush()
def lst():
    r = []
    while True:
        p = rd()
        if p is None:
            return r
        r.append(p.rstrip(b"\n"))
def content():
    r = b""
    while True:
        p = rd()
        if p is None:
            return r
        r += p
tr = bytes.maketrans(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ", b"nopqrstuvwxyzabcdefghijklmNOPQRSTUVWXYZABCDEFGHIJKLM")
lst()
wr(b"git-filter-server\n"); wr(b"version=2\n"); fl()
lst()
wr(b"capability=clean\n"); wr(b"capability=smudge\n"); fl()
log.write("start\n"); log.flush()
while True:
    head = dict(l.split(b"=", 1) for l in lst())
    data = content()
    log.write("%s %s\n" % (head[b"command"].decode(), head[b"pathname"].decode())); log.flush()
    if head[b"pathname"].endswith(b".fail"):
        wr(b"status=error\n"); fl()
        continue
    wr(b"status=success\n"); fl()
    res = data.translate(tr)
    for i in range(0, len(res), 65516):
        wr(res[i:i + 65516])
    fl(); fl()
"#;

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    oracle: PathBuf,
    ours: PathBuf,
}

impl Pair {
    /// Two repositories with `config` appended to `.git/config` (`{log}` becomes the repository's process log) and
    /// `files` written but not added.
    fn new(format: &str, config: &str, files: &[(&str, &[u8])]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::write(root.join("proc.py"), PROCESS).unwrap();
        let (oracle, ours) = (root.join("oracle"), root.join("ours"));
        for (name, repo) in [("oracle", &oracle), ("ours", &ours)] {
            init(repo, format, false);
            run(repo, &["config", "user.name", "t"]);
            run(repo, &["config", "user.email", "t@example.com"]);
            let process = format!(
                "python3 {} {}",
                root.join("proc.py").display(),
                root.join(format!("log-{name}")).display()
            );
            let mut cfg = fs::read_to_string(repo.join(".git/config")).unwrap();
            cfg.push_str(&config.replace("{process}", &process).replace("{rot}", ROT));
            fs::write(repo.join(".git/config"), cfg).unwrap();
            for (path, bytes) in files {
                write(repo, path, bytes);
            }
        }
        Self {
            _tmp: tmp,
            root,
            oracle,
            ours,
        }
    }

    fn pyn(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.ours)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git(&self, args: &[&str]) -> (i32, String, String) {
        let out = git(&self.oracle)
            .env("GIT_EDITOR", "true")
            .args(args)
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Runs `args` with both tools and requires the same exit status class.
    fn both(&self, args: &[&str]) -> i32 {
        let (want, got) = (self.git(args), self.pyn(args));
        assert_eq!(
            oracle::status_class(got.0),
            oracle::status_class(want.0),
            "{args:?}: pyn said {}",
            got.2
        );
        want.0
    }

    fn same_index(&self, what: &str) {
        let ls = |dir: &Path| run(dir, &["ls-files", "-s"]);
        assert_eq!(ls(&self.ours), ls(&self.oracle), "{what}: index");
    }

    fn same_tree(&self, what: &str) {
        assert_eq!(tree(&self.ours), tree(&self.oracle), "{what}: work tree");
    }

    fn same_status(&self, what: &str) {
        let (got, want) = (
            self.pyn(&["status", "--porcelain"]),
            self.git(&["status", "--porcelain"]),
        );
        assert_eq!(got.1, want.1, "{what}: status");
    }

    fn same_diff(&self, what: &str, args: &[&str]) {
        let mut a = vec!["diff"];
        a.extend_from_slice(args);
        let (got, want) = (self.pyn(&a), self.git(&a));
        assert_eq!(got.0, want.0, "{what}: diff status");
        assert_eq!(got.1, want.1, "{what}: diff");
    }

    fn each(&self, f: impl Fn(&Path)) {
        f(&self.oracle);
        f(&self.ours);
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(format!("log-{name}"))).unwrap_or_default()
    }
}

fn write(repo: &Path, path: &str, bytes: &[u8]) {
    let file = repo.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, bytes).unwrap();
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy();
                out.insert(rel.into_owned(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

const FILES: &[(&str, &[u8])] = &[
    (".gitattributes", b"*.txt filter=f\n"),
    ("a.txt", b"alpha\nbeta\ngamma\n"),
    ("sub/b.txt", b"Hello World\r\nsecond\r\n"),
    ("m.txt", b"one\ntwo\nthree\nfour\nfive\n"),
    ("plain.dat", b"not filtered\n"),
    ("empty.txt", b""),
];

/// Add, commit, wipe and restore, edit, `commit -a`, switch, reset, stash and merge, comparing each step with git.
fn flow(p: &Pair, format: &str) {
    p.both(&["add", "-A"]);
    p.same_index("add");
    p.same_status("after add");
    p.both(&["commit", "-q", "-m", "one"]);
    p.same_status("after commit");

    p.each(|r| {
        for path in tree(r).keys() {
            fs::remove_file(r.join(path)).unwrap();
        }
    });
    p.same_status("deleted");
    p.both(&["restore", "--", "."]);
    p.same_tree("restore");
    p.same_status("after restore");

    p.each(|r| {
        for (path, mut bytes) in tree(r) {
            if path.ends_with(".txt") {
                bytes.extend_from_slice(b"tail\n");
                fs::write(r.join(path), bytes).unwrap();
            }
        }
    });
    p.same_status("edited");
    p.same_diff("edited", &[]);
    p.same_diff("edited stat", &["--stat"]);
    p.both(&["add", "-A"]);
    p.same_diff("cached", &["--cached"]);
    p.both(&["commit", "-q", "-m", "two"]);

    p.each(|r| write(r, "a.txt", b"changed\n"));
    p.both(&["commit", "-q", "-a", "-m", "three"]);
    p.same_index("commit -a");
    p.same_status("after commit -a");

    // Stash is compared in SHA-1 repositories only: the oracle's support for SHA-256 varies.
    if format == "sha1" {
        p.each(|r| write(r, "a.txt", b"stashed edit\n"));
        p.both(&["stash"]);
        p.same_tree("stash");
        p.same_status("after stash");
        p.both(&["stash", "pop"]);
        p.same_tree("stash pop");
        p.same_status("after pop");
        p.both(&["checkout", "--", "a.txt"]);
    }
    p.each(|r| {
        run(r, &["branch", "other", "HEAD~2"]);
    });
    p.both(&["switch", "other"]);
    p.same_tree("switch");
    p.same_status("after switch");
    p.both(&["switch", "main"]);
    p.both(&["reset", "-q", "--hard", "HEAD~1"]);
    p.same_tree("reset --hard");
    p.same_status("after reset");
    p.both(&["reset", "-q", "--hard", "other"]);
    p.both(&["restore", "--source=main", "--", "a.txt"]);
    p.same_tree("restore --source");

    p.both(&["switch", "main"]);
    p.both(&["switch", "-c", "side"]);
    p.each(|r| write(r, "m.txt", b"ONE\ntwo\nthree\nfour\nfive\n"));
    p.both(&["commit", "-q", "-a", "-m", "side"]);
    p.both(&["switch", "main"]);
    p.each(|r| write(r, "m.txt", b"one\ntwo\nthree\nfour\nFIVE\n"));
    p.both(&["commit", "-q", "-a", "-m", "main"]);
    p.both(&["merge", "-q", "--no-edit", "side"]);
    p.same_tree("merge");
    p.same_index("merge");
    p.same_status("after merge");
}

#[test]
fn clean_and_smudge_commands_match_git() {
    for format in FORMATS {
        let cfg = "[filter \"f\"]\n\tclean = {rot}\n\tsmudge = {rot}\n";
        flow(&Pair::new(format, cfg, FILES), format);
    }
}

#[test]
fn percent_f_names_the_path() {
    let cfg = "[filter \"f\"]\n\tclean = \"echo %f; cat\"\n\tsmudge = sed 1d\n";
    let p = Pair::new("sha1", cfg, FILES);
    p.both(&["add", "-A"]);
    p.same_index("add");
    p.both(&["commit", "-q", "-m", "one"]);
    p.each(|r| fs::remove_file(r.join("sub/b.txt")).unwrap());
    p.both(&["restore", "--", "."]);
    p.same_tree("restore");
}

#[test]
fn process_filter_matches_git_and_starts_once_per_command() {
    for format in FORMATS {
        let cfg = "[filter \"f\"]\n\tprocess = {process}\n";
        let p = Pair::new(format, cfg, FILES);
        p.both(&["add", "-A"]);
        let ours = p.log("ours");
        assert_eq!(ours.matches("start").count(), 1, "{ours}");
        for path in ["a.txt", "sub/b.txt", "m.txt", "empty.txt"] {
            assert!(ours.contains(&format!("clean {path}")), "{ours}");
        }
        assert!(!ours.contains("plain.dat"), "{ours}");
        p.both(&["commit", "-q", "-m", "one"]);
        p.each(|r| fs::remove_file(r.join("a.txt")).unwrap());
        p.both(&["restore", "a.txt"]);
        assert!(p.log("ours").contains("smudge a.txt"));
        flow(&Pair::new(format, cfg, FILES), format);
    }
}

#[test]
fn process_wins_over_clean_and_smudge() {
    let cfg = "[filter \"f\"]\n\tprocess = {process}\n\tclean = false\n\tsmudge = false\n";
    let p = Pair::new("sha1", cfg, FILES);
    flow(&p, "sha1");
}

#[test]
fn process_error_status_leaves_the_file_unfiltered() {
    let files: &[(&str, &[u8])] = &[
        (".gitattributes", b"*.fail filter=f\n*.txt filter=f\n"),
        ("x.fail", b"some text\n"),
        ("a.txt", b"alpha\n"),
    ];
    let p = Pair::new("sha1", "[filter \"f\"]\n\tprocess = {process}\n", files);
    p.both(&["add", "-A"]);
    p.same_index("add");
    let p = Pair::new(
        "sha1",
        "[filter \"f\"]\n\tprocess = {process}\n\trequired\n",
        files,
    );
    assert_ne!(p.both(&["add", "-A"]), 0);
    p.same_index("required add");
}

#[test]
fn required_driver_without_a_command_or_failing_is_fatal() {
    let files: &[(&str, &[u8])] = &[(".gitattributes", b"*.txt filter=r\n"), ("a.txt", b"a\n")];
    for cfg in [
        "[filter \"r\"]\n\trequired = true\n",
        "[filter \"r\"]\n\tclean = false\n\trequired = true\n",
        "[filter \"r\"]\n\tclean = cat\n\trequired = true\n",
    ] {
        let p = Pair::new("sha1", cfg, files);
        let code = p.both(&["add", "-A"]);
        p.same_index(cfg);
        if code == 0 {
            p.both(&["commit", "-q", "-m", "x"]);
            p.each(|r| fs::remove_file(r.join("a.txt")).unwrap());
            assert_ne!(p.both(&["restore", "a.txt"]), 0, "{cfg}");
        }
    }
}

#[test]
fn failing_driver_that_is_not_required_is_ignored() {
    let files: &[(&str, &[u8])] = &[(".gitattributes", b"*.txt filter=r\n"), ("a.txt", b"a\n")];
    for cfg in [
        "[filter \"r\"]\n\tclean = false\n\tsmudge = false\n",
        "[filter \"r\"]\n\tclean = exit 3\n",
        "[filter \"r\"]\n\tsmudge = cat\n",
    ] {
        let p = Pair::new("sha1", cfg, files);
        p.both(&["add", "-A"]);
        p.same_index(cfg);
        p.both(&["commit", "-q", "-m", "x"]);
        p.each(|r| fs::remove_file(r.join("a.txt")).unwrap());
        p.both(&["restore", "a.txt"]);
        p.same_tree(cfg);
    }
}

#[test]
fn attribute_without_a_configured_driver_changes_nothing() {
    let files: &[(&str, &[u8])] = &[
        (".gitattributes", b"*.txt filter=ghost\n"),
        ("a.txt", b"a\n"),
    ];
    let p = Pair::new("sha1", "", files);
    p.both(&["add", "-A"]);
    p.same_index("add");
}

#[test]
fn filters_run_before_and_after_line_ending_conversion() {
    let cfg = "[filter \"f\"]\n\tclean = sed 's/$/!/'\n\tsmudge = sed 's/!$//'\n";
    let files: &[(&str, &[u8])] = &[
        (".gitattributes", b"*.txt filter=f text eol=crlf\n"),
        ("a.txt", b"a\r\nb\n"),
        ("c.txt", b"x\ny\n"),
    ];
    let p = Pair::new("sha1", cfg, files);
    p.both(&["add", "-A"]);
    p.same_index("add");
    p.both(&["commit", "-q", "-m", "x"]);
    p.each(|r| {
        fs::remove_file(r.join("a.txt")).unwrap();
        fs::remove_file(r.join("c.txt")).unwrap();
    });
    p.both(&["restore", "."]);
    p.same_tree("restore");
    p.same_status("status");
}

#[test]
fn filter_runs_in_the_work_tree_root() {
    let cfg = "[filter \"f\"]\n\tclean = \"test -f .gitattributes && echo root; cat\"\n";
    let files: &[(&str, &[u8])] = &[(".gitattributes", b"*.txt filter=f\n"), ("d/a.txt", b"a\n")];
    let p = Pair::new("sha1", cfg, files);
    let (want, got) = (
        p.git(&["add", "-A"]),
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(p.ours.join("d"))
            .args(["add", "-A"])
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", p.root.join("home"))
            .output()
            .unwrap(),
    );
    assert_eq!(want.0, 0);
    assert!(got.status.success());
    let blob = |dir: &Path| run(dir, &["cat-file", "blob", ":d/a.txt"]);
    assert_eq!(blob(&p.ours), blob(&p.oracle));
}
