//! `pyn pr`: pull requests on a pyn server (docs/concepts/pull-requests.md).

use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use pyn_proto as api;

use crate::backend::Backend;
use crate::client::Api;
use crate::transport::Method;
use crate::{table, time};

#[derive(Subcommand)]
pub enum PrCommand {
    /// Propose merging a branch into another; the current branch into the default branch unless told otherwise.
    Create {
        #[arg(short, long)]
        title: String,
        /// The description.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read the description from a file (`-` for standard input).
        #[arg(short = 'F', long)]
        body_file: Option<String>,
        /// Open it as a draft, which cannot be merged until it is edited ready.
        #[arg(long)]
        draft: bool,
        /// The branch to merge into; the default branch if omitted.
        #[arg(long)]
        base: Option<String>,
        /// The branch to propose; the workspace's branch if omitted.
        #[arg(long)]
        head: Option<String>,
    },
    /// List pull requests, newest first.
    List {
        /// `open` (the default), `merged`, `closed` or `all`.
        #[arg(long, default_value = "open")]
        state: String,
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        head: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Show a pull request: its state, mergeability, changes, locks, description and discussion.
    Show {
        number: u64,
        /// Also list its commits.
        #[arg(long)]
        commits: bool,
    },
    /// Merge a pull request into its base branch.
    Merge {
        number: u64,
        /// Always make a merge commit.
        #[arg(long, conflicts_with = "ff_only")]
        no_ff: bool,
        /// Refuse unless the base can fast-forward.
        #[arg(long)]
        ff_only: bool,
        /// The merge commit's message.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Close a pull request without merging.
    Close { number: u64 },
    /// Reopen a closed pull request.
    Reopen { number: u64 },
    /// Add a comment to a pull request.
    Comment {
        number: u64,
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read the comment from a file (`-` for standard input).
        #[arg(short = 'F', long)]
        body_file: Option<String>,
    },
}

fn text(body: Option<String>, file: Option<String>) -> Result<Option<String>> {
    match (body, file) {
        (Some(b), _) => Ok(Some(b)),
        (None, Some(f)) if f == "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(Some(s))
        }
        (None, Some(f)) => Ok(Some(
            std::fs::read_to_string(&f).with_context(|| format!("reading {f}"))?,
        )),
        (None, None) => Ok(None),
    }
}

fn pull_route(api: &Api, number: u64, tail: &str) -> Result<String> {
    api.repo_route(&format!("/pulls/{number}{tail}"))
}

fn state_name(s: api::PullRequestState, draft: bool) -> &'static str {
    match (s, draft) {
        (api::PullRequestState::Open, true) => "draft",
        (api::PullRequestState::Open, false) => "open",
        (api::PullRequestState::Merged, _) => "merged",
        (api::PullRequestState::Closed, _) => "closed",
    }
}

fn who(i: &api::PublicIdentity) -> &str {
    &i.user
}

fn short(id: &str) -> &str {
    &id[..id.len().min(10)]
}

fn list(api: &Api, cmd: PrCommand) -> Result<()> {
    let PrCommand::List {
        state,
        author,
        base,
        head,
        limit,
    } = cmd
    else {
        unreachable!("only list reaches here");
    };
    let mut query: Vec<(&str, String)> = vec![("limit", limit.clamp(1, 100).to_string())];
    match state.as_str() {
        "all" => {}
        "open" | "merged" | "closed" => query.push(("state", state.clone())),
        other => bail!("unknown state {other:?}: use open, merged, closed or all"),
    }
    for (k, v) in [("author", author), ("base", base), ("head", head)] {
        if let Some(v) = v {
            query.push((k, v));
        }
    }
    let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let page: api::PullRequestPage = api
        .send(api.get(&api.repo_route("/pulls")?).query(&pairs))?
        .json()?;
    let rows: Vec<Vec<String>> = page
        .pulls
        .iter()
        .map(|p| {
            vec![
                format!("#{}", p.number),
                state_name(p.state, p.draft).to_string(),
                who(&p.author).to_string(),
                p.comments.to_string(),
                format!("{} -> {}", p.head, p.base),
                p.title.clone(),
            ]
        })
        .collect();
    table::show(
        &["NUMBER", "STATE", "AUTHOR", "COMMENTS", "BRANCHES", "TITLE"],
        &rows,
        "no pull requests",
    );
    if page.next_before.is_some() {
        eprintln!("more pull requests: raise --limit");
    }
    Ok(())
}

fn show(api: &Api, number: u64, commits: bool) -> Result<()> {
    let p: api::PullRequest = api.send(api.get(&pull_route(api, number, "")?))?.json()?;
    let s = &p.summary;
    println!("#{} {}", s.number, s.title);
    println!(
        "{} by {}: {} into {}",
        state_name(s.state, s.draft),
        who(&s.author),
        s.head,
        s.base
    );
    println!("opened {}", time::local(s.opened_at));
    if let (Some(at), Some(by)) = (s.merged_at, &s.merged_by) {
        let commit = p
            .merge_commit
            .as_deref()
            .map_or(String::new(), |c| format!(" as {}", short(c)));
        println!("merged {} by {}{commit}", time::local(at), who(by));
    } else if let Some(at) = s.closed_at {
        println!("closed {}", time::local(at));
    }
    println!(
        "{} commits, {} files changed, +{} -{}",
        p.commits, p.changed_files, p.additions, p.deletions
    );
    if let Some(outcome) = p.outcome {
        let text = match outcome {
            api::MergeOutcome::UpToDate => "the head is already in the base",
            api::MergeOutcome::FastForward => "can be merged by fast-forward",
            api::MergeOutcome::Merge => "can be merged with a merge commit",
            api::MergeOutcome::Conflicted => "has conflicts",
        };
        println!("mergeability: {text}");
    }
    for c in &p.conflicts {
        println!("  conflict  {}", c.path);
    }
    if !p.locks.is_empty() {
        println!("locked by others on changed paths blocks the merge:");
        let rows: Vec<Vec<String>> = p
            .locks
            .iter()
            .map(|l| vec![l.owner.clone(), time::local(l.expires_at), l.path.clone()])
            .collect();
        println!("{}", table::render(&["HOLDER", "EXPIRES", "PATH"], &rows));
    }
    if !p.body.trim().is_empty() {
        println!("\n{}", p.body.trim_end());
    }
    if commits {
        let list: Vec<api::PullCommit> = api
            .send(api.get(&pull_route(api, number, "/commits")?))?
            .json()?;
        let rows: Vec<Vec<String>> = list
            .iter()
            .map(|c| {
                vec![
                    short(&c.id).to_string(),
                    who(&c.author).to_string(),
                    c.message.lines().next().unwrap_or_default().to_string(),
                ]
            })
            .collect();
        println!(
            "\n{}",
            table::render(&["COMMIT", "AUTHOR", "MESSAGE"], &rows)
        );
    }
    let events: Vec<api::PullEvent> = api
        .send(
            api.get(&pull_route(api, number, "/events")?)
                .query(&[("limit", "100")]),
        )?
        .json()?;
    for e in events {
        let what = match e.kind {
            api::PullEventKind::Comment => "commented",
            api::PullEventKind::Merged => "merged",
            api::PullEventKind::Closed => "closed",
            api::PullEventKind::Reopened => "reopened",
        };
        println!("\n{} {what} {}", who(&e.actor), time::local(e.at));
        if let Some(body) = e.body {
            for line in body.trim_end().lines() {
                println!("    {line}");
            }
        }
    }
    Ok(())
}

pub fn pr_command(backend: &dyn Backend, cmd: PrCommand) -> Result<()> {
    let api = backend.pyn_server("pr")?;
    match cmd {
        PrCommand::Create {
            title,
            body,
            body_file,
            draft,
            base,
            head,
        } => {
            let head = head.or_else(|| api.branch.clone()).context(
                "name the branch to propose with --head (the workspace is on the default branch)",
            )?;
            let req = api::CreatePullRequest {
                title,
                body: text(body, body_file)?,
                draft: draft.then_some(true),
                base,
                head,
            };
            let p: api::PullRequest = api
                .send(
                    api.request(Method::POST, &api.repo_route("/pulls")?)
                        .json(&req),
                )?
                .json()?;
            println!(
                "opened #{} {} -> {}",
                p.summary.number, p.summary.head, p.summary.base
            );
        }
        PrCommand::List { .. } => list(api, cmd)?,
        PrCommand::Show { number, commits } => show(api, number, commits)?,
        PrCommand::Merge {
            number,
            no_ff,
            ff_only,
            message,
        } => {
            let req = api::MergePullRequest {
                fast_forward: if no_ff {
                    Some(api::MergeFastForward::Never)
                } else if ff_only {
                    Some(api::MergeFastForward::Only)
                } else {
                    None
                },
                message,
                ..Default::default()
            };
            let p: api::PullRequest = api
                .send(
                    api.request(Method::POST, &pull_route(api, number, "/merge")?)
                        .json(&req),
                )?
                .json()?;
            match p.merge_commit {
                Some(c) => println!("merged #{number} into {} as {}", p.summary.base, short(&c)),
                None => println!(
                    "merged #{number}: {} already held the commits",
                    p.summary.base
                ),
            }
        }
        PrCommand::Close { number } => {
            api.send(api.request(Method::POST, &pull_route(api, number, "/close")?))?;
            println!("closed #{number}");
        }
        PrCommand::Reopen { number } => {
            api.send(api.request(Method::POST, &pull_route(api, number, "/reopen")?))?;
            println!("reopened #{number}");
        }
        PrCommand::Comment {
            number,
            body,
            body_file,
        } => {
            let body =
                text(body, body_file)?.context("give the comment with --body or --body-file")?;
            api.send(
                api.request(Method::POST, &pull_route(api, number, "/comments")?)
                    .json(&api::CommentPullRequest { body }),
            )?;
            println!("commented on #{number}");
        }
    }
    Ok(())
}
