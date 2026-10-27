//! `pyn bisect` on a git repository compared with the real `git bisect` on a twin copy of the same repository:
//! what both print, their exit statuses and the state they leave (the `BISECT_*` files, `refs/bisect/*`, `HEAD`
//! and its reflog), on SHA-1 and SHA-256 repositories with linear and random merge histories. Real git is the
//! oracle in these tests only; wording that differs between the git releases in use (quotes around the terms) is
//! normalized on both sides.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::gitfix::copy_tree;
use gitsrv::*;
use tempfile::{TempDir, tempdir};

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
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[derive(Clone, Copy, Default)]
struct Shape {
    /// Every commit has the same time.
    same_dates: bool,
    /// Some commits are dated before their parents.
    skew: bool,
    /// Files in folders, so a path limit has something to select.
    paths: bool,
    /// Only a straight line of commits.
    linear: bool,
}

struct Commit {
    id: String,
    parents: Vec<usize>,
}

/// Two copies of one repository: the oracle's and the one `pyn` works on.
struct Pair {
    _tmp: TempDir,
    git: PathBuf,
    pyn: PathBuf,
    format: &'static str,
    commits: Vec<Commit>,
}

fn history(rng: &mut Rng, n: usize, shape: Shape) -> Vec<(Vec<usize>, i64)> {
    let mut out: Vec<(Vec<usize>, i64)> = Vec::new();
    for i in 0..n {
        let mut parents = Vec::new();
        if i > 0 {
            let want = if shape.linear {
                1
            } else if i > 4 && rng.chance(5) {
                3
            } else if i > 2 && rng.chance(30) {
                2
            } else {
                1
            };
            if rng.chance(70) || shape.linear {
                parents.push(i - 1);
            }
            while parents.len() < want {
                let p = rng.below(i);
                if !parents.contains(&p) {
                    parents.push(p);
                }
            }
        }
        let base = 1_700_000_000 + i as i64 * 100;
        let time = if shape.same_dates {
            1_700_000_000
        } else if shape.skew && rng.chance(20) {
            (base - 100 * (1 + rng.below(4)) as i64).max(1_700_000_000)
        } else if rng.chance(30) {
            1_700_000_000 + (i as i64 / 3) * 300
        } else {
            base
        };
        out.push((parents, time));
    }
    out
}

impl Pair {
    fn new(format: &'static str, seed: u64, n: usize, shape: Shape) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let git_dir = root.join("git");
        init(&git_dir, format, false);
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let plan = history(&mut rng, n, shape);
        let mut files: Vec<std::collections::BTreeMap<String, String>> = Vec::new();
        let mut stream = String::new();
        for (i, (parents, time)) in plan.iter().enumerate() {
            let mut tree = parents
                .first()
                .map(|p| files[*p].clone())
                .unwrap_or_default();
            tree.insert("f".into(), format!("{i}-f\n"));
            if shape.paths {
                for name in ["a/x", "b/y", "c"] {
                    if tree.contains_key(name) && rng.chance(10) {
                        tree.remove(name);
                    } else if parents.is_empty() || rng.chance(35) {
                        tree.insert(name.into(), format!("{i}-{name}\n"));
                    }
                }
            }
            let msg = format!("c{i}\n");
            stream.push_str(&format!(
                "commit refs/heads/main\nmark :{}\nauthor T <t@example.com> {time} +0000\ncommitter T <t@example.com> {time} +0000\ndata {}\n{msg}",
                i + 1,
                msg.len()
            ));
            if let Some(first) = parents.first() {
                stream.push_str(&format!("from :{}\n", first + 1));
            }
            for other in parents.iter().skip(1) {
                stream.push_str(&format!("merge :{}\n", other + 1));
            }
            stream.push_str("deleteall\n");
            for (name, content) in &tree {
                stream.push_str(&format!(
                    "M 100644 inline {name}\ndata {}\n{content}",
                    content.len()
                ));
            }
            stream.push('\n');
            files.push(tree);
        }
        let marks = root.join("marks");
        let mut import = git(&git_dir);
        import
            .args(["fast-import", "--quiet"])
            .arg(format!("--export-marks={}", marks.display()))
            .stdin(std::process::Stdio::piped());
        let mut child = import.spawn().unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), stream.as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
        let ids: Vec<String> = fs::read_to_string(&marks)
            .unwrap()
            .lines()
            .map(|l| l.split_once(' ').unwrap())
            .map(|(m, id)| (m[1..].parse::<usize>().unwrap(), id.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_values()
            .collect();
        run(&git_dir, &["reset", "-q", "--hard", "main"]);
        let pyn_dir = root.join("pyn");
        copy_tree(&git_dir, &pyn_dir);
        let commits = ids
            .into_iter()
            .zip(plan)
            .map(|(id, (parents, _))| Commit { id, parents })
            .collect();
        Self {
            _tmp: tmp,
            git: git_dir,
            pyn: pyn_dir,
            format,
            commits,
        }
    }

    fn ancestors(&self, of: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![of];
        while let Some(i) = todo.pop() {
            if seen.insert(i) {
                todo.extend(&self.commits[i].parents);
            }
        }
        seen
    }

    fn index_of(&self, id: &str) -> usize {
        self.commits.iter().position(|c| c.id == id).unwrap()
    }

    fn tip(&self) -> usize {
        self.commits.len() - 1
    }

    fn id(&self, i: usize) -> &str {
        &self.commits[i].id
    }

    /// The commit the oracle is asking about: where `HEAD` is, or `BISECT_HEAD` without a checkout.
    fn testing(&self) -> usize {
        let head = fs::read_to_string(self.git.join(".git/BISECT_HEAD"))
            .unwrap_or_else(|_| run(&self.git, &["rev-parse", "HEAD"]));
        self.index_of(head.trim())
    }

    fn pyn_cmd(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .arg("bisect")
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.git.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git_cmd(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let out = git(cwd)
            .env("TZ", "UTC")
            .arg("bisect")
            .args(args)
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Everything a bisection leaves behind in the repository at `dir`.
    fn state(&self, dir: &Path, loose: bool) -> String {
        let mut out = String::new();
        for name in [
            "BISECT_START",
            "BISECT_LOG",
            "BISECT_TERMS",
            "BISECT_NAMES",
            "BISECT_EXPECTED_REV",
            "BISECT_ANCESTORS_OK",
            "BISECT_FIRST_PARENT",
            "BISECT_HEAD",
            "BISECT_RUN",
        ] {
            if loose && name == "BISECT_START" {
                continue;
            }
            if let Ok(text) = fs::read_to_string(dir.join(".git").join(name)) {
                out.push_str(&format!("--{name}\n{text}"));
            }
        }
        out.push_str(&run(dir, &["for-each-ref"]));
        if loose {
            return norm(&out);
        }
        out.push_str(&fs::read_to_string(dir.join(".git/HEAD")).unwrap());
        out.push_str(&run(dir, &["log", "-g", "--format=%gs", "HEAD"]));
        out.push_str(&run(dir, &["status", "--porcelain"]));
        out.push_str(&run(dir, &["ls-files", "-s"]));
        norm(&out)
    }

    /// Runs `bisect args` with both tools; they must print, exit and leave state alike. Returns the output.
    fn step(&self, args: &[&str]) -> (i32, String) {
        self.step_as(args, false)
    }

    /// Like `step`, but the state compared leaves out where `HEAD` and the start point end up (git 2.56 detaches
    /// when a replayed log only starts).
    fn step_loose(&self, args: &[&str]) -> (i32, String) {
        self.step_as(args, true)
    }

    fn step_as(&self, args: &[&str], loose: bool) -> (i32, String) {
        let (want_code, want) = self.git_cmd(&self.git, args);
        let (code, got, err) = self.pyn_cmd(&self.pyn, args);
        let (want, got) = (norm(&want), norm(&got));
        assert_eq!(code, want_code, "{} bisect {args:?}: {err}", self.format);
        assert_eq!(got, want, "{} bisect {args:?}", self.format);
        let (g, p) = (self.state(&self.git, loose), self.state(&self.pyn, loose));
        assert_eq!(p, g, "{} state after bisect {args:?}\n{want}", self.format);
        (code, want)
    }
}

/// Git releases differ in whether a term is quoted in a message, and in telling that nothing is bisected.
fn norm(text: &str) -> String {
    let words = ["good", "bad", "old", "new"];
    let unquoted = text
        .lines()
        .filter(|l| *l != "We are not bisecting.")
        .map(|l| {
            if l.starts_with("Your current terms are '") || l.starts_with("and '") {
                l.replace('\'', "")
            } else {
                words
                    .iter()
                    .fold(l.to_string(), |t, w| t.replace(&format!("'{w}'"), w))
            }
        })
        .collect::<Vec<_>>();
    if unquoted.is_empty() {
        return String::new();
    }
    unquoted.join("\n") + "\n"
}

fn finished(code: i32, out: &str) -> bool {
    code != 0
        || out.contains("is the first")
        || out.contains("waiting")
        || out.contains("only 'skip'ped")
}

/// A bisection over `pair` whose first bad commit is `culprit`: answers every question as the property would.
fn drive(pair: &Pair, rng: &mut Rng, culprit: usize, start_extra: &[&str], skip_percent: u64) {
    let bad_set: BTreeSet<usize> = (0..pair.commits.len())
        .filter(|i| pair.ancestors(*i).contains(&culprit))
        .collect();
    let goods: Vec<usize> = (0..pair.commits.len())
        .filter(|i| !bad_set.contains(i))
        .collect();
    let good = goods[rng.below(goods.len())];
    let mut args = vec!["start"];
    args.extend_from_slice(start_extra);
    let (bad_id, good_id) = (pair.id(pair.tip()).to_string(), pair.id(good).to_string());
    args.push(&bad_id);
    args.push(&good_id);
    let trailing: Vec<String> = Vec::new();
    args.extend(trailing.iter().map(String::as_str));
    let (mut code, mut out) = pair.step(&args);
    for _ in 0..80 {
        if finished(code, &out) {
            return;
        }
        let at = pair.testing();
        let answer = if rng.chance(skip_percent) {
            "skip"
        } else if bad_set.contains(&at) {
            "bad"
        } else {
            "good"
        };
        (code, out) = pair.step(&[answer]);
    }
    panic!("the bisection does not end");
}

fn each_format(check: impl Fn(&'static str)) {
    for format in FORMATS {
        check(format);
    }
}

fn session(format: &'static str, seed: u64, n: usize, shape: Shape, extra: &[&str], skips: u64) {
    let pair = Pair::new(format, seed, n, shape);
    let mut rng = Rng(seed ^ 0xabcdef);
    let reach: Vec<usize> = pair
        .ancestors(pair.tip())
        .into_iter()
        .filter(|i| *i != 0)
        .collect();
    if reach.len() < 3 {
        return;
    }
    let culprit = reach[rng.below(reach.len())];
    drive(&pair, &mut rng, culprit, extra, skips);
}

#[test]
fn the_bad_commit_at_each_position_of_a_straight_history() {
    each_format(|format| {
        for culprit in [1, 2, 5, 8, 11] {
            let pair = Pair::new(
                format,
                7,
                12,
                Shape {
                    linear: true,
                    ..Shape::default()
                },
            );
            let mut rng = Rng(culprit as u64);
            drive(&pair, &mut rng, culprit, &[], 0);
        }
    });
}

#[test]
fn random_merge_histories() {
    each_format(|format| {
        for seed in 0..6 {
            session(
                format,
                seed,
                14 + seed as usize * 5,
                Shape::default(),
                &[],
                0,
            );
        }
    });
}

#[test]
fn equal_and_skewed_commit_times() {
    each_format(|format| {
        for seed in 10..14 {
            let same = Shape {
                same_dates: true,
                ..Shape::default()
            };
            session(format, seed, 24, same, &[], 0);
            let skew = Shape {
                skew: true,
                ..Shape::default()
            };
            session(format, seed, 24, skew, &[], 0);
        }
    });
}

#[test]
fn skipped_commits() {
    each_format(|format| {
        for seed in 20..28 {
            session(format, seed, 22, Shape::default(), &[], 30);
        }
    });
}

#[test]
fn a_path_limit_and_the_first_parent_chain() {
    each_format(|format| {
        let paths = Shape {
            paths: true,
            ..Shape::default()
        };
        for seed in 30..36 {
            let pair = Pair::new(format, seed, 24, paths);
            let mut rng = Rng(seed);
            let culprit = {
                let reach: Vec<usize> = pair
                    .ancestors(pair.tip())
                    .into_iter()
                    .filter(|i| *i != 0)
                    .collect();
                reach[rng.below(reach.len())]
            };
            let which: &[&str] = [&["a"][..], &["b/y"], &["c"], &["a", "c"]][rng.below(4)];
            let bad_set: BTreeSet<usize> = (0..pair.commits.len())
                .filter(|i| pair.ancestors(*i).contains(&culprit))
                .collect();
            let good = (0..pair.commits.len())
                .filter(|i| !bad_set.contains(i))
                .nth(rng.below(3))
                .unwrap_or(0);
            let (bad_id, good_id) = (pair.id(pair.tip()), pair.id(good));
            let mut args = vec!["start", bad_id, good_id, "--"];
            args.extend_from_slice(which);
            let (mut code, mut out) = pair.step(&args);
            for _ in 0..60 {
                if finished(code, &out) {
                    break;
                }
                let answer = if bad_set.contains(&pair.testing()) {
                    "bad"
                } else {
                    "good"
                };
                (code, out) = pair.step(&[answer]);
            }
        }
        for seed in 40..46 {
            session(format, seed, 24, Shape::default(), &["--first-parent"], 10);
        }
    });
}

#[test]
fn bisecting_without_a_checkout() {
    each_format(|format| {
        for seed in 50..54 {
            session(format, seed, 18, Shape::default(), &["--no-checkout"], 10);
        }
    });
}

#[test]
fn custom_terms_and_the_terms_command() {
    each_format(|format| {
        let pair = Pair::new(
            format,
            3,
            12,
            Shape {
                linear: true,
                ..Shape::default()
            },
        );
        let (tip, root) = (pair.id(pair.tip()), pair.id(0));
        pair.step(&["terms"]);
        pair.step(&["start", "--term-old=before", "--term-new=after", tip, root]);
        pair.step(&["terms"]);
        pair.step(&["terms", "--term-old"]);
        pair.step(&["terms", "--term-new"]);
        pair.step(&["good"]);
        pair.step(&["bad"]);
        pair.step(&["before"]);
        pair.step(&["after"]);
        pair.step(&["log"]);
        pair.step(&["reset"]);
        pair.step(&["start", "--term-new=new", "--term-old=old", tip, root]);
        pair.step(&["old"]);
        pair.step(&["new"]);
        pair.step(&["terms"]);
        pair.step(&["reset"]);
        pair.step(&[
            "start",
            "--term-good=x",
            "--term-good=y",
            "--term-bad=z",
            tip,
            root,
        ]);
        pair.step(&["y"]);
        pair.step(&["z"]);
        pair.step(&["good"]);
        pair.step(&["reset"]);
        pair.step(&["start", "--term-bad=skip", "--term-good=ok"]);
        pair.step(&["start", "--term-bad=same", "--term-good=same"]);
        pair.step(&["start", "--term-bad=bad"]);
        pair.step(&["start", "--term-bad=good"]);
        pair.step(&["start", "--term-bad"]);
        pair.step(&["start", "--term-bad=a..b", "--term-good=c"]);
        pair.step(&["reset"]);
    });
}

#[test]
fn starting_and_stopping_from_a_branch_and_from_a_detached_head() {
    each_format(|format| {
        let pair = Pair::new(format, 4, 14, Shape::default());
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        pair.step(&["reset"]);
        pair.step(&["log"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["reset"]);
        pair.step(&["reset"]);
        for p in [&pair.git, &pair.pyn] {
            run(p, &["checkout", "-q", "--detach", "main~3"]);
        }
        pair.step(&["start", "main", &root]);
        pair.step(&["reset"]);
        pair.step(&["start", "main", &root]);
        pair.step(&["reset", "main"]);
        pair.step(&["reset", "nosuchrev"]);
        pair.step(&["reset", "a", "b"]);
        pair.step(&["start", "main", &root]);
        pair.step(&["reset", "main~2"]);
        pair.step(&["start", &tip]);
        pair.step(&["good", &root]);
        pair.step(&["reset"]);
    });
}

#[test]
fn commands_that_need_a_started_bisection_and_bad_arguments() {
    each_format(|format| {
        let pair = Pair::new(format, 5, 10, Shape::default());
        let tip = pair.id(pair.tip()).to_string();
        let root = pair.id(0).to_string();
        for args in [
            &["good"][..],
            &["bad"],
            &["next"],
            &["skip"],
            &["log"],
            &["run", "true"],
            &["terms"],
            &["replay", "nosuchfile"],
            &["start", "nosuchrev", "--"],
            &["start", "--bogus"],
            &["start", &tip, &root],
            &["bad", "nosuchrev"],
            &["good", "nosuchrev"],
            &["bad", &tip, &root],
            &["good", &tip, &root],
            &["next"],
            &["terms", "--bogus"],
            &["terms", "--term-good", "x"],
            &["reset", "x", "y"],
            &["next", "x"],
        ] {
            pair.step(args);
        }
    });
}

#[test]
fn a_bad_commit_that_is_not_below_the_good_ones_asks_for_the_merge_base() {
    each_format(|format| {
        for seed in 60..66 {
            let pair = Pair::new(format, seed, 20, Shape::default());
            let mut rng = Rng(seed);
            let a = rng.below(pair.commits.len());
            let b = rng.below(pair.commits.len());
            let (a_id, b_id) = (pair.id(a).to_string(), pair.id(b).to_string());
            let (mut code, mut out) = pair.step(&["start", &a_id, &b_id]);
            for _ in 0..30 {
                if finished(code, &out) {
                    break;
                }
                let at = pair.testing();
                let answer = if pair.ancestors(at).contains(&a) || at == a {
                    "bad"
                } else {
                    "good"
                };
                (code, out) = pair.step(&[answer]);
            }
        }
    });
}

#[test]
fn skipping_ranges_and_everything_left() {
    each_format(|format| {
        let pair = Pair::new(
            format,
            6,
            12,
            Shape {
                linear: true,
                ..Shape::default()
            },
        );
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        pair.step(&["start", &tip, &root]);
        pair.step(&["skip", "main~6..main~3"]);
        pair.step(&["skip", "main~9..main~6", "main~1"]);
        pair.step(&["skip", "..main"]);
        pair.step(&["skip", "main~5..main~5"]);
        pair.step(&["skip", "nosuch..main"]);
        pair.step(&["skip", "main~2"]);
        pair.step(&["skip", "main~4"]);
        pair.step(&["skip", "main..main"]);
        pair.step(&["log"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        let ids: Vec<String> = (1..11).map(|i| pair.id(i).to_string()).collect();
        let all: Vec<&str> = std::iter::once("skip")
            .chain(ids.iter().map(String::as_str))
            .collect();
        pair.step(&all);
        pair.step(&["next"]);
        pair.step(&["bad", &root]);
    });
}

fn script(pair: &Pair, name: &str, body: &str) {
    for dir in [&pair.git, &pair.pyn] {
        let file = dir.join(name);
        fs::write(&file, body).unwrap();
        run(dir, &["config", "core.fileMode", "false"]);
        std::os::unix::fs::PermissionsExt::set_mode(
            &mut fs::metadata(&file).unwrap().permissions(),
            0o755,
        );
        let mut perms = fs::metadata(&file).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&file, perms).unwrap();
    }
}

#[test]
fn running_a_script_to_the_end_and_its_exit_statuses() {
    each_format(|format| {
        let pair = Pair::new(format, 8, 16, Shape::default());
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        let culprit = 6;
        let bad: Vec<&str> = (0..pair.commits.len())
            .filter(|i| pair.ancestors(*i).contains(&culprit))
            .map(|i| pair.id(i))
            .collect();
        script(
            &pair,
            "bad.sh",
            &format!(
                "#!/bin/sh\ncase \"$(git rev-parse HEAD)\" in {}) exit 1;; esac\nexit 0\n",
                bad.join("|")
            ),
        );
        script(&pair, "skip.sh", "#!/bin/sh\nexit 125\n");
        script(&pair, "abort.sh", "#!/bin/sh\nexit 130\n");
        script(&pair, "arg.sh", "#!/bin/sh\ntest \"$1\" = ok\n");
        pair.step(&["run", "./bad.sh"]);
        pair.step(&["log"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run", "./skip.sh"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run", "./abort.sh"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run", "./nosuchscript.sh"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run", "./arg.sh", "ok"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run", "sh", "-c", "exit 3"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip, &root]);
        pair.step(&["run"]);
        pair.step(&["run", "true"]);
        pair.step(&["run", "false"]);
        pair.step(&["reset"]);
        pair.step(&["start", &tip]);
        pair.step(&["run", "true"]);
        pair.step(&["reset"]);
    });
}

#[test]
fn replaying_a_log() {
    each_format(|format| {
        let pair = Pair::new(format, 9, 18, Shape::default());
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        let culprit = 7;
        let bad_set: BTreeSet<usize> = (0..pair.commits.len())
            .filter(|i| pair.ancestors(*i).contains(&culprit))
            .collect();
        let (mut code, mut out) = pair.step(&["start", &tip, &root]);
        let mut n = 0;
        while !finished(code, &out) && n < 30 {
            let answer = if n == 1 {
                "skip"
            } else if bad_set.contains(&pair.testing()) {
                "bad"
            } else {
                "good"
            };
            (code, out) = pair.step(&[answer]);
            n += 1;
        }
        let (log_code, log) = pair.step(&["log"]);
        assert_eq!(log_code, 0);
        for dir in [&pair.git, &pair.pyn] {
            fs::write(dir.join("../session.log"), &log).unwrap();
            fs::write(dir.join("../short.log"), "# comment\ngit bisect start\n").unwrap();
            fs::write(dir.join("../bad.log"), "git bisect frobnicate\n").unwrap();
        }
        pair.step(&["reset"]);
        pair.step(&["replay", "../session.log"]);
        pair.step(&["log"]);
        pair.step(&["replay"]);
        pair.step(&["replay", "../session.log", "../session.log"]);
        pair.step_loose(&["replay", "../bad.log"]);
        pair.step_loose(&["replay", "../short.log"]);
    });
}

#[test]
fn either_tool_can_continue_the_others_bisection() {
    each_format(|format| {
        for seed in 70..74 {
            let pair = Pair::new(format, seed, 22, Shape::default());
            let mut rng = Rng(seed);
            let reach: Vec<usize> = pair
                .ancestors(pair.tip())
                .into_iter()
                .filter(|i| *i != 0)
                .collect();
            let culprit = reach[rng.below(reach.len())];
            let bad_set: BTreeSet<usize> = (0..pair.commits.len())
                .filter(|i| pair.ancestors(*i).contains(&culprit))
                .collect();
            // The oracle twin always runs git; the other twin alternates between pyn and git.
            let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
            let mut args: Vec<String> = vec!["start".into(), tip, root];
            for turn in 0..40 {
                let strs: Vec<&str> = args.iter().map(String::as_str).collect();
                let (want_code, want) = pair.git_cmd(&pair.git, &strs);
                let (code, got) = if (turn + seed) % 2 == 0 {
                    let (c, o, _) = pair.pyn_cmd(&pair.pyn, &strs);
                    (c, o)
                } else {
                    pair.git_cmd(&pair.pyn, &strs)
                };
                assert_eq!(code, want_code, "{format} {args:?}");
                assert_eq!(norm(&got), norm(&want), "{format} {args:?}");
                assert_eq!(
                    pair.state(&pair.pyn, false),
                    pair.state(&pair.git, false),
                    "{format} {args:?}"
                );
                if finished(want_code, &want) {
                    break;
                }
                let answer = if bad_set.contains(&pair.testing()) {
                    "bad"
                } else {
                    "good"
                };
                args = vec![answer.into()];
            }
        }
    });
}

#[test]
fn a_linked_work_tree_keeps_its_own_bisection() {
    each_format(|format| {
        let pair = Pair::new(format, 11, 14, Shape::default());
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        let trees: Vec<PathBuf> = [&pair.git, &pair.pyn]
            .iter()
            .map(|dir| {
                let linked = dir.parent().unwrap().join(format!(
                    "linked-{}",
                    dir.file_name().unwrap().to_string_lossy()
                ));
                run(
                    dir,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "other",
                        linked.to_str().unwrap(),
                        &root,
                    ],
                );
                linked
            })
            .collect();
        let state = |linked: &Path, dir: &Path| {
            let admin = dir.join(".git/worktrees").join(linked.file_name().unwrap());
            let mut out = String::new();
            for name in [
                "BISECT_START",
                "BISECT_LOG",
                "BISECT_TERMS",
                "BISECT_EXPECTED_REV",
            ] {
                out.push_str(&fs::read_to_string(admin.join(name)).unwrap_or_default());
            }
            out.push_str(&run(linked, &["for-each-ref", "refs/bisect"]));
            out.push_str(&run(linked, &["rev-parse", "HEAD"]));
            out.push_str(&run(dir, &["rev-parse", "HEAD"]));
            assert!(!dir.join(".git/BISECT_START").exists());
            norm(&out)
        };
        for args in [
            vec!["start", &tip, &root],
            vec!["good"],
            vec!["bad"],
            vec!["log"],
            vec!["reset"],
        ] {
            let (want_code, want) = pair.git_cmd(&trees[0], &args);
            let (code, got, err) = pair.pyn_cmd(&trees[1], &args);
            assert_eq!(code, want_code, "{format} {args:?}: {err}");
            assert_eq!(norm(&got), norm(&want), "{format} {args:?}");
            assert_eq!(
                state(&trees[1], &pair.pyn),
                state(&trees[0], &pair.git),
                "{format} {args:?}"
            );
        }
    });
}

#[test]
fn local_changes_that_stop_a_checkout() {
    each_format(|format| {
        let pair = Pair::new(format, 12, 14, Shape::default());
        let (tip, root) = (pair.id(pair.tip()).to_string(), pair.id(0).to_string());
        let write = |name: &str, text: &str| {
            for dir in [&pair.git, &pair.pyn] {
                fs::write(dir.join(name), text).unwrap();
            }
        };
        pair.step(&["start", &tip, &root]);
        write("f", "local edit\n");
        pair.step(&["good"]);
        pair.step(&["bad"]);
        pair.step(&["log"]);
        pair.step(&["reset"]);
        for dir in [&pair.git, &pair.pyn] {
            run(dir, &["checkout", "-q", "f"]);
        }
        pair.step(&["reset"]);
        write("f", "local edit\n");
        pair.step(&["start", &tip, &root]);
        pair.step(&["log"]);
        for dir in [&pair.git, &pair.pyn] {
            run(dir, &["checkout", "-q", "f"]);
        }
        pair.step(&["start", &tip, &root]);
        pair.step(&["reset"]);
    });
}
