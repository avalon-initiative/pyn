//! Pull requests: open, list, read, edit, comment, close, reopen and merge, with the commits and diff of the range.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use pyn_core::merge::{ConflictKind, FastForward, MergeOutcome};
use pyn_core::pull::{PullEdit, PullEvent, PullEventKind, PullFilter, PullRequest, PullState};
use pyn_core::service::{DIFF_DEFAULT_CONTEXT, MergePull, OpenPull, PullDetail};
use pyn_core::{Commit, DiffOptions, ObjectId, PynError, REPO_ALGORITHM, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::{RepoCaller, RepoReader};
use crate::self_service_api::identities;
use crate::{ApiResult, AppState, RepoAddress, lock_dto};

type Idents = HashMap<UserId, api::PublicIdentity>;

async fn idents(s: &AppState, users: Vec<UserId>) -> ApiResult<Idents> {
    let found = identities(s, &users).await?;
    Ok(users
        .into_iter()
        .zip(found)
        .map(|(u, i)| {
            let fallback = api::PublicIdentity {
                user: u.to_string(),
                display_name: None,
                avatar_url: None,
            };
            (u, i.unwrap_or(fallback))
        })
        .collect())
}

fn who(ids: &Idents, user: &UserId) -> api::PublicIdentity {
    ids.get(user)
        .cloned()
        .unwrap_or_else(|| api::PublicIdentity {
            user: user.to_string(),
            display_name: None,
            avatar_url: None,
        })
}

fn pull_users(p: &PullRequest) -> impl Iterator<Item = UserId> + '_ {
    std::iter::once(p.author.clone()).chain(p.merged_by.clone())
}

fn state_dto(s: PullState) -> api::PullRequestState {
    match s {
        PullState::Open => api::PullRequestState::Open,
        PullState::Merged => api::PullRequestState::Merged,
        PullState::Closed => api::PullRequestState::Closed,
    }
}

fn summary_dto(p: &PullRequest, ids: &Idents) -> api::PullRequestSummary {
    api::PullRequestSummary {
        number: p.number,
        title: p.title.clone(),
        state: state_dto(p.state),
        draft: p.draft,
        author: who(ids, &p.author),
        base: p.base.clone(),
        head: p.head.clone(),
        opened_at: p.created_at,
        closed_at: p.closed_at.filter(|_| p.state == PullState::Closed),
        merged_at: p.merged_at,
        merged_by: p.merged_by.as_ref().map(|u| who(ids, u)),
        comments: p.comments,
    }
}

fn hex(id: &str, what: &str) -> Result<ObjectId, PynError> {
    ObjectId::from_hex(REPO_ALGORITHM, id)
        .map_err(|_| PynError::InvalidRequest(format!("{what} {id:?} is not an object id")))
}

fn conflict_kind(k: ConflictKind) -> api::MergeConflictKind {
    match k {
        ConflictKind::Content => api::MergeConflictKind::Content,
        ConflictKind::BothAdded => api::MergeConflictKind::BothAdded,
        ConflictKind::Binary => api::MergeConflictKind::Binary,
        ConflictKind::DeleteModify => api::MergeConflictKind::DeleteModify,
        ConflictKind::FileMode => api::MergeConflictKind::FileMode,
        ConflictKind::ExclusiveConversion => api::MergeConflictKind::ExclusiveConversion,
    }
}

fn outcome_dto(o: MergeOutcome) -> api::MergeOutcome {
    match o {
        MergeOutcome::UpToDate => api::MergeOutcome::UpToDate,
        MergeOutcome::FastForward => api::MergeOutcome::FastForward,
        MergeOutcome::Merge => api::MergeOutcome::Merge,
        MergeOutcome::Conflicted => api::MergeOutcome::Conflicted,
    }
}

async fn detail_dto(
    s: &AppState,
    repo: &pyn_core::repositories::OpenRepo,
    d: PullDetail,
) -> ApiResult<api::PullRequest> {
    let mut users: Vec<UserId> = pull_users(&d.pull).collect();
    users.extend(d.participants.iter().cloned());
    let ids = idents(s, users).await?;
    let p = &d.pull;
    let (base_commit, head_commit) = match p.frozen {
        Some((b, h)) => (Some(b), Some(h)),
        None => (
            repo.service.head_of(&p.base).await.ok().flatten(),
            repo.service.head_of(&p.head).await.ok().flatten(),
        ),
    };
    let mut locks: Vec<api::Lock> = d.locks.into_iter().map(lock_dto).collect();
    crate::self_service_api::annotate_held(s, locks.iter_mut()).await?;
    let (outcome, conflicts) = match d.mergeability {
        Some(m) => (
            Some(outcome_dto(m.outcome)),
            m.conflicts
                .into_iter()
                .map(|(path, kind)| api::PullConflict {
                    path: path.to_string(),
                    kind: conflict_kind(kind),
                })
                .collect(),
        ),
        None => (None, Vec::new()),
    };
    Ok(api::PullRequest {
        summary: summary_dto(p, &ids),
        body: p.body.clone(),
        head_commit: head_commit.map(|c| c.to_hex()),
        base_commit: base_commit.map(|c| c.to_hex()),
        merge_commit: p.merge_commit.map(|c| c.to_hex()),
        commits: d.commits,
        changed_files: d.changed_files,
        additions: d.additions,
        deletions: d.deletions,
        outcome,
        conflicts,
        locks,
        participants: d.participants.iter().map(|u| who(&ids, u)).collect(),
    })
}

async fn detail_of(
    s: &AppState,
    repo: &pyn_core::repositories::OpenRepo,
    number: u64,
) -> ApiResult<Json<api::PullRequest>> {
    let d = repo.service.pull_detail(number).await?;
    Ok(Json(detail_dto(s, repo, d).await?))
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    state: Option<String>,
    base: Option<String>,
    head: Option<String>,
    author: Option<String>,
    before: Option<u64>,
    limit: Option<usize>,
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/pulls",
    params(RepoAddress,
           ("state" = Option<String>, Query, description = "open, merged or closed; every state if omitted"),
           ("base" = Option<String>, Query, description = "only pull requests into this branch"),
           ("head" = Option<String>, Query, description = "only pull requests from this branch"),
           ("author" = Option<String>, Query, description = "only this account's pull requests"),
           ("before" = Option<u64>, Query, description = "the `next_before` of the previous page"),
           ("limit" = Option<usize>, Query, description = "1 to 100, default 30")),
    responses((status = 200, body = api::PullRequestPage, description = "newest first; `comments` is counted, the change counts, locks and mergeability are on the single pull request"),
              (status = 400, body = api::ErrorBody, description = "invalid_request for an unknown state")))]
/// List a repository's pull requests.
pub(crate) async fn list_pulls(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<api::PullRequestPage>> {
    let filter = PullFilter {
        state: q.state.as_deref().map(PullState::parse).transpose()?,
        base: q.base,
        head: q.head,
        author: q.author.as_deref().map(UserId::new),
    };
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let pulls = repo
        .service
        .list_pulls(&filter, q.before, Some(limit))
        .await?;
    let users: Vec<UserId> = pulls.iter().flat_map(pull_users).collect();
    let ids = idents(&s, users).await?;
    let next_before = (pulls.len() == limit)
        .then(|| pulls.last().map(|p| p.number))
        .flatten();
    Ok(Json(api::PullRequestPage {
        pulls: pulls.iter().map(|p| summary_dto(p, &ids)).collect(),
        next_before,
    }))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/pulls", params(RepoAddress),
    request_body = api::CreatePullRequest,
    responses((status = 200, body = api::PullRequest),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad title or body, the same branch twice, a head with nothing to merge"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "branch_not_found"),
              (status = 409, body = api::ErrorBody, description = "pull_request_exists: an open one has the same branches")))]
/// Open a pull request.
pub(crate) async fn create_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Json(req): Json<api::CreatePullRequest>,
) -> ApiResult<Json<api::PullRequest>> {
    let p = repo
        .service
        .open_pull(
            &who,
            OpenPull {
                title: req.title,
                body: req.body.unwrap_or_default(),
                draft: req.draft.unwrap_or(false),
                base: req.base,
                head: req.head,
            },
        )
        .await?;
    detail_of(&s, &repo, p.number).await
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/pulls/{number}",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    responses((status = 200, body = api::PullRequest),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found")))]
/// One pull request with its mergeability, change counts, locks and participants.
pub(crate) async fn get_pull(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, number)): Path<(String, String, u64)>,
) -> ApiResult<Json<api::PullRequest>> {
    detail_of(&s, &repo, number).await
}

#[utoipa::path(patch, path = "/v1/repos/{owner}/{name}/pulls/{number}",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    request_body = api::UpdatePullRequest,
    responses((status = 200, body = api::PullRequest),
              (status = 400, body = api::ErrorBody, description = "invalid_request"),
              (status = 403, body = api::ErrorBody, description = "only the author, or someone with the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found"),
              (status = 409, body = api::ErrorBody, description = "pull_request_not_open")))]
/// Change an open pull request's title, body or draft flag.
pub(crate) async fn update_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, number)): Path<(String, String, u64)>,
    Json(req): Json<api::UpdatePullRequest>,
) -> ApiResult<Json<api::PullRequest>> {
    repo.service
        .edit_pull(
            &who,
            number,
            PullEdit {
                title: req.title,
                body: req.body,
                draft: req.draft,
            },
        )
        .await?;
    detail_of(&s, &repo, number).await
}

fn commit_dto(c: &Commit, ids: &Idents) -> api::PullCommit {
    api::PullCommit {
        id: c.id.to_hex(),
        message: c.message.clone(),
        author: who(ids, &UserId::new(&c.author.name)),
        committed_at: c.created_at,
    }
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/pulls/{number}/commits",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    responses((status = 200, body = Vec<api::PullCommit>, description = "the commits the head has and the base lacks, at most 1000"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found")))]
/// The commits of a pull request.
pub(crate) async fn pull_commits(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, number)): Path<(String, String, u64)>,
) -> ApiResult<Json<Vec<api::PullCommit>>> {
    let commits = repo.service.pull_commits(number).await?;
    let users: Vec<UserId> = commits
        .iter()
        .map(|c| UserId::new(&c.author.name))
        .collect();
    let ids = idents(&s, users).await?;
    Ok(Json(commits.iter().map(|c| commit_dto(c, &ids)).collect()))
}

#[derive(Deserialize)]
pub(crate) struct DiffQuery {
    context: Option<usize>,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/pulls/{number}/diff",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number"),
           ("context" = Option<usize>, Query, description = "unchanged lines around each change, 0 to 100, default 3")),
    responses((status = 200, body = api::RepoDiff, description = "the changes of the head since the merge base, as the diff endpoint reports them; a closed or merged pull request is read at the commits frozen on it"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found")))]
/// The line diff of a pull request.
pub(crate) async fn pull_diff(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, number)): Path<(String, String, u64)>,
    Query(q): Query<DiffQuery>,
) -> ApiResult<Json<api::RepoDiff>> {
    let _: Option<DiffOptions> = None;
    let d = repo
        .service
        .pull_diff(number, q.context.unwrap_or(DIFF_DEFAULT_CONTEXT))
        .await?;
    Ok(Json(crate::diff_api::diff_dto(&s, d).await?))
}

fn event_dto(e: &PullEvent, ids: &Idents) -> api::PullEvent {
    api::PullEvent {
        id: e.id.to_string(),
        kind: match e.kind {
            PullEventKind::Comment => api::PullEventKind::Comment,
            PullEventKind::Merged => api::PullEventKind::Merged,
            PullEventKind::Closed => api::PullEventKind::Closed,
            PullEventKind::Reopened => api::PullEventKind::Reopened,
        },
        actor: who(ids, &e.actor),
        at: e.at,
        body: e.body.clone(),
        commit: e.commit.map(|c| c.to_hex()),
    }
}

#[derive(Deserialize)]
pub(crate) struct EventsQuery {
    after: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/pulls/{number}/events",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number"),
           ("after" = Option<String>, Query, description = "the id of the last event already seen"),
           ("limit" = Option<usize>, Query, description = "1 to 100, default 30")),
    responses((status = 200, body = Vec<api::PullEvent>, description = "the comments and state changes, oldest first"),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a bad `after`"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found")))]
/// The timeline of a pull request.
pub(crate) async fn pull_events(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, number)): Path<(String, String, u64)>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Vec<api::PullEvent>>> {
    let after = q
        .after
        .as_deref()
        .map(|a| {
            a.parse::<u64>()
                .map_err(|_| PynError::InvalidRequest(format!("after {a:?} is not an event id")))
        })
        .transpose()?;
    let events = repo.service.pull_events(number, after, q.limit).await?;
    let ids = idents(&s, events.iter().map(|e| e.actor.clone()).collect()).await?;
    Ok(Json(events.iter().map(|e| event_dto(e, &ids)).collect()))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/pulls/{number}/comments",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    request_body = api::CommentPullRequest,
    responses((status = 200, body = api::PullEvent),
              (status = 400, body = api::ErrorBody, description = "invalid_request for an empty or oversized comment"),
              (status = 403, body = api::ErrorBody, description = "needs the read permission"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found")))]
/// Comment on a pull request in any state.
pub(crate) async fn comment_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who_,
        ..
    }: RepoCaller,
    Path((_, _, number)): Path<(String, String, u64)>,
    Json(req): Json<api::CommentPullRequest>,
) -> ApiResult<Json<api::PullEvent>> {
    let e = repo.service.comment_pull(&who_, number, &req.body).await?;
    let ids = idents(&s, vec![e.actor.clone()]).await?;
    Ok(Json(event_dto(&e, &ids)))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/pulls/{number}/close",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    responses((status = 200, body = api::PullRequest),
              (status = 403, body = api::ErrorBody, description = "only the author, or someone with the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found"),
              (status = 409, body = api::ErrorBody, description = "pull_request_not_open")))]
/// Close an open pull request.
pub(crate) async fn close_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, number)): Path<(String, String, u64)>,
) -> ApiResult<Json<api::PullRequest>> {
    repo.service.close_pull(&who, number).await?;
    detail_of(&s, &repo, number).await
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/pulls/{number}/reopen",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    responses((status = 200, body = api::PullRequest),
              (status = 400, body = api::ErrorBody, description = "invalid_request when the head has nothing the base lacks, or no longer exists"),
              (status = 403, body = api::ErrorBody, description = "only the author, or someone with the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found"),
              (status = 409, body = api::ErrorBody, description = "pull_request_not_open for a merged one, pull_request_exists when an open one has the same branches")))]
/// Reopen a closed pull request.
pub(crate) async fn reopen_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, number)): Path<(String, String, u64)>,
) -> ApiResult<Json<api::PullRequest>> {
    repo.service.reopen_pull(&who, number).await?;
    detail_of(&s, &repo, number).await
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/pulls/{number}/merge",
    params(RepoAddress, ("number" = u64, Path, description = "the pull request's number")),
    request_body = api::MergePullRequest,
    responses((status = 200, body = api::PullRequest, description = "the merged pull request, with `merge_commit`"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "pull_request_not_found, or branch_not_found when a branch is gone"),
              (status = 409, body = api::ErrorBody, description = "pull_request_not_open, pull_request_draft, merge_conflict, merge_stale, not_fast_forward, or pull_locks_held when others hold locks on changed paths and the caller lacks force_unlock")))]
/// Merge a pull request's head branch into its base branch.
pub(crate) async fn merge_pull(
    State(s): State<AppState>,
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, number)): Path<(String, String, u64)>,
    Json(req): Json<api::MergePullRequest>,
) -> ApiResult<Json<api::PullRequest>> {
    let request = MergePull {
        fast_forward: match req.fast_forward.unwrap_or_default() {
            api::MergeFastForward::Allow => FastForward::Allow,
            api::MergeFastForward::Never => FastForward::Never,
            api::MergeFastForward::Only => FastForward::Only,
        },
        message: req.message,
        expect_target: req
            .expect_target
            .as_deref()
            .map(|h| hex(h, "expect_target"))
            .transpose()?,
        expect_head: req
            .expect_head
            .as_deref()
            .map(|h| hex(h, "expect_head"))
            .transpose()?,
    };
    repo.service.merge_pull(&who, number, request).await?;
    detail_of(&s, &repo, number).await
}
