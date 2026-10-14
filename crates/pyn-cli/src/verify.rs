//! `pyn verify`: the server's report plus an independent recomputation of the revision hashes it served.

use std::collections::BTreeMap;

use anyhow::Result;
use pyn_hash::{
    REVISION_V1, REVISION_V2, RevisionFields, RevisionV2Fields, revision_hash, revision_hash_v2,
};
use pyn_object::{ObjectId, Signature};
use pyn_proto as api;

use crate::client::{Api, ApiError};
use crate::table;

/// One break: its kind, the revision or event, and where.
type Finding = (String, String, String);

/// Prints every break found and returns whether history is intact.
pub fn run(api: &Api, path: Option<&str>, verbose: bool) -> Result<bool> {
    let mut req = api.get(&api.repo_route("/verify")?);
    if let Some(p) = path {
        req = req.query(&[("path", p)]);
    }
    let report: api::VerifyReport = api.send(req)?.json()?;
    let mut found: Vec<Finding> = report.problems.iter().map(finding).collect();

    // A server that refuses to serve a corrupt revision has already reported it above.
    match served_revisions(api, path) {
        Ok(revs) => found.extend(check_chains(&revs)),
        Err(e) if !found.is_empty() && e.downcast_ref::<ApiError>().is_some() => {}
        Err(e) => return Err(e),
    }
    if path.is_none() {
        match served_commits(api) {
            Ok(commits) => found.extend(check_commits(&commits)),
            Err(e) if !found.is_empty() && e.downcast_ref::<ApiError>().is_some() => {}
            Err(e) => return Err(e),
        }
    }
    found.sort();
    found.dedup();

    if !found.is_empty() {
        let rows: Vec<Vec<String>> = found
            .iter()
            .map(|(k, w, p)| vec![k.clone(), w.clone(), p.clone()])
            .collect();
        table::show(&["PROBLEM", "WHAT", "PATH"], &rows, "");
    }
    if verbose {
        let log = report
            .audit_events
            .map_or(String::new(), |n| format!(" and {n} audit events"));
        eprintln!(
            "checked {} revisions of {} paths and {} commits{log}: {}",
            report.revisions,
            report.paths,
            report.commits,
            if found.is_empty() { "ok" } else { "broken" }
        );
    }
    Ok(found.is_empty())
}

fn finding(p: &api::VerifyProblem) -> Finding {
    let what = match (&p.commit, p.revision, p.event) {
        (Some(c), _, _) => format!("commit {}", crate::snapshot::short(c)),
        (None, Some(r), _) => format!("r{r}"),
        (None, None, Some(e)) => format!("event {e}"),
        _ => "-".into(),
    };
    let place = match (&p.commit, &p.path) {
        (Some(_), _) => "(commits)".to_string(),
        (None, path) => path.clone().unwrap_or_else(|| "(audit log)".into()),
    };
    (p.kind.clone(), what, place)
}

fn served_revisions(api: &Api, path: Option<&str>) -> Result<Vec<api::Revision>> {
    let mut all: Vec<api::Revision> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut req = api.get(&api.repo_route("/history")?);
        req = match path {
            Some(p) => req.query(&[("path", p)]),
            None => req.query(&[("limit", "200")]),
        };
        if let Some(b) = &before {
            req = req.query(&[("before", b)]);
        }
        let page: api::HistoryPage = api.send(req)?.json()?;
        all.extend(page.revisions);
        before = page.next_cursor;
        if before.is_none() {
            return Ok(all);
        }
    }
}

/// Whether the revision's `hash_version` disagrees with its stored fields.
fn version_problem(r: &api::Revision) -> bool {
    !matches!(
        (r.hash_version, &r.author_identity),
        (REVISION_V1, None) | (REVISION_V2, Some(_))
    )
}

fn expected(r: &api::Revision) -> String {
    let Some(who) = &r.author_identity else {
        return revision_hash(&RevisionFields {
            path: &r.path,
            content: r.legacy_content.as_deref().unwrap_or(&r.content),
            previous: r.previous.as_deref(),
            author: &r.author,
            created_micros: r.created_at.timestamp_micros(),
            message: &r.message,
            restored_from: r.restored_from,
        });
    };
    let algorithm = |hex: &str| ObjectId::parse(hex).map_or("unknown", |id| id.algorithm().name());
    revision_hash_v2(&RevisionV2Fields {
        path: &r.path,
        file_mode: r.file_mode.as_str(),
        blob_algorithm: algorithm(&r.content),
        blob_id: &r.content,
        previous: r.previous.as_deref(),
        user: &r.author,
        author_name: &who.name,
        author_email: &who.email,
        created_micros: r.created_at.timestamp_micros(),
        author_offset_minutes: who.offset_minutes,
        message: &r.message,
        restored_from: r.restored_from,
        commit_algorithm: algorithm(&r.commit),
        commit_id: &r.commit,
    })
}

fn served_commits(api: &Api) -> Result<Vec<api::Commit>> {
    let mut all: Vec<api::Commit> = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let mut req = api
            .get(&api.repo_route("/commits")?)
            .query(&[("limit", "200")]);
        if let Some(b) = &before {
            req = req.query(&[("before", b)]);
        }
        let page: api::CommitPage = api.send(req)?.json()?;
        all.extend(page.commits);
        before = page.next_before;
        if before.is_none() {
            return Ok(all);
        }
    }
}

fn signature(p: &api::Person, at: chrono::DateTime<chrono::Utc>) -> Signature {
    Signature {
        name: p.name.clone().into_bytes(),
        email: p.email.clone().into_bytes(),
        time: at.timestamp(),
        offset_minutes: p.offset_minutes,
    }
}

/// The git commit id the commit's fields give, or `None` if they are not valid ids.
fn commit_id(c: &api::Commit) -> Option<String> {
    let tree = ObjectId::parse(&c.tree).ok()?;
    let parents = c
        .parents
        .iter()
        .map(|p| ObjectId::parse(p))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let message = if c.message.ends_with('\n') {
        c.message.clone()
    } else {
        format!("{}\n", c.message)
    };
    let git = pyn_object::Commit {
        tree,
        parents,
        author: signature(&c.author, c.created_at),
        committer: signature(&c.committer, c.created_at),
        extra_headers: c
            .signature
            .iter()
            .map(|s| (b"gpgsig".to_vec(), s.clone().into_bytes()))
            .collect(),
        message: message.into_bytes(),
    };
    let algorithm = ObjectId::parse(&c.tree).ok()?.algorithm();
    git.id(algorithm).ok().map(|id: ObjectId| id.to_hex())
}

/// Recomputes every commit id from its fields and checks that each commit's parents are commits that came before it.
/// `commits` is newest first, as served.
fn check_commits(commits: &[api::Commit]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for c in commits.iter().rev() {
        let at = |kind: &str| {
            (
                kind.to_string(),
                format!("commit {}", crate::snapshot::short(&c.id)),
                "(commits)".to_string(),
            )
        };
        if commit_id(c).as_deref() != Some(c.id.as_str()) {
            out.push(at("hash_mismatch"));
        }
        let linked = c.parents.iter().all(|p| seen.contains(p.as_str()))
            && (!c.parents.is_empty() || seen.is_empty());
        if !linked {
            out.push(at("broken_link"));
        }
        if c.revisions.iter().any(|r| r.commit != c.id) {
            out.push(at("broken_link"));
        }
        seen.insert(c.id.as_str());
    }
    out.reverse();
    out
}

/// Recomputes every hash and checks each path's links, whatever order the revisions arrive in.
fn check_chains(revs: &[api::Revision]) -> Vec<Finding> {
    let mut by_path: BTreeMap<&str, Vec<&api::Revision>> = BTreeMap::new();
    for r in revs {
        by_path.entry(&r.path).or_default().push(r);
    }
    let mut out = Vec::new();
    for (path, mut chain) in by_path {
        chain.sort_by_key(|r| r.id);
        let ids: std::collections::HashSet<u64> = chain.iter().map(|r| r.id).collect();
        let mut prior: Option<&api::Revision> = None;
        for r in chain {
            let at = |kind: &str| (kind.to_string(), format!("r{}", r.id), path.to_string());
            if version_problem(r) {
                out.push(at("version_mismatch"));
            } else if expected(r) != r.hash {
                out.push(at("hash_mismatch"));
            }
            let linked = match prior {
                None => r.previous.is_none() && r.id == 1,
                Some(p) => r.previous.as_deref() == Some(p.hash.as_str()) && r.id == p.id + 1,
            };
            if !linked {
                out.push(at("broken_link"));
            }
            if r.base.is_some_and(|b| b >= r.id || !ids.contains(&b)) {
                out.push(at("base_mismatch"));
            }
            prior = Some(r);
        }
    }
    out
}
