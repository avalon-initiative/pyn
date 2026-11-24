//! The in-memory pull request store.

use super::*;
use crate::pull::{
    NewPull, PullEdit, PullEvent, PullEventKind, PullFilter, PullRequest, PullState, PullStore,
    PullTransition,
};

#[derive(Default, Clone)]
pub(super) struct PullData {
    pulls: Vec<PullRequest>,
    events: Vec<PullEvent>,
    next_event: u64,
}

impl PullData {
    fn find(&mut self, number: u64) -> Result<&mut PullRequest> {
        self.pulls
            .iter_mut()
            .find(|p| p.number == number)
            .ok_or(PynError::PullRequestNotFound(number))
    }

    fn comments(&self, number: u64) -> u32 {
        self.events
            .iter()
            .filter(|e| e.number == number && e.kind == PullEventKind::Comment)
            .count() as u32
    }

    fn view(&self, p: &PullRequest) -> PullRequest {
        PullRequest {
            comments: self.comments(p.number),
            ..p.clone()
        }
    }

    fn push_event(
        &mut self,
        number: u64,
        kind: PullEventKind,
        actor: &UserId,
        at: DateTime<Utc>,
        body: Option<String>,
        commit: Option<ObjectId>,
    ) -> PullEvent {
        self.next_event += 1;
        let event = PullEvent {
            id: self.next_event,
            number,
            kind,
            actor: actor.clone(),
            at,
            body,
            commit,
        };
        self.events.push(event.clone());
        event
    }

    fn open_twin(&self, base: &str, head: &str, except: u64) -> Option<u64> {
        self.pulls
            .iter()
            .find(|p| {
                p.state == PullState::Open && p.base == base && p.head == head && p.number != except
            })
            .map(|p| p.number)
    }
}

#[async_trait::async_trait]
impl PullStore for MemoryMetadataStore {
    async fn create_pull(&self, repo: &RepoId, new: NewPull) -> Result<PullRequest> {
        let mut st = self.state.lock().unwrap();
        let data = st.pulls.entry(repo.clone()).or_default();
        if let Some(n) = data.open_twin(&new.base, &new.head, 0) {
            return Err(PynError::PullRequestExists(n));
        }
        let pull = PullRequest {
            number: data.pulls.iter().map(|p| p.number).max().unwrap_or(0) + 1,
            title: new.title,
            body: new.body,
            state: PullState::Open,
            draft: new.draft,
            base: new.base,
            head: new.head,
            author: new.author,
            created_at: new.now,
            updated_at: new.now,
            closed_at: None,
            merged_at: None,
            merged_by: None,
            merge_commit: None,
            frozen: None,
            comments: 0,
        };
        data.pulls.push(pull.clone());
        Ok(pull)
    }

    async fn get_pull(&self, repo: &RepoId, number: u64) -> Result<Option<PullRequest>> {
        let st = self.state.lock().unwrap();
        Ok(st.pulls.get(repo).and_then(|d| {
            d.pulls
                .iter()
                .find(|p| p.number == number)
                .map(|p| d.view(p))
        }))
    }

    async fn list_pulls(
        &self,
        repo: &RepoId,
        filter: &PullFilter,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullRequest>> {
        let st = self.state.lock().unwrap();
        let Some(d) = st.pulls.get(repo) else {
            return Ok(Vec::new());
        };
        let mut found: Vec<_> = d
            .pulls
            .iter()
            .filter(|p| {
                before.is_none_or(|b| p.number < b)
                    && filter.state.is_none_or(|s| p.state == s)
                    && filter.base.as_ref().is_none_or(|b| &p.base == b)
                    && filter.head.as_ref().is_none_or(|h| &p.head == h)
                    && filter.author.as_ref().is_none_or(|a| &p.author == a)
            })
            .collect();
        found.sort_by_key(|p| std::cmp::Reverse(p.number));
        Ok(found.into_iter().take(limit).map(|p| d.view(p)).collect())
    }

    async fn edit_pull(
        &self,
        repo: &RepoId,
        number: u64,
        edit: PullEdit,
        now: DateTime<Utc>,
    ) -> Result<PullRequest> {
        let mut st = self.state.lock().unwrap();
        let data = st.pulls.entry(repo.clone()).or_default();
        let p = data.find(number)?;
        if p.state != PullState::Open {
            return Err(PynError::PullRequestNotOpen {
                number,
                state: p.state.as_str().into(),
            });
        }
        if let Some(t) = edit.title {
            p.title = t;
        }
        if let Some(b) = edit.body {
            p.body = b;
        }
        if let Some(d) = edit.draft {
            p.draft = d;
        }
        p.updated_at = now;
        let p = p.clone();
        Ok(data.view(&p))
    }

    async fn transition_pull(
        &self,
        repo: &RepoId,
        number: u64,
        transition: PullTransition,
        actor: &UserId,
        now: DateTime<Utc>,
    ) -> Result<PullRequest> {
        let mut st = self.state.lock().unwrap();
        let data = st.pulls.entry(repo.clone()).or_default();
        let (state, base, head) = {
            let p = data.find(number)?;
            (p.state, p.base.clone(), p.head.clone())
        };
        let refuse = || PynError::PullRequestNotOpen {
            number,
            state: state.as_str().into(),
        };
        let (kind, commit) = match &transition {
            PullTransition::Close { .. } if state == PullState::Open => {
                (PullEventKind::Closed, None)
            }
            PullTransition::Reopen if state == PullState::Closed => {
                if let Some(n) = data.open_twin(&base, &head, number) {
                    return Err(PynError::PullRequestExists(n));
                }
                (PullEventKind::Reopened, None)
            }
            PullTransition::Merge { commit, .. } if state != PullState::Merged => {
                (PullEventKind::Merged, *commit)
            }
            _ => return Err(refuse()),
        };
        let p = data.find(number)?;
        p.updated_at = now;
        match transition {
            PullTransition::Close { base, head } => {
                p.state = PullState::Closed;
                p.closed_at = Some(now);
                p.frozen = Some((base, head));
            }
            PullTransition::Reopen => {
                p.state = PullState::Open;
                p.closed_at = None;
                p.frozen = None;
            }
            PullTransition::Merge { base, head, commit } => {
                p.state = PullState::Merged;
                p.closed_at = Some(now);
                p.merged_at = Some(now);
                p.merged_by = Some(actor.clone());
                p.merge_commit = commit;
                p.frozen = Some((base, head));
            }
        }
        let p = p.clone();
        data.push_event(number, kind, actor, now, None, commit);
        Ok(data.view(&p))
    }

    async fn comment_pull(
        &self,
        repo: &RepoId,
        number: u64,
        actor: &UserId,
        body: &str,
        now: DateTime<Utc>,
    ) -> Result<PullEvent> {
        let mut st = self.state.lock().unwrap();
        let data = st.pulls.entry(repo.clone()).or_default();
        data.find(number)?.updated_at = now;
        Ok(data.push_event(
            number,
            PullEventKind::Comment,
            actor,
            now,
            Some(body.to_string()),
            None,
        ))
    }

    async fn pull_events(
        &self,
        repo: &RepoId,
        number: u64,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullEvent>> {
        let st = self.state.lock().unwrap();
        let Some(d) = st.pulls.get(repo) else {
            return Ok(Vec::new());
        };
        Ok(d.events
            .iter()
            .filter(|e| e.number == number && after.is_none_or(|a| e.id > a))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn rename_pull_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(d) = st.pulls.get_mut(repo) {
            for p in &mut d.pulls {
                if p.base == from {
                    p.base = to.to_string();
                }
                if p.head == from {
                    p.head = to.to_string();
                }
            }
        }
        Ok(())
    }
}
