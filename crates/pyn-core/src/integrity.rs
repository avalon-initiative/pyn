//! Hashes of revisions and audit events and their verification; the encoding is in `pyn-hash`.

use chrono::{DateTime, Utc};
use pyn_hash::{
    AuditFields, RevisionFields, RevisionV2Fields, audit_hash, revision_hash, revision_hash_v2,
};
use pyn_object::ObjectId;

use crate::audit::{AuditEvent, AuditScope, NewAuditEvent};
use crate::error::{PynError, Result};
use crate::types::{NewRevision, RepoPath, Revision, RevisionHash, RevisionId};

fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

impl NewRevision {
    /// The revision this becomes as revision `id` of its path after `previous`, in `commit`; always the v2 encoding.
    pub fn create(
        self,
        id: RevisionId,
        previous: Option<RevisionHash>,
        commit: ObjectId,
    ) -> Revision {
        let mut rev = Revision {
            id,
            path: self.path,
            content: self.content,
            legacy_content: None,
            author: self.author,
            person: Some(self.person),
            message: self.message,
            created_at: self.created_at,
            restored_from: self.restored_from,
            mode: Some(self.mode),
            file_mode: self.file_mode,
            changeset: self.changeset,
            commit,
            hash: RevisionHash::new(""),
            previous,
        };
        rev.hash = rev.expected_hash();
        rev
    }
}

impl Revision {
    /// The hash its own fields and `previous` give, by the encoding its version uses; equals `hash` unless the
    /// record was altered.
    pub fn expected_hash(&self) -> RevisionHash {
        let previous = self.previous.as_ref().map(RevisionHash::as_str);
        let hash = match &self.person {
            Some(person) => revision_hash_v2(&RevisionV2Fields {
                path: self.path.as_str(),
                file_mode: self.file_mode.as_str(),
                blob_algorithm: self.content.algorithm().name(),
                blob_id: &self.content.to_hex(),
                previous,
                user: self.author.as_str(),
                author_name: &person.name,
                author_email: &person.email,
                created_micros: micros(self.created_at),
                author_offset_minutes: person.offset_minutes,
                message: &self.message,
                restored_from: self.restored_from.map(|r| r.0),
                commit_algorithm: self.commit.algorithm().name(),
                commit_id: &self.commit.to_hex(),
            }),
            None => {
                let content = self
                    .legacy_content
                    .clone()
                    .unwrap_or_else(|| self.content.to_hex());
                revision_hash(&RevisionFields {
                    path: self.path.as_str(),
                    content: &content,
                    previous,
                    author: self.author.as_str(),
                    created_micros: micros(self.created_at),
                    message: &self.message,
                    restored_from: self.restored_from.map(|r| r.0),
                })
            }
        };
        RevisionHash::new(hash)
    }

    pub fn verify(&self) -> Result<()> {
        if self.expected_hash() == self.hash {
            Ok(())
        } else {
            Err(PynError::HistoryCorrupt(format!(
                "{}@r{} does not match its hash",
                self.path, self.id
            )))
        }
    }
}

/// What is wrong with a stored revision or event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakKind {
    /// The record's fields do not give its hash.
    Hash,
    /// `previous` is not the hash of the record before it, or the first record has one.
    Link,
    /// A commit's tree is not the tree of the repository state it names.
    Tree,
}

impl BreakKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hash => "hash_mismatch",
            Self::Link => "broken_link",
            Self::Tree => "tree_mismatch",
        }
    }
}

/// Every break in one path's revisions, oldest first; a missing revision shows as a broken link.
pub fn verify_revisions(revisions: &[Revision]) -> Vec<(RevisionId, BreakKind)> {
    let mut breaks = Vec::new();
    let mut prior: Option<&Revision> = None;
    for r in revisions {
        if r.expected_hash() != r.hash {
            breaks.push((r.id, BreakKind::Hash));
        }
        let linked = match prior {
            None => r.previous.is_none() && r.id.0 == 1,
            Some(p) => r.previous.as_ref() == Some(&p.hash) && r.id.0 == p.id.0 + 1,
        };
        if !linked {
            breaks.push((r.id, BreakKind::Link));
        }
        prior = Some(r);
    }
    breaks
}

impl NewAuditEvent {
    pub fn hash(&self, scope: &AuditScope, previous: Option<&str>) -> String {
        audit_hash(&AuditFields {
            scope: &scope.key(),
            previous,
            created_micros: micros(self.at),
            actor: self.actor.as_str(),
            action: self.action.as_str(),
            path: self.path.as_ref().map(RepoPath::as_str),
            detail: &self.detail,
        })
    }
}

impl AuditEvent {
    pub fn expected_hash(&self, scope: &AuditScope) -> String {
        audit_hash(&AuditFields {
            scope: &scope.key(),
            previous: self.previous.as_deref(),
            created_micros: micros(self.at),
            actor: self.actor.as_str(),
            action: self.action.as_str(),
            path: self.path.as_ref().map(RepoPath::as_str),
            detail: &self.detail,
        })
    }

    pub fn verify(&self, scope: &AuditScope) -> Result<()> {
        if self.expected_hash(scope) == self.hash {
            Ok(())
        } else {
            Err(PynError::HistoryCorrupt(format!(
                "audit event {} does not match its hash",
                self.id
            )))
        }
    }
}

/// Every break in one audit log, oldest first.
pub fn verify_audit(scope: &AuditScope, events: &[AuditEvent]) -> Vec<(i64, BreakKind)> {
    let mut breaks = Vec::new();
    let mut prior: Option<&AuditEvent> = None;
    for e in events {
        if e.expected_hash(scope) != e.hash {
            breaks.push((e.id, BreakKind::Hash));
        }
        let linked = e.previous.as_deref() == prior.map(|p| p.hash.as_str());
        if !linked {
            breaks.push((e.id, BreakKind::Link));
        }
        prior = Some(e);
    }
    breaks
}

/// The events unchanged if each matches its own hash, else the first mismatch as an error.
pub fn check_events(scope: &AuditScope, events: Vec<AuditEvent>) -> Result<Vec<AuditEvent>> {
    events.iter().try_for_each(|e| e.verify(scope))?;
    Ok(events)
}

/// One break found by a verification run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: BreakKind,
    pub path: Option<RepoPath>,
    pub revision: Option<RevisionId>,
    pub event: Option<i64>,
    pub commit: Option<ObjectId>,
}

/// The result of verifying a path, or a repository's revisions and audit log. `audit_events` is `None` when the log was not checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    pub paths: u64,
    pub revisions: u64,
    pub commits: u64,
    pub audit_events: Option<u64>,
    pub problems: Vec<Problem>,
}
