//! `pyn clean` on a git repository: removes untracked files (and, with `-d`, folders) from the work tree, with git's
//! output. It lists first and removes second, and only descends real folders, never a link.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::{Ignore, Level, Pathspec, fs_path, parse_rules};

use super::stage::{Ignores, child, dir_entries};
use super::{GitHome, path_string};

#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Show what would be removed without removing it.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Required unless `-n` is given (`clean.requireForce`); twice also removes nested repositories.
    #[arg(short = 'f', long, action = clap::ArgAction::Count)]
    force: u8,
    /// Remove untracked folders too.
    #[arg(short = 'd')]
    dirs: bool,
    /// Remove ignored files too.
    #[arg(short = 'x')]
    ignored: bool,
    /// Remove only ignored files.
    #[arg(short = 'X')]
    ignored_only: bool,
    /// Interactive mode; not built.
    #[arg(short = 'i', long)]
    interactive: bool,
    #[arg(short, long)]
    quiet: bool,
    /// Treat the pattern as an ignore rule for this run.
    #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
    exclude: Vec<String>,
    /// Limit the clean to these files and folders (literal paths, no globs).
    paths: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    File,
    Dir {
        bare: bool,
    },
    Skip,
    /// The work tree folder you stand in is not removed.
    Refuse,
}

#[derive(Debug)]
struct Item {
    path: Vec<u8>,
    kind: Kind,
    /// Overrides the path relative to the current folder, for what is listed from a folder you stand in.
    display: Option<Vec<u8>>,
}

/// What a folder holds: `whole` when everything in it is to be removed, so it is one entry.
#[derive(Default)]
struct Scan {
    whole: bool,
    nested: bool,
    items: Vec<Item>,
}

struct Cleaner<'a> {
    git: &'a GitHome,
    ignores: Ignore,
    spec: Pathspec,
    files: HashSet<Vec<u8>>,
    dirs: HashSet<Vec<u8>>,
    standard: bool,
    only_ignored: bool,
    list_dirs: bool,
    nested_too: bool,
    cwd_prefix: Vec<u8>,
    /// Number of command-line levels kept at the end of `levels`, which win over every file.
    cmd: usize,
}

fn is_git_name(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b".git")
}

impl Cleaner<'_> {
    fn push(&self, levels: &mut Vec<Level>, dir: &[u8]) -> Result<bool> {
        if !self.standard {
            return Ok(false);
        }
        let Some(level) = Ignores::level(self.git, dir)? else {
            return Ok(false);
        };
        levels.insert(levels.len() - self.cmd, level);
        Ok(true)
    }

    fn pop(&self, levels: &mut Vec<Level>) {
        levels.remove(levels.len() - self.cmd - 1);
    }

    fn ignored(&self, levels: &[Level], path: &[u8], is_dir: bool, inherited: bool) -> bool {
        inherited || self.ignores.is_ignored(levels, path, is_dir)
    }

    fn wanted(&self, ignored: bool) -> bool {
        ignored == self.only_ignored
    }

    fn has_git(&self, dir: &[u8]) -> bool {
        self.git
            .repo
            .root()
            .is_ok_and(|root| std::fs::symlink_metadata(fs_path(root, dir).join(".git")).is_ok())
    }

    /// Walks a folder the index knows, reporting untracked files and folders under it.
    fn tracked(
        &self,
        dir: &[u8],
        levels: &mut Vec<Level>,
        inherited: bool,
        out: &mut Vec<Item>,
    ) -> Result<()> {
        let pushed = self.push(levels, dir)?;
        for (name, kind) in dir_entries(&fs_path(self.git.repo.root()?, dir))? {
            if is_git_name(&name) || (dir.is_empty() && name == b".pyn") {
                continue;
            }
            let path = child(dir, &name);
            if !self.spec.may_contain(&path) {
                continue;
            }
            if !kind.is_dir() {
                let ignored = self.ignored(levels, &path, false, inherited);
                if !self.files.contains(&path) && self.spec.matches(&path) && self.wanted(ignored) {
                    out.push(Item {
                        path,
                        kind: Kind::File,
                        display: None,
                    });
                }
                continue;
            }
            if self.files.contains(&path) {
                continue;
            }
            let ignored = self.ignored(levels, &path, true, inherited);
            if self.dirs.contains(&path) {
                self.tracked(&path, levels, ignored, out)?;
            } else if self.list_dirs || self.only_ignored {
                self.untracked(&path, levels, ignored, out)?;
            }
        }
        if pushed {
            self.pop(levels);
        }
        Ok(())
    }

    fn untracked(
        &self,
        dir: &[u8],
        levels: &mut Vec<Level>,
        ignored: bool,
        out: &mut Vec<Item>,
    ) -> Result<()> {
        if self.has_git(dir) && !self.nested_too {
            return Ok(());
        }
        if !self.only_ignored && ignored {
            return Ok(());
        }
        let scan = self.scan(dir, levels, ignored)?;
        if scan.whole {
            if self.list_dirs {
                out.push(Item {
                    path: dir.to_vec(),
                    kind: Kind::Dir { bare: false },
                    display: None,
                });
            }
        } else if self.list_dirs {
            out.extend(scan.items);
        } else {
            out.extend(scan.items.into_iter().filter(|i| i.kind == Kind::File));
        }
        Ok(())
    }

    fn scan(&self, dir: &[u8], levels: &mut Vec<Level>, inherited: bool) -> Result<Scan> {
        let pushed = self.push(levels, dir)?;
        let mut scan = Scan {
            whole: self.spec.matches(dir),
            ..Scan::default()
        };
        let mut any = false;
        for (name, kind) in dir_entries(&fs_path(self.git.repo.root()?, dir))? {
            if is_git_name(&name) {
                if self.nested_too {
                    any = true;
                    scan.whole &= !self.only_ignored;
                }
                continue;
            }
            any = true;
            let path = child(dir, &name);
            if !self.spec.may_contain(&path) {
                scan.whole = false;
                continue;
            }
            let ignored = self.ignored(levels, &path, kind.is_dir(), inherited);
            if !kind.is_dir() {
                if self.wanted(ignored) {
                    scan.items.push(Item {
                        path,
                        kind: Kind::File,
                        display: None,
                    });
                } else {
                    scan.whole = false;
                }
            } else if self.has_git(&path) && !self.nested_too {
                scan.whole = false;
                if !self.only_ignored {
                    scan.nested = true;
                    scan.items.push(Item {
                        path,
                        kind: Kind::Skip,
                        display: None,
                    });
                }
            } else if !self.only_ignored && ignored {
                scan.whole = false;
            } else {
                let sub = self.scan(&path, levels, ignored)?;
                scan.nested |= sub.nested;
                if sub.whole && self.spec.matches(&path) {
                    scan.items.push(Item {
                        path,
                        kind: Kind::Dir { bare: false },
                        display: None,
                    });
                } else {
                    scan.whole = false;
                    scan.items.extend(sub.items);
                }
            }
        }
        scan.whole &= any || !self.only_ignored || inherited;
        if scan.nested {
            for item in &mut scan.items {
                if let Kind::Dir { bare } = &mut item.kind {
                    *bare = true;
                }
            }
        }
        if pushed {
            self.pop(levels);
        }
        Ok(scan)
    }
}

fn relative(path: &[u8], cwd: &[u8]) -> Vec<u8> {
    let mut p: Vec<&[u8]> = path
        .split(|b| *b == b'/')
        .filter(|s| !s.is_empty())
        .collect();
    let mut c: Vec<&[u8]> = cwd
        .split(|b| *b == b'/')
        .filter(|s| !s.is_empty())
        .collect();
    while !p.is_empty() && !c.is_empty() && p[0] == c[0] {
        p.remove(0);
        c.remove(0);
    }
    let mut out = Vec::new();
    for _ in &c {
        out.extend_from_slice(b"../");
    }
    out.extend_from_slice(&p.join(&b'/'));
    out
}

fn holds_cwd(dir: &[u8], cwd: &[u8]) -> bool {
    cwd.starts_with(dir) && (cwd.len() == dir.len() || cwd[dir.len()] == b'/')
}

/// Lists what is in a folder that holds the current folder, one entry each, descending only towards the current folder.
fn expand_cwd(
    git: &GitHome,
    dir: &[u8],
    display: &[u8],
    cwd: &[u8],
    out: &mut Vec<Item>,
) -> Result<()> {
    for (name, kind) in dir_entries(&fs_path(git.repo.root()?, dir))? {
        let path = child(dir, &name);
        let mut shown = display.to_vec();
        shown.push(b'/');
        shown.extend_from_slice(&name);
        if kind.is_dir() && holds_cwd(&path, cwd) {
            expand_cwd(git, &path, &shown, cwd, out)?;
            continue;
        }
        out.push(Item {
            path,
            kind: if kind.is_dir() {
                Kind::Dir { bare: true }
            } else {
                Kind::File
            },
            display: Some(shown),
        });
    }
    Ok(())
}

fn shown(item: &Item, cwd: &[u8]) -> String {
    let mut text = item
        .display
        .clone()
        .unwrap_or_else(|| relative(&item.path, cwd));
    if matches!(item.kind, Kind::Dir { bare: false }) {
        text.push(b'/');
    }
    path_string(&text)
}

fn remove(git: &GitHome, item: &Item) -> std::io::Result<()> {
    let root = git.repo.root().map_err(std::io::Error::other)?;
    let file = fs_path(root, &item.path);
    match item.kind {
        Kind::File => std::fs::remove_file(file),
        Kind::Dir { .. } => std::fs::remove_dir_all(file),
        Kind::Skip | Kind::Refuse => Ok(()),
    }
}

/// `pyn clean`: exit status 1 when something could not be removed.
pub fn clean(git: &GitHome, cwd: &Path, args: &CleanArgs) -> Result<bool> {
    if args.ignored && args.ignored_only {
        bail!("-x and -X cannot be used together");
    }
    if args.interactive {
        crate::usage_error("`pyn clean -i` is not built");
    }
    if !args.dry_run && args.force == 0 && git.repo.bool_setting("clean.requireForce", true) {
        bail!(
            "clean.requireForce defaults to true and neither -i, -n, nor -f given; refusing to clean"
        );
    }
    if let Some(magic) = args.paths.iter().find(|p| p.starts_with(':')) {
        bail!("pathspec '{magic}': pathspec magic is not supported; name files and folders");
    }
    let prefix = git.ws.repo_prefix(cwd, ".")?;
    let named: Vec<String> = if args.paths.is_empty() {
        vec![".".into()]
    } else {
        args.paths.clone()
    };
    let specs = super::stage::resolve_specs(git, cwd, &named)?;
    let opened = git.open_index()?;
    let mut files = HashSet::new();
    let mut dirs = HashSet::new();
    for e in &opened.index.entries {
        files.insert(e.path.clone());
        let mut end = 0;
        while let Some(i) = e.path[end..].iter().position(|b| *b == b'/') {
            end += i;
            dirs.insert(e.path[..end].to_vec());
            end += 1;
        }
    }
    let standard = !args.ignored;
    let ignores = if standard {
        git.repo.ignore()?
    } else {
        Ignore::default()
    };
    let mut levels = Vec::new();
    let cmd = usize::from(!args.exclude.is_empty());
    if cmd == 1 {
        levels.push(Level::new(
            b"",
            parse_rules(args.exclude.join("\n").as_bytes()),
        ));
    }
    let cleaner = Cleaner {
        git,
        ignores,
        spec: specs.spec,
        files,
        dirs,
        standard,
        only_ignored: args.ignored_only,
        list_dirs: args.dirs || !args.paths.is_empty(),
        nested_too: args.force > 1,
        cwd_prefix: prefix.clone().into_bytes(),
        cmd,
    };
    let mut items = Vec::new();
    cleaner.tracked(b"", &mut levels, false, &mut items)?;
    items.sort_by_key(|i| {
        let mut k = i.path.clone();
        if matches!(i.kind, Kind::Dir { .. }) {
            k.push(b'/');
        }
        k
    });
    let cwd_bytes = &cleaner.cwd_prefix;
    let mut listed = Vec::new();
    for item in items {
        if matches!(item.kind, Kind::Dir { .. }) && holds_cwd(&item.path, cwd_bytes) {
            let mut display = relative(&item.path, cwd_bytes);
            while display.last() == Some(&b'/') {
                display.pop();
            }
            if display.is_empty() {
                display.push(b'.');
            }
            listed.push(Item {
                path: item.path.clone(),
                kind: Kind::Refuse,
                display: None,
            });
            expand_cwd(git, &item.path, &display, cwd_bytes, &mut listed)?;
        } else {
            listed.push(item);
        }
    }
    let items = listed;
    let mut failed = false;
    for item in &items {
        let name = shown(item, cwd_bytes);
        if item.kind == Kind::Refuse {
            if !args.quiet {
                let verb = if args.dry_run {
                    "Would refuse"
                } else {
                    "Refusing"
                };
                println!("{verb} to remove current working directory");
            }
            continue;
        }
        if item.kind == Kind::Skip {
            if !args.quiet {
                let verb = if args.dry_run {
                    "Would skip"
                } else {
                    "Skipping"
                };
                println!("{verb} repository {name}");
            }
        } else if args.dry_run {
            if !args.quiet {
                println!("Would remove {name}");
            }
        } else if remove(git, item).is_err() {
            eprintln!("warning: failed to remove {name}");
            failed = true;
        } else if !args.quiet {
            println!("Removing {name}");
        }
    }
    Ok(failed)
}
