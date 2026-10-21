//! `branch -u` and `--unset-upstream` on a git repository: the `branch.<name>.remote` and `merge` settings git writes.

use anyhow::{Result, bail};
use pyn_git::ConfigFile;
use pyn_gitwork::Head;

use super::GitHome;
use super::refwrite::BRANCHES;

const REMOTES: &str = "refs/remotes/";

fn config_path(git: &GitHome) -> std::path::PathBuf {
    git.repo.git().common_dir().join("config")
}

/// The branch the command acts on: the one named, else the current one.
fn target(git: &GitHome, names: &[String], doing: &str) -> Result<(String, bool)> {
    match names.first() {
        Some(name) => Ok((name.clone(), false)),
        None => match git.repo.head()? {
            Head::Branch { name, .. } => Ok((name, true)),
            Head::Detached(_) => bail!("{doing} when it does not point to any branch"),
        },
    }
}

/// The full ref `spec` names, tried the way git's `dwim_ref` does.
fn dwim(git: &GitHome, spec: &str) -> Result<Option<String>> {
    if spec == "HEAD" || spec == "@" {
        return git.head_target();
    }
    for form in [
        spec.to_string(),
        format!("refs/{spec}"),
        format!("refs/tags/{spec}"),
        format!("{BRANCHES}{spec}"),
        format!("{REMOTES}{spec}"),
        format!("{REMOTES}{spec}/HEAD"),
    ] {
        if form.starts_with("refs/")
            && pyn_git::check_ref_name(&form).is_ok()
            && git.repo.refs().resolve(&form)?.is_some()
        {
            return Ok(Some(form));
        }
    }
    Ok(None)
}

/// The remote and the ref on it that a fetch refspec maps onto `local`.
fn remote_source(file: &ConfigFile, local: &str) -> Option<(String, String)> {
    for remote in file.subsections("remote") {
        for spec in file.values("remote", Some(&remote), "fetch") {
            let spec = spec.strip_prefix('+').unwrap_or(&spec);
            let Some((src, dst)) = spec.split_once(':') else {
                continue;
            };
            let found = match (src.split_once('*'), dst.split_once('*')) {
                (Some((sp, ss)), Some((dp, ds))) => local
                    .strip_prefix(dp)
                    .and_then(|rest| rest.strip_suffix(ds))
                    .map(|mid| format!("{sp}{mid}{ss}")),
                (None, None) if dst == local => Some(src.to_string()),
                _ => None,
            };
            if let Some(source) = found {
                return Some((remote, source));
            }
        }
    }
    None
}

fn fail_missing(spec: &str, hints: bool) -> ! {
    eprintln!("fatal: the requested upstream branch '{spec}' does not exist");
    if hints {
        eprint!(
            "hint: \n\
             hint: If you are planning on basing your work on an upstream\n\
             hint: branch that already exists at the remote, you may need to\n\
             hint: run \"git fetch\" to retrieve it.\n\
             hint: \n\
             hint: If you are planning to push out a new local branch that\n\
             hint: will track its remote counterpart, you may want to use\n\
             hint: \"git push -u\" to set the upstream config as you push.\n\
             hint: Disable this message with \"git config advice.setUpstreamFailure false\"\n"
        );
    }
    std::process::exit(128);
}

fn replace(file: &mut ConfigFile, branch: &str, key: &str, value: &str) -> Result<()> {
    if file.set("branch", Some(branch), key, value).is_err() {
        file.unset_all("branch", Some(branch), key);
        file.add("branch", Some(branch), key, value)?;
    }
    Ok(())
}

pub fn set_upstream(git: &GitHome, upstream: &str, names: &[String]) -> Result<()> {
    if names.len() > 1 {
        bail!("too many arguments to set new upstream");
    }
    let (branch, is_current) = target(
        git,
        names,
        &format!("could not set upstream of HEAD to {upstream}"),
    )?;
    if git.branch_commit(&branch)?.is_none() {
        if is_current || git.checked_out_path(&branch)?.is_some() {
            bail!("no commit on branch '{branch}' yet");
        }
        bail!("branch '{branch}' does not exist");
    }
    let path = config_path(git);
    let mut file = ConfigFile::read(&path)?;
    let Some(full) = dwim(git, upstream)? else {
        let hints = git
            .repo
            .config()
            .get("advice.setupstreamfailure")
            .is_none_or(|v| v != "false");
        fail_missing(upstream, hints);
    };
    let (remote, merge, shown) = if let Some(local) = full.strip_prefix(BRANCHES) {
        if local == branch {
            eprintln!("warning: not setting branch '{branch}' as its own upstream");
            return Ok(());
        }
        (".".to_string(), full.clone(), local.to_string())
    } else if let Some(short) = full.strip_prefix(REMOTES)
        && let Some((remote, source)) = remote_source(&file, &full)
    {
        (remote, source, short.to_string())
    } else {
        bail!("cannot set up tracking information; starting point '{upstream}' is not a branch");
    };
    replace(&mut file, &branch, "remote", &remote)?;
    replace(&mut file, &branch, "merge", &merge)?;
    file.write(&path)?;
    println!("branch '{branch}' set up to track '{shown}'.");
    Ok(())
}

pub fn unset_upstream(git: &GitHome, names: &[String]) -> Result<()> {
    if names.len() > 1 {
        bail!("too many arguments to unset upstream");
    }
    let (branch, _) = target(git, names, "could not unset upstream of HEAD")?;
    let path = config_path(git);
    let mut file = ConfigFile::read(&path)?;
    if file.values("branch", Some(&branch), "merge").is_empty() {
        bail!("branch '{branch}' has no upstream information");
    }
    file.unset_all("branch", Some(&branch), "remote");
    file.unset_all("branch", Some(&branch), "merge");
    file.write(&path)?;
    Ok(())
}
