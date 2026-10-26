//! `pyn fsck` on a git repository: object validity, connectivity from the refs, and what dangles.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
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
    /// Accepted; objects are not named.
    #[arg(long)]
    name_objects: bool,
    #[arg(short, long)]
    verbose: bool,
    #[arg(long)]
    progress: bool,
    #[arg(long)]
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
        let mut ids: Vec<ObjectId> = Vec::new();
        for l in &loose {
            self.paths.insert(l.id, l.path.clone());
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
                    ids.push(id);
                }
            }
        }
        ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        ids.dedup();
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
                                self.status |= ERROR_OBJECT;
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
                        self.status |= ERROR_OBJECT;
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

    fn walk_from_roots(&mut self) -> Result<()> {
        let db = self.git.repo.objects();
        let args = self.args;
        let mut roots: Vec<(ObjectId, Origin)> = Vec::new();
        if args.objects.is_empty() {
            let found = storage_roots::collect(self.git, !args.no_reflogs, true)?;
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
            roots.extend(found.roots.into_iter().map(|r| (r.id, r.origin)));
        } else {
            for spec in &args.objects {
                match self.git.repo.resolve(spec) {
                    Ok(id) => roots.push((id, Origin::Ref(spec.clone()))),
                    Err(_) => {
                        eprintln!("error: invalid parameter: expected sha1, got '{spec}'");
                        self.status |= ERROR_OBJECT;
                    }
                }
            }
        }

        let mut reachable: HashSet<ObjectId> = HashSet::new();
        let mut used: HashSet<ObjectId> = HashSet::new();
        for info in self.infos.values() {
            for l in info.links.iter().flatten() {
                used.insert(l.id);
            }
        }
        let mut missing: HashMap<ObjectId, Kind> = HashMap::new();
        let mut stack: Vec<ObjectId> = Vec::new();
        for (id, origin) in &roots {
            if !self.ensure(id) {
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
            let kids: Vec<(Kind, ObjectId)> = info
                .links
                .iter()
                .flatten()
                .filter(|l| !(shallow && l.kind == Kind::Commit))
                .map(|l| (l.kind, l.id))
                .collect();
            for (kind, child) in kids {
                if !reachable.insert(child) {
                    continue;
                }
                used.insert(child);
                if self.ensure(&child) {
                    stack.push(child);
                } else {
                    broken.push((parent_kind, id, kind, child));
                    missing.insert(child, kind);
                }
            }
        }
        for (fk, fid, tk, tid) in &broken {
            writeln!(self.out, "broken link from {:>7} {fid}", fk.name())?;
            writeln!(self.out, "              to {:>7} {tid}", tk.name())?;
        }
        let mut missing: Vec<_> = missing.into_iter().collect();
        missing.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (id, kind) in &missing {
            writeln!(self.out, "missing {} {id}", kind.name())?;
            self.status |= ERROR_REACHABLE;
        }

        self.report_extras(&reachable, &used)?;
        Ok(())
    }

    /// Whether `id` can be read, loading it when it lives in an alternate.
    fn ensure(&mut self, id: &ObjectId) -> bool {
        if self.infos.contains_key(id) {
            return true;
        }
        match self.git.repo.objects().read_opt(id) {
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
