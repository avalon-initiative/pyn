//! Merging branches: the plan for a merge, its preview and the write (docs/concepts/merging.md).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::ops::Bound;
use std::pin::Pin;
use std::sync::Arc;

use pyn_merge::{Outcome, merge_text, render_markers};
use pyn_object::{FileMode, ObjectId};

use super::RepoService;
use crate::access::{Permission, Principal};
use crate::audit::{AuditAction, brief};
use crate::branch::Branch;
use crate::error::{PynError, Result};
use crate::merge::{
    Change, Conflict, ConflictKind, Content, Decision, Entry, FastForward, Inputs, MAX_BASE_DEPTH,
    MAX_MERGE_COST, MAX_TEXT_MERGE, MergeOutcome, MergeReport, MergeRequest, MergeWrite, Pick,
    Resolution, Side, Version, decide,
};
use crate::rules::{Mode, POLICY_PATH, Rules};
use crate::stranded::{ConversionNote, StrandedVersion};
use crate::types::{ChangesetItem, NewRevision, RepoPath, Revision};

/// What the merge does to one path.
enum Verdict {
    /// The target's revision stands.
    Keep,
    /// The path points at an existing revision.
    Point(Box<Revision>),
    /// A new revision of the path's line.
    Write(Content, FileMode),
    Conflict(ConflictKind),
}

struct PathPlan {
    path: RepoPath,
    mode: Mode,
    base: Option<Entry>,
    ours: Option<Revision>,
    theirs: Option<Revision>,
    verdict: Verdict,
}

struct Plan {
    target: Branch,
    source: ObjectId,
    bases: Vec<ObjectId>,
    virtual_base: bool,
    outcome: MergeOutcome,
    /// The policy the merge is decided under: the later of the two sides' effective policies.
    rules: Arc<Rules>,
    paths: Vec<PathPlan>,
}

/// Three-way merges the bytes; over the cost bound the result is a whole-file conflict.
fn merge_bytes(base: &[u8], ours: &[u8], theirs: &[u8], has_base: bool) -> Merged {
    if pyn_merge::merge_cost(base, ours, theirs) > MAX_MERGE_COST {
        return Merged::Conflict {
            kind: ConflictKind::Binary,
            markers: None,
        };
    }
    match merge_text(base, ours, theirs) {
        Outcome::Clean(bytes) => Merged::Clean(bytes),
        Outcome::Conflicted(chunks) => Merged::Conflict {
            kind: if has_base {
                ConflictKind::Content
            } else {
                ConflictKind::BothAdded
            },
            markers: Some(render_markers(&chunks, "ours", "base", "theirs")),
        },
        Outcome::BinaryConflict => Merged::Conflict {
            kind: ConflictKind::Binary,
            markers: None,
        },
    }
}

/// The merged text of a path, or why it could not be merged.
enum Merged {
    Clean(Vec<u8>),
    /// `markers` is the text with conflict markers; `None` for a whole-file conflict.
    Conflict {
        kind: ConflictKind,
        markers: Option<Vec<u8>>,
    },
}

type Tree = BTreeMap<RepoPath, Entry>;

struct BaseTree {
    tree: Tree,
    bases: Vec<ObjectId>,
    virtual_base: bool,
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

fn version_of(rev: &Revision) -> Version {
    Version {
        content: rev.content,
        file_mode: rev.file_mode,
    }
}

fn side_of(rev: &Revision) -> Side {
    Side {
        revision: Some(rev.id),
        content: rev.content,
        file_mode: rev.file_mode,
    }
}

/// Where the path ends up when `content` is its merged content: on a revision that already holds it, or a new one.
fn settle(
    ours: Option<&Revision>,
    theirs: Option<&Revision>,
    content: Content,
    file_mode: FileMode,
) -> Result<Verdict> {
    let id = content.id()?;
    let (natural, is_ours) = match (ours, theirs) {
        (Some(o), Some(t)) if o.id >= t.id => (o, true),
        (Some(_), Some(t)) => (t, false),
        (Some(o), None) => (o, true),
        (None, Some(t)) => (t, false),
        (None, None) => return Ok(Verdict::Keep),
    };
    Ok(if natural.content == id && natural.file_mode == file_mode {
        if is_ours {
            Verdict::Keep
        } else {
            Verdict::Point(Box::new(natural.clone()))
        }
    } else {
        Verdict::Write(content, file_mode)
    })
}

impl RepoService {
    /// What merging `source` into `target` would do, with nothing written.
    pub async fn merge_preview(&self, request: &MergeRequest) -> Result<MergeReport> {
        let (target, source, _) = self.merge_ends(request).await?;
        let plan = self
            .plan_merge(&target, source, request.fast_forward)
            .await?;
        self.report(&plan, None).await
    }

    /// Merges the source into the target: a fast-forward, or a merge commit with the target's head first. Conflicts
    /// need a resolution each, or nothing is written and the answer is `MergeConflict`.
    pub async fn merge(&self, who: &Principal, request: MergeRequest) -> Result<MergeReport> {
        who.require(Permission::Checkin)?;
        self.ensure_writable().await?;
        let (target, source, label) = self.merge_ends(&request).await?;
        if let Some(expected) = request.expect_target
            && target.head != Some(expected)
        {
            return Err(PynError::MergeStale(target.name));
        }
        let mut plan = self
            .plan_merge(&target, source, request.fast_forward)
            .await?;
        match plan.outcome {
            MergeOutcome::UpToDate | MergeOutcome::FastForward
                if !request.resolutions.is_empty() =>
            {
                Err(PynError::InvalidRequest(
                    "there is nothing to resolve: the merge has no conflicts".into(),
                ))
            }
            MergeOutcome::UpToDate => self.report(&plan, None).await,
            MergeOutcome::FastForward => self.write_fast_forward(who, &plan, &label).await,
            MergeOutcome::Merge | MergeOutcome::Conflicted => {
                self.resolve(&mut plan, &request.resolutions).await?;
                let unresolved: Vec<RepoPath> = plan
                    .paths
                    .iter()
                    .filter(|p| matches!(p.verdict, Verdict::Conflict(_)))
                    .map(|p| p.path.clone())
                    .collect();
                if !unresolved.is_empty() {
                    return Err(PynError::MergeConflict(unresolved));
                }
                plan.outcome = MergeOutcome::Merge;
                let message = request
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("Merge branch '{label}' into {}", plan.target.name));
                self.write_merge(who, &plan, &label, message).await
            }
        }
    }

    /// The target branch, the source commit, and the name the source goes by in messages.
    async fn merge_ends(&self, request: &MergeRequest) -> Result<(Branch, ObjectId, String)> {
        let target = match &request.target {
            Some(name) => self.branch(name).await?,
            None => self.branch(crate::branch::DEFAULT_BRANCH).await?,
        };
        let spec = request.source.trim();
        let (source, label) = match self.meta.get_branch(&self.repo, spec).await? {
            Some(b) => (
                b.head.ok_or_else(|| {
                    PynError::InvalidRequest(format!("the branch {spec} has no commits yet"))
                })?,
                spec.to_string(),
            ),
            None => {
                let id = self.resolve_commit(spec).await?;
                (id, format!("commit {}", &id.to_hex()[..10]))
            }
        };
        Ok((target, source, label))
    }

    async fn snapshot_tree(&self, commit: &ObjectId) -> Result<BTreeMap<RepoPath, Revision>> {
        let revisions = self.meta.snapshot(&self.repo, commit).await?;
        revisions.iter().try_for_each(Revision::verify)?;
        Ok(revisions.into_iter().map(|r| (r.path.clone(), r)).collect())
    }

    async fn plan_merge(
        &self,
        target: &Branch,
        source: ObjectId,
        fast_forward: FastForward,
    ) -> Result<Plan> {
        let mut plan = Plan {
            target: target.clone(),
            source,
            bases: Vec::new(),
            virtual_base: false,
            outcome: MergeOutcome::Merge,
            rules: self.merge_rules(target, &source).await?,
            paths: Vec::new(),
        };
        let Some(head) = target.head else {
            plan.outcome = MergeOutcome::FastForward;
            return Ok(plan);
        };
        if head == source || self.meta.is_ancestor(&self.repo, &source, &head).await? {
            plan.outcome = MergeOutcome::UpToDate;
            plan.bases = vec![source];
            return Ok(plan);
        }
        if self.meta.is_ancestor(&self.repo, &head, &source).await? {
            plan.bases = vec![head];
            if fast_forward != FastForward::Never {
                plan.outcome = MergeOutcome::FastForward;
                return Ok(plan);
            }
        } else if fast_forward == FastForward::Only {
            return Err(PynError::NotFastForward(target.name.clone()));
        }
        let ours = self.snapshot_tree(&head).await?;
        let theirs = self.snapshot_tree(&source).await?;
        let base = if plan.bases.is_empty() {
            self.base_tree(head, source, 0).await?
        } else {
            BaseTree {
                tree: ours
                    .values()
                    .cloned()
                    .map(|r| (r.path.clone(), Entry::of(r)))
                    .collect(),
                bases: plan.bases.clone(),
                virtual_base: false,
            }
        };
        plan.bases = base.bases;
        plan.virtual_base = base.virtual_base;
        let rules = plan.rules.clone();
        let paths: BTreeSet<&RepoPath> = ours.keys().chain(theirs.keys()).collect();
        let names: BTreeSet<&str> = paths.iter().map(|p| p.as_str()).collect();
        for path in &names {
            let folder = format!("{path}/");
            if names
                .range::<str, _>((Bound::Included(folder.as_str()), Bound::Unbounded))
                .next()
                .is_some_and(|p| p.starts_with(&folder))
            {
                return Err(PynError::InvalidRequest(format!(
                    "{path} is a file on one side and a folder on the other; \
                     a pyn server cannot record deletions yet, so the branches cannot be merged"
                )));
            }
        }
        for path in paths {
            let (o, t) = (ours.get(path), theirs.get(path));
            if let (Some(o), Some(t)) = (o, t)
                && o.id == t.id
            {
                continue;
            }
            let mode = rules.mode_for(path);
            let b = base.tree.get(path);
            let verdict = match mode {
                Mode::Shared => self.shared_verdict(b, o, t).await?,
                Mode::Exclusive => self.exclusive_verdict(o, t, head, source).await?,
            };
            plan.paths.push(PathPlan {
                path: path.clone(),
                mode,
                base: b.cloned(),
                ours: o.cloned(),
                theirs: t.cloned(),
                verdict,
            });
        }
        plan.outcome = if plan
            .paths
            .iter()
            .any(|p| matches!(p.verdict, Verdict::Conflict(_)))
        {
            MergeOutcome::Conflicted
        } else {
            MergeOutcome::Merge
        };
        Ok(plan)
    }

    /// The policy a merge into `target` is decided under: the default branch's current one, or for another branch the
    /// later of the target's and the source's effective policies.
    async fn merge_rules(&self, target: &Branch, source: &ObjectId) -> Result<Arc<Rules>> {
        if target.default {
            return Ok(self.rules());
        }
        let ours = match &target.head {
            Some(head) => self.policy_revision_at(head).await?,
            None => None,
        };
        let theirs = self.policy_revision_at(source).await?;
        match ours.into_iter().chain(theirs).max_by_key(|r| r.id) {
            Some(rev) => self.rules_of(&rev).await,
            None => Ok(self.fallback.clone()),
        }
    }

    /// The tree the merge of `a` and `b` is made against: their merge base, or a virtual one when they have several.
    fn base_tree<'a>(&'a self, a: ObjectId, b: ObjectId, depth: u8) -> Boxed<'a, BaseTree> {
        Box::pin(async move {
            let bases = self.meta.merge_bases(&self.repo, &a, &b).await?;
            let Some((first, rest)) = bases.split_first() else {
                return Ok(BaseTree {
                    tree: Tree::new(),
                    bases,
                    virtual_base: false,
                });
            };
            let mut tree: Tree = self
                .snapshot_tree(first)
                .await?
                .into_values()
                .map(|r| (r.path.clone(), Entry::of(r)))
                .collect();
            for next in rest {
                let inner = if depth < MAX_BASE_DEPTH {
                    self.base_tree(*first, *next, depth + 1).await?.tree
                } else {
                    Tree::new()
                };
                let other: Tree = self
                    .snapshot_tree(next)
                    .await?
                    .into_values()
                    .map(|r| (r.path.clone(), Entry::of(r)))
                    .collect();
                tree = self.merge_virtual(&inner, tree, other).await?;
            }
            let virtual_base = !rest.is_empty();
            Ok(BaseTree {
                tree,
                bases,
                virtual_base,
            })
        })
    }

    /// Merges two base trees into one that is never committed; what cannot merge keeps the conflict markers.
    async fn merge_virtual(&self, base: &Tree, ours: Tree, theirs: Tree) -> Result<Tree> {
        let paths: BTreeSet<RepoPath> = ours.keys().chain(theirs.keys()).cloned().collect();
        let mut out = Tree::new();
        for path in paths {
            let (b, o, t) = (base.get(&path), ours.get(&path), theirs.get(&path));
            let (bv, ov, tv) = (
                b.map(Entry::version).transpose()?,
                o.map(Entry::version).transpose()?,
                t.map(Entry::version).transpose()?,
            );
            let entry = match decide(bv.as_ref(), ov.as_ref(), tv.as_ref()) {
                Decision::Conflict(_) => o.or(t).cloned(),
                Decision::Take { content, mode } => {
                    let side = |p: Pick| if p == Pick::Theirs { t.or(o) } else { o.or(t) };
                    let file_mode = side(mode).map(|e| e.file_mode);
                    match (content, o, t, file_mode) {
                        (Pick::Merge, Some(o), Some(t), Some(file_mode)) => {
                            let merged = match self.merge_contents(b, o, t).await? {
                                Merged::Clean(bytes)
                                | Merged::Conflict {
                                    markers: Some(bytes),
                                    ..
                                } => Content::Bytes(bytes),
                                Merged::Conflict { markers: None, .. } => o.content.clone(),
                            };
                            Some(Entry {
                                revision: None,
                                content: merged,
                                file_mode,
                            })
                        }
                        (_, _, _, file_mode) => side(content).cloned().map(|mut e| {
                            if file_mode.is_some_and(|m| m != e.file_mode) {
                                e.revision = None;
                                e.file_mode = file_mode.unwrap_or(e.file_mode);
                            }
                            e
                        }),
                    }
                }
            };
            if let Some(entry) = entry {
                out.insert(path, entry);
            }
        }
        Ok(out)
    }

    async fn content_size(&self, content: &Content) -> Result<u64> {
        match content {
            Content::Bytes(b) => Ok(b.len() as u64),
            Content::Blob(id) => self
                .objects
                .size(id)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(id.to_string())),
        }
    }

    async fn load(&self, content: &Content) -> Result<Vec<u8>> {
        match content {
            Content::Bytes(b) => Ok(b.clone()),
            Content::Blob(id) => self
                .objects
                .get_limited(id, MAX_TEXT_MERGE)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(id.to_string())),
        }
    }

    /// Merges two versions of a file line by line against their base; a file that is too large or binary is a conflict.
    async fn merge_contents(
        &self,
        base: Option<&Entry>,
        ours: &Entry,
        theirs: &Entry,
    ) -> Result<Merged> {
        let sizes = [
            Some(self.content_size(&ours.content).await?),
            Some(self.content_size(&theirs.content).await?),
            match base {
                Some(b) => Some(self.content_size(&b.content).await?),
                None => None,
            },
        ];
        if sizes.iter().flatten().any(|s| *s > MAX_TEXT_MERGE) {
            return Ok(Merged::Conflict {
                kind: ConflictKind::Binary,
                markers: None,
            });
        }
        let base_bytes = match base {
            Some(b) => self.load(&b.content).await?,
            None => Vec::new(),
        };
        let (our_bytes, their_bytes) = (
            self.load(&ours.content).await?,
            self.load(&theirs.content).await?,
        );
        let permit = self
            .merge_slots
            .acquire()
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        let has_base = base.is_some();
        let merged = tokio::task::spawn_blocking(move || {
            merge_bytes(&base_bytes, &our_bytes, &their_bytes, has_base)
        })
        .await
        .map_err(|e| PynError::Storage(e.to_string()));
        drop(permit);
        merged
    }

    async fn shared_verdict(
        &self,
        base: Option<&Entry>,
        ours: Option<&Revision>,
        theirs: Option<&Revision>,
    ) -> Result<Verdict> {
        let bv = base.map(Entry::version).transpose()?;
        let (ov, tv) = (ours.map(version_of), theirs.map(version_of));
        let (content, mode) = match decide(bv.as_ref(), ov.as_ref(), tv.as_ref()) {
            Decision::Conflict(kind) => return Ok(Verdict::Conflict(kind)),
            Decision::Take { content, mode } => (content, mode),
        };
        let file_mode = match (mode, ours, theirs) {
            (Pick::Theirs, _, Some(t)) => t.file_mode,
            (_, Some(o), _) => o.file_mode,
            (_, None, Some(t)) => t.file_mode,
            (_, None, None) => FileMode::Regular,
        };
        let merged = match (content, ours, theirs) {
            (Pick::Merge, Some(o), Some(t)) => {
                match self
                    .merge_contents(base, &Entry::of(o.clone()), &Entry::of(t.clone()))
                    .await?
                {
                    Merged::Clean(bytes) => Content::Bytes(bytes),
                    Merged::Conflict { kind, .. } => return Ok(Verdict::Conflict(kind)),
                }
            }
            (Pick::Theirs, _, Some(t)) => Content::Blob(t.content),
            (_, Some(o), _) => Content::Blob(o.content),
            (_, None, Some(t)) => Content::Blob(t.content),
            (_, None, None) => return Ok(Verdict::Keep),
        };
        settle(ours, theirs, merged, file_mode)
    }

    /// An exclusive path takes the pointer that descends from the other; pointers that diverged, or a shared version
    /// outside the other side's history, conflict.
    async fn exclusive_verdict(
        &self,
        ours: Option<&Revision>,
        theirs: Option<&Revision>,
        ours_head: ObjectId,
        source: ObjectId,
    ) -> Result<Verdict> {
        match (ours, theirs) {
            (Some(_), None) | (None, None) => Ok(Verdict::Keep),
            (None, Some(t)) => Ok(Verdict::Point(Box::new(t.clone()))),
            (Some(o), Some(t)) => {
                let same = version_of(o) == version_of(t);
                if !same
                    && (self.is_diverged(o, &source).await?
                        || self.is_diverged(t, &ours_head).await?)
                {
                    return Ok(Verdict::Conflict(ConflictKind::ExclusiveConversion));
                }
                if same {
                    return Ok(if o.id > t.id {
                        Verdict::Keep
                    } else {
                        Verdict::Point(Box::new(t.clone()))
                    });
                }
                if self.descends(o, t).await? {
                    Ok(Verdict::Keep)
                } else if self.descends(t, o).await? {
                    Ok(Verdict::Point(Box::new(t.clone())))
                } else {
                    Ok(Verdict::Conflict(ConflictKind::Binary))
                }
            }
        }
    }

    /// Whether `rev` is `ancestor` or follows it: through the recorded bases, or because the commit that made `rev`
    /// has the one that made `ancestor` in its history (a merge's revision follows both sides).
    pub(super) async fn descends(&self, rev: &Revision, ancestor: &Revision) -> Result<bool> {
        let mut at = rev.clone();
        while let Some(base) = at.base {
            if base == ancestor.id {
                return Ok(true);
            }
            if base < ancestor.id {
                break;
            }
            match self.meta.get_revision(&self.repo, &at.path, base).await? {
                Some(next) => at = next,
                None => break,
            }
        }
        Ok(rev.id > ancestor.id
            && self
                .meta
                .is_ancestor(&self.repo, &ancestor.commit, &rev.commit)
                .await?)
    }

    /// Whether `rev` is a shared version the history ending at `other` does not contain.
    pub(super) async fn is_diverged(&self, rev: &Revision, other: &ObjectId) -> Result<bool> {
        Ok(rev.mode == Some(Mode::Shared)
            && !self
                .meta
                .is_ancestor(&self.repo, &rev.commit, other)
                .await?)
    }

    /// What a conversion conflict met: the conversion revision on the side that holds it, and the diverged version.
    async fn conversion_note(&self, plan: &Plan, p: &PathPlan) -> Result<Option<ConversionNote>> {
        let (Some(o), Some(t), Some(head)) = (&p.ours, &p.theirs, plan.target.head) else {
            return Ok(None);
        };
        let (diverged, other_head) = if self.is_diverged(o, &plan.source).await? {
            (o, plan.source)
        } else {
            (t, head)
        };
        let line = self
            .meta
            .history(&self.repo, &p.path, Some(&other_head))
            .await?;
        let Some(conv) = line
            .windows(2)
            .rev()
            .find(|w| w[0].mode == Some(Mode::Shared) && w[1].mode == Some(Mode::Exclusive))
            .map(|w| w[1].clone())
        else {
            return Ok(None);
        };
        let policy = RepoPath::new(POLICY_PATH)?;
        let policy = self
            .meta
            .revision_at(&self.repo, &conv.commit, &policy)
            .await?
            .map(|r| r.id);
        Ok(Some(ConversionNote {
            revision: conv.id,
            policy,
            by: conv.author,
            at: conv.created_at,
            diverged: diverged.id,
            diverged_by: diverged.author.clone(),
            diverged_at: diverged.created_at,
        }))
    }

    /// Applies the person's resolutions to the plan, each to a path the merge touches.
    async fn resolve(&self, plan: &mut Plan, resolutions: &[Resolution]) -> Result<()> {
        let mut by_path: BTreeMap<&RepoPath, &Resolution> = BTreeMap::new();
        for r in resolutions {
            if by_path.insert(&r.path, r).is_some() {
                return Err(PynError::InvalidRequest(format!(
                    "{} is resolved twice",
                    r.path
                )));
            }
            if !plan.paths.iter().any(|p| p.path == r.path) {
                return Err(PynError::InvalidRequest(format!(
                    "{} is not part of this merge",
                    r.path
                )));
            }
        }
        for p in &mut plan.paths {
            let Some(r) = by_path.get(&p.path) else {
                continue;
            };
            self.referenced_size(&r.content).await?;
            p.verdict = if p.mode == Mode::Exclusive
                && matches!(p.verdict, Verdict::Conflict(ConflictKind::Binary))
            {
                Verdict::Write(Content::Blob(r.content), r.file_mode)
            } else {
                settle(
                    p.ours.as_ref(),
                    p.theirs.as_ref(),
                    Content::Blob(r.content),
                    r.file_mode,
                )?
            };
        }
        Ok(())
    }

    async fn inputs(&self, p: &PathPlan) -> Result<Inputs> {
        let mut inputs = Inputs {
            base: None,
            ours: p.ours.as_ref().map(side_of),
            theirs: p.theirs.as_ref().map(side_of),
            base_inline: None,
        };
        if let Some(b) = &p.base {
            inputs.base = Some(Side {
                revision: b.revision.as_ref().map(|r| r.id),
                content: b.content.id()?,
                file_mode: b.file_mode,
            });
            if b.revision.is_none() && self.content_size(&b.content).await? <= MAX_TEXT_MERGE {
                inputs.base_inline = Some(self.load(&b.content).await?);
            }
        }
        Ok(inputs)
    }

    /// The report of a plan: what changes in the target and what conflicts. `commit` is what was written, if anything.
    async fn report(&self, plan: &Plan, commit: Option<ObjectId>) -> Result<MergeReport> {
        let mut changes = Vec::new();
        let mut conflicts = Vec::new();
        if plan.outcome == MergeOutcome::FastForward {
            if let Some(head) = plan.target.head {
                let ours = self.snapshot_tree(&head).await?;
                for (path, t) in self.snapshot_tree(&plan.source).await? {
                    let same = ours
                        .get(&path)
                        .is_some_and(|o| version_of(o) == version_of(&t));
                    if !same {
                        changes.push(Change {
                            mode: plan.rules.mode_for(&path),
                            path,
                            content: t.content,
                            file_mode: t.file_mode,
                            revision: Some(t.id),
                            inputs: None,
                        });
                    }
                }
            } else {
                for (path, t) in self.snapshot_tree(&plan.source).await? {
                    changes.push(Change {
                        mode: plan.rules.mode_for(&path),
                        path,
                        content: t.content,
                        file_mode: t.file_mode,
                        revision: Some(t.id),
                        inputs: None,
                    });
                }
            }
        }
        for p in &plan.paths {
            let (content, file_mode, holder) = match &p.verdict {
                Verdict::Keep => continue,
                Verdict::Conflict(kind) => {
                    let conversion = if *kind == ConflictKind::ExclusiveConversion {
                        self.conversion_note(plan, p).await?
                    } else {
                        None
                    };
                    conflicts.push(Conflict {
                        path: p.path.clone(),
                        mode: p.mode,
                        kind: *kind,
                        inputs: self.inputs(p).await?,
                        conversion,
                    });
                    continue;
                }
                Verdict::Point(r) => (r.content, r.file_mode, Some(r.id)),
                Verdict::Write(c, fm) => (c.id()?, *fm, None),
            };
            let same_as_ours = p
                .ours
                .as_ref()
                .is_some_and(|o| o.content == content && o.file_mode == file_mode);
            if same_as_ours {
                continue;
            }
            let revision = holder.or_else(|| {
                [p.theirs.as_ref(), p.ours.as_ref()]
                    .into_iter()
                    .flatten()
                    .find(|r| r.content == content && r.file_mode == file_mode)
                    .map(|r| r.id)
            });
            let inputs = if revision.is_none() {
                Some(self.inputs(p).await?)
            } else {
                None
            };
            changes.push(Change {
                path: p.path.clone(),
                mode: p.mode,
                content,
                file_mode,
                revision,
                inputs,
            });
        }
        Ok(MergeReport {
            target: plan.target.name.clone(),
            target_head: plan.target.head,
            source: plan.source,
            bases: plan.bases.clone(),
            virtual_base: plan.virtual_base,
            outcome: plan.outcome,
            changes,
            conflicts,
            commit,
            stranded: Vec::new(),
        })
    }

    /// Checks a policy file arriving on the default branch: the merger may edit policy and the file must parse.
    async fn check_arriving_policy(
        &self,
        who: &Principal,
        target: &Branch,
        content: &ObjectId,
    ) -> Result<()> {
        if target.default {
            self.check_policy_edit(who, content).await?;
        }
        Ok(())
    }

    /// Applies the policy at `commit` if it is newer than the one in force, converting paths as a policy commit does.
    async fn apply_arrived_policy(
        &self,
        who: &Principal,
        commit: &ObjectId,
    ) -> Result<Vec<StrandedVersion>> {
        let policy = RepoPath::new(POLICY_PATH)?;
        let Some(rev) = self.meta.revision_at(&self.repo, commit, &policy).await? else {
            return Ok(Vec::new());
        };
        let rules = self.parse_policy(&rev.content).await?;
        if let Some(old) = self.apply(rev.id, rules) {
            let new = self.rules();
            return self.policy_applied(who, &rev, &old, &new).await;
        }
        Ok(Vec::new())
    }

    async fn write_fast_forward(
        &self,
        who: &Principal,
        plan: &Plan,
        label: &str,
    ) -> Result<MergeReport> {
        let target = &plan.target;
        if target.default {
            let policy = RepoPath::new(POLICY_PATH)?;
            let arriving = self
                .meta
                .revision_at(&self.repo, &plan.source, &policy)
                .await?;
            let current = match &target.head {
                Some(h) => self.meta.revision_at(&self.repo, h, &policy).await?,
                None => None,
            };
            if let Some(rev) = arriving
                && current.as_ref().map(|c| c.id) != Some(rev.id)
            {
                self.check_arriving_policy(who, target, &rev.content)
                    .await?;
            }
        }
        self.meta
            .fast_forward_branch(&self.repo, &target.name, target.head.as_ref(), &plan.source)
            .await?;
        self.record_repo(
            &who.user,
            AuditAction::BranchMerged,
            format!(
                "{label} into {} (fast-forward to {})",
                target.name, plan.source
            ),
        )
        .await?;
        let stranded = if target.default {
            self.apply_arrived_policy(who, &plan.source).await?
        } else {
            Vec::new()
        };
        let mut report = self.report(plan, Some(plan.source)).await?;
        report.stranded = stranded;
        Ok(report)
    }

    async fn write_merge(
        &self,
        who: &Principal,
        plan: &Plan,
        label: &str,
        message: String,
    ) -> Result<MergeReport> {
        let target = &plan.target;
        let head = target
            .head
            .ok_or_else(|| PynError::InvalidRequest("the target has no commits".into()))?;
        let now = self.clock.now();
        let mut adopted = Vec::new();
        let mut items = Vec::new();
        let mut policy_content = None;
        for p in &plan.paths {
            let is_policy = p.path.as_str() == POLICY_PATH;
            match &p.verdict {
                Verdict::Keep | Verdict::Conflict(_) => {}
                Verdict::Point(r) => {
                    if is_policy {
                        policy_content = Some(r.content);
                    }
                    adopted.push((**r).clone());
                }
                Verdict::Write(content, file_mode) => {
                    let blob = match content {
                        Content::Blob(id) => *id,
                        Content::Bytes(bytes) => self.objects.put(bytes.clone()).await?,
                    };
                    if is_policy {
                        policy_content = Some(blob);
                    }
                    let size = self
                        .objects
                        .size(&blob)
                        .await?
                        .ok_or_else(|| PynError::ObjectMissing(blob.to_string()))?;
                    let (expected_head, lock_holder) = match p.mode {
                        Mode::Shared => (p.ours.as_ref().map(|r| r.id), None),
                        Mode::Exclusive => (p.ours.as_ref().map(|r| r.id), Some(who.user.clone())),
                    };
                    items.push(ChangesetItem {
                        revision: NewRevision {
                            path: p.path.clone(),
                            content: blob,
                            file_mode: *file_mode,
                            author: who.user.clone(),
                            person: who.person(),
                            message: message.clone(),
                            created_at: now,
                            restored_from: None,
                            mode: p.mode,
                            deleted: false,
                            size,
                            changeset: None,
                        },
                        expected_head,
                        lock_holder,
                    });
                }
            }
        }
        if let Some(content) = &policy_content {
            self.check_arriving_policy(who, target, content).await?;
        }
        let cap = self.storage_cap().await?;
        let written = self
            .limit_result(
                self.meta
                    .commit_merge_on(
                        &self.repo,
                        &target.name,
                        MergeWrite {
                            expected_head: head,
                            other: plan.source,
                            adopted,
                            items,
                            author: who.person(),
                            message,
                        },
                        now,
                        cap,
                    )
                    .await,
            )
            .await?;
        self.record_repo(
            &who.user,
            AuditAction::BranchMerged,
            format!("{label} into {} at {}", target.name, written.commit),
        )
        .await?;
        for rev in &written.revisions {
            self.record(
                &who.user,
                AuditAction::Checkin,
                &rev.path,
                format!(
                    "r{}: {} (merge {})",
                    rev.id,
                    brief(&rev.message),
                    written.commit
                ),
            )
            .await?;
        }
        let stranded = if target.default {
            self.apply_arrived_policy(who, &written.commit).await?
        } else {
            Vec::new()
        };
        let mut report = self.report(plan, Some(written.commit)).await?;
        report.stranded = stranded;
        Ok(report)
    }
}
