use std::path::Path;

use pyn_object::Algorithm;

use crate::config_file::ConfigFile;
use crate::error::{Result, io_at};

/// Creates an empty repository in `git_dir`: object, ref, hook and info folders, `description`, `HEAD` on `branch` and a minimal config.
pub fn init(git_dir: &Path, bare: bool, algorithm: Algorithm, branch: &str) -> Result<()> {
    for sub in [
        "objects/info",
        "objects/pack",
        "refs/heads",
        "refs/tags",
        "hooks",
        "info",
    ] {
        let dir = git_dir.join(sub);
        std::fs::create_dir_all(&dir).map_err(io_at(&dir))?;
    }
    let description = git_dir.join("description");
    if !description.exists() {
        std::fs::write(
            &description,
            "Unnamed repository; edit this file 'description' to name the repository.\n",
        )
        .map_err(io_at(&description))?;
    }
    let head = git_dir.join("HEAD");
    std::fs::write(&head, format!("ref: refs/heads/{branch}\n")).map_err(io_at(&head))?;
    let mut config = ConfigFile::default();
    let sha256 = algorithm == Algorithm::Sha256;
    let version = if sha256 { "1" } else { "0" };
    config.set("core", None, "repositoryformatversion", version)?;
    config.set("core", None, "filemode", &cfg!(unix).to_string())?;
    config.set("core", None, "bare", &bare.to_string())?;
    if !bare {
        config.set("core", None, "logallrefupdates", "true")?;
    }
    if sha256 {
        config.set("extensions", None, "objectformat", algorithm.name())?;
    }
    config.write(&git_dir.join("config"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Repository;

    #[test]
    fn an_initialized_directory_opens_as_an_empty_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("w");
        init(&work.join(".git"), false, Algorithm::Sha256, "trunk").unwrap();
        let repo = Repository::open(&work).unwrap();
        assert_eq!(repo.algorithm(), Algorithm::Sha256);
        assert_eq!(repo.work_tree(), Some(work.as_path()));
        let bare = tmp.path().join("b.git");
        init(&bare, true, Algorithm::Sha1, "main").unwrap();
        let repo = Repository::open(&bare).unwrap();
        assert!(repo.work_tree().is_none());
        assert_eq!(repo.algorithm(), Algorithm::Sha1);
    }
}
