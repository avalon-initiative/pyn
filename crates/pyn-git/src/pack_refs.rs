use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;

use pyn_object::Kind;

use crate::error::{Result, io_at};
use crate::odb::ObjectDb;
use crate::refs::{PackedRef, Ref, RefStore, check_name, lock_path, walk};
use crate::walk::peel;

const HEADER: &str = "# pack-refs with: peeled fully-peeled sorted \n";

impl RefStore {
    /// Writes loose refs (all of `refs/`, or only tags) into `packed-refs`, keeping what is already packed.
    /// With `prune` the loose files that were packed are removed. Returns the number of loose refs packed.
    pub fn pack_refs(&self, db: &ObjectDb, all: bool, prune: bool) -> Result<usize> {
        let include: &[&str] = if all { &["refs/*"] } else { &["refs/tags/*"] };
        self.pack_refs_matching(db, include, &[], prune)
    }

    /// Like [`RefStore::pack_refs`] for the loose refs matching a pattern of `include` and none of `exclude`
    /// (`*` also matches `/`).
    pub fn pack_refs_matching(
        &self,
        db: &ObjectDb,
        include: &[&str],
        exclude: &[&str],
        prune: bool,
    ) -> Result<usize> {
        let packed_path = self.common_dir.join("packed-refs");
        let lock = lock_path(&packed_path);
        let mut lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result =
            self.pack_refs_locked(db, (include, exclude), prune, &mut lock_file, &packed_path);
        drop(lock_file);
        let _ = fs::remove_file(&lock);
        result
    }

    fn pack_refs_locked(
        &self,
        db: &ObjectDb,
        (include, exclude): (&[&str], &[&str]),
        prune: bool,
        lock_file: &mut fs::File,
        packed_path: &std::path::Path,
    ) -> Result<usize> {
        let mut merged: BTreeMap<String, PackedRef> = self
            .packed()?
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();
        let mut loose = Vec::new();
        walk(&self.common_dir.join("refs"), "refs", &mut loose)?;
        let mut taken = Vec::new();
        for name in loose {
            if check_name(&name).is_err()
                || self.dir_for(&name) != self.common_dir
                || !include
                    .iter()
                    .any(|p| wildmatch(p.as_bytes(), name.as_bytes()))
                || exclude
                    .iter()
                    .any(|p| wildmatch(p.as_bytes(), name.as_bytes()))
            {
                continue;
            }
            if let Some(Ref::Direct(id)) = self.read_loose_only(&name)? {
                merged.insert(
                    name.clone(),
                    PackedRef {
                        name: name.clone(),
                        id,
                        peeled: None,
                    },
                );
                taken.push((name, id));
            }
        }
        for p in merged.values_mut() {
            let tag = matches!(db.read_opt(&p.id), Ok(Some(o)) if o.kind == Kind::Tag);
            p.peeled = match (tag, p.peeled) {
                (false, _) => None,
                (true, Some(x)) => Some(x),
                (true, None) => peel(db, p.id).ok().map(|(_, target, _)| target),
            };
        }
        let mut text = String::from(HEADER);
        for p in merged.values() {
            text.push_str(&format!("{} {}\n", p.id, p.name));
            if let Some(t) = p.peeled {
                text.push_str(&format!("^{t}\n"));
            }
        }
        let lock = lock_path(packed_path);
        lock_file
            .write_all(text.as_bytes())
            .and_then(|()| lock_file.sync_all())
            .and_then(|()| fs::rename(&lock, packed_path))
            .map_err(io_at(packed_path))?;
        if prune {
            for (name, id) in &taken {
                self.prune_loose(name, id);
            }
        }
        Ok(taken.len())
    }

    fn read_loose_only(&self, name: &str) -> Result<Option<Ref>> {
        let path = self.dir_for(name).join(name);
        match fs::read(&path) {
            Ok(bytes) => self.parse_loose(name, &bytes).map(Some),
            Err(_) => Ok(None),
        }
    }

    fn prune_loose(&self, name: &str, id: &pyn_object::ObjectId) {
        let path = self.dir_for(name).join(name);
        let lock = lock_path(&path);
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .is_err()
        {
            return;
        }
        if matches!(self.read_loose_only(name), Ok(Some(Ref::Direct(cur))) if cur == *id)
            && fs::remove_file(&path).is_ok()
            && let Some(parent) = path.parent()
        {
            self.prune_empty_dirs(&self.common_dir.join("refs"), parent);
        }
        let _ = fs::remove_file(&lock);
    }
}

/// git's `wildmatch` without `WM_PATHNAME`: `*` and `?` match any character, `[...]` a class.
fn wildmatch(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(b'*') => (0..=text.len()).any(|i| wildmatch(&pat[1..], &text[i..])),
        Some(b'?') => !text.is_empty() && wildmatch(&pat[1..], &text[1..]),
        Some(b'[') => {
            let Some(end) = pat[1..]
                .iter()
                .skip(1)
                .position(|b| *b == b']')
                .map(|e| e + 2)
            else {
                return text.first() == Some(&b'[') && wildmatch(&pat[1..], &text[1..]);
            };
            let Some(c) = text.first() else {
                return false;
            };
            let set = &pat[1..end];
            let (negate, set) = match set.first() {
                Some(b'!' | b'^') => (true, &set[1..]),
                _ => (false, set),
            };
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == b'-' {
                    hit |= (set[i]..=set[i + 2]).contains(c);
                    i += 3;
                } else {
                    hit |= set[i] == *c;
                    i += 1;
                }
            }
            hit != negate && wildmatch(&pat[end + 1..], &text[1..])
        }
        Some(b'\\') if pat.len() > 1 => {
            text.first() == Some(&pat[1]) && wildmatch(&pat[2..], &text[1..])
        }
        Some(c) => text.first() == Some(c) && wildmatch(&pat[1..], &text[1..]),
    }
}
