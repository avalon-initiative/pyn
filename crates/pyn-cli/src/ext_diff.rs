//! `diff.<name>.command`: a user-configured program run on the two sides of a changed path in place of the patch.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::diff::{self, Blob, Pair, Source};

struct Side {
    file: PathBuf,
    hex: String,
    mode: String,
    _dir: Option<tempfile::TempDir>,
}

/// A side as the program gets it: a work tree file as it is, anything else in a temporary file; `/dev/null` when absent.
fn side(backend: &dyn Backend, path: &str, blob: Option<&Blob>) -> Result<Side> {
    let Some(b) = blob else {
        return Ok(Side {
            file: "/dev/null".into(),
            hex: ".".into(),
            mode: ".".into(),
            _dir: None,
        });
    };
    let loose = matches!(b.source, Source::Worktree { .. } | Source::Local { .. });
    let hex = if loose {
        "0".repeat(b.hash.len())
    } else {
        b.hash.clone()
    };
    let mode = b.mode().to_string();
    if let Source::Worktree { file, .. } | Source::Local { file } = &b.source
        && b.is_regular()
    {
        let file = std::path::absolute(file).unwrap_or_else(|_| file.clone());
        return Ok(Side {
            file,
            hex,
            mode,
            _dir: None,
        });
    }
    let dir = tempfile::Builder::new()
        .prefix("pyn-blob-")
        .tempdir()
        .context("creating a file for the external diff")?;
    let name = Path::new(path).file_name().unwrap_or(path.as_ref());
    let file = dir.path().join(name);
    std::fs::write(&file, diff::blob_bytes(backend, path, b)?)?;
    Ok(Side {
        file,
        hex,
        mode,
        _dir: Some(dir),
    })
}

/// Runs `command` for `p` and returns what it printed; `total` is how many paths the diff lists and `meta` the
/// header lines of a rename or copy. The counter spans the whole run, as git's does across the commits of a `log`.
pub(crate) fn run(
    backend: &dyn Backend,
    command: &str,
    p: &Pair,
    (total, meta): (usize, &str),
) -> Result<String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let old = side(backend, &p.old_path, p.old.as_ref())?;
    let new = side(backend, &p.new_path, p.new.as_ref())?;
    let mut run = Command::new("sh");
    run.arg("-c")
        .arg(format!("{command} \"$@\""))
        .arg(command)
        .arg(if p.moved.is_some() {
            &p.old_path
        } else {
            &p.new_path
        })
        .arg(&old.file)
        .arg(&old.hex)
        .arg(&old.mode)
        .arg(&new.file)
        .arg(&new.hex)
        .arg(&new.mode);
    if p.moved.is_some() {
        run.arg(&p.new_path).arg(meta);
    }
    run.env("GIT_DIFF_PATH_COUNTER", n.to_string())
        .env("GIT_DIFF_PATH_TOTAL", total.to_string())
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    let out = run
        .output()
        .with_context(|| format!("running external diff `{command}`"))?;
    if !out.status.success() {
        let _ = std::io::Write::write_all(&mut std::io::stdout(), &out.stdout);
        bail!("external diff died, stopping at {}", p.new_path);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
