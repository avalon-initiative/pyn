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
        let packed_path = self.common_dir.join("packed-refs");
        let lock = lock_path(&packed_path);
        let mut lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = self.pack_refs_locked(db, all, prune, &mut lock_file, &packed_path);
        drop(lock_file);
        let _ = fs::remove_file(&lock);
        result
    }

    fn pack_refs_locked(
        &self,
        db: &ObjectDb,
        all: bool,
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
                || !(all || name.starts_with("refs/tags/"))
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
        if merged.is_empty() && !packed_path.exists() {
            return Ok(0);
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
