//! Protocol v0 receive-pack: ref commands and a pack applied to a repository, answered with a report-status.

use std::io::Read;
use std::path::PathBuf;

use pyn_git::{
    Config, Expect, IngestLimits, Ref, Repository, check_ref_name, commits_newest_first, existing,
    index_pack, reachable_objects,
};
use pyn_gitwire::Error as Wire;
use pyn_gitwire::caps::{AGENT, Capabilities};
use pyn_gitwire::pkt::{Packet, PktReader, PktWriter, text};

use crate::Result;
use pyn_object::{Kind, ObjectId};

use super::{BandWriter, parse_id, refuse_hidden_refs, zero_id};

const HIDDEN: [&str; 2] = ["receive.hiderefs", "transfer.hiderefs"];
const HOOKS: [&str; 6] = [
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
    "push-to-checkout",
    "reference-transaction",
];

const UNCONFIGURED_CHECKED_OUT: &str = "By default, updating the current branch in a non-bare repository\nis denied, because it will make the index and work tree inconsistent\nwith what you pushed, and will require 'git reset --hard' to match\nthe work tree to HEAD.\n\nYou can set the 'receive.denyCurrentBranch' configuration variable\nto 'ignore' or 'warn' in the remote repository to allow pushing into\nits current branch; however, this is not recommended unless you\narranged to update its work tree to match what you pushed in some\nother way.\n\nTo squelch this message and still keep the default behaviour, set\n'receive.denyCurrentBranch' configuration variable to 'refuse'.";
const UNCONFIGURED_DELETE_CURRENT: &str = "By default, deleting the current branch is denied, because the next\n'git clone' won't result in any file checked out, causing confusion.\n\nYou can set 'receive.denyDeleteCurrent' configuration variable to\n'warn' or 'ignore' in the remote repository to allow deleting the\ncurrent branch, with or without a warning message.\n\nTo squelch this message, you can set it to 'refuse'.";

pub(super) fn advertisement(repo: &Repository, config: &Config) -> Result<Vec<u8>> {
    refuse_hidden_refs(config, &HIDDEN)?;
    let caps = format!(
        "report-status delete-refs side-band-64k quiet atomic ofs-delta object-format={} {AGENT}",
        repo.algorithm().name()
    );
    let mut refs: Vec<(String, ObjectId)> = repo.refs().list("refs/")?;
    if refs.is_empty() {
        refs.push(("capabilities^{}".into(), zero_id(repo.algorithm())));
    }
    let mut w = PktWriter::new(Vec::new());
    for (i, (name, id)) in refs.iter().enumerate() {
        if i == 0 {
            w.write_data(format!("{id} {name}\0{caps}\n").as_bytes())?;
        } else {
            w.write_line(&format!("{id} {name}"))?;
        }
    }
    w.write_flush()?;
    Ok(w.into_inner())
}

struct Command {
    old: ObjectId,
    new: ObjectId,
    name: String,
}

fn parse_commands(
    pkts: &mut PktReader<&mut dyn Read>,
    repo: &Repository,
) -> Result<(Vec<Command>, Capabilities)> {
    let alg = repo.algorithm();
    let mut caps = Capabilities::default();
    let mut commands = Vec::new();
    loop {
        let data = match pkts.read()? {
            Packet::Data(d) => d,
            Packet::Flush => return Ok((commands, caps)),
            other => return Err(Wire::Protocol(format!("unexpected {other:?} in commands")).into()),
        };
        let (head, tail) = match data.iter().position(|b| *b == 0) {
            Some(i) => (&data[..i], Some(&data[i + 1..])),
            None => (&data[..], None),
        };
        if let Some(t) = tail
            && commands.is_empty()
        {
            caps = Capabilities::parse_tokens(text(t)?);
        }
        let line = text(head)?;
        let mut parts = line.splitn(3, ' ');
        let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Wire::Remote(format!(
                "protocol error: expected old/new/ref, got '{line}'"
            ))
            .into());
        };
        commands.push(Command {
            old: parse_id(alg, old)?,
            new: parse_id(alg, new)?,
            name: name.to_owned(),
        });
    }
}

fn hook_dir(repo: &Repository, config: &Config) -> PathBuf {
    match config.get("core.hookspath").filter(|p| !p.is_empty()) {
        Some(p) => repo.common_dir().join(p),
        None => repo.common_dir().join("hooks"),
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Settings and hooks that change what a push does but are not run here.
fn refuse_unsupported(repo: &Repository, config: &Config) -> Result<()> {
    let dir = hook_dir(repo, config);
    if let Some(h) = HOOKS.iter().find(|h| is_executable(&dir.join(h))) {
        return Err(Wire::Unsupported(format!("running the {h} hook")).into());
    }
    for key in ["receive.fsckobjects", "transfer.fsckobjects"] {
        if config.get_bool(key).ok().flatten() == Some(true) {
            return Err(Wire::Unsupported(format!("the {key} setting")).into());
        }
    }
    let update_instead = config
        .get("receive.denycurrentbranch")
        .is_some_and(|v| v.eq_ignore_ascii_case("updateinstead"));
    if update_instead && repo.work_tree().is_some() {
        return Err(Wire::Unsupported("receive.denyCurrentBranch=updateInstead".into()).into());
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
enum Deny {
    Unconfigured,
    Ignore,
    Warn,
    Refuse,
}

fn deny_action(config: &Config, key: &str) -> Deny {
    match config.get(key) {
        None => Deny::Unconfigured,
        Some(v) if v.eq_ignore_ascii_case("ignore") => Deny::Ignore,
        Some(v) if v.eq_ignore_ascii_case("warn") => Deny::Warn,
        Some(v) if v.eq_ignore_ascii_case("refuse") => Deny::Refuse,
        Some(_) => match config.get_bool(key) {
            Ok(Some(false)) => Deny::Ignore,
            _ => Deny::Refuse,
        },
    }
}

struct Context<'a> {
    repo: &'a Repository,
    config: &'a Config,
    current: Option<String>,
    messages: Vec<String>,
}

impl Context<'_> {
    fn error(&mut self, m: impl std::fmt::Display) {
        self.messages.push(format!("error: {m}"));
    }

    /// The reason a command is refused before anything changes, or `None`.
    fn check(&mut self, c: &Command) -> Result<Option<&'static str>> {
        let alg = self.repo.algorithm();
        let (create, delete) = (c.old == zero_id(alg), c.new == zero_id(alg));
        if !c.name.starts_with("refs/") || check_ref_name(&c.name).is_err() {
            self.error(format!(
                "refusing to create funny ref '{}' remotely",
                c.name
            ));
            return Ok(Some("funny refname"));
        }
        let db = self.repo.objects()?;
        let is_current = self.current.as_deref() == Some(c.name.as_str());
        if is_current && self.repo.work_tree().is_some() {
            match deny_action(self.config, "receive.denycurrentbranch") {
                Deny::Ignore => {}
                Deny::Warn => self
                    .messages
                    .push("warning: updating the current branch".into()),
                action => {
                    self.error(format!("refusing to update checked out branch: {}", c.name));
                    if action == Deny::Unconfigured {
                        self.error(UNCONFIGURED_CHECKED_OUT);
                    }
                    return Ok(Some("branch is currently checked out"));
                }
            }
        }
        if !delete && !db.contains(&c.new)? {
            self.error(format!(
                "unpack should have generated {}, but I can't find it!",
                c.new
            ));
            return Ok(Some("bad pack"));
        }
        if delete {
            if c.name.starts_with("refs/heads/")
                && self.config.get_bool("receive.denydeletes").ok().flatten() == Some(true)
            {
                self.error(format!("denying ref deletion for {}", c.name));
                return Ok(Some("deletion prohibited"));
            }
            if is_current {
                match deny_action(self.config, "receive.denydeletecurrent") {
                    Deny::Ignore => {}
                    Deny::Warn => self
                        .messages
                        .push("warning: deleting the current branch".into()),
                    action => {
                        if action == Deny::Unconfigured {
                            self.error(UNCONFIGURED_DELETE_CURRENT);
                        }
                        self.error(format!("refusing to delete the current branch: {}", c.name));
                        return Ok(Some("deletion of the current branch prohibited"));
                    }
                }
            }
            return Ok(None);
        }
        let heads = c.name.starts_with("refs/heads/");
        if !create
            && heads
            && self
                .config
                .get_bool("receive.denynonfastforwards")
                .ok()
                .flatten()
                == Some(true)
            && !(db.contains(&c.old)?
                && commits_newest_first(&db, &[c.new], usize::MAX)?.contains(&c.old))
        {
            self.error(format!(
                "denying non-fast-forward {} (you should pull first)",
                c.name
            ));
            return Ok(Some("non-fast-forward"));
        }
        if heads && db.read(&c.new)?.kind != Kind::Commit {
            self.error(format!(
                "trying to write non-commit object {} to branch '{}'",
                c.new, c.name
            ));
            return Ok(Some("failed to update ref"));
        }
        let tips: Vec<ObjectId> = self
            .repo
            .refs()
            .list("refs/")?
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        if reachable_objects(&db, &[c.new], &existing(&db, &tips)?).is_err() {
            self.error(format!("missing necessary objects for {}", c.name));
            return Ok(Some("missing necessary objects"));
        }
        Ok(None)
    }

    fn apply(&mut self, c: &Command) -> Option<&'static str> {
        let alg = self.repo.algorithm();
        let expect = if c.old == zero_id(alg) {
            Expect::Absent
        } else {
            Expect::Id(c.old)
        };
        let refs = self.repo.refs();
        let result = if c.new == zero_id(alg) {
            refs.delete(&c.name, expect)
        } else {
            refs.update_checked(&c.name, expect, &c.new)
        };
        match result {
            Ok(()) => None,
            Err(e) => {
                self.error(format!("cannot update ref '{}': {e}", c.name));
                Some("failed to update ref")
            }
        }
    }
}

pub(super) fn serve(
    repo: &Repository,
    config: &Config,
    body: &mut dyn Read,
) -> Result<Box<dyn Read>> {
    refuse_unsupported(repo, config)?;
    let alg = repo.algorithm();
    let mut pkts = PktReader::new(body);
    let (commands, caps) = parse_commands(&mut pkts, repo)?;
    if commands.is_empty() {
        return Ok(Box::new(std::io::empty()));
    }
    let unpack = if commands.iter().any(|c| c.new != zero_id(alg)) {
        let db = repo.objects()?;
        let limits = IngestLimits::from_config(config)?;
        index_pack(&db, pkts.into_inner(), &limits)
            .map(|_| ())
            .map_err(|e| e.to_string())
    } else {
        Ok(())
    };
    let current = match repo.refs().head() {
        Ok(Ref::Symbolic(t)) => Some(t),
        _ => None,
    };
    let mut cx = Context {
        repo,
        config,
        current,
        messages: Vec::new(),
    };
    let mut results: Vec<Option<&'static str>> = Vec::new();
    match &unpack {
        Err(_) => results.resize(commands.len(), Some("unpacker error")),
        Ok(()) => {
            for c in &commands {
                results.push(cx.check(c)?);
            }
            if caps.has("atomic") && results.iter().any(Option::is_some) {
                for r in results.iter_mut().filter(|r| r.is_none()) {
                    *r = Some("atomic push failure");
                }
            }
            for (c, r) in commands.iter().zip(results.iter_mut()) {
                if r.is_none() {
                    *r = cx.apply(c);
                }
            }
        }
    }
    let mut report = PktWriter::new(Vec::new());
    match &unpack {
        Ok(()) => report.write_line("unpack ok")?,
        Err(e) => report.write_line(&format!("unpack {e}"))?,
    }
    for (c, r) in commands.iter().zip(&results) {
        match r {
            None => report.write_line(&format!("ok {}", c.name))?,
            Some(why) => report.write_line(&format!("ng {} {why}", c.name))?,
        }
    }
    report.write_flush()?;
    let report = report.into_inner();
    if !caps.has("side-band-64k") {
        return Ok(Box::new(std::io::Cursor::new(report)));
    }
    use std::io::Write;
    let mut out = Vec::new();
    for m in &cx.messages {
        let mut band = BandWriter::new(&mut out, 2);
        band.write_all(format!("{m}\n").as_bytes())?;
        band.finish()?;
    }
    let mut band = BandWriter::new(&mut out, 1);
    band.write_all(&report)?;
    band.finish()?;
    out.extend_from_slice(b"0000");
    Ok(Box::new(std::io::Cursor::new(out)))
}
