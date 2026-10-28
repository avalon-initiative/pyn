//! `merge -s <strategy>` and `-X <option>` on a git repository: git's strategy names, the options its tree merge takes,
//! and the subtree shift.

use std::collections::BTreeMap;

use anyhow::Result;
use pyn_gitwork::Entry;
use pyn_merge::{Favor, Options};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;
use super::stage::write_leaves;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Which {
    Ort,
    Recursive,
    Resolve,
    Octopus,
    Ours,
    Subtree,
}

const NAMES: [(&str, Which); 6] = [
    ("octopus", Which::Octopus),
    ("ours", Which::Ours),
    ("recursive", Which::Recursive),
    ("resolve", Which::Resolve),
    ("subtree", Which::Subtree),
    ("ort", Which::Ort),
];

impl Which {
    pub(super) fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(_, k)| *k == self)
            .map_or("ort", |n| n.0)
    }

    /// `ours` and `subtree` always make a merge commit, even when the merge could fast-forward.
    pub(super) fn no_fast_forward(self) -> bool {
        matches!(self, Which::Ours | Which::Subtree)
    }

    /// `resolve` and `octopus` first try a merge that needs no file-level merging.
    pub(super) fn tries_trivial(self) -> bool {
        matches!(self, Which::Resolve | Which::Octopus)
    }

    /// Strategies run by the tree merge, which take `-X` options.
    pub(super) fn is_tree_merge(self) -> bool {
        matches!(self, Which::Ort | Which::Recursive | Which::Subtree)
    }

    /// Git 2.55 records `AUTO_MERGE` for every strategy that runs on its ort engine.
    pub(super) fn writes_auto_merge(self) -> bool {
        self.is_tree_merge()
    }
}

/// What the tree merge honors from `-X`.
#[derive(Clone, Debug, Default)]
pub(super) struct Settings {
    pub text: Options,
    /// Conflicts get git's two-sided markers instead of diff3 ones.
    pub plain_markers: bool,
    pub renormalize: bool,
    /// Extra marker width, two per level of a virtual merge base.
    pub marker_extra: usize,
    /// `-X subtree[=<path>]` or `-s subtree`: the prefix, empty to find it.
    pub shift: Option<String>,
}

#[derive(Debug)]
pub(super) struct Strategy {
    pub kind: Which,
    /// The `-X` values as given; checked when a real merge needs them, as git does.
    xopts: Vec<String>,
}

impl Strategy {
    /// Reads `-s` and `-X`: an unknown strategy ends the command as git does, and options the merge cannot honor are a
    /// usage error before anything changes.
    pub(super) fn parse(strategies: &[String], xopts: &[String]) -> Self {
        if strategies.len() > 1 {
            crate::usage_error(
                "only one merge strategy can be given with `-s`; trying several is not built",
            );
        }
        let kind = match strategies.first() {
            None => Which::Ort,
            Some(name) => match NAMES.iter().find(|(n, _)| n == name) {
                Some((_, kind)) => *kind,
                None => {
                    eprintln!("Could not find merge strategy '{name}'.");
                    let names: Vec<&str> = {
                        let mut v: Vec<&str> = NAMES[..5].iter().map(|n| n.0).collect();
                        v.sort_unstable();
                        v
                    };
                    eprintln!("Available strategies are: {}.", names.join(" "));
                    std::process::exit(1);
                }
            },
        };
        let strategy = Self {
            kind,
            xopts: xopts.to_vec(),
        };
        if kind.is_tree_merge()
            && let Err(BadOption::Unsupported(opt, why)) = strategy.settings()
        {
            crate::usage_error(&format!("`-X{opt}`: {why}"));
        }
        strategy
    }

    pub(super) fn first_option(&self) -> Option<&str> {
        self.xopts.first().map(String::as_str)
    }

    /// Whether any `-X` was given.
    pub(super) fn has_options(&self) -> bool {
        !self.xopts.is_empty()
    }

    /// The tree merge's settings, or git's `unknown strategy option` for a value it does not define.
    pub(super) fn settings(&self) -> Result<Settings, BadOption> {
        let mut s = Settings::default();
        if self.kind == Which::Subtree {
            s.shift = Some(String::new());
        }
        for opt in &self.xopts {
            apply(&mut s, opt)?;
        }
        Ok(s)
    }
}

/// `-s` and `-X` as a replay (rebase, cherry-pick, revert) keeps and saves them: the tree merge only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Replay {
    pub name: Option<String>,
    pub xopts: Vec<String>,
}

impl Replay {
    /// Reads and checks the options for `command`, ending it with git's or a usage error before anything changes.
    pub(super) fn parse(command: &str, name: Option<&str>, xopts: &[String]) -> Self {
        let names: Vec<String> = name.map(str::to_string).into_iter().collect();
        let strategy = Strategy::parse(&names, xopts);
        if !matches!(strategy.kind, Which::Ort | Which::Recursive) {
            crate::usage_error(&format!(
                "`pyn {command} --strategy={}`: a replay merges with ort or recursive only",
                strategy.kind.name()
            ));
        }
        match strategy.settings() {
            Ok(s) if s.shift.is_some() => crate::usage_error(&format!(
                "`pyn {command} -Xsubtree`: the subtree shift is not built for a replay"
            )),
            Ok(_) => {}
            Err(BadOption::Unknown(opt) | BadOption::Unsupported(opt, _)) => {
                eprintln!("fatal: unknown strategy option: -X{opt}");
                std::process::exit(128);
            }
        }
        Self {
            name: name.map(str::to_string),
            xopts: xopts.to_vec(),
        }
    }

    pub(super) fn settings(&self) -> Settings {
        let strategy = Strategy {
            kind: Which::Ort,
            xopts: self.xopts.clone(),
        };
        strategy.settings().unwrap_or_default()
    }
}

pub(super) enum BadOption {
    Unknown(String),
    Unsupported(String, &'static str),
}

const NO_RENAMES: &str = "rename detection is not built, so merges never follow renames";
const NO_ALGORITHM: &str = "only the Myers diff is built";

fn rename_score(arg: &str) -> bool {
    let digits = arg.strip_suffix('%').unwrap_or(arg);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn apply(s: &mut Settings, opt: &str) -> Result<(), BadOption> {
    let unknown = || BadOption::Unknown(opt.to_string());
    let space = &mut s.text.space;
    match opt {
        "ours" => s.text.favor = Some(Favor::Ours),
        "theirs" => s.text.favor = Some(Favor::Theirs),
        "subtree" => s.shift = Some(String::new()),
        "ignore-space-change" => space.change = true,
        "ignore-all-space" => space.all = true,
        "ignore-space-at-eol" => space.at_eol = true,
        "ignore-cr-at-eol" => space.cr_at_eol = true,
        "renormalize" => s.renormalize = true,
        "no-renormalize" => s.renormalize = false,
        "no-renames" => {}
        "patience" | "histogram" => {
            return Err(BadOption::Unsupported(opt.to_string(), NO_ALGORITHM));
        }
        "find-renames" => return Err(BadOption::Unsupported(opt.to_string(), NO_RENAMES)),
        _ => {
            if let Some(path) = opt.strip_prefix("subtree=") {
                s.shift = Some(path.to_string());
            } else if let Some(algorithm) = opt.strip_prefix("diff-algorithm=") {
                match algorithm {
                    "myers" | "default" | "minimal" => {}
                    "patience" | "histogram" => {
                        return Err(BadOption::Unsupported(opt.to_string(), NO_ALGORITHM));
                    }
                    _ => return Err(unknown()),
                }
            } else if let Some(score) = opt
                .strip_prefix("find-renames=")
                .or_else(|| opt.strip_prefix("rename-threshold="))
            {
                if !rename_score(score) {
                    return Err(unknown());
                }
                return Err(BadOption::Unsupported(opt.to_string(), NO_RENAMES));
            } else {
                return Err(unknown());
            }
        }
    }
    Ok(())
}

fn is_dir(mode: FileMode) -> bool {
    mode == FileMode::Tree
}

fn score_missing(mode: FileMode) -> i32 {
    match mode {
        FileMode::Tree => -1000,
        FileMode::Symlink => -500,
        _ => -50,
    }
}

fn score_differs(a: FileMode, b: FileMode) -> i32 {
    if is_dir(a) != is_dir(b) {
        -100
    } else if (a == FileMode::Symlink) != (b == FileMode::Symlink) {
        -50
    } else {
        -5
    }
}

fn score_matches(a: FileMode, b: FileMode) -> i32 {
    if is_dir(a) != is_dir(b) {
        -100
    } else if (a == FileMode::Symlink) != (b == FileMode::Symlink) {
        -50
    } else if is_dir(a) {
        1000
    } else if a == FileMode::Symlink {
        500
    } else {
        250
    }
}

type Names = BTreeMap<(Vec<u8>, bool), (FileMode, ObjectId)>;

fn names(git: &GitHome, tree: &ObjectId) -> Result<Names> {
    Ok(git
        .repo
        .tree(tree)?
        .into_iter()
        .map(|e| ((e.name, is_dir(e.mode)), (e.mode, e.id)))
        .collect())
}

/// How alike two trees are, by their top-level entries (git's `score_trees`).
fn score_trees(git: &GitHome, one: &ObjectId, two: &ObjectId) -> Result<i32> {
    let (a, b) = (names(git, one)?, names(git, two)?);
    let mut score = 0;
    for (key, (mode, id)) in &a {
        score += match b.get(key) {
            None => score_missing(*mode),
            Some((other, other_id)) if other_id == id => score_matches(*mode, *other),
            Some((other, _)) => score_differs(*mode, *other),
        };
    }
    score += b
        .iter()
        .filter(|(key, _)| !a.contains_key(*key))
        .map(|(_, (mode, _))| score_missing(*mode))
        .sum::<i32>();
    Ok(score)
}

/// The subfolder of `one` (searched `limit` levels down) that best matches `two`, if it beats `best`.
fn match_trees(
    git: &GitHome,
    one: &ObjectId,
    two: &ObjectId,
    best: &mut (i32, Vec<u8>),
    base: &[u8],
    limit: u32,
) -> Result<()> {
    for entry in git.repo.tree(one)? {
        if !is_dir(entry.mode) {
            continue;
        }
        let path = [base, entry.name.as_slice()].concat();
        let score = score_trees(git, &entry.id, two)?;
        if best.0 < score {
            *best = (score, path.clone());
        }
        if limit > 0 {
            match_trees(
                git,
                &entry.id,
                two,
                best,
                &[path.as_slice(), b"/"].concat(),
                limit - 1,
            )?;
        }
    }
    Ok(())
}

fn under<'a>(path: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    path.strip_prefix(prefix)?.strip_prefix(b"/")
}

/// `one` with the folder `prefix` replaced by the whole of `two`.
fn splice(git: &GitHome, one: &ObjectId, prefix: &[u8], two: &ObjectId) -> Result<ObjectId> {
    let mut leaves = leaves(git, one)?;
    leaves.retain(|p, _| under(p, prefix).is_none());
    for (p, e) in self::leaves(git, two)? {
        leaves.insert([prefix, b"/", p.as_slice()].concat(), e);
    }
    write_leaves(git, &leaves)
}

fn leaves(git: &GitHome, tree: &ObjectId) -> Result<BTreeMap<Vec<u8>, Entry>> {
    Ok(git
        .repo
        .flatten(Some(tree), &pyn_gitwork::Pathspec::default())?)
}

fn is_folder(git: &GitHome, tree: &ObjectId, path: &[u8]) -> Result<Option<ObjectId>> {
    Ok(git
        .repo
        .entry_at(tree, path)?
        .filter(|e| is_dir(e.mode))
        .map(|e| e.id))
}

/// `two` moved to line up with `one`: a folder is picked out of it or it is put into a folder, as git's subtree merge
/// does. An empty `prefix` finds the folder by comparing trees (two levels deep).
pub(super) fn shift_tree(
    git: &GitHome,
    one: &ObjectId,
    two: &ObjectId,
    prefix: &str,
) -> Result<ObjectId> {
    let prefix = prefix.trim_end_matches('/').as_bytes();
    if prefix.is_empty() {
        return find_shift(git, one, two);
    }
    let inside_one = is_folder(git, one, prefix)?;
    let inside_two = is_folder(git, two, prefix)?;
    let mut candidate = u8::from(inside_one.is_some()) | (u8::from(inside_two.is_some()) << 1);
    if candidate == 3 {
        let mut best = score_trees(git, one, two)?;
        candidate = 0;
        if let Some(sub) = &inside_one {
            let score = score_trees(git, sub, two)?;
            if score > best {
                candidate = 1;
                best = score;
            }
        }
        if let Some(sub) = &inside_two
            && score_trees(git, sub, one)? > best
        {
            candidate = 2;
        }
    }
    match candidate {
        1 => splice(git, one, prefix, two),
        2 => Ok(inside_two.expect("candidate 2 has the folder")),
        _ => Ok(*two),
    }
}

fn find_shift(git: &GitHome, one: &ObjectId, two: &ObjectId) -> Result<ObjectId> {
    let start = score_trees(git, one, two)?;
    let (mut add, mut del) = ((start, Vec::new()), (start, Vec::new()));
    match_trees(git, one, two, &mut add, b"", 2)?;
    match_trees(git, two, one, &mut del, b"", 2)?;
    if add.0 < del.0 {
        if del.1.is_empty() {
            return Ok(*two);
        }
        return Ok(git.repo.entry_at(two, &del.1)?.map_or(*two, |e| e.id));
    }
    if add.1.is_empty() {
        return Ok(*two);
    }
    splice(git, one, &add.1, two)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyn_merge::Space;

    fn xopts(opts: &[&str]) -> Strategy {
        Strategy {
            kind: Which::Ort,
            xopts: opts.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn options_accumulate_and_the_last_favor_wins() {
        let s = xopts(&["ours", "theirs", "ignore-all-space", "renormalize"])
            .settings()
            .ok()
            .unwrap();
        assert_eq!(s.text.favor, Some(Favor::Theirs));
        assert!(
            s.text.space
                == Space {
                    all: true,
                    ..Space::default()
                }
                && s.renormalize
        );
    }

    #[test]
    fn unknown_and_unbuilt_options_differ() {
        assert!(matches!(
            xopts(&["bogus"]).settings(),
            Err(BadOption::Unknown(_))
        ));
        assert!(matches!(
            xopts(&[""]).settings(),
            Err(BadOption::Unknown(_))
        ));
        assert!(matches!(
            xopts(&["patience"]).settings(),
            Err(BadOption::Unsupported(..))
        ));
        assert!(matches!(
            xopts(&["rename-threshold=40%"]).settings(),
            Err(BadOption::Unsupported(..))
        ));
        assert!(matches!(
            xopts(&["rename-threshold=x"]).settings(),
            Err(BadOption::Unknown(_))
        ));
        assert!(
            xopts(&["diff-algorithm=minimal", "no-renames"])
                .settings()
                .is_ok()
        );
    }

    #[test]
    fn subtree_strategy_shifts_without_a_path() {
        let s = Strategy {
            kind: Which::Subtree,
            xopts: vec!["subtree=lib".into()],
        };
        assert_eq!(s.settings().ok().unwrap().shift.as_deref(), Some("lib"));
    }
}
