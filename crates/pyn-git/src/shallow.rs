use std::collections::HashSet;
use std::path::Path;

use pyn_object::{Algorithm, ObjectId};

use crate::error::{Result, corrupt, io_at};

/// The commits listed in `<common_dir>/shallow`; their parents are treated as absent. Empty when there is no file.
pub fn read_shallow(common_dir: &Path, algorithm: Algorithm) -> Result<HashSet<ObjectId>> {
    let path = common_dir.join("shallow");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(io_at(&path)(e)),
    };
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            ObjectId::from_hex(algorithm, l.trim())
                .map_err(|_| corrupt("shallow file", format!("bad object id {l:?}")))
        })
        .collect()
}

/// Rewrites `shallow` sorted, or removes it when `ids` is empty.
pub fn write_shallow(common_dir: &Path, ids: &HashSet<ObjectId>) -> Result<()> {
    let path = common_dir.join("shallow");
    if ids.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_at(&path)(e)),
            _ => Ok(()),
        };
    }
    let mut hex: Vec<String> = ids.iter().map(ObjectId::to_hex).collect();
    hex.sort();
    let tmp = common_dir.join("shallow.lock");
    std::fs::write(&tmp, hex.join("\n") + "\n").map_err(io_at(&tmp))?;
    std::fs::rename(&tmp, &path).map_err(io_at(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_sorted_and_disappears_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let id = |b: u8| ObjectId::from_bytes(Algorithm::Sha1, &[b; 20]).unwrap();
        assert!(
            read_shallow(dir.path(), Algorithm::Sha1)
                .unwrap()
                .is_empty()
        );
        let ids: HashSet<_> = [id(9), id(1)].into();
        write_shallow(dir.path(), &ids).unwrap();
        let text = std::fs::read_to_string(dir.path().join("shallow")).unwrap();
        assert_eq!(text, format!("{}\n{}\n", id(1), id(9)));
        assert_eq!(read_shallow(dir.path(), Algorithm::Sha1).unwrap(), ids);
        write_shallow(dir.path(), &HashSet::new()).unwrap();
        assert!(!dir.path().join("shallow").exists());
    }
}
