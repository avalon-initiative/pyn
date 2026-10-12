use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use pyn_object::{Algorithm, ObjectId, Signature};

use crate::error::{Result, corrupt, io_at};
use crate::refs::{RefStore, check_name};

/// One line of a reflog: a ref moving from `old` to `new`, who did it and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    pub old: ObjectId,
    pub new: ObjectId,
    pub who: Signature,
    pub message: String,
}

impl ReflogEntry {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{} {} ", self.old, self.new).as_bytes());
        out.extend_from_slice(&self.who.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(&self.who.email);
        let sign = if self.who.offset_minutes < 0 {
            '-'
        } else {
            '+'
        };
        let abs = self.who.offset_minutes.unsigned_abs();
        out.extend_from_slice(
            format!("> {} {sign}{:02}{:02}", self.who.time, abs / 60, abs % 60).as_bytes(),
        );
        let message = self.message.replace('\n', " ");
        if !message.is_empty() {
            out.push(b'\t');
            out.extend_from_slice(message.trim_end().as_bytes());
        }
        out.push(b'\n');
        out
    }

    fn parse(algorithm: Algorithm, line: &[u8]) -> Result<Self> {
        let bad = || corrupt("reflog", "malformed line");
        let hex_len = algorithm.digest_len() * 2;
        let (head, message) = match line.iter().position(|b| *b == b'\t') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, &line[line.len()..]),
        };
        fn text(b: &[u8]) -> Result<&str> {
            std::str::from_utf8(b).map_err(|_| corrupt("reflog", "malformed line"))
        }
        if head.len() < 2 * hex_len + 2 {
            return Err(bad());
        }
        let old = ObjectId::from_hex(algorithm, text(&head[..hex_len])?).map_err(|_| bad())?;
        let new = ObjectId::from_hex(algorithm, text(&head[hex_len + 1..2 * hex_len + 1])?)
            .map_err(|_| bad())?;
        let who = &head[2 * hex_len + 2..];
        let gt = who.iter().rposition(|b| *b == b'>').ok_or_else(bad)?;
        let lt = who[..gt].iter().position(|b| *b == b'<').ok_or_else(bad)?;
        let tail = text(&who[gt + 1..])?.trim();
        let (time, zone) = tail.split_once(' ').ok_or_else(bad)?;
        let sign = match zone.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return Err(bad()),
        };
        if zone.len() != 5 {
            return Err(bad());
        }
        let hours: i32 = zone[1..3].parse().map_err(|_| bad())?;
        let minutes: i32 = zone[3..5].parse().map_err(|_| bad())?;
        Ok(Self {
            old,
            new,
            who: Signature {
                name: who[..lt].strip_suffix(b" ").unwrap_or(&who[..lt]).to_vec(),
                email: who[lt + 1..gt].to_vec(),
                time: time.parse().map_err(|_| bad())?,
                offset_minutes: sign * (hours * 60 + minutes),
            },
            message: String::from_utf8_lossy(message).into_owned(),
        })
    }
}

impl RefStore {
    fn reflog_path(&self, name: &str) -> PathBuf {
        self.dir_for(name).join("logs").join(name)
    }

    /// Whether `name` has a reflog file.
    pub fn has_reflog(&self, name: &str) -> bool {
        check_name(name).is_ok() && self.reflog_path(name).is_file()
    }

    /// Appends to the reflog of `name` when it exists or `create` is set; returns whether a line was written.
    pub fn append_reflog(&self, name: &str, entry: &ReflogEntry, create: bool) -> Result<bool> {
        check_name(name)?;
        let path = self.reflog_path(name);
        if !path.is_file() && !create {
            return Ok(false);
        }
        let parent = path.parent().expect("reflog path has a parent");
        fs::create_dir_all(parent).map_err(io_at(parent))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_at(&path))?;
        file.write_all(&entry.encode()).map_err(io_at(&path))?;
        Ok(true)
    }

    /// The reflog of `name`, oldest line first; empty when there is none.
    pub fn read_reflog(&self, name: &str) -> Result<Vec<ReflogEntry>> {
        check_name(name)?;
        let path = self.reflog_path(name);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at(&path)(e)),
        };
        bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| ReflogEntry::parse(self.algorithm, l))
            .collect()
    }

    /// Replaces the reflog of `name` with `entries`, or removes the file when there are none.
    pub fn write_reflog(&self, name: &str, entries: &[ReflogEntry]) -> Result<()> {
        check_name(name)?;
        let path = self.reflog_path(name);
        if entries.is_empty() {
            return match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_at(&path)(e)),
            };
        }
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let body: Vec<u8> = entries.iter().flat_map(ReflogEntry::encode).collect();
        let result = file
            .write_all(&body)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&lock, &path));
        if let Err(e) = result {
            let _ = fs::remove_file(&lock);
            return Err(io_at(&path)(e));
        }
        Ok(())
    }

    /// Moves the reflog of `old` to `new`, as a branch rename does.
    pub fn rename_reflog(&self, old: &str, new: &str) -> Result<()> {
        check_name(old)?;
        check_name(new)?;
        let (from, to) = (self.reflog_path(old), self.reflog_path(new));
        if !from.is_file() {
            return Ok(());
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        fs::rename(&from, &to).map_err(io_at(&from))
    }
}
