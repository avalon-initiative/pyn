//! Verification in bounded steps: each step checks a share of the repository and says where to continue.

use std::collections::HashSet;

use pyn_object::ObjectId;

use super::RepoService;
use crate::audit::{AuditQuery, AuditScope};
use crate::commit_record::{TreeChange, tree_of};
use crate::error::{PynError, Result};
use crate::integrity::{BreakKind, Problem, VerifyReport, verify_audit_run};
use crate::rules::Mode;
use crate::types::{Commit, RepoPath, Revision, RevisionId};

/// Revisions, commits, audit events and tree entries one verification step examines.
pub const VERIFY_BUDGET: usize = 20_000;
const HEAD_PAGE: usize = 100;
const COMMIT_PAGE: usize = 200;
const SNAPSHOT_PAGE: usize = 5_000;

/// Where a step stops and the next one starts. The string form is opaque to clients.
enum Position {
    /// Paths sorting after this one, or from the first.
    Paths(Option<RepoPath>),
    /// This path from this revision.
    InPath(RepoPath, RevisionId),
    /// Commits older than this one, or from the newest.
    Commits(Option<ObjectId>),
    /// The branch at this position in the branch list.
    Trees(usize),
    /// Audit events older than this id, or from the newest.
    Audit(Option<i64>),
}

impl Position {
    fn encode(&self) -> String {
        match self {
            Self::Paths(after) => format!("h:{}", after.as_ref().map_or("", RepoPath::as_str)),
            Self::InPath(path, from) => format!("r:{}:{path}", from.0),
            Self::Commits(before) => {
                format!("c:{}", before.map(|b| b.to_hex()).unwrap_or_default())
            }
            Self::Trees(index) => format!("t:{index}"),
            Self::Audit(before) => {
                format!("a:{}", before.map(|b| b.to_string()).unwrap_or_default())
            }
        }
    }

    fn decode(s: &str) -> Result<Self> {
        let bad = || PynError::InvalidRequest(format!("invalid verify cursor {s:?}"));
        let (kind, rest) = s.split_once(':').ok_or_else(bad)?;
        fn optional(rest: &str) -> Option<&str> {
            (!rest.is_empty()).then_some(rest)
        }
        Ok(match kind {
            "h" => Self::Paths(
                optional(rest)
                    .map(RepoPath::from_stored)
                    .transpose()
                    .map_err(|_| bad())?,
            ),
            "r" => {
                let (from, path) = rest.split_once(':').ok_or_else(bad)?;
                let from = from
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= i64::MAX as u64);
                Self::InPath(
                    RepoPath::from_stored(path).map_err(|_| bad())?,
                    RevisionId(from.ok_or_else(bad)?),
                )
            }
            "c" => Self::Commits(
                optional(rest)
                    .map(|h| ObjectId::parse(h).map_err(|_| bad()))
                    .transpose()?,
            ),
            "t" => Self::Trees(rest.parse().map_err(|_| bad())?),
            "a" => Self::Audit(
                optional(rest)
                    .map(|n| n.parse::<i64>().ok().filter(|n| *n > 0).ok_or_else(bad))
                    .transpose()?,
            ),
            _ => return Err(bad()),
        })
    }
}

/// Work still allowed in one step.
struct Budget(usize);

impl Budget {
    fn spend(&mut self, n: usize) {
        self.0 = self.0.saturating_sub(n);
    }

    fn spent(&self) -> bool {
        self.0 == 0
    }

    /// A page size no larger than `cap` or what remains.
    fn page(&self, cap: usize) -> usize {
        self.0.clamp(1, cap)
    }
}

impl RepoService {
    /// Checks every path's revision chain, the commits, each branch head's tree and, when `audit` is set, the audit log,
    /// reporting all breaks instead of stopping at the first. With `path`, only that path.
    pub async fn verify(&self, path: Option<&RepoPath>, audit: bool) -> Result<VerifyReport> {
        let mut total = VerifyReport::default();
        let mut cursor = None;
        loop {
            let step = self
                .verify_step(path, audit, cursor.as_deref(), VERIFY_BUDGET)
                .await?;
            let next = step.next.clone();
            total.merge(step);
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(total),
            }
        }
    }

    /// One step of `verify`: it examines about `budget` records and returns `next` to continue with, or none when the
    /// work is done. `InvalidRequest` for a `cursor` that does not parse or does not belong to `path`.
    pub async fn verify_step(
        &self,
        path: Option<&RepoPath>,
        audit: bool,
        cursor: Option<&str>,
        budget: usize,
    ) -> Result<VerifyReport> {
        let position = match (cursor.map(Position::decode).transpose()?, path) {
            (None, Some(p)) => Position::InPath(p.clone(), RevisionId(1)),
            (None, None) => Position::Paths(None),
            (Some(Position::InPath(p, from)), Some(only)) if &p == only => {
                Position::InPath(p, from)
            }
            (Some(_), Some(_)) => {
                return Err(PynError::InvalidRequest(
                    "the cursor does not belong to this path".into(),
                ));
            }
            (Some(position), None) => position,
        };
        let mut report = VerifyReport::default();
        let mut budget = Budget(budget.max(1));
        let mut position = Some(position);
        while let Some(at) = position.take() {
            position = match at {
                Position::Paths(after) => {
                    self.verify_paths(after, &mut report, &mut budget).await?
                }
                Position::InPath(p, from) => {
                    let stop = self.verify_path(&p, from, &mut report, &mut budget).await?;
                    match (stop, path) {
                        (Some(at), _) => Some(at),
                        (None, Some(_)) => None,
                        (None, None) => Some(Position::Paths(Some(p))),
                    }
                }
                Position::Commits(before) => {
                    self.verify_commits(before, &mut report, &mut budget)
                        .await?
                }
                Position::Trees(index) => {
                    self.verify_trees(index, &mut report, &mut budget).await?
                }
                Position::Audit(before) if audit => {
                    self.verify_audit_log(before, &mut report, &mut budget)
                        .await?
                }
                Position::Audit(_) => None,
            };
            if budget.spent() && position.is_some() {
                break;
            }
        }
        report.next = position.map(|p| p.encode());
        Ok(report)
    }

    async fn verify_paths(
        &self,
        mut after: Option<RepoPath>,
        report: &mut VerifyReport,
        budget: &mut Budget,
    ) -> Result<Option<Position>> {
        loop {
            let heads = self
                .meta
                .list_head_revisions(&self.repo, after.as_ref(), budget.page(HEAD_PAGE))
                .await?;
            let Some(last) = heads.last() else {
                return Ok(Some(Position::Commits(None)));
            };
            after = Some(last.path.clone());
            for head in heads {
                if let Some(at) = self
                    .verify_path(&head.path, RevisionId(1), report, budget)
                    .await?
                {
                    return Ok(Some(at));
                }
                if budget.spent() {
                    return Ok(Some(Position::Paths(Some(head.path))));
                }
            }
        }
    }

    /// Checks `path` from revision `from` on; the position to resume at when the budget ends inside the path.
    async fn verify_path(
        &self,
        path: &RepoPath,
        mut from: RevisionId,
        report: &mut VerifyReport,
        budget: &mut Budget,
    ) -> Result<Option<Position>> {
        loop {
            let want = budget.page(COMMIT_PAGE * 5);
            let mut run = self
                .meta
                .history_page(
                    &self.repo,
                    path,
                    None,
                    Some(RevisionId(from.0 - 1)),
                    want + 1,
                )
                .await?;
            if run.is_empty() && from.0 == 1 {
                return Err(PynError::PathNotFound(path.to_string()));
            }
            let more = run.len() > want;
            run.truncate(want);
            if from.0 == 1 && !run.is_empty() {
                report.paths += 1;
            }
            report.revisions += run.len() as u64;
            budget.spend(run.len().max(1));
            let mut found = self.run_breaks(path, &run).await?;
            for id in self.pointer_mismatches(&run).await? {
                if !found.contains(&(id, BreakKind::Base)) {
                    found.push((id, BreakKind::Base));
                }
            }
            for (id, kind) in found {
                report.problems.push(Problem {
                    kind,
                    path: Some(path.clone()),
                    revision: Some(id),
                    event: None,
                    commit: None,
                });
            }
            let Some(last) = run.last() else {
                return Ok(None);
            };
            if !more {
                return Ok(None);
            }
            from = RevisionId(last.id.0 + 1);
            if budget.spent() {
                return Ok(Some(Position::InPath(path.clone(), from)));
            }
        }
    }

    /// Exclusive revisions whose stored base is not the path's pointer at the first parent of their commit.
    async fn pointer_mismatches(&self, revisions: &[Revision]) -> Result<Vec<RevisionId>> {
        let mut bad = Vec::new();
        for r in revisions.iter().filter(|r| r.mode == Some(Mode::Exclusive)) {
            let Some(commit) = self.meta.get_commit(&self.repo, &r.commit).await? else {
                continue;
            };
            let expected = match commit.parents.first() {
                Some(parent) => self
                    .meta
                    .revision_at(&self.repo, parent, &r.path)
                    .await?
                    .map(|p| p.id),
                None => None,
            };
            if expected != r.base {
                bad.push(r.id);
            }
        }
        Ok(bad)
    }

    /// Checks each commit's id against its fields and that its parents exist; only the oldest commit has none.
    async fn verify_commits(
        &self,
        mut before: Option<ObjectId>,
        report: &mut VerifyReport,
        budget: &mut Budget,
    ) -> Result<Option<Position>> {
        let problem = |c: &Commit| Problem {
            kind: BreakKind::Hash,
            path: None,
            revision: None,
            event: None,
            commit: Some(c.id),
        };
        loop {
            let want = budget.page(COMMIT_PAGE);
            let mut page = self
                .meta
                .commits(&self.repo, None, before.as_ref(), want + 1)
                .await?;
            let more = page.len() > want;
            page.truncate(want);
            let Some(last) = page.last() else {
                return Ok(Some(Position::Trees(0)));
            };
            report.commits += page.len() as u64;
            budget.spend(page.len());
            let in_page: HashSet<ObjectId> = page.iter().map(|c| c.id).collect();
            for (i, c) in page.iter().enumerate() {
                if c.verify().is_err() {
                    report.problems.push(problem(c));
                }
                let oldest = i + 1 == page.len() && !more;
                let mut linked = !c.parents.is_empty() || oldest;
                for p in &c.parents {
                    linked &=
                        in_page.contains(p) || self.meta.get_commit(&self.repo, p).await?.is_some();
                }
                if !linked {
                    report.problems.push(Problem {
                        kind: BreakKind::Link,
                        ..problem(c)
                    });
                }
            }
            if !more {
                return Ok(Some(Position::Trees(0)));
            }
            before = Some(last.id);
            if budget.spent() {
                return Ok(Some(Position::Commits(before)));
            }
        }
    }

    /// Checks each branch head's tree against the snapshot at that head, starting at branch number `index`.
    async fn verify_trees(
        &self,
        mut index: usize,
        report: &mut VerifyReport,
        budget: &mut Budget,
    ) -> Result<Option<Position>> {
        let branches = self.meta.list_branches(&self.repo).await?;
        while let Some(branch) = branches.get(index) {
            index += 1;
            let Some(head) = &branch.head else { continue };
            let Some(commit) = self.meta.get_commit(&self.repo, head).await? else {
                continue;
            };
            let mut files = Vec::new();
            let mut after: Option<RepoPath> = None;
            loop {
                let page = self
                    .meta
                    .snapshot_page(&self.repo, head, None, after.as_ref(), SNAPSHOT_PAGE)
                    .await?;
                let Some(last) = page.last() else { break };
                after = Some(last.path.clone());
                budget.spend(page.len());
                let full = page.len() == SNAPSHOT_PAGE;
                files.extend(page.iter().map(|r| TreeChange {
                    path: r.path.clone(),
                    mode: r.file_mode,
                    id: r.content,
                    removed: false,
                }));
                if !full {
                    break;
                }
            }
            if tree_of(files)?.root != commit.tree {
                report.problems.push(Problem {
                    kind: BreakKind::Tree,
                    path: None,
                    revision: None,
                    event: None,
                    commit: Some(commit.id),
                });
            }
            budget.spend(1);
            if budget.spent() && index < branches.len() {
                return Ok(Some(Position::Trees(index)));
            }
        }
        Ok(Some(Position::Audit(None)))
    }

    /// Checks the repository's audit log, newest events first, chunk by chunk with the link to the event before each.
    async fn verify_audit_log(
        &self,
        mut before: Option<i64>,
        report: &mut VerifyReport,
        budget: &mut Budget,
    ) -> Result<Option<Position>> {
        let scope = AuditScope::Repo(self.repo.clone());
        let mut checked = report.audit_events.unwrap_or(0);
        loop {
            let want = budget.page(COMMIT_PAGE);
            let mut page = self
                .audit
                .list(
                    &scope,
                    &AuditQuery {
                        before,
                        limit: want + 1,
                        ..AuditQuery::default()
                    },
                )
                .await?;
            let more = page.len() > want;
            let predecessor = if more { page.pop() } else { None };
            page.reverse();
            checked += page.len() as u64;
            budget.spend(page.len());
            for (id, kind) in verify_audit_run(&scope, &page, predecessor.as_ref()) {
                report.problems.push(Problem {
                    kind,
                    path: None,
                    revision: None,
                    event: Some(id),
                    commit: None,
                });
            }
            report.audit_events = Some(checked);
            if !more {
                return Ok(None);
            }
            before = page.first().map(|e| e.id);
            if budget.spent() {
                return Ok(Some(Position::Audit(before)));
            }
        }
    }
}

impl VerifyReport {
    fn merge(&mut self, step: VerifyReport) {
        self.paths += step.paths;
        self.revisions += step.revisions;
        self.commits += step.commits;
        self.audit_events = match (self.audit_events, step.audit_events) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        self.problems.extend(step.problems);
    }
}
