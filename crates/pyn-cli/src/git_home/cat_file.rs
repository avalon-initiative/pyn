//! `pyn cat-file` on a git repository: the type, size and content of an object.

use std::io::BufRead;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{FileMode, Kind, ObjectId};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, peel, prefix, quote, usage};

/// Show an object's type, size or content.
#[derive(Debug, Args)]
pub struct CatFileArgs {
    /// Show the object's type.
    #[arg(short = 't')]
    show_type: bool,
    /// Show the object's size in bytes.
    #[arg(short = 's')]
    show_size: bool,
    /// Show the object's content prettily: a tree as a listing.
    #[arg(short = 'p')]
    pretty: bool,
    /// Exit with 0 if the object exists and 1 if not, printing nothing.
    #[arg(short = 'e')]
    exists: bool,
    /// Read object names from standard input and show `<id> <type> <size>` and the content of each.
    #[arg(long)]
    batch: bool,
    /// Read object names from standard input and show `<id> <type> <size>` for each.
    #[arg(long)]
    batch_check: bool,
    /// `<object>` with one of the options, or `<type> <object>` to show the raw content as that type.
    #[arg(value_name = "ARG")]
    args: Vec<String>,
}

pub(super) fn cat_file(git: &GitHome, cwd: &Path, args: CatFileArgs) -> Result<()> {
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    if args.batch || args.batch_check {
        if !args.args.is_empty() {
            usage("batch modes take no object argument");
        }
        return batch(git, &resolver, args.batch);
    }
    let flags = [args.show_type, args.show_size, args.pretty, args.exists]
        .iter()
        .filter(|f| **f)
        .count();
    let (kind, name) = match (flags, args.args.as_slice()) {
        (1, [name]) => (None, name),
        (0, [kind, name]) => (Some(kind), name),
        _ => usage("usage: cat-file (-t | -s | -p | -e | <type>) <object>"),
    };
    let Some(id) = resolver.object(name)? else {
        bail!("Not a valid object name {name}");
    };
    let Some(object) = git.repo.objects().read_opt(&id)? else {
        if args.exists {
            std::process::exit(1);
        }
        bail!("git cat-file: could not get object info");
    };
    if args.exists {
        return Ok(());
    }
    if args.show_type {
        emit_bytes(format!("{}\n", object.kind.name()).as_bytes());
    } else if args.show_size {
        emit_bytes(format!("{}\n", object.data.len()).as_bytes());
    } else if args.pretty {
        emit_bytes(&pretty(git, &id, &object)?);
    } else if let Some(kind) = kind {
        let Some(want) = Kind::from_name(kind.as_bytes()) else {
            bail!("invalid object type \"{kind}\"");
        };
        let Some(found) = peel(git, &id, Some(want))? else {
            bail!("git cat-file {name}: bad file");
        };
        emit_bytes(&git.repo.objects().read(&found)?.data);
    }
    Ok(())
}

fn pretty(git: &GitHome, id: &ObjectId, object: &pyn_git::Object) -> Result<Vec<u8>> {
    if object.kind != Kind::Tree {
        return Ok(object.data.clone());
    }
    let mut out = Vec::new();
    for e in git.repo.tree(id)? {
        let kind = match e.mode {
            FileMode::Tree => "tree",
            FileMode::Gitlink => "commit",
            _ => "blob",
        };
        let mode = u32::from_str_radix(e.mode.as_str(), 8).unwrap_or(0);
        out.extend_from_slice(
            format!(
                "{mode:06o} {kind} {}\t{}\n",
                e.id.to_hex(),
                quote(git, &e.name)
            )
            .as_bytes(),
        );
    }
    Ok(out)
}

fn batch(git: &GitHome, resolver: &Resolver, contents: bool) -> Result<()> {
    let mut out = Vec::new();
    for line in std::io::stdin().lock().split(b'\n') {
        let line = line?;
        let text = String::from_utf8_lossy(&line).into_owned();
        let found = match resolver.object(&text)? {
            Some(id) => git.repo.objects().read_opt(&id)?.map(|o| (id, o)),
            None => None,
        };
        match found {
            Some((id, o)) => {
                out.extend_from_slice(
                    format!("{} {} {}\n", id.to_hex(), o.kind.name(), o.data.len()).as_bytes(),
                );
                if contents {
                    out.extend_from_slice(&o.data);
                    out.push(b'\n');
                }
            }
            None => out.extend_from_slice(format!("{text} missing\n").as_bytes()),
        }
    }
    emit_bytes(&out);
    Ok(())
}
