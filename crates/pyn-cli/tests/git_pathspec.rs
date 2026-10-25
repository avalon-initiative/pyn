//! Pathspecs (globs, magic, relative paths, the `GIT_*_PATHSPECS` settings) on a git repository, compared with the
//! real git on the same repository: standard output, the exit status and whether standard error carries anything.
//! A seeded random pathspec and tree fuzz checks the matcher against `git ls-files`. Real git is the oracle in these
//! tests only, and only semantics git 2.43 and newer releases agree on are compared.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::{git, init, path_env};
use tempfile::{TempDir, tempdir};

/// Exit status, standard output and standard error of one run.
type Ran = (i32, Vec<u8>, Vec<u8>);

struct Fx {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
    format: &'static str,
}

impl Fx {
    fn empty(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        init(&repo, format, false);
        fs::create_dir_all(root.join("home")).unwrap();
        let fx = Self {
            _tmp: tmp,
            root,
            repo,
            format,
        };
        fx.g(&["config", "user.name", "T"]);
        fx.g(&["config", "user.email", "t@example.com"]);
        fx
    }

    fn g(&self, args: &[&str]) -> String {
        let out = git(&self.repo).args(args).output().unwrap();
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

    fn run(
        &self,
        program: &Path,
        repo: &Path,
        cwd: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Ran {
        let mut c = Command::new(program);
        c.args(args)
            .current_dir(repo.join(cwd))
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("LC_ALL", "C")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = c.output().unwrap();
        (
            out.status.code().unwrap_or_else(|| {
                panic!(
                    "{program:?} {args:?} in '{cwd}' ended with {:?}",
                    out.status
                )
            }),
            out.stdout,
            out.stderr,
        )
    }

    fn pair(
        &self,
        repos: (&Path, &Path),
        cwd: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> (Ran, Ran) {
        let want = self.run(Path::new("git"), repos.0, cwd, args, env);
        let got = self.run(
            Path::new(env!("CARGO_BIN_EXE_pyn")),
            repos.1,
            cwd,
            args,
            env,
        );
        (want, got)
    }

    /// Runs the oracle and pyn on the same repository and demands the same output, status and stream use.
    fn same(&self, cwd: &str, args: &[&str], env: &[(&str, &str)]) {
        let (want, got) = self.pair((&self.repo, &self.repo), cwd, args, env);
        self.check(cwd, args, env, &want, &got, sorted(args));
    }

    fn check(
        &self,
        cwd: &str,
        args: &[&str],
        env: &[(&str, &str)],
        want: &Ran,
        got: &Ran,
        sort: bool,
    ) {
        let shown = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        if args[0] == "show" && want.0 == 0 && want.1.is_empty() {
            // Before 2.55 git leaves out a commit the paths do not touch; later ones print it.
            return;
        }
        let order = |b: &[u8]| {
            let mut lines: Vec<String> = shown(b).lines().map(str::to_string).collect();
            if sort {
                lines.sort();
            }
            lines
        };
        let at = format!("{} {args:?} in '{cwd}' with {env:?}", self.format);
        assert_eq!(
            order(&got.1),
            order(&want.1),
            "stdout of {at} (pyn stderr: {}, git stderr: {})",
            shown(&got.2),
            shown(&want.2)
        );
        assert_eq!(
            got.0,
            want.0,
            "exit status of {at} (pyn stderr: {}, git stderr: {})",
            shown(&got.2),
            shown(&want.2)
        );
        // git warns about a missing folder named with a trailing slash, and `checkout` counts the paths it updated.
        let noise = |e: &[u8]| {
            shown(e)
                .lines()
                .any(|l| !l.starts_with("warning: could not open directory"))
                && args[0] != "checkout"
        };
        assert_eq!(
            noise(&got.2),
            noise(&want.2),
            "stderr use of {at}: pyn {:?}, git {:?}",
            shown(&got.2),
            shown(&want.2)
        );
    }
}

/// `add -n` lists new files after the tracked ones in git and in path order here.
fn sorted(args: &[&str]) -> bool {
    args.first() == Some(&"add")
}

fn copy_repo(from: &Path, to: &Path) {
    let status = Command::new("cp")
        .arg("-a")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(status.success());
}

/// A tree with nested folders, dot files, names with spaces and glob characters, a symlink and case variants, with
/// committed, staged, modified, untracked and ignored files.
fn build(format: &'static str) -> Fx {
    let fx = Fx::empty(format);
    fx.write(".gitignore", "*.log\nbuild/\n");
    for (path, content) in [
        ("a.txt", "a\n"),
        ("b.rs", "b\n"),
        ("README.md", "readme\n"),
        (".dot", "dot\n"),
        ("src/main.rs", "main\n"),
        ("src/lib.rs", "lib\n"),
        ("src/a.txt", "sa\n"),
        ("src/deep/x.rs", "x\n"),
        ("src/deep/er/y.rs", "y\n"),
        ("src/deep/er/Y.RS", "Y\n"),
        ("docs/a.md", "a\n"),
        ("docs/B.MD", "B\n"),
        ("with space/f g.txt", "fg\n"),
        ("g*b/s.txt", "s\n"),
        ("g*b/t.rs", "t\n"),
        (".hid/c", "c\n"),
        ("a*c", "star\n"),
        ("a?c", "question\n"),
        ("a[1]", "bracket\n"),
        ("a1", "one\n"),
        ("Up/X.txt", "X\n"),
        ("up.txt", "up\n"),
    ] {
        fx.write(path, &format!("{content}one\n"));
    }
    symlink("a.txt", fx.repo.join("link.txt")).unwrap();
    fx.g(&["add", "-A"]);
    fx.g(&["commit", "-q", "-m", "one"]);
    for path in [
        "src/main.rs",
        "docs/a.md",
        "a.txt",
        "a*c",
        "a?c",
        "a[1]",
        "a1",
    ] {
        let old = fs::read_to_string(fx.repo.join(path)).unwrap();
        fx.write(path, &format!("{old}more\n"));
    }
    fs::remove_file(fx.repo.join("docs/B.MD")).unwrap();
    fx.write("src/lib.rs", "staged\n");
    fx.g(&["add", "src/lib.rs"]);
    fx.write("src/lib.rs", "staged\nand more\n");
    for path in [
        "newtop.txt",
        "src/deep/new.rs",
        "u*v",
        "u?v",
        "x.log",
        "build/out.o",
    ] {
        fx.write(path, "new\n");
    }
    fx
}

const SPECS: &[&str] = &[
    ".",
    "src",
    "src/",
    "*.rs",
    "src/*.rs",
    "**/*.rs",
    "**/x.rs",
    "src/**",
    "src/**/y.rs",
    "a*",
    "a?c",
    "a[1]",
    "a[[]1]",
    r"a\*c",
    "*",
    "?",
    "[a-c]*",
    ":(glob)*.rs",
    ":(glob)**/*.rs",
    ":(glob)src/**/*.rs",
    ":(glob)src/*",
    ":(glob)**",
    ":(literal)a*c",
    ":(literal)g*b/",
    ":(icase)READme.md",
    ":(icase)SRC/*.RS",
    ":(icase,glob)SRC/**/y.rs",
    ":(icase)[A-Z]rc/m*",
    ":(icase)up.txt",
    ":!src",
    ":^src",
    ":(exclude)src",
    ":(exclude)*.rs",
    ":/src",
    ":(top)src",
    ":(top,exclude)src",
    ":(glob,exclude)**/*.rs",
    ":(exclude,glob)src/*.rs",
    ":(top,glob)src/*.rs",
    ":(top,literal)a*c",
    ":foo",
    ":::src",
    ":/",
    ":",
    "./src",
    "src/../docs",
    "src//deep",
    "src/./deep/",
    "a.txt/",
    "a.txt/.",
    "link.txt",
    "link.txt/",
    "with space",
    "with space/*",
    "with*",
    "g*b",
    "g*b/",
    r"g\*b",
    "g[*]b/*",
    "Up",
    "up*",
    "UP*",
    ".dot",
    ".hid",
    ".*",
    "nope",
    "nope*",
    "nope/x",
    ":(exclude)nope",
    "tab*",
    "newdir",
    "newdir/",
    "new*",
    "newdir/*",
    "*/",
    "**/",
    "ne*/",
    "newdir/*/",
    ":(glob)new*",
    ":(glob)newdir/*",
    ":(glob)**/f.txt",
    "newdir/f.txt",
    "newdir/sub",
    "newdir/s*",
    "only/*",
    "only/d*/h.txt",
    "*.txt",
    "**",
    "n*/s*",
    "*/*",
    "*dir",
    "newdi?",
    "*dir/*",
    ":!newdir/sub",
    ":!newdir/f.txt",
    ":!only",
    ":(exclude)*.txt",
    ":(icase)NEWDIR",
];

const READ_ONLY: &[&[&str]] = &[
    &["ls-files"],
    &["ls-files", "-o"],
    &["ls-files", "-o", "-i", "--exclude-standard"],
    &["ls-files", "-m", "-d"],
    &["ls-files", "--error-unmatch"],
    &["status", "-s"],
    &["diff", "--name-status"],
    &["diff", "--cached", "--name-status"],
    &["diff", "HEAD", "--name-status"],
    &["add", "-n"],
    &["log", "--name-only", "--format=%s"],
    &["show", "--name-only", "--format=%s", "HEAD"],
    &["shortlog", "-s", "HEAD"],
    &["grep", "-l", "one"],
    &["grep", "-l", "one", "--cached"],
    &["grep", "-l", "one", "HEAD"],
    &["rm", "-n", "-r"],
    &["clean", "-n", "-d"],
    &["clean", "-n", "-x"],
    &["clean", "-n", "-X"],
    &["ls-tree", "-r", "--name-only", "HEAD"],
    &["ls-tree", "HEAD"],
];

fn read_only(format: &'static str, folders: &[&str], specs: &[&str]) {
    let fx = build(format);
    for cwd in folders {
        for spec in specs {
            for cmd in READ_ONLY {
                let args: Vec<&str> = cmd.iter().copied().chain(["--", spec]).collect();
                fx.same(cwd, &args, &[]);
            }
        }
    }
}

fn every_other<'a>(specs: &[&'a str]) -> Vec<&'a str> {
    specs.iter().copied().step_by(2).collect()
}

#[test]
fn read_only_commands_agree_from_the_root_sha1() {
    read_only("sha1", &[""], SPECS);
}

#[test]
fn read_only_commands_agree_from_the_root_sha256() {
    read_only("sha256", &[""], &every_other(SPECS));
}

#[test]
fn read_only_commands_agree_from_a_folder_sha1() {
    read_only("sha1", &["src"], SPECS);
}

#[test]
fn read_only_commands_agree_from_a_folder_sha256() {
    read_only("sha256", &["src/deep"], &every_other(SPECS));
}

#[test]
fn read_only_commands_agree_from_odd_folders() {
    read_only("sha1", &["g*b", "with space"], SPECS);
}

const MUTATING: &[&[&str]] = &[
    &["restore"],
    &["restore", "--staged"],
    &["restore", "-s", "HEAD"],
    &["checkout"],
    &["checkout", "HEAD"],
    &["reset", "-q"],
    &["stash", "push"],
    &["rm", "-r"],
    &["rm", "-r", "--cached"],
    &["add"],
    &["add", "-u"],
    &["clean", "-f"],
    &["clean", "-f", "-x", "-d"],
];

/// Everything a command could have changed: the work tree files, the index, the stash and the history.
fn state(fx: &Fx, repo: &Path) -> String {
    let mut files = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_symlink() {
                files.push(format!(
                    "{} -> {}",
                    path.strip_prefix(repo).unwrap().display(),
                    fs::read_link(&path).unwrap().display()
                ));
            } else {
                files.push(format!(
                    "{} = {:?}",
                    path.strip_prefix(repo).unwrap().display(),
                    fs::read(&path).unwrap()
                ));
            }
        }
    }
    files.sort();
    let mut text = files.join("\n");
    for args in [
        &["status", "--porcelain", "--ignored", "-uall"][..],
        &["ls-files", "-s"],
        &["stash", "list"],
        &["log", "--oneline", "--all"],
    ] {
        let out = fx.run(Path::new("git"), repo, "", args, &[]);
        text.push_str(&format!(
            "\n== {args:?}\n{}",
            String::from_utf8_lossy(&out.1)
        ));
    }
    text
}

fn mutating(format: &'static str, folders: &[&str], specs: &[&str]) {
    mutating_with(format, folders, specs, MUTATING);
}

fn mutating_with(format: &'static str, folders: &[&str], specs: &[&str], commands: &[&[&str]]) {
    let fx = build(format);
    let (a, b) = (fx.root.join("git-copy"), fx.root.join("pyn-copy"));
    for cwd in folders {
        for spec in specs {
            for cmd in commands {
                // Git before 2.5x cannot stash with paths in a SHA-256 repository.
                if format == "sha256" && cmd[0] == "stash" {
                    continue;
                }
                let _ = fs::remove_dir_all(&a);
                let _ = fs::remove_dir_all(&b);
                copy_repo(&fx.repo, &a);
                copy_repo(&fx.repo, &b);
                let args: Vec<&str> = cmd.iter().copied().chain(["--", spec]).collect();
                let (want, got) = fx.pair((&a, &b), cwd, &args, &[]);
                fx.check(cwd, &args, &[], &want, &got, true);
                let (pyn, git) = (state(&fx, &b), state(&fx, &a));
                let only = |x: &str, y: &str| -> Vec<String> {
                    x.lines()
                        .filter(|l| !y.lines().any(|m| m == *l))
                        .map(str::to_string)
                        .collect()
                };
                assert!(
                    pyn == git,
                    "{format} state after {args:?} in '{cwd}': only pyn has {:?}, only git has {:?}",
                    only(&pyn, &git),
                    only(&git, &pyn)
                );
            }
        }
    }
}

const SOME: &[&str] = &[
    "src",
    "*.rs",
    "src/*.rs",
    ":(glob)**/*.rs",
    ":!src",
    ":(icase)UP.TXT",
    ":(literal)a*c",
    "a?c",
    "a[1]",
    "docs",
    ":(glob)docs/*",
    "nope*",
    ":/docs",
    "../docs",
];

#[test]
fn mutating_commands_agree_from_the_root_sha1() {
    mutating("sha1", &[""], SOME);
}

#[test]
fn mutating_commands_agree_from_the_root_sha256() {
    mutating("sha256", &[""], &every_other(SOME));
}

#[test]
fn mutating_commands_agree_from_a_folder() {
    mutating("sha1", &["src", "g*b"], SOME);
}

#[test]
fn stash_with_untracked_files_takes_the_selected_ones() {
    let specs = [
        "new*",
        ":(glob)newdir/**",
        "only",
        ":!src",
        "newdir/sub/*",
        "*.txt",
    ];
    mutating_with("sha1", &["", "src"], &specs, &[&["stash", "push", "-u"]]);
}

#[test]
fn the_global_pathspec_settings_agree() {
    let fx = build("sha1");
    let settings: &[&[(&str, &str)]] = &[
        &[("GIT_LITERAL_PATHSPECS", "1")],
        &[("GIT_GLOB_PATHSPECS", "1")],
        &[("GIT_NOGLOB_PATHSPECS", "1")],
        &[("GIT_ICASE_PATHSPECS", "1")],
        &[
            ("GIT_GLOB_PATHSPECS", "yes"),
            ("GIT_ICASE_PATHSPECS", "true"),
        ],
        &[("GIT_NOGLOB_PATHSPECS", "on"), ("GIT_ICASE_PATHSPECS", "2")],
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_GLOB_PATHSPECS", "1")],
        &[
            ("GIT_LITERAL_PATHSPECS", "1"),
            ("GIT_NOGLOB_PATHSPECS", "1"),
        ],
        &[("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")],
        &[("GIT_LITERAL_PATHSPECS", "1"), ("GIT_ICASE_PATHSPECS", "1")],
        &[("GIT_GLOB_PATHSPECS", "bogus")],
        &[
            ("GIT_LITERAL_PATHSPECS", "0"),
            ("GIT_ICASE_PATHSPECS", "no"),
        ],
    ];
    let specs = [
        "*.rs",
        "src/*.rs",
        "**/*.rs",
        "src/**",
        ":(glob)*.rs",
        ":(literal)a*c",
        ":!src",
        "SRC",
        "Up",
        "src/DEEP/*.RS",
        ":(icase)SRC",
        ":(literal,glob)x",
        ":/src",
        "a.txt",
    ];
    let commands: &[&[&str]] = &[
        &["ls-files"],
        &["ls-files", "-o"],
        &["status", "-s"],
        &["diff", "--name-status"],
        &["add", "-n"],
        &["log", "--name-only", "--format=%s"],
        &["grep", "-l", "one"],
        &["rm", "-n", "-r"],
    ];
    for env in settings {
        for spec in specs {
            for cmd in commands {
                for cwd in ["", "src"] {
                    let args: Vec<&str> = cmd.iter().copied().chain(["--", spec]).collect();
                    fx.same(cwd, &args, env);
                }
            }
        }
    }
}

#[test]
fn bad_pathspecs_fail_with_the_same_message_and_status() {
    let fx = build("sha1");
    let specs = [
        "",
        ":(bogus)x",
        ":(top",
        ":(top,glob,literal)x",
        ":(literal,glob)x",
        ":#x",
        ":~x",
        ":-x",
        ":,x",
        ":%x",
        "../x",
        "../../x",
        "/etc/passwd",
        ":(",
        ":(icase,glob)x",
        ":(exclude,icase,glob)x",
        ":!x",
    ];
    let commands: &[&[&str]] = &[
        &["ls-files"],
        &["status", "-s"],
        &["diff", "--name-status"],
        &["add", "-n"],
        &["log", "--oneline"],
        &["grep", "-l", "one"],
        &["rm", "-n", "-r"],
        &["clean", "-n"],
        &["show", "--name-only"],
        &["shortlog", "-s", "HEAD"],
        &["ls-tree", "HEAD"],
    ];
    for spec in specs {
        for cmd in commands {
            for cwd in ["", "src"] {
                let args: Vec<&str> = cmd.iter().copied().chain(["--", spec]).collect();
                let (want, got) = fx.pair((&fx.repo, &fx.repo), cwd, &args, &[]);
                fx.check(cwd, &args, &[], &want, &got, sorted(&args));
                if want.0 == 128 {
                    let text = |e: &[u8]| {
                        String::from_utf8_lossy(e)
                            .lines()
                            .next()
                            .map(str::to_string)
                    };
                    assert_eq!(
                        text(&got.2),
                        text(&want.2),
                        "first stderr line of {args:?} in '{cwd}'"
                    );
                }
            }
        }
    }
}

#[test]
fn clean_refuses_a_glob_that_names_folders() {
    let fx = build("sha1");
    for spec in [":(glob)*/", ":(glob)ne*/"] {
        let got = fx.run(
            Path::new(env!("CARGO_BIN_EXE_pyn")),
            &fx.repo,
            "",
            &["clean", "-n", "--", spec],
            &[],
        );
        assert_eq!(got.0, 129, "{spec}: {}", String::from_utf8_lossy(&got.2));
    }
}

#[test]
fn magic_git_accepts_but_pyn_does_not_build_is_a_usage_error() {
    let fx = build("sha1");
    let got = fx.run(
        Path::new(env!("CARGO_BIN_EXE_pyn")),
        &fx.repo,
        "",
        &["ls-files", "--", ":(attr:text)*"],
        &[],
    );
    assert_eq!(got.0, 129, "{}", String::from_utf8_lossy(&got.2));
    assert!(String::from_utf8_lossy(&got.2).contains("attr"));
}

/// A small linear congruential generator, so a failure reproduces from the seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap() % n
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const NAMES: &[&str] = &[
    "a", "b", "A", "ab", "a b", "a*", "a?", "[a]", "x.rs", "X.RS", ".d", "d", "dd", "b.txt",
];

/// Folders never have wildcard characters in their names: git's tree walk (`log`) mismatches a pathspec there.
fn random_tree(rng: &mut Rng) -> Vec<String> {
    let folders: Vec<&str> = NAMES
        .iter()
        .copied()
        .filter(|n| !n.contains(['*', '?', '[']))
        .collect();
    let mut paths: Vec<String> = Vec::new();
    while paths.len() < 40 {
        let depth = 1 + rng.below(3);
        let mut parts: Vec<&str> = (1..depth).map(|_| rng.pick(&folders)).collect();
        parts.push(rng.pick(NAMES));
        let path = parts.join("/");
        let clash = paths.iter().any(|p| {
            p == &path || p.starts_with(&format!("{path}/")) || path.starts_with(&format!("{p}/"))
        });
        if !clash {
            paths.push(path);
        }
    }
    paths
}

const MAGICS: &[&str] = &[
    "",
    "",
    "",
    ":(glob)",
    ":(literal)",
    ":(icase)",
    ":(top)",
    ":/",
    ":!",
    ":(exclude)",
    ":(icase,glob)",
    ":(top,glob)",
    ":^",
    ":(top,exclude)",
    ":(glob,exclude)",
];

const TOKENS: &[&str] = &[
    "a",
    "b",
    "A",
    "ab",
    "a b",
    "a*",
    "a?",
    "x.rs",
    "X.RS",
    "d",
    "dd",
    ".d",
    "*",
    "**",
    "?",
    "[ab]",
    "[!a]",
    "[a-b]",
    "[[:alpha:]]",
    "[[:upper:]]",
    "*b",
    "?b",
    "a*b",
    "*.rs",
    "*.RS",
    r"\*",
    r"\?",
    r"a\*",
    ".",
    "..",
    "",
    "[A]",
    "*/*",
    "d*",
];

fn random_spec(rng: &mut Rng) -> String {
    let mut spec = rng.pick(MAGICS).to_string();
    let mut parts: Vec<&str> = (0..1 + rng.below(3)).map(|_| rng.pick(TOKENS)).collect();
    while parts.first() == Some(&"") {
        parts.remove(0);
    }
    if parts.is_empty() {
        parts.push("*");
    }
    spec.push_str(&parts.join("/"));
    if rng.below(5) == 0 {
        spec.push('/');
    }
    spec
}

fn fuzz(format: &'static str, seed: u64, rounds: usize) {
    let mut rng = Rng(seed);
    let fx = Fx::empty(format);
    let tree = random_tree(&mut rng);
    for path in &tree {
        fx.write(path, &format!("{path}\n"));
    }
    fx.g(&["add", "-A"]);
    fx.g(&["commit", "-q", "-m", "tree"]);
    for path in tree.iter().step_by(3) {
        fx.write(path, "changed\n");
    }
    fx.write("untracked/u.rs", "u\n");
    fx.write("untracked/deep/v.txt", "v\n");
    fx.write("newd/new/w.rs", "w\n");
    let folders: BTreeSet<String> = tree
        .iter()
        .filter_map(|p| p.rsplit_once('/').map(|(d, _)| d.to_string()))
        .collect();
    let folders: Vec<String> = std::iter::once(String::new()).chain(folders).collect();
    for round in 0..rounds {
        let spec = random_spec(&mut rng);
        let cwd = &folders[rng.below(folders.len())];
        for cmd in [
            &["ls-files"][..],
            &["ls-files", "-o"],
            &["status", "-s"],
            &["diff", "--name-status"],
            &["log", "--name-only", "--format=%s"],
        ] {
            let args: Vec<&str> = cmd.iter().copied().chain(["--", &spec]).collect();
            let (want, got) = fx.pair((&fx.repo, &fx.repo), cwd, &args, &[]);
            let note = format!("seed {seed}, round {round}");
            let (w, g) = (&want, &got);
            assert_eq!(
                String::from_utf8_lossy(&g.1),
                String::from_utf8_lossy(&w.1),
                "stdout of {args:?} in '{cwd}' ({note}); pyn stderr: {}",
                String::from_utf8_lossy(&g.2)
            );
            assert_eq!(
                g.0,
                w.0,
                "status of {args:?} in '{cwd}' ({note}); pyn stderr: {}, git stderr: {}",
                String::from_utf8_lossy(&g.2),
                String::from_utf8_lossy(&w.2)
            );
        }
    }
}

#[test]
fn random_pathspecs_on_random_trees_match_git_sha1() {
    for seed in [1, 2, 3] {
        fuzz("sha1", seed, 120);
    }
}

#[test]
fn random_pathspecs_on_random_trees_match_git_sha256() {
    fuzz("sha256", 7, 120);
}
