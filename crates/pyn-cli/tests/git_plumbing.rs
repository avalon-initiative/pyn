//! `pyn rev-parse`, `ls-files`, `ls-tree`, `cat-file`, `show-ref`, `shortlog`, `describe` and `name-rev` on a git
//! repository, compared byte for byte with the real git on the same repository: standard output, the exit status and
//! whether standard error carries anything. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::{FORMATS, git, init, path_env};
use tempfile::{TempDir, tempdir};

struct Fx {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    format: &'static str,
    tick: i64,
}

impl Fx {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        init(&repo, format, false);
        fs::create_dir_all(root.join("home")).unwrap();
        Self {
            _tmp: tmp,
            root,
            repo,
            format,
            tick: 1_700_000_000,
        }
    }

    fn g(&mut self, args: &[&str]) -> String {
        self.tick += 86_400;
        self.g_as(args, "Fixture Author", "author@example.com")
    }

    fn g_as(&mut self, args: &[&str], name: &str, email: &str) -> String {
        let date = format!("{} +0000", self.tick);
        let out = git(&self.repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", name)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_NAME", "Fixture Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, path: &str, content: &str) {
        let file = self.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn commit(&mut self, message: &str) {
        self.g(&["add", "-A"]);
        self.g(&["commit", "-q", "-m", message]);
    }

    fn rev(&self, spec: &str) -> String {
        let out = git(&self.repo).args(["rev-parse", spec]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Runs the oracle and pyn in `cwd` and demands the same output, status and stream use.
    fn same(&self, cwd: &Path, args: &[&str]) {
        self.same_in(cwd, args, b"");
    }

    fn same_in(&self, cwd: &Path, args: &[&str], stdin: &[u8]) {
        let want = self.run(Command::new("git"), cwd, args, stdin, true);
        let got = self.run(
            Command::new(env!("CARGO_BIN_EXE_pyn")),
            cwd,
            args,
            stdin,
            false,
        );
        let at = cwd.strip_prefix(&self.repo).unwrap_or(cwd).display();
        let shown = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        assert_eq!(
            shown(&got.1),
            shown(&want.1),
            "{} stdout of {args:?} in {at}",
            self.format
        );
        assert_eq!(
            got.0,
            want.0,
            "{} exit status of {args:?} in {at} (pyn stderr: {})",
            self.format,
            shown(&got.2)
        );
        assert_eq!(
            got.2.is_empty(),
            want.2.is_empty(),
            "{} stderr use of {args:?} in {at}: pyn {:?}, git {:?}",
            self.format,
            shown(&got.2),
            shown(&want.2)
        );
    }

    fn run(
        &self,
        mut c: Command,
        cwd: &Path,
        args: &[&str],
        stdin: &[u8],
        oracle: bool,
    ) -> (i32, Vec<u8>, Vec<u8>) {
        if oracle {
            c.args(["-c", "advice.detachedHead=false"]);
        }
        c.args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap(), out.stdout, out.stderr)
    }

    fn all(&self, cases: &[&[&str]]) {
        for cwd in [
            self.repo.clone(),
            self.repo.join("d"),
            self.repo.join("d/sub"),
        ] {
            for args in cases {
                self.same(&cwd, args);
            }
        }
    }
}

/// A repository with a merge, tags of every kind, remote-tracking refs, authors and a dirty work tree.
fn build(format: &'static str, packed: bool, dirty: bool) -> Fx {
    let mut f = Fx::new(format);
    f.write(".gitignore", "*.log\nbuild/\n");
    f.write("a.txt", "a\n");
    f.write("d/e.txt", "e\n");
    f.write("d/sub/f.txt", "f\n");
    f.write("s p.txt", "sp\n");
    f.write("run.sh", "#!/bin/sh\n");
    f.write("tab\u{e9}.txt", "t\n");
    fs::set_permissions(
        f.repo.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    symlink("a.txt", f.repo.join("link")).unwrap();
    f.commit("initial commit");
    f.g(&["tag", "light"]);
    f.write("a.txt", "a\nmore\n");
    f.commit("second commit\n\nBody text.");
    f.g(&["tag", "-a", "-m", "release one", "v1"]);
    f.g(&["tag", "-a", "-m", "release one b", "v1b"]);
    f.g(&["switch", "-q", "-c", "feature"]);
    f.write("feat.txt", "feature\n");
    f.g(&["add", "-A"]);
    f.g_as(
        &["commit", "-q", "-m", "feature work"],
        "Other Person",
        "other@example.com",
    );
    f.g(&["switch", "-q", "main"]);
    f.write("d/e.txt", "e3\n");
    f.commit("third commit");
    f.g(&[
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "Merge branch 'feature'",
        "feature",
    ]);
    f.write("d/sub/f.txt", "f5\n");
    f.commit("fifth commit");
    f.g(&["tag", "light2"]);
    f.write("a.txt", "six\n");
    f.g(&["add", "-A"]);
    f.g(&[
        "commit",
        "-q",
        "-m",
        "[PATCH v2 1/2] sixth subject\ncontinues here\n\nbody",
    ]);
    let head = f.rev("HEAD");
    f.g(&["update-ref", "refs/remotes/origin/main", &head]);
    f.g(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    if packed {
        f.g(&["gc", "-q"]);
    }
    if dirty {
        f.write("staged.txt", "s\n");
        f.g(&["add", "staged.txt"]);
        f.write("a.txt", "six\nunstaged\n");
        fs::remove_file(f.repo.join("d/e.txt")).unwrap();
        f.write("u.txt", "u\n");
        f.write("ud/x.txt", "x\n");
        f.write("x.log", "log\n");
        f.write("build/o.bin", "o\n");
        f.write("with space.txt", "x\n");
        f.write("caf\u{e9}.txt", "x\n");
        fs::set_permissions(
            f.repo.join("run.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        init(&f.repo.join("nested"), format, false);
    }
    f
}

fn each(test: impl Fn(&Fx)) {
    for format in FORMATS {
        for (packed, dirty) in [(false, true), (true, false)] {
            test(&build(format, packed, dirty));
        }
    }
}

#[test]
fn rev_parse_matches_git() {
    each(|f| {
        let head = f.rev("HEAD");
        let short = head[..9].to_string();
        let other = f.rev("feature");
        let tag = f.rev("v1");
        let cases: Vec<Vec<&str>> = vec![
            vec!["HEAD"],
            vec!["HEAD~1"],
            vec!["HEAD~3"],
            vec!["HEAD^"],
            vec!["HEAD^2"],
            vec!["HEAD~2^2"],
            vec!["HEAD^0"],
            vec!["HEAD~9"],
            vec!["main"],
            vec!["@"],
            vec!["feature"],
            vec!["origin/main"],
            vec!["origin"],
            vec!["v1"],
            vec!["light"],
            vec!["v1^{}"],
            vec!["v1^{commit}"],
            vec!["v1^{tree}"],
            vec!["v1^{tag}"],
            vec!["HEAD^{tree}"],
            vec!["HEAD^{blob}"],
            vec!["HEAD^{}"],
            vec!["HEAD^{commit}"],
            vec!["HEAD:a.txt"],
            vec!["HEAD:d"],
            vec!["HEAD:d/sub/f.txt"],
            vec!["HEAD:nosuch"],
            vec!["HEAD~2:a.txt"],
            vec!["HEAD:./a.txt"],
            vec!["HEAD:"],
            vec![":a.txt"],
            vec![":0:a.txt"],
            vec![":nosuch"],
            vec!["--verify", "HEAD"],
            vec!["--verify", "-q", "nosuch"],
            vec!["--verify", "nosuch"],
            vec!["--verify", "HEAD", "main"],
            vec!["--verify"],
            vec!["--verify", "HEAD^{tree}"],
            vec!["--verify", "--short", "HEAD"],
            vec!["--short", "HEAD"],
            vec!["--short=4", "HEAD"],
            vec!["--short=3", "HEAD"],
            vec!["--short=12", "HEAD", "HEAD~1"],
            vec!["--short=99", "HEAD"],
            vec!["--abbrev-ref", "HEAD"],
            vec!["--abbrev-ref", "main"],
            vec!["--abbrev-ref", "origin/HEAD"],
            vec!["--abbrev-ref", "origin/main"],
            vec!["--abbrev-ref", "v1"],
            vec!["--abbrev-ref", "HEAD~1"],
            vec!["--abbrev-ref=loose", "main"],
            vec!["--abbrev-ref=strict", "main"],
            vec!["--symbolic-full-name", "HEAD"],
            vec!["--symbolic-full-name", "main"],
            vec!["--symbolic-full-name", "v1"],
            vec!["--symbolic-full-name", "HEAD~1"],
            vec!["--symbolic", "main", "HEAD~1"],
            vec!["--show-toplevel"],
            vec!["--git-dir"],
            vec!["--absolute-git-dir"],
            vec!["--git-common-dir"],
            vec!["--is-inside-work-tree"],
            vec!["--is-bare-repository"],
            vec!["--is-inside-git-dir"],
            vec!["--show-prefix"],
            vec!["--show-cdup"],
            vec!["--show-object-format"],
            vec!["--show-prefix", "--show-cdup", "HEAD"],
            vec!["nosuch"],
            vec!["a.txt"],
            vec!["nosuch", "HEAD"],
            vec!["HEAD", "main"],
            vec!["HEAD..feature"],
            vec!["feature..HEAD"],
            vec!["..feature"],
            vec!["HEAD...feature"],
            vec!["^feature", "HEAD"],
            vec!["HEAD^@"],
            vec!["HEAD^!"],
            vec!["HEAD~2^@"],
            vec!["--all"],
            vec!["--branches"],
            vec!["--tags"],
            vec!["--remotes"],
            vec![&head],
            vec![&short],
            vec![&other],
            vec![&tag],
            vec!["--verify", &head],
            vec!["--verify", &short],
            vec!["--short", &head],
        ];
        for cwd in [f.repo.clone(), f.repo.join("d"), f.repo.join("d/sub")] {
            for case in &cases {
                f.same(&cwd, case);
            }
        }
    });
}

#[test]
fn rev_parse_past_the_root_fails_like_git() {
    each(|f| {
        let bare = f.root.join("bare.git");
        let out = Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(["clone", "-q", "--bare"])
            .arg(&f.repo)
            .arg(&bare)
            .output()
            .unwrap();
        assert!(out.status.success());
        for cwd in [f.repo.clone(), bare] {
            for case in [
                vec!["HEAD~99"],
                vec!["main~99"],
                vec!["HEAD^2"],
                vec!["nosuch"],
                vec!["HEAD"],
                vec!["--verify", "HEAD~99"],
            ] {
                f.same(&cwd, &case);
            }
        }
    });
}

#[test]
fn option_errors_exit_like_git() {
    each(|f| {
        for case in [
            vec!["log", "--bogus"],
            vec!["log", "-Z"],
            vec!["log", "--bogus=1"],
            vec!["show", "--bogus"],
            vec!["show", "-Z", "HEAD"],
            vec!["reflog", "--bogus"],
            vec!["reflog", "show", "--bogus"],
            vec!["rev-parse", "--bogus"],
            vec!["rev-parse", "--bogus", "HEAD"],
            vec!["rev-parse", "-Z", "HEAD", "--nosuch=1"],
            vec!["name-rev"],
            vec!["name-rev", "--tags"],
            vec!["name-rev", "--bogus"],
            vec!["shortlog", "--bogus"],
        ] {
            f.same(&f.repo, &case);
        }
        f.same_in(&f.root, &["shortlog"], b"");
        f.same_in(&f.root, &["shortlog", "-s"], b"");
        f.same_in(&f.root, &["shortlog", "HEAD"], b"");
    });
}

#[test]
fn ls_files_matches_git() {
    each(|f| {
        f.all(&[
            &["ls-files"],
            &["ls-files", "-c"],
            &["ls-files", "--cached"],
            &["ls-files", "-s"],
            &["ls-files", "--stage"],
            &["ls-files", "-m"],
            &["ls-files", "-d"],
            &["ls-files", "-m", "-d"],
            &["ls-files", "-c", "-m"],
            &["ls-files", "-c", "-d"],
            &["ls-files", "-s", "-m"],
            &["ls-files", "-o"],
            &["ls-files", "-o", "--exclude-standard"],
            &["ls-files", "-o", "-i", "--exclude-standard"],
            &["ls-files", "-c", "-i", "--exclude-standard"],
            &["ls-files", "-i"],
            &["ls-files", "-i", "-o"],
            &["ls-files", "-z"],
            &["ls-files", "-s", "-z"],
            &["ls-files", "-o", "-z"],
            &["ls-files", "--full-name"],
            &["ls-files", "-o", "--full-name"],
            &["ls-files", "-u"],
            &["ls-files", "a.txt"],
            &["ls-files", "d"],
            &["ls-files", "d/"],
            &["ls-files", "*.txt"],
            &["ls-files", "d/*"],
            &["ls-files", "-s", "d"],
            &["ls-files", "-o", "d"],
            &["ls-files", "-o", "ud"],
            &["ls-files", "-m", "a.txt"],
            &["ls-files", "--", "a.txt"],
            &["ls-files", "nosuch"],
            &["ls-files", "--error-unmatch", "a.txt"],
            &["ls-files", "--error-unmatch", "nosuch"],
            &["ls-files", ".."],
            &["ls-files", "../a.txt"],
            &["ls-files", "."],
            &["ls-files", "s p.txt"],
            &["ls-files", "link"],
        ]);
    });
}

/// A repository in the middle of a conflicted merge.
fn conflicted(format: &'static str) -> Fx {
    let mut f = Fx::new(format);
    f.write("c.txt", "base\n");
    f.write("ok.txt", "ok\n");
    f.commit("base");
    f.g(&["switch", "-q", "-c", "side"]);
    f.write("c.txt", "side\n");
    f.commit("side");
    f.g(&["switch", "-q", "main"]);
    f.write("c.txt", "main\n");
    f.commit("main");
    let out = git(&f.repo).args(["merge", "side"]).output().unwrap();
    assert!(!out.status.success());
    f
}

#[test]
fn ls_files_shows_conflict_stages_like_git() {
    for format in FORMATS {
        let f = conflicted(format);
        for args in [
            &["ls-files"][..],
            &["ls-files", "-s"],
            &["ls-files", "-u"],
            &["ls-files", "-m"],
            &["ls-files", "-d"],
            &["ls-files", "-s", "-m"],
            &["ls-files", "-u", "-z"],
            &["ls-files", "-o"],
            &["ls-files", "c.txt"],
        ] {
            f.same(&f.repo, args);
        }
        fs::remove_file(f.repo.join("c.txt")).unwrap();
        for args in [
            &["ls-files", "-d"][..],
            &["ls-files", "-m"],
            &["ls-files", "-s", "-d"],
        ] {
            f.same(&f.repo, args);
        }
    }
}

#[test]
fn ls_tree_matches_git() {
    each(|f| {
        f.all(&[
            &["ls-tree", "HEAD"],
            &["ls-tree", "-r", "HEAD"],
            &["ls-tree", "-t", "HEAD"],
            &["ls-tree", "-r", "-t", "HEAD"],
            &["ls-tree", "-d", "HEAD"],
            &["ls-tree", "-d", "-r", "HEAD"],
            &["ls-tree", "-d", "-t", "HEAD"],
            &["ls-tree", "-l", "HEAD"],
            &["ls-tree", "-r", "-l", "HEAD"],
            &["ls-tree", "-l", "-t", "HEAD"],
            &["ls-tree", "--long", "-d", "HEAD"],
            &["ls-tree", "--name-only", "HEAD"],
            &["ls-tree", "--name-only", "-r", "HEAD"],
            &["ls-tree", "--name-status", "HEAD"],
            &["ls-tree", "--object-only", "HEAD"],
            &["ls-tree", "--object-only", "-r", "HEAD"],
            &["ls-tree", "-z", "HEAD"],
            &["ls-tree", "-z", "--name-only", "HEAD"],
            &["ls-tree", "--abbrev", "HEAD"],
            &["ls-tree", "--abbrev=10", "HEAD"],
            &["ls-tree", "--abbrev=2", "HEAD"],
            &["ls-tree", "HEAD", "d"],
            &["ls-tree", "HEAD", "d/"],
            &["ls-tree", "-r", "HEAD", "d"],
            &["ls-tree", "-r", "HEAD", "d/"],
            &["ls-tree", "-t", "HEAD", "d"],
            &["ls-tree", "-t", "-r", "HEAD", "d/sub/f.txt"],
            &["ls-tree", "-r", "HEAD", "d/sub/f.txt"],
            &["ls-tree", "HEAD", "d/sub/f.txt"],
            &["ls-tree", "HEAD", "d/sub"],
            &["ls-tree", "HEAD", "a.txt", "d"],
            &["ls-tree", "-d", "HEAD", "d/"],
            &["ls-tree", "-d", "-r", "HEAD", "d"],
            &["ls-tree", "HEAD", "nosuch"],
            &["ls-tree", "HEAD", "a"],
            &["ls-tree", "HEAD", "link"],
            &["ls-tree", "HEAD", "s p.txt"],
            &["ls-tree", "HEAD", "../a.txt"],
            &["ls-tree", "HEAD", ".."],
            &["ls-tree", "HEAD", "../d"],
            &["ls-tree", "HEAD", "../d/"],
            &["ls-tree", "HEAD", "./"],
            &["ls-tree", "HEAD", "sub/.."],
            &["ls-tree", "-r", "HEAD", "."],
            &["ls-tree", "-d", "HEAD", "."],
            &["ls-tree", "HEAD", "."],
            &["ls-tree", "HEAD", "e.txt"],
            &["ls-tree", "--full-name", "HEAD"],
            &["ls-tree", "--full-name", "-r", "HEAD"],
            &["ls-tree", "--full-tree", "HEAD"],
            &["ls-tree", "--full-tree", "-r", "HEAD", "d"],
            &["ls-tree", "nosuch"],
            &["ls-tree", "HEAD:d"],
            &["ls-tree", "HEAD:a.txt"],
            &["ls-tree", "v1"],
            &["ls-tree", "HEAD~2"],
            &["ls-tree", "HEAD^{tree}"],
            &["ls-tree", "feature", "-r"],
            &["ls-tree"],
        ]);
        let tree = f.rev("HEAD^{tree}");
        f.same(&f.repo, &["ls-tree", &tree]);
        f.same(&f.repo, &["ls-tree", "-r", &tree[..10]]);
    });
}

#[test]
fn cat_file_matches_git() {
    each(|f| {
        let zero = "0".repeat(f.rev("HEAD").len());
        let head = f.rev("HEAD");
        let blob = f.rev("HEAD:a.txt");
        f.all(&[
            &["cat-file", "-t", "HEAD"],
            &["cat-file", "-s", "HEAD"],
            &["cat-file", "-p", "HEAD"],
            &["cat-file", "-e", "HEAD"],
            &["cat-file", "-p", "HEAD^{tree}"],
            &["cat-file", "-p", "HEAD:d"],
            &["cat-file", "-p", "HEAD:a.txt"],
            &["cat-file", "-p", "HEAD:link"],
            &["cat-file", "-t", "HEAD:a.txt"],
            &["cat-file", "-s", "HEAD:a.txt"],
            &["cat-file", "-t", "HEAD:d"],
            &["cat-file", "-s", "HEAD:d"],
            &["cat-file", "-t", "v1"],
            &["cat-file", "-p", "v1"],
            &["cat-file", "-s", "v1"],
            &["cat-file", "-t", "light"],
            &["cat-file", "-e", "v1"],
            &["cat-file", "-e", "nosuch"],
            &["cat-file", "-t", "nosuch"],
            &["cat-file", "-p", "nosuch"],
            &["cat-file", "commit", "HEAD"],
            &["cat-file", "tree", "HEAD"],
            &["cat-file", "tree", "HEAD~1"],
            &["cat-file", "blob", "HEAD:a.txt"],
            &["cat-file", "blob", "HEAD:link"],
            &["cat-file", "tree", "v1"],
            &["cat-file", "commit", "v1"],
            &["cat-file", "tag", "v1"],
            &["cat-file", "tag", "HEAD"],
            &["cat-file", "blob", "HEAD"],
            &["cat-file", "blob", "v1"],
            &["cat-file", "bogus", "HEAD"],
            &["cat-file", "commit", "nosuch"],
            &["cat-file", "-p"],
            &["cat-file", "HEAD"],
            &["cat-file"],
            &["cat-file", "-t", "-s", "HEAD"],
            &["cat-file", "-p", "HEAD", "HEAD"],
            &["cat-file", "-e", "HEAD:nosuch"],
            &["cat-file", "-p", ":a.txt"],
            &["cat-file", "-p", "HEAD:./a.txt"],
            &["cat-file", "-p", "HEAD:./e.txt"],
        ]);
        f.same(&f.repo, &["cat-file", "-e", &zero]);
        f.same(&f.repo, &["cat-file", "-t", &zero]);
        f.same(&f.repo, &["cat-file", "-p", &head]);
        f.same(&f.repo, &["cat-file", "-p", &head[..8]]);
        f.same(&f.repo, &["cat-file", "blob", &blob]);
        f.same_in(
            &f.repo,
            &["cat-file", "--batch-check"],
            format!("HEAD\n{blob}\nnosuch\nv1\n").as_bytes(),
        );
        f.same_in(
            &f.repo,
            &["cat-file", "--batch"],
            format!("HEAD:a.txt\nHEAD:d\nnosuch\n{head}\n").as_bytes(),
        );
    });
}

#[test]
fn show_ref_matches_git() {
    each(|f| {
        f.all(&[
            &["show-ref"],
            &["show-ref", "--head"],
            &["show-ref", "--tags"],
            &["show-ref", "--heads"],
            &["show-ref", "--tags", "--heads"],
            &["show-ref", "-d"],
            &["show-ref", "-d", "--tags"],
            &["show-ref", "--dereference", "v1"],
            &["show-ref", "-s"],
            &["show-ref", "--hash"],
            &["show-ref", "--hash=8"],
            &["show-ref", "--hash=2"],
            &["show-ref", "--abbrev"],
            &["show-ref", "--abbrev=10"],
            &["show-ref", "--abbrev=3"],
            &["show-ref", "-s", "-d"],
            &["show-ref", "--head", "-s"],
            &["show-ref", "main"],
            &["show-ref", "v1"],
            &["show-ref", "v1", "v1b"],
            &["show-ref", "origin/main"],
            &["show-ref", "heads/main"],
            &["show-ref", "refs/heads/main"],
            &["show-ref", "HEAD"],
            &["show-ref", "ain"],
            &["show-ref", "nosuch"],
            &["show-ref", "-q", "main"],
            &["show-ref", "-q", "nosuch"],
            &["show-ref", "-q"],
            &["show-ref", "--verify", "refs/heads/main"],
            &["show-ref", "--verify", "main"],
            &["show-ref", "--verify", "-q", "main"],
            &["show-ref", "--verify", "-q", "refs/heads/main"],
            &["show-ref", "--verify", "-q", "refs/heads/nosuch"],
            &["show-ref", "--verify", "refs/heads/nosuch"],
            &["show-ref", "--verify"],
            &["show-ref", "--verify", "HEAD"],
            &["show-ref", "--verify", "-d", "refs/tags/v1"],
            &["show-ref", "--verify", "refs/heads/main", "refs/tags/v1"],
            &[
                "show-ref",
                "--verify",
                "refs/heads/main",
                "refs/heads/nosuch",
            ],
            &["show-ref", "--verify", "-s", "refs/heads/main"],
            &["show-ref", "--exists", "refs/heads/main"],
            &["show-ref", "--exists", "refs/heads/nosuch"],
            &["show-ref", "--exists", "HEAD"],
            &["show-ref", "--exists"],
            &["show-ref", "--exists", "refs/heads/main", "refs/tags/v1"],
            &["show-ref", "--head", "main"],
            &["show-ref", "--head", "HEAD"],
            &["show-ref", "--head", "--tags"],
            &["show-ref", "--head", "-d", "v1"],
        ]);
    });
}

#[test]
fn shortlog_matches_git() {
    each(|f| {
        f.all(&[
            &["shortlog", "HEAD"],
            &["shortlog", "-s", "HEAD"],
            &["shortlog", "-n", "HEAD"],
            &["shortlog", "-sn", "HEAD"],
            &["shortlog", "-ns", "HEAD"],
            &["shortlog", "-e", "HEAD"],
            &["shortlog", "-se", "HEAD"],
            &["shortlog", "-nse", "HEAD"],
            &["shortlog", "-c", "HEAD"],
            &["shortlog", "-sc", "HEAD"],
            &["shortlog", "feature"],
            &["shortlog", "main..feature"],
            &["shortlog", "feature..main"],
            &["shortlog", "^feature", "HEAD"],
            &["shortlog", "HEAD...feature"],
            &["shortlog", "HEAD", "--", "a.txt"],
            &["shortlog", "HEAD", "--", "d"],
            &["shortlog", "HEAD", "--", "nosuch"],
            &["shortlog", "--summary", "--numbered", "HEAD"],
            &["shortlog", "--email", "HEAD"],
            &["shortlog", "HEAD~2"],
            &["shortlog", "HEAD~1", "feature"],
            &["shortlog", "v1"],
            &["shortlog", "nosuch"],
            &["shortlog", "HEAD", "nosuch"],
        ]);
        f.same_in(&f.repo, &["shortlog", "-s"], b"");
        let log = String::from_utf8(
            git(&f.repo)
                .args(["log", "--format=fuller", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        f.same_in(&f.repo, &["shortlog"], log.as_bytes());
        f.same_in(&f.repo, &["shortlog", "-ns"], log.as_bytes());
    });
}

#[test]
fn describe_matches_git() {
    each(|f| {
        f.all(&[
            &["describe"],
            &["describe", "--tags"],
            &["describe", "--always"],
            &["describe", "--long"],
            &["describe", "--tags", "--long"],
            &["describe", "--abbrev=4"],
            &["describe", "--abbrev=0"],
            &["describe", "--abbrev=12"],
            &["describe", "--abbrev=1"],
            &["describe", "--abbrev=0", "--long"],
            &["describe", "--dirty"],
            &["describe", "--dirty=-x"],
            &["describe", "--dirty", "--long"],
            &["describe", "--dirty", "HEAD"],
            &["describe", "--match", "v*"],
            &["describe", "--match", "v2*"],
            &["describe", "--match", "v1", "--match", "v1b"],
            &["describe", "--match", "light*", "--tags"],
            &["describe", "--exclude", "v1*"],
            &["describe", "--exclude", "v*", "--tags"],
            &["describe", "--exclude", "light*", "--match", "*", "--tags"],
            &["describe", "v1"],
            &["describe", "v1b"],
            &["describe", "light"],
            &["describe", "light", "--tags"],
            &["describe", "light", "--tags", "--long"],
            &["describe", "HEAD~1"],
            &["describe", "HEAD~1", "--tags"],
            &["describe", "HEAD~2"],
            &["describe", "HEAD~2", "--tags"],
            &["describe", "HEAD~3", "--tags", "--long"],
            &["describe", "HEAD~4"],
            &["describe", "HEAD", "HEAD~2", "feature"],
            &["describe", "feature"],
            &["describe", "feature", "--tags"],
            &["describe", "HEAD^2"],
            &["describe", "HEAD^2", "--always"],
            &["describe", "--all"],
            &["describe", "--all", "HEAD~1"],
            &["describe", "--all", "feature"],
            &["describe", "--all", "--long"],
            &["describe", "--exact-match"],
            &["describe", "--exact-match", "v1"],
            &["describe", "--exact-match", "--tags", "light"],
            &["describe", "--candidates=1", "--tags"],
            &["describe", "--candidates=0", "--tags", "HEAD~1"],
            &["describe", "--always", "--abbrev=9"],
            &["describe", "--always", "--abbrev=0"],
            &["describe", "nosuch"],
            &["describe", "--match", "nosuch"],
            &["describe", "--match", "nosuch", "--always"],
        ]);
    });
}

#[test]
fn describe_without_tags_matches_git() {
    for format in FORMATS {
        let mut f = Fx::new(format);
        f.write("a.txt", "a\n");
        f.commit("one");
        f.write("a.txt", "b\n");
        f.commit("two");
        let cases: &[&[&str]] = &[
            &["describe"],
            &["describe", "--always"],
            &["describe", "--tags"],
            &["describe", "--all"],
            &["describe", "--all", "--always"],
            &["describe", "--exact-match"],
        ];
        for c in cases {
            f.same(&f.repo, c);
        }
        f.g(&["tag", "light"]);
        for c in cases {
            f.same(&f.repo, c);
        }
        f.g(&["tag", "-a", "-m", "m", "ann", "HEAD~1"]);
        for c in cases {
            f.same(&f.repo, c);
        }
        f.same(&f.repo, &["describe", "--tags", "HEAD~1", "HEAD"]);
    }
}

#[test]
fn describe_chooses_among_tags_like_git() {
    for format in FORMATS {
        let mut f = Fx::new(format);
        for n in 0..4 {
            f.write("a.txt", &format!("{n}\n"));
            f.commit(&format!("c{n}"));
        }
        f.g(&["tag", "-a", "-m", "m", "old", "HEAD~3"]);
        f.g(&["tag", "-a", "-m", "m", "mid", "HEAD~1"]);
        f.g(&["switch", "-q", "-c", "side", "HEAD~2"]);
        f.write("s.txt", "s\n");
        f.commit("side1");
        f.g(&["tag", "-a", "-m", "m", "sidetag"]);
        f.write("s.txt", "s2\n");
        f.commit("side2");
        f.g(&["switch", "-q", "main"]);
        f.g(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        f.write("a.txt", "after\n");
        f.commit("after");
        for c in [
            &["describe"][..],
            &["describe", "--long"],
            &["describe", "HEAD~1"],
            &["describe", "HEAD^2"],
            &["describe", "HEAD^2^"],
            &["describe", "side"],
            &["describe", "--candidates=1"],
            &["describe", "--candidates=2"],
            &["describe", "--match", "old"],
            &["describe", "--match", "mid"],
            &["describe", "--match", "side*"],
            &["describe", "--all"],
            &["describe", "--debug"],
        ] {
            if c == ["describe", "--debug"] {
                continue;
            }
            f.same(&f.repo, c);
        }
    }
}

#[test]
fn name_rev_matches_git() {
    each(|f| {
        let head = f.rev("HEAD");
        let mut dangling = git(&f.repo)
            .args(["commit-tree", "-m", "dangling", &f.rev("HEAD^{tree}")])
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap()
            .stdout;
        while dangling.last() == Some(&b'\n') {
            dangling.pop();
        }
        let dangling = String::from_utf8(dangling).unwrap();
        f.all(&[
            &["name-rev", "HEAD"],
            &["name-rev", "--name-only", "HEAD"],
            &["name-rev", "HEAD~1"],
            &["name-rev", "HEAD~2"],
            &["name-rev", "HEAD~3"],
            &["name-rev", "HEAD~4"],
            &["name-rev", "HEAD^2"],
            &["name-rev", "HEAD~2^2"],
            &["name-rev", "feature"],
            &["name-rev", "main"],
            &["name-rev", "v1"],
            &["name-rev", "v1b"],
            &["name-rev", "light"],
            &["name-rev", "light2"],
            &["name-rev", "origin/main"],
            &["name-rev", "--tags", "HEAD"],
            &["name-rev", "--tags", "HEAD~3"],
            &["name-rev", "--tags", "feature"],
            &["name-rev", "--tags", "--name-only", "HEAD~3"],
            &["name-rev", "--tags", "--name-only", "v1"],
            &["name-rev", "--tags", "--name-only", "HEAD"],
            &["name-rev", "--name-only", "HEAD", "HEAD~1", "feature", "v1"],
            &["name-rev", "HEAD", "nosuch", "HEAD~1"],
            &["name-rev", "nosuch"],
            &["name-rev", "HEAD:a.txt"],
            &["name-rev", "HEAD^{tree}"],
            &["name-rev", "--always", "HEAD"],
        ]);
        f.same(&f.repo, &["name-rev", &head]);
        f.same(&f.repo, &["name-rev", &head[..8]]);
        f.same(&f.repo, &["name-rev", &dangling]);
        f.same(&f.repo, &["name-rev", "--name-only", &dangling]);
        f.same(&f.repo, &["name-rev", "--always", &dangling]);
        f.same(&f.repo, &["name-rev", "--always", "--name-only", &dangling]);
        f.same(&f.repo, &["name-rev", "--no-undefined", &dangling]);
        f.same(&f.repo, &["name-rev", "--no-undefined", "HEAD"]);
        f.same(&f.repo, &["name-rev", "--tags", &dangling, "HEAD"]);
    });
}

#[test]
fn name_rev_prefers_like_git_in_merged_history() {
    for format in FORMATS {
        let mut f = Fx::new(format);
        for n in 0..3 {
            f.write("a.txt", &format!("{n}\n"));
            f.commit(&format!("c{n}"));
        }
        f.g(&["tag", "-a", "-m", "m", "first", "HEAD~2"]);
        f.g(&["switch", "-q", "-c", "topic", "HEAD~1"]);
        for n in 0..3 {
            f.write("t.txt", &format!("{n}\n"));
            f.commit(&format!("t{n}"));
        }
        f.g(&["switch", "-q", "main"]);
        f.write("m.txt", "m\n");
        f.commit("m1");
        f.g(&["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);
        f.g(&["tag", "second"]);
        f.write("a.txt", "x\n");
        f.commit("m2");
        f.g(&["switch", "-q", "-c", "other", "topic~1"]);
        f.write("o.txt", "o\n");
        f.commit("o1");
        f.g(&["switch", "-q", "main"]);
        f.g(&["merge", "-q", "--no-ff", "-m", "merge other", "other"]);
        for rev in [
            "HEAD",
            "HEAD~1",
            "HEAD^2",
            "HEAD^2~1",
            "HEAD~1^2",
            "HEAD~1^2~1",
            "HEAD~1^2~2",
            "HEAD~1^2~3",
            "HEAD~2",
            "HEAD~3",
            "HEAD~4",
            "topic",
            "topic~1",
            "topic~2",
            "other",
            "other~1",
            "first",
            "second",
            "second^2",
            "second^2^",
        ] {
            for extra in [
                &[][..],
                &["--tags"],
                &["--name-only"],
                &["--tags", "--name-only"],
            ] {
                let mut args = vec!["name-rev"];
                args.extend_from_slice(extra);
                args.push(rev);
                f.same(&f.repo, &args);
            }
        }
    }
}

#[test]
fn abbreviations_stay_unique_like_git() {
    for format in FORMATS {
        for packed in [false, true] {
            let mut f = Fx::new(format);
            f.write("a.txt", "a\n");
            f.commit("one");
            let files = f.root.join("blobs");
            fs::create_dir_all(&files).unwrap();
            let mut names = String::new();
            for n in 0..2500 {
                let p = files.join(format!("b{n}"));
                fs::write(&p, format!("blob {n}\n")).unwrap();
                names.push_str(&format!("{}\n", p.display()));
            }
            let mut c = git(&f.repo);
            c.args(["hash-object", "-w", "--stdin-paths"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
            let mut child = c.spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let writer = std::thread::spawn(move || stdin.write_all(names.as_bytes()).unwrap());
            let ids = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
            writer.join().unwrap();
            if packed {
                f.g(&["gc", "-q"]);
            }
            let mut by_prefix: std::collections::BTreeMap<String, Vec<&str>> = Default::default();
            for id in ids.lines() {
                by_prefix.entry(id[..4].to_string()).or_default().push(id);
            }
            let crowded: Vec<&str> = by_prefix
                .values()
                .filter(|v| v.len() > 1)
                .flatten()
                .copied()
                .take(40)
                .collect();
            assert!(!crowded.is_empty());
            for id in crowded {
                f.same(&f.repo, &["rev-parse", "--short=4", id]);
                f.same(&f.repo, &["rev-parse", "--short=5", id]);
                f.same(&f.repo, &["show-ref", "--abbrev=4"]);
            }
            f.same(&f.repo, &["ls-tree", "--abbrev=4", "HEAD"]);
        }
    }
}
