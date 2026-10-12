//! Revision names: `HEAD`, `@`, branches, tags, full or abbreviated ids, then `~N`, `^` and `^N` steps.

use pyn_object::{Algorithm, Kind, ObjectId};

use crate::error::{Error, Result, io_at};
use crate::repo::Repo;

const MIN_ABBREV: usize = 4;

impl Repo {
    /// The commit a revision names; annotated tags are followed to their commit.
    pub fn resolve(&self, spec: &str) -> Result<ObjectId> {
        let bad = || Error::Revision(format!("bad revision '{spec}'"));
        let split = spec.find(['~', '^']).unwrap_or(spec.len());
        let (base, mut rest) = spec.split_at(split);
        if base.is_empty() && !rest.is_empty() {
            return Err(bad());
        }
        let mut id = self.resolve_base(base).ok_or_else(bad)??;
        id = self.peel(&id)?;
        while let Some(op) = rest.chars().next() {
            rest = &rest[1..];
            let digits = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let n: Option<usize> = rest[..digits].parse().ok();
            rest = &rest[digits..];
            id = match (op, n) {
                ('~', n) => {
                    for _ in 0..n.unwrap_or(1) {
                        id = self.parent(&id, 1, spec)?;
                    }
                    id
                }
                ('^', Some(0)) => id,
                ('^', n) => self.parent(&id, n.unwrap_or(1), spec)?,
                _ => return Err(bad()),
            };
        }
        Ok(id)
    }

    fn parent(&self, id: &ObjectId, nth: usize, spec: &str) -> Result<ObjectId> {
        self.commit(id)?
            .parents
            .get(nth - 1)
            .copied()
            .ok_or_else(|| Error::Revision(format!("bad revision '{spec}'")))
    }

    fn peel(&self, id: &ObjectId) -> Result<ObjectId> {
        let mut current = *id;
        loop {
            let object = self.objects().read(&current)?;
            match object.kind {
                Kind::Commit => return Ok(current),
                Kind::Tag => {
                    current = pyn_object::Tag::parse(self.algorithm(), &object.data)?.object;
                }
                _ => return Err(Error::Revision(format!("{id} is not a commit"))),
            }
        }
    }

    /// `None` when `base` names nothing, so the caller can report the whole revision.
    fn resolve_base(&self, base: &str) -> Option<Result<ObjectId>> {
        let hex_len = self.algorithm().digest_len() * 2;
        if base.len() == hex_len
            && let Ok(id) = ObjectId::from_hex(self.algorithm(), &base.to_ascii_lowercase())
        {
            return Some(Ok(id));
        }
        match self.resolve_ref(base) {
            Ok(Some(id)) => return Some(Ok(id)),
            Ok(None) => {}
            Err(e) => return Some(Err(e)),
        }
        if base.len() >= MIN_ABBREV && base.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self.expand(&base.to_ascii_lowercase());
        }
        None
    }

    fn resolve_ref(&self, name: &str) -> Result<Option<ObjectId>> {
        if name.is_empty() {
            return Ok(None);
        }
        let name = if name == "@" { "HEAD" } else { name };
        let candidates = [
            name.to_string(),
            format!("refs/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ];
        for candidate in candidates {
            if candidate != "HEAD" && !candidate.starts_with("refs/") {
                continue;
            }
            if pyn_git::check_ref_name(&candidate).is_err() && candidate != "HEAD" {
                continue;
            }
            if let Some(id) = self.refs().resolve(&candidate)? {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// The one commit whose id starts with `prefix`.
    fn expand(&self, prefix: &str) -> Option<Result<ObjectId>> {
        let found = match self.ids_with_prefix(prefix) {
            Ok(f) => f,
            Err(e) => return Some(Err(e)),
        };
        let mut commits = Vec::new();
        for id in found {
            match self.objects().read(&id) {
                Ok(o) if o.kind == Kind::Commit || o.kind == Kind::Tag => commits.push(id),
                Ok(_) => {}
                Err(e) => return Some(Err(e.into())),
            }
        }
        match commits.len() {
            0 => None,
            1 => Some(Ok(commits[0])),
            _ => Some(Err(Error::Revision(format!(
                "short object ID {prefix} is ambiguous"
            )))),
        }
    }

    fn ids_with_prefix(&self, prefix: &str) -> Result<Vec<ObjectId>> {
        let algorithm: Algorithm = self.algorithm();
        let mut found = Vec::new();
        for pack in self.objects().packs() {
            found.extend(
                pack.ids()
                    .filter(|id| id.to_hex().starts_with(prefix))
                    .collect::<Vec<_>>(),
            );
        }
        if prefix.len() >= 2 {
            let dir = self.objects().dir().join(&prefix[..2]);
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd {
                    let name = entry.map_err(io_at(&dir))?.file_name();
                    let hex = format!("{}{}", &prefix[..2], name.to_string_lossy());
                    if hex.starts_with(prefix)
                        && let Ok(id) = ObjectId::from_hex(algorithm, &hex)
                    {
                        found.push(id);
                    }
                }
            }
        }
        found.sort_by_key(|id| id.to_hex());
        found.dedup();
        Ok(found)
    }
}
