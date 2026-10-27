//! `pyn fsck` on a git repository: object validity, connectivity from the refs, and what dangles.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::commands::emit;
use super::fsck_check::{self, Checked, Severity};
use super::storage_links::{Link, links};
use super::storage_roots::{self, Origin};

const ERROR_OBJECT: i32 = 1;
const ERROR_REACHABLE: i32 = 2;
const ERROR_PACK: i32 = 4;
const ERROR_REFS: i32 = 8;

/// Verify the connectivity and validity of the objects in the database.
#[derive(Debug, Args)]
pub struct FsckArgs {
    /// Report objects no ref reaches.
    #[arg(long)]
    unreachable: bool,
    /// Print objects nothing refers to (the default).
    #[arg(long, overrides_with = "no_dangling")]
    dangling: bool,
    #[arg(long, overrides_with = "dangling")]
    no_dangling: bool,
    /// Report root commits.
    #[arg(long)]
    root: bool,
    /// Report tags.
    #[arg(long)]
    tags: bool,
    /// Do not treat reflog entries as roots.
    #[arg(long)]
    no_reflogs: bool,
    /// Treat objects in the index as roots (the default).
    #[arg(long)]
    cache: bool,
    /// Check packs too (the default).
    #[arg(long, overrides_with = "no_full")]
    full: bool,
    #[arg(long, overrides_with = "full")]
    no_full: bool,
    /// Check only connectivity, not object contents.
    #[arg(long)]
    connectivity_only: bool,
    /// Report bad file modes in trees too.
    #[arg(long)]
    strict: bool,
    /// Write dangling objects to `.git/lost-found`.
    #[arg(long)]
    lost_found: bool,
    /// Name objects after the ref, path or ancestry that reaches them.
    #[arg(long, overrides_with = "no_name_objects")]
    name_objects: bool,
    #[arg(long, overrides_with = "name_objects")]
    no_name_objects: bool,
    /// Say what is being checked.
    #[arg(short, long)]
    verbose: bool,
    /// Show progress (the default on a terminal).
    #[arg(long, overrides_with = "no_progress")]
    progress: bool,
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
    /// Start from these objects instead of the refs.
    objects: Vec<String>,
}

struct Info {
    kind: Kind,
    links: Option<Vec<Link>>,
}

struct Fsck<'a> {
    git: &'a GitHome,
    args: &'a FsckArgs,
    status: i32,
    out: String,
    infos: HashMap<ObjectId, Info>,
    paths: HashMap<ObjectId, PathBuf>,
    names: HashMap<ObjectId, String>,
    seen: HashSet<ObjectId>,
    packed: HashSet<ObjectId>,
    loose: HashSet<ObjectId>,
}

/// Where a root is marked in git's order: refs, `HEAD`, reflogs, then the index and its cache-tree.
fn root_rank(origin: &Origin) -> u8 {
    match origin {
        Origin::Ref(n) if n != "HEAD" => 0,
        Origin::Ref(_) => 1,
        Origin::Reflog(n) if n == "HEAD" => 2,
        Origin::Reflog(_) => 3,
        Origin::Index => 4,
        Origin::CacheTree(_) => 5,
    }
}

/// The name git gives a commit's first parent: `x^` becomes `x~2`, `x~n` becomes `x~(n+1)`.
fn first_parent_name(name: &str) -> String {
    if let Some(base) = name.strip_suffix('^') {
        return format!("{base}~2");
    }
    if let Some((base, n)) = name.rsplit_once('~')
        && let Ok(n) = n.parse::<u32>()
    {
        return format!("{base}~{}", n + 1);
    }
    format!("{name}^")
}

/// Whether progress is shown: asked for, or stderr is a terminal and it was not refused.
fn show_progress(args: &FsckArgs) -> bool {
    !args.verbose && (args.progress || !args.no_progress && std::io::stderr().is_terminal())
}

/// The size of git's object hash for `n` objects: a power of two from 32 that stays at most half full.
fn hash_slots(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut size = 32;
    while n > size / 2 {
        size *= 2;
    }
    size
}

fn shown_path(path: &std::path::Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    path.strip_prefix(&cwd)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub(super) fn fsck(git: &GitHome, args: FsckArgs) -> Result<()> {
    let mut f = Fsck {
        git,
        args: &args,
        status: 0,
        out: String::new(),
        infos: HashMap::new(),
        paths: HashMap::new(),
        names: HashMap::new(),
        seen: HashSet::new(),
        packed: HashSet::new(),
        loose: HashSet::new(),
    };
    f.examine_objects()?;
    f.walk_from_roots()?;
    emit(&f.out);
    if f.status != 0 {
        std::process::exit(f.status);
    }
    Ok(())
}

impl Fsck<'_> {
    fn examine_objects(&mut self) -> Result<()> {
        let db = self.git.repo.objects();
        let alg = self.git.algorithm();
        let (loose, _) = db.loose_objects()?;
        if self.args.verbose && !self.args.connectivity_only {
            eprintln!("Checking object directory");
        } else if !self.args.verbose && show_progress(self.args) {
            eprint!("Checking object directories: 100% (256/256)\r");
            eprintln!("Checking object directories: 100% (256/256), done.");
        }
        let mut ids: Vec<ObjectId> = Vec::new();
        for l in &loose {
            self.paths.insert(l.id, l.path.clone());
            self.loose.insert(l.id);
            ids.push(l.id);
        }
        if !self.args.no_full {
            for pack in db.packs() {
                if let Err(e) = pack.verify() {
                    eprintln!("error: {}: {e}", pack.idx_path().display());
                    self.status |= ERROR_PACK;
                }
                for id in pack.ids() {
                    self.paths
                        .entry(id)
                        .or_insert_with(|| pack.pack_path().to_path_buf());
                    self.packed.insert(id);
                    ids.push(id);
                }
            }
        }
        if !self.args.no_full && !self.args.connectivity_only && show_progress(self.args) {
            let packed: usize = db.packs().iter().map(|p| p.ids().count()).sum();
            if packed > 0 {
                eprint!("Checking objects: 100% ({packed}/{packed})\r");
                eprintln!("Checking objects: 100% ({packed}/{packed}), done.");
            }
        }
        ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        ids.dedup();
        self.seen.extend(ids.iter().copied());
        for id in ids {
            let path = shown_path(&self.paths[&id]);
            let object = match db.read_opt(&id) {
                Ok(Some(o)) => o,
                other => {
                    if let Err(e) = &other
                        && e.to_string().contains("does not match its id")
                    {
                        eprintln!("error: {id}: hash-path mismatch, found at: {path}");
                    } else {
                        eprintln!("error: unable to unpack header of {path}");
                        eprintln!("error: {id}: object corrupt or missing: {path}");
                    }
                    self.status |= ERROR_OBJECT;
                    continue;
                }
            };
            if !self.args.connectivity_only {
                if self.args.verbose {
                    eprintln!("Checking {} {id}", object.kind.name());
                }
                let checked: Checked = match object.kind {
                    Kind::Commit => fsck_check::commit(alg, &id, &object.data),
                    Kind::Tree => fsck_check::tree(alg, &object.data, self.args.strict),
                    Kind::Tag => fsck_check::tag(alg, &object.data),
                    Kind::Blob => Ok(Vec::new()),
                };
                match checked {
                    Ok(findings) => {
                        for f in findings {
                            let is_error = match f.severity {
                                Severity::Error => true,
                                Severity::Warning => self.args.strict,
                                Severity::Info => false,
                            };
                            if is_error {
                                self.flag_object(&id);
                            }
                            let word = if is_error { "error" } else { "warning" };
                            eprintln!(
                                "{word} in {} {id}: {}: {}",
                                object.kind.name(),
                                f.id,
                                f.text
                            );
                        }
                    }
                    Err(unparsable) => {
                        for line in unparsable.0 {
                            eprintln!("{line}");
                        }
                        if object.kind == Kind::Tree {
                            eprintln!("error in tree {id}: badTree: cannot be parsed as a tree");
                        } else {
                            eprintln!("error: {id}: object could not be parsed: {path}");
                        }
                        self.flag_object(&id);
                        continue;
                    }
                }
            }
            self.infos.insert(
                id,
                Info {
                    kind: object.kind,
                    links: links(alg, object.kind, &object.data),
                },
            );
        }
        Ok(())
    }

    /// A finding in a packed object is a pack error to git, in a loose one an object error.
    fn flag_object(&mut self, id: &ObjectId) {
        if self.packed.contains(id) {
            self.status |= ERROR_PACK;
        }
        if self.loose.contains(id) || !self.packed.contains(id) {
            self.status |= ERROR_OBJECT;
        }
    }

    fn walk_from_roots(&mut self) -> Result<()> {
        let db = self.git.repo.objects();
        let args = self.args;
        let mut roots: Vec<(ObjectId, Origin, Option<String>)> = Vec::new();
        if args.objects.is_empty() {
            let found = storage_roots::collect(self.git, !args.no_reflogs, true)?;
            if args.verbose {
                for head in &found.head_links {
                    eprintln!("Checking {head} link");
                }
                for (old, new) in &found.reflog_lines {
                    eprintln!("Checking reflog {old}->{new}");
                }
                for r in &found.roots {
                    if let Origin::CacheTree(path) = &r.origin {
                        eprintln!("Checking cache tree of {}", shown_path(path));
                    }
                }
            }
            for (name, hex) in &found.bad_refs {
                eprintln!("error: {name}: invalid sha1 pointer {hex}");
                self.status |= ERROR_REACHABLE;
            }
            if let Some(branch) = &found.unborn_head {
                eprintln!("notice: HEAD points to an unborn branch ({branch})");
            }
            if found.roots.is_empty() && found.bad_refs.is_empty() {
                eprintln!("notice: No default references");
            }
            roots.extend(found.roots.into_iter().map(|r| (r.id, r.origin, r.name)));
            roots.sort_by_key(|(_, o, _)| root_rank(o));
        } else {
            for spec in &args.objects {
                match self.git.repo.resolve(spec) {
                    Ok(id) => roots.push((id, Origin::Ref(spec.clone()), Some(spec.clone()))),
                    Err(_) => {
                        eprintln!("error: invalid parameter: expected sha1, got '{spec}'");
                        self.status |= ERROR_OBJECT;
                    }
                }
            }
        }

        let mut reachable: HashSet<ObjectId> = HashSet::new();
        let mut used: HashSet<ObjectId> = HashSet::new();
        for (id, info) in &self.infos {
            let shallow = info.kind == Kind::Commit && db.is_shallow(id);
            for l in info.links.iter().flatten() {
                if !(shallow && l.kind == Kind::Commit) {
                    used.insert(l.id);
                }
            }
        }
        let mut missing: HashMap<ObjectId, Kind> = HashMap::new();
        let mut stack: Vec<ObjectId> = Vec::new();
        for (id, origin, name) in &roots {
            self.seen.insert(*id);
            if let Some(n) = name
                && (!matches!(origin, Origin::CacheTree(_)) || self.ensure(id))
            {
                self.names.entry(*id).or_insert_with(|| n.clone());
            }
        }
        for (id, origin, _) in &roots {
            if !self.ensure(id) {
                if db.is_promised(id) {
                    continue;
                }
                match origin {
                    Origin::Ref(name) => {
                        eprintln!("error: {name}: invalid sha1 pointer {id}");
                        self.status |= ERROR_REACHABLE;
                    }
                    Origin::Reflog(name) => {
                        eprintln!("error: {name}: invalid reflog entry {id}");
                        self.status |= ERROR_REACHABLE;
                    }
                    Origin::Index => {
                        reachable.insert(*id);
                        missing.insert(*id, Kind::Blob);
                    }
                    Origin::CacheTree(path) => {
                        eprintln!(
                            "error: {id}: invalid sha1 pointer in cache-tree of {}",
                            shown_path(path)
                        );
                        self.status |= ERROR_REFS;
                    }
                }
                continue;
            }
            if reachable.insert(*id) {
                stack.push(*id);
            }
        }
        let mut broken: Vec<(Kind, ObjectId, Kind, ObjectId)> = Vec::new();
        while let Some(id) = stack.pop() {
            let info = &self.infos[&id];
            let parent_kind = info.kind;
            let shallow = parent_kind == Kind::Commit && db.is_shallow(&id);
            let own = self.names.get(&id);
            let mut parents = 0;
            let mut kids: Vec<(Kind, ObjectId, Option<String>)> = Vec::new();
            for l in info.links.iter().flatten() {
                if shallow && l.kind == Kind::Commit {
                    continue;
                }
                let name = own.map(|n| match parent_kind {
                    Kind::Commit if l.kind == Kind::Tree => format!("{n}:"),
                    Kind::Commit => {
                        parents += 1;
                        if parents == 1 {
                            first_parent_name(n)
                        } else {
                            format!("{n}^{parents}")
                        }
                    }
                    Kind::Tree => {
                        let entry = String::from_utf8_lossy(l.name.as_deref().unwrap_or_default());
                        let slash = if l.kind == Kind::Tree { "/" } else { "" };
                        format!("{n}{entry}{slash}")
                    }
                    _ => n.clone(),
                });
                kids.push((l.kind, l.id, name));
            }
            for (kind, child, name) in kids {
                self.seen.insert(child);
                if let Some(n) = name {
                    self.names.entry(child).or_insert(n);
                }
                if !reachable.insert(child) {
                    continue;
                }
                used.insert(child);
                if self.ensure(&child) {
                    stack.push(child);
                } else if !db.is_promised(&child) {
                    broken.push((parent_kind, id, kind, child));
                    missing.insert(child, kind);
                }
            }
        }
        for (fk, fid, tk, tid) in &broken {
            let (from, to) = (self.describe(fid), self.describe(tid));
            writeln!(self.out, "broken link from {:>7} {from}", fk.name())?;
            writeln!(self.out, "              to {:>7} {to}", tk.name())?;
        }
        let mut missing: Vec<_> = missing.into_iter().collect();
        missing.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (id, kind) in &missing {
            writeln!(self.out, "missing {} {}", kind.name(), self.describe(id))?;
            self.status |= ERROR_REACHABLE;
        }

        if args.verbose {
            self.seen.extend(used.iter().copied());
            let mut all: Vec<ObjectId> = self.seen.iter().copied().collect();
            all.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            eprintln!("Checking connectivity ({} objects)", hash_slots(all.len()));
            for id in &all {
                eprintln!("Checking {}", self.describe(id));
            }
        }
        self.report_extras(&reachable, &used)?;
        Ok(())
    }

    fn describe(&self, id: &ObjectId) -> String {
        match self.names.get(id) {
            Some(n) if self.args.name_objects => format!("{id} ({n})"),
            _ => id.to_string(),
        }
    }

    /// Whether `id` can be read, loading it when it lives in an alternate.
    fn ensure(&mut self, id: &ObjectId) -> bool {
        if self.infos.contains_key(id) {
            return true;
        }
        match self.git.repo.objects().read_local_opt(id) {
            Ok(Some(o)) => {
                let l = links(self.git.algorithm(), o.kind, &o.data);
                self.infos.insert(
                    *id,
                    Info {
                        kind: o.kind,
                        links: l,
                    },
                );
                true
            }
            _ => false,
        }
    }

    fn report_extras(
        &mut self,
        reachable: &HashSet<ObjectId>,
        used: &HashSet<ObjectId>,
    ) -> Result<()> {
        let args = self.args;
        let db = self.git.repo.objects();
        let mut ids: Vec<ObjectId> = self.infos.keys().copied().collect();
        ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        if args.root {
            for id in &ids {
                let info = &self.infos[id];
                if info.kind == Kind::Commit
                    && reachable.contains(id)
                    && info.links.iter().flatten().all(|l| l.kind != Kind::Commit)
                {
                    writeln!(self.out, "root {id}")?;
                }
            }
        }
        if args.tags {
            for id in &ids {
                let info = &self.infos[id];
                if info.kind != Kind::Tag || !reachable.contains(id) {
                    continue;
                }
                let Some(target) = info.links.iter().flatten().next() else {
                    continue;
                };
                let body = db.read(id)?.data;
                let name = body
                    .split(|b| *b == b'\n')
                    .find_map(|l| l.strip_prefix(b"tag "))
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default();
                writeln!(
                    self.out,
                    "tagged {} {} ({name}) in {id}",
                    target.kind.name(),
                    target.id
                )?;
            }
        }
        let show_dangling = !args.no_dangling;
        for id in &ids {
            if reachable.contains(id) {
                continue;
            }
            let kind = self.infos[id].kind;
            if args.unreachable {
                writeln!(self.out, "unreachable {} {id}", kind.name())?;
            } else if show_dangling && !used.contains(id) {
                writeln!(self.out, "dangling {} {id}", kind.name())?;
                if args.lost_found {
                    self.lost_found(kind, id)?;
                }
            }
        }
        Ok(())
    }

    fn lost_found(&self, kind: Kind, id: &ObjectId) -> Result<()> {
        let repo = self.git.repo.git();
        let dir = repo
            .common_dir()
            .join("lost-found")
            .join(if kind == Kind::Commit {
                "commit"
            } else {
                "other"
            });
        fs::create_dir_all(&dir)?;
        let content = if kind == Kind::Blob {
            self.git.repo.objects().read(id)?.data
        } else {
            format!("{id}\n").into_bytes()
        };
        fs::write(dir.join(id.to_hex()), content)?;
        Ok(())
    }
}
