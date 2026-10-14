//! What a push does to each remote ref: refspec matching and the accept or reject decision, as git's `remote.c` makes them.

use pyn_object::ObjectId;

/// A push refspec as typed: `[+]<src>[:<dst>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub force: bool,
    pub src: String,
    pub dst: Option<String>,
}

impl Spec {
    pub fn parse(text: &str) -> Spec {
        let (force, rest) = match text.strip_prefix('+') {
            Some(r) => (true, r),
            None => (false, text),
        };
        match rest.split_once(':') {
            Some((s, d)) => Spec {
                force,
                src: s.to_owned(),
                dst: Some(d.to_owned()),
            },
            None => Spec {
                force,
                src: rest.to_owned(),
                dst: None,
            },
        }
    }

    pub fn plain(src: &str, dst: &str) -> Spec {
        Spec {
            force: false,
            src: src.to_owned(),
            dst: Some(dst.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NonFastForward,
    AlreadyExists,
    FetchFirst,
    NeedsForce,
    Stale,
    NoDelete,
    Atomic,
}

impl Reason {
    pub fn text(self) -> &'static str {
        match self {
            Reason::NonFastForward => "non-fast-forward",
            Reason::AlreadyExists => "already exists",
            Reason::FetchFirst => "fetch first",
            Reason::NeedsForce => "needs force",
            Reason::Stale => "stale info",
            Reason::NoDelete => "remote does not support deleting refs",
            Reason::Atomic => "atomic push failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Decided to send, not yet answered.
    Pending,
    UpToDate,
    Reject(Reason),
    Ok,
    RemoteReject(Option<String>),
    NoReport,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// The remote ref's full name.
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
    /// The local source as git names it (a full ref name, or the text typed for `HEAD` and revisions); `None` to delete.
    pub src: Option<String>,
    /// The branch a `HEAD` or branch source stands for, for setting upstreams.
    pub branch: Option<String>,
    pub force: bool,
    pub forced_update: bool,
    pub status: Status,
}

/// A match that ends the push with exit 128 rather than failing one ref.
#[derive(Debug, PartialEq, Eq)]
pub enum PlanError {
    Fatal(String),
}

/// What the repository offers a refspec to match against.
pub struct LocalView<'a> {
    /// Every ref under `refs/`, sorted.
    pub refs: &'a [(String, ObjectId)],
    /// The full ref name `HEAD` is on, even when unborn.
    pub head_ref: Option<&'a str>,
    pub rev: &'a dyn Fn(&str) -> Option<ObjectId>,
}

fn rule_match(pattern: &str, name: &str) -> bool {
    name == pattern
        || name == format!("refs/{pattern}")
        || name == format!("refs/tags/{pattern}")
        || name == format!("refs/heads/{pattern}")
        || name == format!("refs/remotes/{pattern}")
        || name == format!("refs/remotes/{pattern}/HEAD")
}

pub fn ref_name_matches(pattern: &str, name: &str) -> bool {
    rule_match(pattern, name)
}

/// The names `pattern` matches, git's `count_refspec_match`: one strong match wins over weak ones.
fn matching<'a>(pattern: &str, names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let (mut strong, mut weak) = (Vec::new(), Vec::new());
    for name in names {
        if !rule_match(pattern, name) {
            continue;
        }
        let exactish = name.len() == pattern.len()
            || pattern.len() + 5 == name.len()
            || name.starts_with("refs/heads/")
            || name.starts_with("refs/tags/");
        if exactish {
            strong.push(name);
        } else {
            weak.push(name);
        }
    }
    if strong.is_empty() { weak } else { strong }
}

fn glob_map(src_pat: &str, dst_pat: &str, name: &str) -> Option<String> {
    let (pre, post) = src_pat.split_once('*')?;
    if name.len() < pre.len() + post.len() || !name.starts_with(pre) || !name.ends_with(post) {
        return None;
    }
    let middle = &name[pre.len()..name.len() - post.len()];
    Some(match dst_pat.split_once('*') {
        Some((dpre, dpost)) => format!("{dpre}{middle}{dpost}"),
        None => dst_pat.to_owned(),
    })
}

struct Matcher<'a> {
    local: &'a LocalView<'a>,
    remote: &'a [(String, ObjectId)],
    entries: Vec<Entry>,
    errors: Vec<String>,
}

impl Matcher<'_> {
    fn remote_id(&self, name: &str) -> Option<ObjectId> {
        self.remote.iter().find(|(n, _)| n == name).map(|(_, i)| *i)
    }

    fn add(&mut self, entry: Entry, explicit: bool) {
        if let Some(prev) = self.entries.iter().find(|e| e.name == entry.name) {
            if explicit && (prev.src != entry.src || prev.new != entry.new) {
                self.errors.push(format!(
                    "multiple updates for ref '{}' not allowed",
                    entry.name
                ));
            }
            return;
        }
        self.entries.push(entry);
    }

    fn push_local(&mut self, spec: &Spec, name: &str, id: ObjectId, dst: String, explicit: bool) {
        let old = self.remote_id(&dst);
        let branch = name.starts_with("refs/heads/").then(|| name.to_owned());
        self.add(
            Entry {
                name: dst,
                old,
                new: Some(id),
                src: Some(name.to_owned()),
                branch,
                force: spec.force,
                forced_update: false,
                status: Status::Pending,
            },
            explicit,
        );
    }

    fn pattern(&mut self, spec: &Spec) {
        let dst_pat = spec.dst.as_deref().filter(|d| !d.is_empty());
        for (name, id) in self.local.refs {
            if let Some(dst) = glob_map(&spec.src, dst_pat.unwrap_or(&spec.src), name) {
                self.push_local(spec, name, *id, dst, false);
            }
        }
    }

    fn matching_branches(&mut self, spec: &Spec) {
        for (name, id) in self.local.refs {
            if name.starts_with("refs/heads/") && self.remote_id(name).is_some() {
                self.push_local(spec, name, *id, name.clone(), false);
            }
        }
    }

    fn delete(&mut self, dst: &str) -> Result<(), PlanError> {
        let found = matching(dst, self.remote.iter().map(|(n, _)| n.as_str()));
        match found.as_slice() {
            [name] => {
                let name = (*name).to_owned();
                let old = self.remote_id(&name);
                self.add(
                    Entry {
                        name,
                        old,
                        new: None,
                        src: None,
                        branch: None,
                        force: false,
                        forced_update: false,
                        status: Status::Pending,
                    },
                    true,
                );
                Ok(())
            }
            [] => {
                self.errors.push(format!(
                    "unable to delete '{dst}': remote ref does not exist"
                ));
                Ok(())
            }
            _ => {
                self.errors
                    .push(format!("dst refspec {dst} matches more than one"));
                Ok(())
            }
        }
    }

    fn explicit(&mut self, spec: &Spec) -> Result<(), PlanError> {
        if spec.src.is_empty() {
            return match spec.dst.as_deref() {
                Some(d) if !d.is_empty() => self.delete(d),
                _ => Ok(()),
            };
        }
        let found = matching(&spec.src, self.local.refs.iter().map(|(n, _)| n.as_str()));
        let (name, id, resolved): (String, ObjectId, Option<String>) = match found.as_slice() {
            [one] => {
                let id = self.local.refs.iter().find(|(n, _)| n == one).unwrap().1;
                ((*one).to_owned(), id, Some((*one).to_owned()))
            }
            [] => match (self.local.rev)(&spec.src) {
                Some(id) => {
                    let head = (spec.src == "HEAD" || spec.src == "@")
                        .then(|| self.local.head_ref.map(str::to_owned))
                        .flatten();
                    (spec.src.clone(), id, head)
                }
                None => {
                    self.errors
                        .push(format!("src refspec {} does not match any", spec.src));
                    return Ok(());
                }
            },
            _ => {
                self.errors
                    .push(format!("src refspec {} matches more than one", spec.src));
                return Ok(());
            }
        };
        let is_head = spec.src == "HEAD" || spec.src == "@";
        let dst = match spec.dst.as_deref() {
            None | Some("") => match &resolved {
                Some(r) if !is_head || r.starts_with("refs/heads/") => r.clone(),
                _ => {
                    return Err(PlanError::Fatal(format!(
                        "{} cannot be resolved to branch",
                        spec.src
                    )));
                }
            },
            Some(d) => {
                let hits = matching(d, self.remote.iter().map(|(n, _)| n.as_str()));
                match hits.as_slice() {
                    [one] => (*one).to_owned(),
                    [] if d.starts_with("refs/") => d.to_owned(),
                    [] => match guess(d, resolved.as_deref()) {
                        Some(g) => g,
                        None => {
                            self.errors.push(format!(
                                "The destination you provided is not a full refname (i.e.,\nstarting with \"refs/\"). We tried to guess what you meant by:\n\n- Looking for a ref that matches '{d}' on the remote side.\n- Checking if the <src> being pushed ('{}')\n  is a ref in \"refs/{{heads,tags}}/\". If so we add a corresponding\n  refs/{{heads,tags}}/ prefix on the remote side.\n\nNeither worked, so we gave up. You must fully qualify the ref.",
                                spec.src
                            ));
                            return Ok(());
                        }
                    },
                    _ => {
                        self.errors
                            .push(format!("dst refspec {d} matches more than one"));
                        return Ok(());
                    }
                }
            }
        };
        let old = self.remote_id(&dst);
        let branch = resolved.filter(|r| r.starts_with("refs/heads/"));
        self.add(
            Entry {
                name: dst,
                old,
                new: Some(id),
                src: Some(name),
                branch,
                force: spec.force,
                forced_update: false,
                status: Status::Pending,
            },
            true,
        );
        Ok(())
    }
}

/// `refs/heads/` or `refs/tags/` in front of a short destination, following what the source is.
fn guess(dst: &str, src: Option<&str>) -> Option<String> {
    let src = src?;
    if src.starts_with("refs/heads/") {
        Some(format!("refs/heads/{dst}"))
    } else if src.starts_with("refs/tags/") {
        Some(format!("refs/tags/{dst}"))
    } else {
        None
    }
}

/// The remote refs `specs` update, existing ones first in the remote's order and new ones after, with the errors found.
pub fn build(
    specs: &[Spec],
    local: &LocalView<'_>,
    remote: &[(String, ObjectId)],
) -> Result<(Vec<Entry>, Vec<String>), PlanError> {
    let mut m = Matcher {
        local,
        remote,
        entries: Vec::new(),
        errors: Vec::new(),
    };
    for spec in specs {
        if spec.src.is_empty() && spec.dst.as_deref() == Some("") {
            m.matching_branches(spec);
        } else if spec.src.contains('*') {
            m.pattern(spec);
        } else {
            m.explicit(spec)?;
        }
    }
    let position = |e: &Entry| {
        remote
            .iter()
            .position(|(n, _)| *n == e.name)
            .unwrap_or(usize::MAX)
    };
    m.entries.sort_by_key(position);
    Ok((m.entries, m.errors))
}

/// The rules a decision needs from the repository.
pub struct Judge<'a> {
    pub force_all: bool,
    pub allow_delete: bool,
    pub has_object: &'a dyn Fn(&ObjectId) -> bool,
    pub peel_commit: &'a dyn Fn(&ObjectId) -> Option<ObjectId>,
    pub is_ancestor: &'a dyn Fn(&ObjectId, &ObjectId) -> bool,
    /// `Some(expected)` when a lease covers the ref; `Some(None)` expects it absent.
    pub lease: &'a dyn Fn(&Entry) -> Option<Option<ObjectId>>,
}

/// Decides each entry as git's `set_ref_status_for_push` does.
pub fn judge(entries: &mut [Entry], j: &Judge<'_>) {
    for e in entries {
        let deletion = e.new.is_none();
        if !deletion && e.old == e.new {
            e.status = Status::UpToDate;
            continue;
        }
        if deletion && !j.allow_delete {
            e.status = Status::Reject(Reason::NoDelete);
            continue;
        }
        let mut force = e.force || j.force_all;
        if let Some(expect) = (j.lease)(e) {
            if e.old != expect {
                e.status = Status::Reject(Reason::Stale);
                continue;
            }
            force = true;
        }
        let reason = match (e.old, e.new) {
            (Some(old), Some(new)) => {
                if e.name.starts_with("refs/tags/") {
                    Some(Reason::AlreadyExists)
                } else if !(j.has_object)(&old) {
                    Some(Reason::FetchFirst)
                } else {
                    match ((j.peel_commit)(&old), (j.peel_commit)(&new)) {
                        (Some(o), Some(n)) if (j.is_ancestor)(&o, &n) => None,
                        (Some(_), Some(_)) => Some(Reason::NonFastForward),
                        _ => Some(Reason::NeedsForce),
                    }
                }
            }
            _ => None,
        };
        match reason {
            Some(_) if force => e.forced_update = true,
            Some(r) => {
                e.status = Status::Reject(r);
                continue;
            }
            None => {}
        }
        e.status = Status::Pending;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyn_object::Algorithm;

    fn id(b: u8) -> ObjectId {
        ObjectId::from_bytes(Algorithm::Sha1, &[b; 20]).unwrap()
    }

    fn view<'a>(
        refs: &'a [(String, ObjectId)],
        rev: &'a dyn Fn(&str) -> Option<ObjectId>,
    ) -> LocalView<'a> {
        LocalView {
            refs,
            head_ref: Some("refs/heads/main"),
            rev,
        }
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn short_names_match_by_git_rules_and_destinations_follow_the_source() {
        let refs = vec![
            ("refs/heads/main".to_owned(), id(1)),
            ("refs/tags/v1".to_owned(), id(2)),
        ];
        let remote = vec![("refs/heads/main".to_owned(), id(9))];
        let rev = |_: &str| None;
        let local = view(&refs, &rev);
        let (e, errs) = build(&[Spec::parse("main")], &local, &remote).unwrap();
        assert!(errs.is_empty());
        assert_eq!(names(&e), ["refs/heads/main"]);
        assert_eq!(e[0].old, Some(id(9)));
        let (e, _) = build(&[Spec::parse("main:dev")], &local, &remote).unwrap();
        assert_eq!(names(&e), ["refs/heads/dev"]);
        let (e, _) = build(&[Spec::parse("v1:rel")], &local, &remote).unwrap();
        assert_eq!(names(&e), ["refs/tags/rel"]);
        let (e, errs) = build(&[Spec::parse("nope")], &local, &remote).unwrap();
        assert!(e.is_empty() && errs[0].contains("does not match any"));
    }

    #[test]
    fn an_ambiguous_source_and_a_bad_destination_are_errors() {
        let refs = vec![
            ("refs/heads/x".to_owned(), id(1)),
            ("refs/tags/x".to_owned(), id(2)),
        ];
        let rev = |s: &str| (s == "HEAD").then(|| id(1));
        let local = view(&refs, &rev);
        let (_, errs) = build(&[Spec::parse("x")], &local, &[]).unwrap();
        assert!(errs[0].contains("matches more than one"));
        let (e, errs) = build(&[Spec::parse("HEAD:foo")], &local, &[]).unwrap();
        assert!(errs.is_empty());
        assert_eq!(names(&e), ["refs/heads/foo"]);
        let rev = |_: &str| Some(id(7));
        let local = view(&refs, &rev);
        let (_, errs) = build(&[Spec::parse("abc:foo")], &local, &[]).unwrap();
        assert!(errs[0].contains("not a full refname"));
        let err = build(&[Spec::parse("abc")], &local, &[]).unwrap_err();
        assert!(matches!(err, PlanError::Fatal(m) if m.contains("cannot be resolved to branch")));
    }

    #[test]
    fn deletes_patterns_and_matching() {
        let refs = vec![
            ("refs/heads/a".to_owned(), id(1)),
            ("refs/heads/b".to_owned(), id(2)),
        ];
        let remote = vec![
            ("refs/heads/a".to_owned(), id(1)),
            ("refs/heads/gone".to_owned(), id(3)),
        ];
        let rev = |_: &str| None;
        let local = view(&refs, &rev);
        let (e, _) = build(&[Spec::parse(":gone")], &local, &remote).unwrap();
        assert!(e[0].new.is_none() && e[0].name == "refs/heads/gone");
        let (_, errs) = build(&[Spec::parse(":zzz")], &local, &remote).unwrap();
        assert!(errs[0].contains("remote ref does not exist"));
        let (e, _) = build(&[Spec::parse("refs/heads/*")], &local, &remote).unwrap();
        assert_eq!(names(&e), ["refs/heads/a", "refs/heads/b"]);
        let (e, _) = build(&[Spec::parse(":")], &local, &remote).unwrap();
        assert_eq!(names(&e), ["refs/heads/a"]);
    }

    fn decide(old: Option<ObjectId>, new: Option<ObjectId>, name: &str, force: bool) -> Status {
        let mut e = vec![Entry {
            name: name.to_owned(),
            old,
            new,
            src: Some("refs/heads/x".into()),
            branch: None,
            force,
            forced_update: false,
            status: Status::Pending,
        }];
        let j = Judge {
            force_all: false,
            allow_delete: true,
            has_object: &|o| *o != id(9),
            peel_commit: &|o| Some(*o),
            is_ancestor: &|o, n| *o == id(1) && *n == id(2),
            lease: &|_| None,
        };
        judge(&mut e, &j);
        e.remove(0).status
    }

    #[test]
    fn decisions_follow_gits_order() {
        let (b, t) = ("refs/heads/x", "refs/tags/x");
        assert_eq!(decide(Some(id(1)), Some(id(1)), b, false), Status::UpToDate);
        assert_eq!(decide(Some(id(1)), Some(id(2)), b, false), Status::Pending);
        assert_eq!(
            decide(Some(id(2)), Some(id(1)), b, false),
            Status::Reject(Reason::NonFastForward)
        );
        assert_eq!(decide(Some(id(2)), Some(id(1)), b, true), Status::Pending);
        assert_eq!(
            decide(Some(id(9)), Some(id(1)), b, false),
            Status::Reject(Reason::FetchFirst)
        );
        assert_eq!(
            decide(Some(id(1)), Some(id(2)), t, false),
            Status::Reject(Reason::AlreadyExists)
        );
        assert_eq!(decide(None, Some(id(1)), t, false), Status::Pending);
        assert_eq!(decide(Some(id(1)), None, b, false), Status::Pending);
    }
}
