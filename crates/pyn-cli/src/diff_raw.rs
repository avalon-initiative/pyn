//! The `--raw` record: modes, ids and a status letter for one changed file.

use crate::diff::{Blob, Pair, Source, status_of};
use crate::diff_opts::Opts;

/// Whether git knows the blob's id: a work tree file or a dirty submodule has none.
pub(crate) fn id_known(b: &Blob) -> bool {
    !matches!(
        b.source,
        Source::Worktree { .. } | Source::Gitlink { dirty: true }
    )
}

/// The id as git records it, or zeros.
fn id(blob: Option<&Blob>, other: Option<&Blob>, o: &Opts, detached_change: bool) -> String {
    match blob.filter(|b| id_known(b) && !detached_change) {
        Some(b) => o.raw_id(&b.hash),
        None => {
            let len = blob.or(other).map_or(40, |b| b.hash.len());
            o.raw_id(&"0".repeat(len))
        }
    }
}

/// A change compared outside a repository whose two ids git does not know.
fn plain_change(p: &Pair) -> bool {
    p.detached && p.moved.is_none() && p.old.is_some() && p.new.is_some()
}

pub(crate) fn record(p: &Pair, o: &Opts, quote: &dyn Fn(&str) -> String) -> String {
    let mode = |b: &Option<Blob>| b.as_ref().map_or("000000", |b| b.mode()).to_string();
    let (a, b) = (p.old.as_ref(), p.new.as_ref());
    let head = format!(
        ":{} {} {} {} {}",
        mode(&p.old),
        mode(&p.new),
        id(a, b, o, plain_change(p)),
        id(b, a, o, plain_change(p)),
        status_of(p)
    );
    let (sep, end) = if o.nul { ('\0', '\0') } else { ('\t', '\n') };
    match &p.moved {
        Some(_) => format!(
            "{head}{sep}{}{sep}{}{end}",
            quote(&p.old_path),
            quote(&p.new_path)
        ),
        None if p.detached && p.old.is_some() => format!("{head}{sep}{}{end}", quote(&p.old_path)),
        None => format!("{head}{sep}{}{end}", quote(&p.new_path)),
    }
}
