use chrono::{DateTime, SecondsFormat, Utc};

use crate::error::{PynError, Result};
use crate::types::{RepoPath, Revision, RevisionId};

pub const HISTORY_DEFAULT_LIMIT: usize = 50;
pub const HISTORY_MAX_LIMIT: usize = 200;

/// Position in repository history, newest first, ordered by (created_at, path, id) because revision ids are per path.
/// The string form is opaque to clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCursor {
    pub created_at: DateTime<Utc>,
    pub path: RepoPath,
    pub id: RevisionId,
}

impl HistoryCursor {
    pub fn of(rev: &Revision) -> Self {
        Self {
            created_at: rev.created_at,
            path: rev.path.clone(),
            id: rev.id,
        }
    }

    pub(crate) fn sort_key(&self) -> (DateTime<Utc>, &str, RevisionId) {
        (self.created_at, self.path.as_str(), self.id)
    }

    pub fn encode(&self) -> String {
        let at = self.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true);
        format!("{at}|{}|{}", self.id, self.path)
    }

    pub fn decode(s: &str) -> Result<Self> {
        let bad = || PynError::InvalidRequest(format!("invalid history cursor {s:?}"));
        let mut parts = s.splitn(3, '|');
        let (at, id, path) = (parts.next(), parts.next(), parts.next());
        let (Some(at), Some(id), Some(path)) = (at, id, path) else {
            return Err(bad());
        };
        Ok(Self {
            created_at: DateTime::parse_from_rfc3339(at)
                .map_err(|_| bad())?
                .with_timezone(&Utc),
            id: RevisionId(id.parse().map_err(|_| bad())?),
            path: RepoPath::new(path).map_err(|_| bad())?,
        })
    }
}

/// One page of repository history, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub revisions: Vec<Revision>,
    /// Pass back as `before` for the next, older page; `None` on the last page.
    pub next: Option<HistoryCursor>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_including_odd_paths() {
        let cursor = HistoryCursor {
            created_at: "2026-10-08T09:00:00.123456789Z".parse().unwrap(),
            path: RepoPath::new("a|b/c d.txt").unwrap(),
            id: RevisionId(7),
        };
        assert_eq!(HistoryCursor::decode(&cursor.encode()).unwrap(), cursor);
    }

    #[test]
    fn malformed_cursors_are_invalid_requests() {
        for bad in [
            "",
            "x",
            "2026-10-08T09:00:00Z|1",
            "nope|1|a",
            "2026-10-08T09:00:00Z|x|a",
            "2026-10-08T09:00:00Z|1|/a",
        ] {
            let err = HistoryCursor::decode(bad).unwrap_err();
            assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}");
        }
    }
}
