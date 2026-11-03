//! `pyn check-attr` on a git repository: the attributes that apply to paths, from the work tree, the index or a tree.

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::{AttrSource, AttrValue, Attributes, valid_attribute_name};
use pyn_object::Kind;

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, peel, prefix, quote, usage};

/// Show the attributes that apply to paths.
#[derive(Debug, Args)]
pub struct CheckAttrArgs {
    /// Report every attribute that is set on each path.
    #[arg(short = 'a', long)]
    all: bool,
    /// Read `.gitattributes` from the index, not the work tree.
    #[arg(long)]
    cached: bool,
    /// Read the paths from standard input.
    #[arg(long)]
    stdin: bool,
    /// End each input and output record with a NUL and do not quote names.
    #[arg(short = 'z')]
    nul: bool,
    /// Read `.gitattributes` from this tree-ish, not the work tree.
    #[arg(long, value_name = "TREE-ISH")]
    source: Option<String>,
    /// Attribute names, then (after `--`) paths; without `--` the first is the attribute, the rest paths.
    #[arg(value_name = "ATTR")]
    rest: Vec<String>,
    #[arg(last = true, value_name = "PATH")]
    paths: Vec<String>,
}

pub(super) fn check_attr(git: &GitHome, cwd: &Path, args: CheckAttrArgs) -> Result<()> {
    let dashes = !args.paths.is_empty();
    let (names, files): (Vec<String>, Vec<String>) = if args.all {
        if dashes && !args.rest.is_empty() {
            usage("Attributes and --all both specified");
        }
        (Vec::new(), [args.rest.clone(), args.paths.clone()].concat())
    } else if dashes {
        if args.rest.is_empty() {
            usage("No attribute specified");
        }
        (args.rest.clone(), args.paths.clone())
    } else if args.rest.is_empty() {
        usage("No attribute specified");
    } else if args.stdin {
        (args.rest.clone(), Vec::new())
    } else {
        (args.rest[..1].to_vec(), args.rest[1..].to_vec())
    };
    if args.stdin {
        if !files.is_empty() {
            usage("Can't specify files with --stdin");
        }
    } else if files.is_empty() {
        usage("No file specified");
    }
    for name in &names {
        if !valid_attribute_name(name.as_bytes()) {
            bail!("{name} is not a valid attribute name");
        }
    }

    let attrs = engine(git, cwd, &args)?;
    let mut out: Vec<u8> = Vec::new();
    let mut each = |shown: &[u8]| -> Result<()> {
        let path = repo_path(git, cwd, shown)?;
        let found = attrs.for_path(&path);
        let rows: Vec<(String, AttrValue)> = if args.all {
            found
                .iter()
                .map(|(n, v)| (n.to_owned(), v.clone()))
                .collect()
        } else {
            names.iter().map(|n| (n.clone(), found.get(n))).collect()
        };
        for (name, value) in rows {
            record(git, &mut out, shown, &name, &value, args.nul);
        }
        Ok(())
    };
    if args.stdin {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input)?;
        let sep = if args.nul { 0 } else { b'\n' };
        let mut records: Vec<&[u8]> = input.split(|b| *b == sep).collect();
        if records.last().is_some_and(|r| r.is_empty()) {
            records.pop();
        }
        for record in records {
            if args.nul {
                each(record)?;
                continue;
            }
            let record = record.strip_suffix(b"\r").unwrap_or(record);
            match record.first() {
                Some(b'"') => match pyn_gitwork::unquote_c(record) {
                    Some((path, _)) => each(&path)?,
                    None => bail!("line is badly quoted"),
                },
                _ => each(record)?,
            }
        }
    } else {
        for file in &files {
            each(file.as_bytes())?;
        }
    }
    emit_bytes(&out);
    Ok(())
}

fn engine(git: &GitHome, cwd: &Path, args: &CheckAttrArgs) -> Result<Arc<Attributes>> {
    let source = if args.cached && !git.is_bare() {
        AttrSource::Index
    } else if let Some(spec) = &args.source {
        let pre = prefix(git, cwd).unwrap_or_default();
        let resolver = Resolver { git, prefix: &pre };
        let tree = match resolver.object(spec)? {
            Some(id) => peel(git, &id, Some(Kind::Tree))?,
            None => None,
        };
        match tree {
            Some(tree) => AttrSource::Tree(tree),
            None => {
                eprintln!("fatal: {spec}: not a valid tree-ish source");
                std::process::exit(128);
            }
        }
    } else {
        return Ok(git.repo.attributes()?);
    };
    Ok(Arc::new(git.repo.attributes_from(&source)?))
}

/// The path from the repository root; a bare repository has no folder to be in.
fn repo_path(git: &GitHome, cwd: &Path, arg: &[u8]) -> Result<Vec<u8>> {
    if git.is_bare() {
        return Ok(arg.to_vec());
    }
    let text = String::from_utf8_lossy(arg);
    Ok(git.ws.repo_prefix(cwd, &text)?.into_bytes())
}

fn record(git: &GitHome, out: &mut Vec<u8>, path: &[u8], name: &str, value: &AttrValue, nul: bool) {
    let info: &[u8] = match value {
        AttrValue::Set => b"set",
        AttrValue::Unset => b"unset",
        AttrValue::Unspecified => b"unspecified",
        AttrValue::Value(v) => v,
    };
    if nul {
        out.extend_from_slice(path);
        out.push(0);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(info);
        out.push(0);
    } else {
        out.extend_from_slice(quote(git, path).as_bytes());
        out.extend_from_slice(format!(": {name}: ").as_bytes());
        out.extend_from_slice(info);
        out.push(b'\n');
    }
}
