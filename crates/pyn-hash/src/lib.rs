//! Canonical hash encodings of revisions and audit events (docs/concepts/integrity.md). Frozen: a change to any byte
//! of an encoding needs a new domain tag, never an edit.

use sha2::{Digest, Sha256};

pub const REVISION_TAG: &str = "pyn-revision-v1";
pub const REVISION_V2_TAG: &str = "pyn-revision-v2";
pub const AUDIT_TAG: &str = "pyn-audit-v1";

/// The fields a revision hash covers. `previous` is the hash of the path's previous revision, `None` for the first.
#[derive(Debug, Clone, Copy)]
pub struct RevisionFields<'a> {
    pub path: &'a str,
    pub content: &'a str,
    pub previous: Option<&'a str>,
    pub author: &'a str,
    pub created_micros: i64,
    pub message: &'a str,
    pub restored_from: Option<u64>,
}

/// The fields a v2 revision hash covers. Object ids are given as an algorithm name and lowercase hex.
#[derive(Debug, Clone, Copy)]
pub struct RevisionV2Fields<'a> {
    pub path: &'a str,
    /// Git's octal file mode: `100644`, `100755` or `120000`.
    pub file_mode: &'a str,
    pub blob_algorithm: &'a str,
    pub blob_id: &'a str,
    pub previous: Option<&'a str>,
    pub user: &'a str,
    pub author_name: &'a str,
    pub author_email: &'a str,
    pub created_micros: i64,
    /// Minutes east of UTC.
    pub author_offset_minutes: i32,
    pub message: &'a str,
    pub restored_from: Option<u64>,
    pub commit_algorithm: &'a str,
    pub commit_id: &'a str,
}

/// The fields an audit event hash covers. `scope` is the log's storage key; `previous` is the preceding event's hash.
#[derive(Debug, Clone, Copy)]
pub struct AuditFields<'a> {
    pub scope: &'a str,
    pub previous: Option<&'a str>,
    pub created_micros: i64,
    pub actor: &'a str,
    pub action: &'a str,
    pub path: Option<&'a str>,
    pub detail: &'a str,
}

struct Encoder(Sha256);

impl Encoder {
    fn new(tag: &str) -> Self {
        let mut e = Self(Sha256::new());
        e.str(tag);
        e
    }

    fn str(&mut self, s: &str) {
        self.0.update((s.len() as u64).to_be_bytes());
        self.0.update(s.as_bytes());
    }

    fn opt_str(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.0.update([1]);
                self.str(s);
            }
            None => self.0.update([0]),
        }
    }

    fn opt_u64(&mut self, n: Option<u64>) {
        match n {
            Some(n) => {
                self.0.update([1]);
                self.0.update(n.to_be_bytes());
            }
            None => self.0.update([0]),
        }
    }

    fn i64(&mut self, n: i64) {
        self.0.update(n.to_be_bytes());
    }

    fn i32(&mut self, n: i32) {
        self.0.update(n.to_be_bytes());
    }

    fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}

pub fn revision_hash(f: &RevisionFields<'_>) -> String {
    let mut e = Encoder::new(REVISION_TAG);
    e.str(f.path);
    e.str(f.content);
    e.opt_str(f.previous);
    e.str(f.author);
    e.i64(f.created_micros);
    e.str(f.message);
    e.opt_u64(f.restored_from);
    e.finish()
}

pub fn revision_hash_v2(f: &RevisionV2Fields<'_>) -> String {
    let mut e = Encoder::new(REVISION_V2_TAG);
    e.str(f.path);
    e.str(f.file_mode);
    e.str(f.blob_algorithm);
    e.str(f.blob_id);
    e.opt_str(f.previous);
    e.str(f.user);
    e.str(f.author_name);
    e.str(f.author_email);
    e.i64(f.created_micros);
    e.i32(f.author_offset_minutes);
    e.str(f.message);
    e.opt_u64(f.restored_from);
    e.str(f.commit_algorithm);
    e.str(f.commit_id);
    e.finish()
}

pub fn audit_hash(f: &AuditFields<'_>) -> String {
    let mut e = Encoder::new(AUDIT_TAG);
    e.str(f.scope);
    e.opt_str(f.previous);
    e.i64(f.created_micros);
    e.str(f.actor);
    e.str(f.action);
    e.opt_str(f.path);
    e.str(f.detail);
    e.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first() -> RevisionFields<'static> {
        RevisionFields {
            path: "Content/a.umap",
            content: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            previous: None,
            author: "alice",
            created_micros: 1_700_000_000_123_456,
            message: "first",
            restored_from: None,
        }
    }

    #[test]
    fn revision_vectors_are_frozen() {
        let a = revision_hash(&first());
        assert_eq!(a, VECTOR_REVISION_1);
        let b = revision_hash(&RevisionFields {
            previous: Some(&a),
            message: "restore",
            restored_from: Some(1),
            ..first()
        });
        assert_eq!(b, VECTOR_REVISION_2);
    }

    const EMPTY_BLOB: &str = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";

    fn first_v2() -> RevisionV2Fields<'static> {
        RevisionV2Fields {
            path: "Content/a.umap",
            file_mode: "100644",
            blob_algorithm: "sha256",
            blob_id: EMPTY_BLOB,
            previous: None,
            user: "alice",
            author_name: "Alice",
            author_email: "alice@example.com",
            created_micros: 1_700_000_000_123_456,
            author_offset_minutes: 0,
            message: "first",
            restored_from: None,
            commit_algorithm: "sha256",
            commit_id: "abababababababababababababababababababababababababababababababab",
        }
    }

    #[test]
    fn revision_v2_vectors_are_frozen() {
        let a = revision_hash_v2(&first_v2());
        assert_eq!(a, VECTOR_REVISION_V2_1);
        let b = revision_hash_v2(&RevisionV2Fields {
            file_mode: "100755",
            previous: Some(&a),
            author_name: "Alice Liddell",
            author_offset_minutes: -330,
            message: "restore",
            restored_from: Some(1),
            commit_id: "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            ..first_v2()
        });
        assert_eq!(b, VECTOR_REVISION_V2_2);
    }

    #[test]
    fn every_v2_field_changes_the_hash() {
        let base = revision_hash_v2(&first_v2());
        let variants = [
            RevisionV2Fields {
                path: "b",
                ..first_v2()
            },
            RevisionV2Fields {
                file_mode: "120000",
                ..first_v2()
            },
            RevisionV2Fields {
                blob_algorithm: "sha1",
                ..first_v2()
            },
            RevisionV2Fields {
                blob_id: "x",
                ..first_v2()
            },
            RevisionV2Fields {
                previous: Some("p"),
                ..first_v2()
            },
            RevisionV2Fields {
                user: "bob",
                ..first_v2()
            },
            RevisionV2Fields {
                author_name: "B",
                ..first_v2()
            },
            RevisionV2Fields {
                author_email: "b@example.com",
                ..first_v2()
            },
            RevisionV2Fields {
                created_micros: 1,
                ..first_v2()
            },
            RevisionV2Fields {
                author_offset_minutes: 60,
                ..first_v2()
            },
            RevisionV2Fields {
                message: "",
                ..first_v2()
            },
            RevisionV2Fields {
                restored_from: Some(0),
                ..first_v2()
            },
            RevisionV2Fields {
                commit_algorithm: "sha1",
                ..first_v2()
            },
            RevisionV2Fields {
                commit_id: "x",
                ..first_v2()
            },
        ];
        for v in variants {
            assert_ne!(revision_hash_v2(&v), base);
        }
    }

    #[test]
    fn v1_and_v2_never_collide_on_equal_fields() {
        assert_ne!(revision_hash(&first()), revision_hash_v2(&first_v2()));
    }

    #[test]
    fn audit_vectors_are_frozen() {
        let a = audit_hash(&AuditFields {
            scope: "alice/game",
            previous: None,
            created_micros: 1_700_000_000_123_456,
            actor: "alice",
            action: "checkout",
            path: Some("Content/a.umap"),
            detail: "lease 8h",
        });
        assert_eq!(a, VECTOR_AUDIT_1);
        let b = audit_hash(&AuditFields {
            scope: "alice/game",
            previous: Some(&a),
            created_micros: 1_700_000_001_000_000,
            actor: "bob",
            action: "repo_updated",
            path: None,
            detail: "",
        });
        assert_eq!(b, VECTOR_AUDIT_2);
    }

    #[test]
    fn every_field_changes_the_hash() {
        let base = revision_hash(&first());
        let variants = [
            RevisionFields {
                path: "b",
                ..first()
            },
            RevisionFields {
                content: "x",
                ..first()
            },
            RevisionFields {
                previous: Some("p"),
                ..first()
            },
            RevisionFields {
                author: "bob",
                ..first()
            },
            RevisionFields {
                created_micros: 1,
                ..first()
            },
            RevisionFields {
                message: "",
                ..first()
            },
            RevisionFields {
                restored_from: Some(0),
                ..first()
            },
        ];
        for v in variants {
            assert_ne!(revision_hash(&v), base);
        }
    }

    #[test]
    fn field_boundaries_cannot_shift() {
        let a = revision_hash(&RevisionFields {
            path: "ab",
            content: "c",
            ..first()
        });
        let b = revision_hash(&RevisionFields {
            path: "a",
            content: "bc",
            ..first()
        });
        assert_ne!(a, b);
    }

    const VECTOR_REVISION_1: &str =
        "9c4caa53e94d5fa8fb261cb765f73725506641df99e225605e83f179ae0d9b52";
    const VECTOR_REVISION_2: &str =
        "858c075aa1d8e6e9d624aaf0c3cdc032c3e4a165a809ff4a38d6f49ba84e0790";
    const VECTOR_REVISION_V2_1: &str =
        "5b9cb79dd9a6deb197f72c532eb669b72d1e0820c6a8cb56bbc5d0fc838d52ed";
    const VECTOR_REVISION_V2_2: &str =
        "3d2323ad77d4713b6667982fe3f18a4613bdd068a56c023af00a61caa7e5bbe8";
    const VECTOR_AUDIT_1: &str = "4faf45d2108cab101ce1e890a016c314cfde33485307de1eace154d4e0edef8a";
    const VECTOR_AUDIT_2: &str = "ffb456bf011e957ddb3aafa9a4b301b822e22a3029cf8a52ee91c432ddfdc6d9";
}
