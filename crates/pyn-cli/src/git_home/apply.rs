//! `pyn apply` on a git repository: patches applied to the work tree and optionally the index, with git's hunk
//! matching (no fuzz, an offset search from the recorded line) and its checks before anything is written.
//! Binary patches, `--reject` and context options are not supported.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_git::IndexEntry;
use pyn_object::{FileMode, Kind, ObjectId};

use super::GitHome;
use super::apply_hunk::apply_hunks;
use super::apply_parse::{Binary, Fail, FilePatch, Reader, diag};
use super::apply_ws::summarize;
use super::convert;
use super::plumbing::prefix;
use super::stage::{
    Entries, OpenIndex, entries_of, index_of, put_entry, raw_mode, remove_entry, stat_fields,
};
use super::worktree::{check_writes, remove_entry_file, write_bytes};
use crate::backend::Backend;
use crate::diffstat::{self, rename_name};

pub(super) const APPLY_SETTINGS: [(&str, Option<&str>); 3] = [
    ("apply", Some("whitespace")),
    ("apply", Some("ignorewhitespace")),
    ("core", Some("whitespace")),
];

/// Apply a patch to files and the index.
#[derive(Debug, Args)]
pub struct ApplyArgs {
    /// Show a diffstat of the patch instead of applying it.
    #[arg(long)]
    stat: bool,
    /// Show added and deleted line counts per file instead of applying.
    #[arg(long)]
    numstat: bool,
    /// Show creations, deletions, renames and mode changes instead of applying.
    #[arg(long)]
    summary: bool,
    /// Check that the patch applies and change nothing.
    #[arg(long)]
    check: bool,
    /// Apply, also when `--stat`, `--numstat`, `--summary` or `--check` is given.
    #[arg(long)]
    apply: bool,
    /// Apply to the index and the work tree, which must match the index.
    #[arg(long)]
    index: bool,
    /// Apply to the index only.
    #[arg(long)]
    cached: bool,
    /// Apply the patch in reverse.
    #[arg(short = 'R', long)]
    reverse: bool,
    /// Leading path components to remove from file names (1 if omitted).
    #[arg(short = 'p', value_name = "N")]
    strip: Option<usize>,
    /// Fall back on a three-way merge for files that do not apply.
    #[arg(short = '3', long = "3way")]
    three_way: bool,
    /// What to do about whitespace errors in added lines: `warn` (the default) or `nowarn`.
    #[arg(long, value_name = "ACTION")]
    whitespace: Option<String>,
    /// Do not fail when the input has no patch.
    #[arg(long)]
    allow_empty: bool,
    /// Patch files, `-` for standard input (standard input if omitted).
    patches: Vec<String>,
}

pub(super) struct Options {
    pub strip: usize,
    pub reverse: bool,
    pub stat: bool,
    pub numstat: bool,
    pub summary: bool,
    pub check: bool,
    pub apply: bool,
    pub index: bool,
    pub cached: bool,
    pub three_way: bool,
    pub ws: bool,
    pub allow_empty: bool,
    /// The folder the command runs in, from the root; patches elsewhere are skipped.
    pub prefix: Vec<u8>,
}

/// A file's content after a patch, and the base, ours and theirs blobs when it was merged with conflicts.
type Patched = (Vec<u8>, Option<[ObjectId; 3]>);

/// A file's mode and content as the patches see it.
#[derive(Clone)]
pub(super) struct Slot {
    pub mode: u32,
    pub bytes: Vec<u8>,
    /// Base, ours and theirs blobs when a three-way merge left the file conflicted.
    pub stages: Option<[ObjectId; 3]>,
}

pub(super) struct Session<'g> {
    pub git: &'g GitHome,
    pub opts: Options,
    opened: Option<OpenIndex>,
    index_slots: HashMap<Vec<u8>, (u32, ObjectId)>,
    /// Paths earlier patches of this run touched: `None` once removed.
    pub overlay: BTreeMap<Vec<u8>, Option<Slot>>,
    label: String,
    /// Contents standing in for the work tree and index, to apply patches to a reconstructed base.
    fixed: Option<HashMap<Vec<u8>, Slot>>,
    pub conflicted: bool,
}

/// Stops with a usage status when a setting that changes the result is present, since it is not read.
pub(super) fn refuse_settings(git: &GitHome, command: &str, keys: &[(&str, Option<&str>)]) {
    let found = git.config().entries().iter().find(|e| {
        keys.iter().any(|(section, key)| {
            e.section.eq_ignore_ascii_case(section)
                && key.is_none_or(|k| e.key.eq_ignore_ascii_case(k))
        })
    });
    if let Some(e) = found {
        super::plumbing::usage(&format!(
            "{command} does not read the {}.{} setting yet",
            e.section.to_ascii_lowercase(),
            e.key.to_ascii_lowercase()
        ));
    }
}

fn show(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

pub(super) fn apply(git: &GitHome, cwd: &Path, args: ApplyArgs) -> Result<()> {
    refuse_settings(git, "apply", &APPLY_SETTINGS);
    let listing = args.stat || args.numstat || args.summary;
    let applying = args.apply || !(listing || args.check);
    let ws = match args.whitespace.as_deref() {
        None => applying,
        Some("nowarn") => false,
        Some("warn") => true,
        Some(other) => super::plumbing::usage(&format!(
            "--whitespace={other} is not supported yet; use warn or nowarn"
        )),
    };
    let opts = Options {
        strip: args.strip.unwrap_or(1),
        reverse: args.reverse,
        stat: args.stat,
        numstat: args.numstat,
        summary: args.summary,
        check: args.check,
        apply: applying,
        index: args.index || args.cached || args.three_way,
        cached: args.cached,
        three_way: args.three_way,
        ws,
        allow_empty: args.allow_empty,
        prefix: prefix(git, cwd).unwrap_or_default(),
    };
    let mut session = Session::new(git, opts)?;
    let inputs: Vec<String> = if args.patches.is_empty() {
        vec!["-".into()]
    } else {
        args.patches
    };
    for name in &inputs {
        let (buf, label) = if name == "-" {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            (buf, "<stdin>".to_string())
        } else {
            match std::fs::read(name) {
                Ok(buf) => (buf, name.clone()),
                Err(e) => {
                    diag!("error: can't open patch '{name}': {}", io_message(&e));
                    std::process::exit(128);
                }
            }
        };
        if let Err(Fail(code)) = session.run(&buf, &label) {
            let _ = std::io::Write::flush(&mut std::io::stdout());
            std::process::exit(code);
        }
    }
    Ok(())
}

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".into(),
        _ => e.to_string(),
    }
}

impl<'g> Session<'g> {
    pub fn new(git: &'g GitHome, opts: Options) -> Result<Self> {
        let opened = if opts.index {
            Some(git.open_index()?)
        } else {
            None
        };
        if let Some(o) = &opened
            && o.index.entries.iter().any(|e| e.skip_worktree)
        {
            bail!("this repository uses sparse checkout, which pyn does not support yet");
        }
        let index_slots = opened
            .iter()
            .flat_map(|o| o.index.entries.iter())
            .filter(|e| e.stage == 0)
            .map(|e| (e.path.clone(), (e.mode, e.id)))
            .collect();
        Ok(Self {
            git,
            opts,
            opened,
            index_slots,
            overlay: BTreeMap::new(),
            label: String::new(),
            fixed: None,
            conflicted: false,
        })
    }

    /// Parses `buf`, checks every patch, prints the listings and writes the result.
    pub fn run(&mut self, buf: &[u8], label: &str) -> std::result::Result<(), Fail> {
        self.label = label.to_string();
        let parsed = Reader::new(buf, label, self.opts.strip, self.opts.ws).parse()?;
        let mut list: Vec<FilePatch> = parsed.patches;
        if list.is_empty() {
            if self.opts.allow_empty {
                return Ok(());
            }
            diag!("error: No valid patches in input (allow with \"--allow-empty\")");
            return Err(Fail(128));
        }
        if self.opts.reverse {
            list.iter_mut().for_each(FilePatch::reverse);
            list.reverse();
        }
        if !self.opts.prefix.is_empty() {
            let pre = self.opts.prefix.clone();
            list.retain(|p| p.name().starts_with(&pre));
        }
        if self.opts.check || self.opts.apply {
            for patch in &list {
                self.check_patch(patch)?;
            }
        }
        self.listings(&list);
        if self.opts.apply {
            self.write()?;
        }
        summarize(parsed.ws_errors);
        if self.conflicted {
            return Err(Fail(1));
        }
        Ok(())
    }

    fn quote(&self, name: &[u8]) -> String {
        self.git.quote_diff_path(name)
    }

    fn listings(&self, list: &[FilePatch]) {
        if self.opts.stat {
            self.print_stat(list);
        }
        if self.opts.numstat {
            for p in list {
                let name = self.quote(p.name());
                if p.binary == Binary::No {
                    println!("{}\t{}\t{name}", p.added, p.deleted);
                } else {
                    println!("-\t-\t{name}");
                }
            }
        }
        if self.opts.summary {
            for p in list {
                self.print_summary(p);
            }
        }
    }

    fn print_stat(&self, list: &[FilePatch]) {
        let mut max_len = 0;
        let mut max_change = 0;
        for p in list {
            max_change = max_change.max(p.added + p.deleted);
            for n in [&p.old_name, &p.new_name].into_iter().flatten() {
                max_len = max_len.max(self.quote(n).chars().count());
            }
        }
        let (mut files, mut adds, mut dels) = (0, 0, 0);
        for p in list {
            files += 1;
            adds += p.added;
            dels += p.deleted;
            let mut name = self.quote(p.name());
            let cap = max_len.min(50);
            if name.chars().count() > cap {
                let chars: Vec<char> = name.chars().collect();
                let from = chars.len() + 3 - cap;
                let tail: String = chars[from..].iter().collect();
                let tail = match tail.find('/') {
                    Some(i) => tail[i..].to_string(),
                    None => tail,
                };
                name = format!("...{tail}");
            }
            if p.binary != Binary::No {
                println!(" {name:<cap$} |  Bin");
                continue;
            }
            let mut room = max_change;
            if room + cap > 70 {
                room = 70usize.saturating_sub(cap);
            }
            let (mut add, mut del) = (p.added, p.deleted);
            if max_change > room {
                let scale = |n: usize| (n * room + max_change / 2) / max_change;
                add = scale(add);
                del = scale(del);
            }
            println!(
                " {name:<cap$} |{:>5} {}{}",
                p.added + p.deleted,
                "+".repeat(add),
                "-".repeat(del)
            );
        }
        print!("{}", diffstat::summary(files, adds, dels));
    }

    fn print_summary(&self, p: &FilePatch) {
        let mode_line = |what: &str, mode: u32, name: &[u8]| {
            if mode == 0 {
                println!(" {what} {}", self.quote(name));
            } else {
                println!(" {what} mode {mode:06o} {}", self.quote(name));
            }
        };
        if p.is_new != 0 {
            mode_line("create", p.new_mode, p.name());
        } else if p.is_delete {
            mode_line("delete", p.old_mode, p.name());
        } else {
            if (p.is_rename || p.is_copy)
                && let (Some(o), Some(n)) = (&p.old_name, &p.new_name)
            {
                let word = if p.is_rename { "rename" } else { "copy" };
                println!(" {word} {} ({}%)", rename_name(&show(o), &show(n)), p.score);
            }
            if p.old_mode != 0 && p.new_mode != 0 && p.old_mode != p.new_mode {
                let name = if p.is_rename {
                    String::new()
                } else {
                    format!(" {}", self.quote(p.name()))
                };
                println!(
                    " mode change {:06o} => {:06o}{name}",
                    p.old_mode, p.new_mode
                );
            }
        }
    }

    /// The current state of `name`: an earlier patch's result, the index (`--cached`) or the work tree.
    fn current(&self, name: &[u8]) -> Result<Option<Slot>, Fail> {
        if let Some(slot) = self.overlay.get(name) {
            return Ok(slot.clone());
        }
        if let Some(fixed) = &self.fixed {
            return Ok(fixed.get(name).cloned());
        }
        if self.opts.cached {
            return self.indexed(name);
        }
        let file = pyn_gitwork::fs_path(self.git.repo.root().map_err(internal)?, name);
        let Ok(meta) = std::fs::symlink_metadata(&file) else {
            return Ok(None);
        };
        let mode = match pyn_gitwork::worktree_mode(&meta) {
            Some(FileMode::Symlink) => 0o120000,
            Some(FileMode::Executable) => 0o100755,
            Some(_) => 0o100644,
            None => {
                return Ok(Some(Slot {
                    mode: 0o040000,
                    bytes: Vec::new(),
                    stages: None,
                }));
            }
        };
        let mode = if mode == 0o100755 && !self.git.repo.bool_setting("core.filemode", true) {
            self.index_slots.get(name).map_or(0o100644, |(m, _)| *m)
        } else {
            mode
        };
        let bytes = if mode == 0o120000 {
            super::stage::link_target(&file).map_err(internal)?
        } else {
            std::fs::read(&file).map_err(internal)?
        };
        Ok(Some(Slot {
            mode,
            bytes,
            stages: None,
        }))
    }

    fn indexed(&self, name: &[u8]) -> Result<Option<Slot>, Fail> {
        let Some((mode, id)) = self.index_slots.get(name) else {
            return Ok(None);
        };
        let bytes = if *mode == 0o160000 {
            Vec::new()
        } else {
            self.git.repo.blob(id).map_err(internal)?
        };
        Ok(Some(Slot {
            mode: *mode,
            bytes,
            stages: None,
        }))
    }

    fn exists_in_index(&self, name: &[u8]) -> bool {
        self.index_slots.contains_key(name)
    }

    /// One patch checked against the current state; the result is recorded for the patches after it.
    fn check_patch(&mut self, patch: &FilePatch) -> Result<(), Fail> {
        let label = show(patch.name());
        if patch.binary == Binary::Data {
            diag!("error: pyn apply does not support binary patches yet");
            return Err(Fail(129));
        }
        let old_name = patch.old_name.clone();
        let new_name = patch.new_name.clone().or_else(|| old_name.clone());
        let new_name = new_name.unwrap_or_default();
        let mut old_slot = None;
        if patch.is_new <= 0
            && let Some(old) = &old_name
        {
            old_slot = self.preimage(old, patch)?;
        }
        if patch.binary == Binary::Marker {
            diag!("error: cannot apply binary patch to '{label}' without full index line");
            return Err(Fail(1));
        }
        let created = patch.is_new != 0 || patch.is_rename || patch.is_copy;
        let renamed_over = old_name.as_deref() != Some(new_name.as_slice());
        if (patch.is_new > 0 || renamed_over) && created {
            self.check_to_create(&new_name)?;
        }
        let base: Vec<u8> = old_slot
            .as_ref()
            .map(|s| s.bytes.clone())
            .unwrap_or_default();
        let old_mode = old_slot.as_ref().map(|s| s.mode);
        if [old_mode, Some(patch.new_mode), Some(patch.old_mode)]
            .into_iter()
            .flatten()
            .any(|m| m == 0o160000)
        {
            diag!("error: pyn apply does not support submodule patches yet");
            return Err(Fail(129));
        }
        let name_for_errors = old_name.clone().unwrap_or_else(|| new_name.clone());
        let mut merged = None;
        if self.opts.three_way {
            if let Some(ours) = &old_slot {
                merged = self.three_way_file(patch, ours)?;
            }
            if merged.is_none() {
                diag!("Falling back to direct application...");
            }
        }
        let result = if let Some(merged) = merged {
            merged
        } else if patch.hunks.is_empty() {
            (base, None)
        } else {
            match apply_hunks(&base, patch) {
                Ok(done) => {
                    if self.opts.ws
                        && let Some(line) = done.blank_at_eof
                    {
                        diag!("{}:{line}: new blank line at EOF.\n+", self.label);
                    }
                    (done.bytes, None)
                }
                Err(oldpos) => {
                    diag!("error: patch failed: {}:{oldpos}", show(&name_for_errors));
                    diag!("error: {}: patch does not apply", show(&name_for_errors));
                    return Err(Fail(1));
                }
            }
        };
        let (result, stages) = result;
        if patch.is_delete && !result.is_empty() {
            diag!("error: {label}: removal patch leaves file contents");
            return Err(Fail(1));
        }
        let mode = if patch.new_mode != 0 {
            patch.new_mode
        } else {
            old_mode.unwrap_or(0o100644)
        };
        let mode = if patch.new_mode == 0 && patch.old_mode != 0 && old_mode.is_none() {
            patch.old_mode
        } else {
            mode
        };
        if (patch.is_rename || patch.is_delete)
            && let Some(old) = &old_name
        {
            self.overlay.insert(old.clone(), None);
        }
        if !patch.is_delete {
            self.overlay.insert(
                new_name.clone(),
                Some(Slot {
                    mode,
                    bytes: result,
                    stages,
                }),
            );
        }
        Ok(())
    }

    fn preimage(&self, old: &[u8], patch: &FilePatch) -> Result<Option<Slot>, Fail> {
        let name = show(old);
        let earlier = self.overlay.contains_key(old);
        let check_index = self.opts.index && !self.opts.cached && !earlier && self.fixed.is_none();
        let indexed = if check_index {
            self.indexed(old)?
        } else {
            None
        };
        if check_index && indexed.is_none() {
            diag!("error: {name}: does not exist in index");
            return Err(Fail(1));
        }
        let slot = match (self.current(old)?, indexed) {
            (Some(slot), Some(indexed)) => {
                if indexed.bytes != slot.bytes || indexed.mode & 0o170000 != slot.mode & 0o170000 {
                    diag!("error: {name}: does not match index");
                    return Err(Fail(1));
                }
                slot
            }
            (Some(slot), None) => slot,
            (None, Some(indexed)) => {
                let mode = super::stage::leaf_mode(indexed.mode);
                write_bytes(self.git, old, mode, &indexed.bytes).map_err(internal)?;
                indexed
            }
            (None, None) => {
                if self.opts.cached {
                    diag!("error: {name}: does not exist in index");
                } else {
                    diag!("error: {name}: No such file or directory");
                }
                return Err(Fail(1));
            }
        };
        if slot.mode & 0o170000 == 0o040000 {
            diag!("error: {name}: wrong type");
            return Err(Fail(1));
        }
        if patch.old_mode != 0 && (slot.mode ^ patch.old_mode) & 0o170000 != 0 {
            diag!("error: {name}: wrong type");
            return Err(Fail(1));
        }
        if patch.old_mode != 0 && slot.mode != patch.old_mode {
            diag!(
                "warning: {name} has type {:o}, expected {:o}",
                slot.mode,
                patch.old_mode
            );
        }
        Ok(Some(slot))
    }

    fn check_to_create(&self, name: &[u8]) -> Result<(), Fail> {
        match self.overlay.get(name) {
            Some(None) => return Ok(()),
            Some(Some(_)) => {
                self.exists(name);
                return Err(Fail(1));
            }
            None => {}
        }
        if let Some(fixed) = &self.fixed {
            if fixed.contains_key(name) {
                self.exists(name);
                return Err(Fail(1));
            }
            return Ok(());
        }
        if self.opts.index && self.exists_in_index(name) {
            diag!("error: {}: already exists in index", show(name));
            return Err(Fail(1));
        }
        if self.opts.cached {
            return Ok(());
        }
        let file = pyn_gitwork::fs_path(self.git.repo.root().map_err(internal)?, name);
        if std::fs::symlink_metadata(&file).is_ok() {
            diag!("error: {}: already exists in working directory", show(name));
            return Err(Fail(1));
        }
        Ok(())
    }

    fn exists(&self, name: &[u8]) {
        let place = if self.opts.cached {
            "index"
        } else {
            "working directory"
        };
        diag!("error: {}: already exists in {place}", show(name));
    }

    /// Writes the overlay: removed paths first, then the new contents, to the work tree and the index.
    fn write(&mut self) -> Result<(), Fail> {
        self.write_all().map_err(|e| {
            diag!("error: {e:#}");
            Fail(1)
        })
    }

    fn write_all(&mut self) -> Result<()> {
        let git = self.git;
        check_writes(self.overlay.keys())?;
        if !self.opts.cached {
            convert::refuse_conversions(
                git,
                self.overlay.iter().filter_map(|(p, s)| {
                    s.as_ref()
                        .map(|s| (p.as_slice(), super::stage::leaf_mode(s.mode)))
                }),
            )?;
        }
        let mut entries: Option<Entries> = self.opened.as_ref().map(|o| entries_of(&o.index));
        for (path, slot) in &self.overlay {
            if slot.is_some() {
                continue;
            }
            if !self.opts.cached {
                remove_entry_file(git, path)?;
            }
            if let Some(e) = entries.as_mut() {
                remove_entry(e, path);
            }
        }
        for (path, slot) in &self.overlay {
            let Some(slot) = slot else {
                continue;
            };
            let mode = super::stage::leaf_mode(slot.mode);
            let id = git.repo.objects().write(Kind::Blob, &slot.bytes)?;
            let mut entry = IndexEntry::new(path.clone(), raw_mode(mode), id);
            if !self.opts.cached {
                let meta = write_bytes(git, path, mode, &slot.bytes)?;
                stat_fields(&mut entry, &meta);
            }
            let Some(e) = entries.as_mut() else {
                continue;
            };
            match &slot.stages {
                None => put_entry(e, entry),
                Some(ids) => {
                    remove_entry(e, path);
                    for (n, id) in ids.iter().enumerate() {
                        let mut staged = IndexEntry::new(path.clone(), raw_mode(mode), *id);
                        staged.stage = n as u8 + 1;
                        e.insert((path.clone(), staged.stage), staged);
                    }
                }
            }
        }
        if let (Some(opened), Some(entries)) = (&self.opened, entries) {
            let index = index_of(opened.index.version, entries);
            git.save_index(opened, &index)?;
        }
        for (path, slot) in &self.overlay {
            if slot.as_ref().is_some_and(|s| s.stages.is_some()) {
                self.conflicted = true;
                diag!("U {}", show(path));
            }
        }
        self.overlay.clear();
        Ok(())
    }
}

fn internal(e: impl Into<anyhow::Error>) -> Fail {
    diag!("error: {:#}", e.into());
    Fail(128)
}

/// Conflict markers in git's default style, without the base section.
fn render_two_way(chunks: &[pyn_merge::Chunk<&[u8]>]) -> Vec<u8> {
    use pyn_merge::Chunk;
    let side = |out: &mut Vec<u8>, lines: &[&[u8]]| {
        lines.iter().for_each(|l| out.extend_from_slice(l));
        if lines.last().is_some_and(|l| !l.ends_with(b"\n")) {
            out.push(b'\n');
        }
    };
    let mut out = Vec::new();
    for chunk in chunks {
        match chunk {
            Chunk::Resolved(lines) => lines.iter().for_each(|l| out.extend_from_slice(l)),
            Chunk::Conflict(c) => {
                out.extend_from_slice(b"<<<<<<< ours\n");
                side(&mut out, &c.ours);
                out.extend_from_slice(b"=======\n");
                side(&mut out, &c.theirs);
                out.extend_from_slice(b">>>>>>> theirs\n");
            }
        }
    }
    out
}

impl Session<'_> {
    /// Merges the patch into the file: the patch is applied to the blob its `index` line names and the result merged
    /// with the current content. `None` leaves the file to direct application.
    fn three_way_file(&mut self, patch: &FilePatch, ours: &Slot) -> Result<Option<Patched>, Fail> {
        if patch.is_delete
            || patch.is_new != 0
            || (patch.is_rename && patch.added + patch.deleted == 0)
        {
            return Ok(None);
        }
        let lacking =
            || diag!("error: repository lacks the necessary blob to perform 3-way merge.");
        let Some(prefix) = &patch.old_id else {
            lacking();
            return Ok(None);
        };
        let found = self.git.repo.ids_with_prefix(prefix).map_err(internal)?;
        let [base_id] = found.as_slice() else {
            lacking();
            return Ok(None);
        };
        let Ok(base) = self.git.repo.blob(base_id) else {
            lacking();
            return Ok(None);
        };
        let Ok(post) = apply_hunks(&base, patch) else {
            diag!("error: patch failed: {}", show(patch.name()));
            return Ok(None);
        };
        let style = self
            .git
            .config()
            .get("merge.conflictstyle")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if style == "zdiff3" {
            diag!("error: merge.conflictStyle=zdiff3 is not supported by pyn apply yet");
            return Err(Fail(129));
        }
        let name = show(patch.name());
        let objects = self.git.repo.objects();
        match pyn_merge::merge_text(&base, &ours.bytes, &post.bytes) {
            pyn_merge::Outcome::Clean(bytes) => {
                diag!("Applied patch to '{name}' cleanly.");
                Ok(Some((bytes, None)))
            }
            pyn_merge::Outcome::Conflicted(chunks) => {
                let text = if style == "diff3" {
                    pyn_merge::render_markers(&chunks, "ours", "base", "theirs")
                } else {
                    render_two_way(&chunks)
                };
                let ids = [
                    *base_id,
                    objects.write(Kind::Blob, &ours.bytes).map_err(internal)?,
                    objects.write(Kind::Blob, &post.bytes).map_err(internal)?,
                ];
                diag!("Applied patch to '{name}' with conflicts.");
                Ok(Some((text, Some(ids))))
            }
            pyn_merge::Outcome::BinaryConflict => Ok(None),
        }
    }
}

/// The patches in `buf`, for a caller that needs more than applying them.
pub(super) fn parse_patches(buf: &[u8], strip: usize) -> Result<Vec<FilePatch>, Fail> {
    Ok(Reader::new(buf, "", strip, false).parse()?.patches)
}

/// The patches applied to `base` in memory: the content each touched path ends with, `None` for a removed one.
pub(super) fn patched(
    git: &GitHome,
    base: HashMap<Vec<u8>, Slot>,
    patches: &[FilePatch],
) -> Result<BTreeMap<Vec<u8>, Option<Slot>>, Fail> {
    let opts = Options {
        strip: 1,
        reverse: false,
        stat: false,
        numstat: false,
        summary: false,
        check: false,
        apply: false,
        index: false,
        cached: false,
        three_way: false,
        ws: false,
        allow_empty: false,
        prefix: Vec::new(),
    };
    let mut session = Session::new(git, opts).map_err(internal)?;
    session.fixed = Some(base);
    for patch in patches {
        session.check_patch(patch)?;
    }
    Ok(session.overlay)
}
