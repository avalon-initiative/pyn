//! `pyn verify`: the server's report plus an independent recomputation of the revision hashes it served.

use std::collections::BTreeMap;

use anyhow::Result;
use pyn_hash::{RevisionFields, revision_hash};
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
            "checked {} revisions of {} paths{log}: {}",
            report.revisions,
            report.paths,
            if found.is_empty() { "ok" } else { "broken" }
        );
    }
    Ok(found.is_empty())
}

fn finding(p: &api::VerifyProblem) -> Finding {
    let what = match (p.revision, p.event) {
        (Some(r), _) => format!("r{r}"),
        (None, Some(e)) => format!("event {e}"),
        _ => "-".into(),
    };
    (
        p.kind.clone(),
        what,
        p.path.clone().unwrap_or_else(|| "(audit log)".into()),
    )
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

fn expected(r: &api::Revision) -> String {
    revision_hash(&RevisionFields {
        path: &r.path,
        content: &r.content,
        previous: r.previous.as_deref(),
        author: &r.author,
        created_micros: r.created_at.timestamp_micros(),
        message: &r.message,
        restored_from: r.restored_from,
    })
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
        let mut prior: Option<&api::Revision> = None;
        for r in chain {
            let at = |kind: &str| (kind.to_string(), format!("r{}", r.id), path.to_string());
            if expected(r) != r.hash {
                out.push(at("hash_mismatch"));
            }
            let linked = match prior {
                None => r.previous.is_none() && r.id == 1,
                Some(p) => r.previous.as_deref() == Some(p.hash.as_str()) && r.id == p.id + 1,
            };
            if !linked {
                out.push(at("broken_link"));
            }
            prior = Some(r);
        }
    }
    out
}
