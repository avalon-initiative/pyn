//! `pyn locks --watch`: follows a repository's lock changes from the server's stream, or by polling the lock list
//! when the server has none.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use pyn_proto as api;
use serde_json::{Value, json};

use crate::client::{Api, ok};
use crate::transport::Reply;

const HOLD_SECS: &str = "25";
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// One lock change as the watch prints it, whether it came from the stream or from comparing two lock lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Where to resume after this change; none when polling.
    pub cursor: Option<String>,
    /// `lock.acquired`, `lock.released`, `lock.expired` or `lock.forced`.
    pub kind: &'static str,
    pub at: DateTime<Utc>,
    pub path: String,
    pub holder: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub actor: Option<String>,
    pub reason: Option<String>,
}

impl Change {
    pub fn document(&self) -> Value {
        crate::json_out::document(json!({
            "cursor": self.cursor,
            "type": self.kind,
            "at": self.at,
            "path": self.path,
            "holder": self.holder,
            "expires_at": self.expires_at,
            "actor": self.actor,
            "reason": self.reason,
        }))
    }

    pub fn line(&self) -> String {
        let detail = match self.kind {
            "lock.acquired" => format!(
                "until {}",
                crate::time::local(self.expires_at.unwrap_or(self.at))
            ),
            "lock.expired" => format!(
                "ran out {}",
                crate::time::local(self.expires_at.unwrap_or(self.at))
            ),
            "lock.forced" => format!("by {}", self.actor.as_deref().unwrap_or("-")),
            _ => self.reason.clone().unwrap_or_else(|| "-".into()),
        };
        format!(
            "{}  {:<8}  {:<10}  {:<24}  {}",
            crate::time::local(self.at),
            self.kind.trim_start_matches("lock."),
            self.holder,
            detail,
            self.path
        )
    }
}

fn from_stream(item: &api::LockStreamEvent) -> Option<Change> {
    let at = item.event.created_at;
    let cursor = Some(item.cursor.clone());
    Some(match &item.event.data {
        api::EventData::LockAcquired(d) => Change {
            cursor,
            kind: "lock.acquired",
            at,
            path: d.path.clone(),
            holder: d.holder.clone(),
            expires_at: Some(d.expires_at),
            actor: Some(d.holder.clone()),
            reason: None,
        },
        api::EventData::LockReleased(d) => Change {
            cursor,
            kind: "lock.released",
            at,
            path: d.path.clone(),
            holder: d.holder.clone(),
            expires_at: None,
            actor: Some(d.holder.clone()),
            reason: d.reason.clone(),
        },
        api::EventData::LockExpired(d) => Change {
            cursor,
            kind: "lock.expired",
            at,
            path: d.path.clone(),
            holder: d.holder.clone(),
            expires_at: Some(d.expires_at),
            actor: None,
            reason: None,
        },
        api::EventData::LockForced(d) => Change {
            cursor,
            kind: "lock.forced",
            at,
            path: d.path.clone(),
            holder: d.holder.clone(),
            expires_at: None,
            actor: Some(d.actor.clone()),
            reason: d.reason.clone(),
        },
        _ => return None,
    })
}

enum Fetched {
    Page(api::LockStreamPage),
    NoStream,
    Retry(String),
}

fn retryable(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

fn fetch(api: &Api, after: Option<&str>) -> Result<Fetched> {
    let mut call = api.get(&api.repo_route("/locks/events")?);
    if let Some(after) = after {
        call = call.query(&[("after", after), ("wait", HOLD_SECS)]);
    }
    let reply = match api.transport.send(call) {
        Ok(r) => r,
        Err(e) => return Ok(Fetched::Retry(format!("{e:#}"))),
    };
    if reply.is_success() {
        return Ok(Fetched::Page(reply.json()?));
    }
    let (status, headers) = (reply.status, reply.headers.clone());
    let body = reply.bytes().unwrap_or_default();
    let has_error_body = serde_json::from_slice::<api::ErrorBody>(&body).is_ok();
    if matches!(status, 404 | 405 | 501) && !has_error_body {
        return Ok(Fetched::NoStream);
    }
    if retryable(status) {
        return Ok(Fetched::Retry(format!("the server answered {status}")));
    }
    ok(Reply::new(
        status,
        headers,
        Box::new(std::io::Cursor::new(body)),
    ))?;
    unreachable!("a non-success status is an error")
}

/// Follows the stream from `after` (now when none), handing each change to `emit` until it returns false.
/// Reconnects from the last cursor after a dropped or failed request; polls when the server has no stream.
pub fn run(
    api: &Api,
    after: Option<String>,
    interval: Duration,
    emit: &mut dyn FnMut(&Change) -> bool,
    sleep: &dyn Fn(Duration),
) -> Result<()> {
    let mut cursor = after;
    let mut wait = FIRST_RETRY;
    loop {
        match fetch(api, cursor.as_deref())? {
            Fetched::Page(page) => {
                wait = FIRST_RETRY;
                for item in &page.events {
                    if let Some(change) = from_stream(item)
                        && !emit(&change)
                    {
                        return Ok(());
                    }
                }
                cursor = Some(page.cursor);
            }
            Fetched::NoStream => return poll(api, interval, emit, sleep),
            Fetched::Retry(why) => {
                eprintln!(
                    "lock stream interrupted ({why}); retrying in {}s",
                    wait.as_secs()
                );
                sleep(wait);
                wait = (wait * 2).min(LONGEST_RETRY);
            }
        }
    }
}

fn list(api: &Api) -> Result<Option<BTreeMap<String, api::Lock>>> {
    let reply = match api.transport.send(api.get(&api.repo_route("/locks")?)) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if !reply.is_success() && retryable(reply.status) {
        return Ok(None);
    }
    let locks: Vec<api::Lock> = ok(reply)?.json()?;
    Ok(Some(
        locks.into_iter().map(|l| (l.path.clone(), l)).collect(),
    ))
}

fn change(kind: &'static str, lock: &api::Lock, at: DateTime<Utc>) -> Change {
    let acquired = kind == "lock.acquired";
    Change {
        cursor: None,
        kind,
        at,
        path: lock.path.clone(),
        holder: lock.owner.clone(),
        expires_at: (kind != "lock.released").then_some(lock.expires_at),
        actor: acquired.then(|| lock.owner.clone()),
        reason: None,
    }
}

/// What changed between two lock lists. A lock that is gone past its expiry expired; one gone earlier was released
/// (the list cannot say whether by its holder, a check-in or a forced unlock).
fn diff(
    before: &BTreeMap<String, api::Lock>,
    after: &BTreeMap<String, api::Lock>,
    now: DateTime<Utc>,
) -> Vec<Change> {
    let mut out = Vec::new();
    for (path, old) in before {
        let same = after
            .get(path)
            .is_some_and(|l| l.owner == old.owner && l.acquired_at == old.acquired_at);
        if !same {
            let kind = if old.expires_at <= now {
                "lock.expired"
            } else {
                "lock.released"
            };
            out.push(change(kind, old, now));
        }
    }
    for (path, new) in after {
        let same = before
            .get(path)
            .is_some_and(|l| l.owner == new.owner && l.acquired_at == new.acquired_at);
        if !same {
            out.push(change("lock.acquired", new, now));
        }
    }
    out
}

fn poll(
    api: &Api,
    interval: Duration,
    emit: &mut dyn FnMut(&Change) -> bool,
    sleep: &dyn Fn(Duration),
) -> Result<()> {
    let mut known: Option<BTreeMap<String, api::Lock>> = None;
    let mut wait = FIRST_RETRY;
    loop {
        match list(api)? {
            Some(now_locks) => {
                wait = FIRST_RETRY;
                if let Some(before) = &known {
                    for c in diff(before, &now_locks, Utc::now()) {
                        if !emit(&c) {
                            return Ok(());
                        }
                    }
                }
                known = Some(now_locks);
                sleep(interval);
            }
            None => {
                eprintln!("lock list unavailable; retrying in {}s", wait.as_secs());
                sleep(wait);
                wait = (wait * 2).min(LONGEST_RETRY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Mutex;

    use chrono::TimeZone;

    use super::*;
    use crate::client::ApiError;
    use crate::transport::{Call, Transport};

    type Seen = Vec<(String, Vec<(String, String)>)>;

    enum Step {
        Down,
        Answer(u16, String),
    }

    /// Answers each request with the next scripted step and remembers the path and query of every request.
    struct Script {
        steps: Mutex<Vec<Step>>,
        seen: Mutex<Seen>,
    }

    impl Transport for std::sync::Arc<Script> {
        fn send(&self, call: Call) -> Result<Reply> {
            self.seen
                .lock()
                .unwrap()
                .push((call.path.clone(), call.query.clone()));
            match self.steps.lock().unwrap().remove(0) {
                Step::Down => anyhow::bail!("connection refused"),
                Step::Answer(status, body) => Ok(Reply::new(
                    status,
                    Vec::new(),
                    Box::new(std::io::Cursor::new(body.into_bytes())),
                )),
            }
        }
    }

    fn scripted(steps: Vec<Step>) -> (Api, std::sync::Arc<Script>) {
        let script = std::sync::Arc::new(Script {
            steps: Mutex::new(steps),
            seen: Mutex::new(Vec::new()),
        });
        let api = Api {
            transport: Box::new(script.clone()),
            base: String::new(),
            token: None,
            user: None,
            repo: Some("o/r".into()),
            branch: None,
            on_git: false,
        };
        (api, script)
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap() + chrono::Duration::minutes(minutes)
    }

    fn acquired(cursor: &str, path: &str, holder: &str) -> api::LockStreamEvent {
        api::LockStreamEvent {
            cursor: cursor.into(),
            event: api::Event::new(
                format!("e{cursor}"),
                t(0),
                "https://pyn.test",
                api::EventData::LockAcquired(api::LockAcquiredData {
                    owner: "o".into(),
                    owner_id: "u1".into(),
                    repo: "r".into(),
                    repo_id: "r1".into(),
                    path: path.into(),
                    holder: holder.into(),
                    acting_admin: None,
                    expires_at: t(480),
                }),
            ),
        }
    }

    fn page(events: Vec<api::LockStreamEvent>, cursor: &str) -> Step {
        let page = api::LockStreamPage {
            events,
            cursor: cursor.into(),
        };
        Step::Answer(200, serde_json::to_string(&page).unwrap())
    }

    fn locks(list: &[(&str, &str, i64, i64)]) -> Step {
        let locks: Vec<api::Lock> = list
            .iter()
            .map(|(path, owner, acquired, expires)| api::Lock {
                path: path.to_string(),
                owner: owner.to_string(),
                identity: None,
                acquired_at: t(*acquired),
                expires_at: t(*expires),
            })
            .collect();
        Step::Answer(200, serde_json::to_string(&locks).unwrap())
    }

    fn error(status: u16, code: &str) -> Step {
        Step::Answer(status, format!(r#"{{"code":"{code}","message":"m"}}"#))
    }

    /// Runs the watch until `want` changes arrived; returns them with the waits it asked for.
    fn watch(
        api: &Api,
        after: Option<&str>,
        want: usize,
    ) -> (Result<()>, Vec<Change>, Vec<Duration>) {
        let got = RefCell::new(Vec::new());
        let waits = RefCell::new(Vec::new());
        let result = run(
            api,
            after.map(str::to_string),
            Duration::from_secs(2),
            &mut |c| {
                got.borrow_mut().push(c.clone());
                got.borrow().len() < want
            },
            &|d| waits.borrow_mut().push(d),
        );
        (result, got.into_inner(), waits.into_inner())
    }

    fn query<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
        q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn it_starts_now_then_follows_the_cursor_and_reports_each_change() {
        let (api, script) = scripted(vec![
            page(vec![], "7"),
            page(vec![acquired("8", "Content/a.umap", "alice")], "8"),
            page(vec![acquired("9", "Content/b.umap", "bob")], "9"),
        ]);
        let (result, got, waits) = watch(&api, None, 2);
        result.unwrap();
        assert!(waits.is_empty());
        let paths: Vec<_> = got
            .iter()
            .map(|c| (c.kind, c.path.as_str(), c.holder.as_str()))
            .collect();
        assert_eq!(
            paths,
            [
                ("lock.acquired", "Content/a.umap", "alice"),
                ("lock.acquired", "Content/b.umap", "bob")
            ]
        );
        let seen = script.seen.lock().unwrap();
        assert!(seen.iter().all(|(p, _)| p == "/v1/repos/o/r/locks/events"));
        assert_eq!(query(&seen[0].1, "after"), None);
        assert_eq!(query(&seen[1].1, "after"), Some("7"));
        assert_eq!(query(&seen[2].1, "after"), Some("8"));
        assert!(query(&seen[1].1, "wait").is_some());
    }

    #[test]
    fn a_change_prints_as_a_line_and_as_one_versioned_document() {
        let c = from_stream(&acquired("8", "Content/a.umap", "alice")).unwrap();
        let doc = c.document();
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["cursor"], "8");
        assert_eq!(doc["type"], "lock.acquired");
        assert_eq!(
            (doc["path"].as_str(), doc["holder"].as_str()),
            (Some("Content/a.umap"), Some("alice"))
        );
        assert!(doc["expires_at"].is_string());
        assert!(!doc.to_string().contains('\n'));
        let line = c.line();
        assert!(
            line.contains("alice") && line.ends_with("Content/a.umap"),
            "{line}"
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn it_reconnects_from_the_last_cursor_after_a_drop_or_a_server_error() {
        let (api, script) = scripted(vec![
            page(vec![acquired("8", "a", "alice")], "8"),
            Step::Down,
            Step::Answer(503, String::new()),
            page(vec![acquired("9", "b", "bob")], "9"),
        ]);
        let (result, got, waits) = watch(&api, Some("7"), 2);
        result.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(waits, [Duration::from_secs(1), Duration::from_secs(2)]);
        let seen = script.seen.lock().unwrap();
        let afters: Vec<_> = seen.iter().map(|(_, q)| query(q, "after")).collect();
        assert_eq!(afters, [Some("7"), Some("8"), Some("8"), Some("8")]);
    }

    #[test]
    fn a_refusal_ends_the_watch_instead_of_retrying() {
        for (status, code) in [
            (403, "forbidden"),
            (404, "repo_not_found"),
            (401, "unauthenticated"),
        ] {
            let (api, _) = scripted(vec![error(status, code)]);
            let (result, got, waits) = watch(&api, None, 1);
            let err = result.unwrap_err();
            assert_eq!(err.downcast_ref::<ApiError>().unwrap().code, code);
            assert!(got.is_empty() && waits.is_empty());
        }
    }

    #[test]
    fn a_server_without_the_stream_is_polled_and_changes_are_found_by_comparing() {
        let (api, script) = scripted(vec![
            Step::Answer(404, String::new()),
            locks(&[("a", "alice", 0, 480)]),
            locks(&[("a", "alice", 0, 480), ("b", "bob", 1, 481)]),
            Step::Down,
            locks(&[("b", "bob", 1, 481)]),
        ]);
        let (result, got, waits) = watch(&api, None, 2);
        result.unwrap();
        let seen: Vec<_> = got
            .iter()
            .map(|c| (c.kind, c.path.as_str(), c.cursor.is_none()))
            .collect();
        assert_eq!(
            seen,
            [("lock.acquired", "b", true), ("lock.released", "a", true)]
        );
        assert_eq!(
            waits,
            [
                Duration::from_secs(2),
                Duration::from_secs(2),
                Duration::from_secs(1)
            ]
        );
        let paths: Vec<_> = script
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(paths[0], "/v1/repos/o/r/locks/events");
        assert!(paths[1..].iter().all(|p| p == "/v1/repos/o/r/locks"));
    }

    #[test]
    fn a_renewal_is_no_change_and_a_lease_gone_past_its_expiry_is_expired() {
        let mut before = BTreeMap::new();
        let lock = |path: &str, owner: &str, acquired: i64, expires: i64| api::Lock {
            path: path.into(),
            owner: owner.into(),
            identity: None,
            acquired_at: t(acquired),
            expires_at: t(expires),
        };
        for l in [
            lock("renewed", "a", 0, 10),
            lock("lapsed", "a", 0, 10),
            lock("freed", "a", 0, 500),
        ] {
            before.insert(l.path.clone(), l);
        }
        let mut after = BTreeMap::new();
        after.insert("renewed".to_string(), lock("renewed", "a", 0, 600));
        let kinds: Vec<_> = diff(&before, &after, t(60))
            .iter()
            .map(|c| (c.kind, c.path.clone()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("lock.released", "freed".to_string()),
                ("lock.expired", "lapsed".to_string())
            ]
        );
    }

    #[test]
    fn a_lock_taken_over_between_two_polls_is_a_release_and_an_acquisition() {
        let lock = |owner: &str, acquired: i64| api::Lock {
            path: "p".into(),
            owner: owner.into(),
            identity: None,
            acquired_at: t(acquired),
            expires_at: t(500),
        };
        let before = BTreeMap::from([("p".to_string(), lock("alice", 0))]);
        let after = BTreeMap::from([("p".to_string(), lock("bob", 5))]);
        let kinds: Vec<_> = diff(&before, &after, t(10))
            .iter()
            .map(|c| (c.kind, c.holder.clone()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("lock.released", "alice".to_string()),
                ("lock.acquired", "bob".to_string())
            ]
        );
    }
}
