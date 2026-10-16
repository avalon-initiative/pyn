//! Workspace files with their git file mode: the executable bit and symbolic links, which are tracked as their target.

use std::fs::Metadata;
use std::path::Path;

use anyhow::{Context, Result};
use pyn_proto as api;

use crate::backend::Backend;

/// A regular file or a symbolic link; symlinks are never followed.
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
}

pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

#[cfg(unix)]
fn executable(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_: &Metadata) -> bool {
    false
}

pub fn mode_of(path: &Path) -> Result<api::FileMode> {
    let meta =
        std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(if meta.file_type().is_symlink() {
        api::FileMode::Symlink
    } else if executable(&meta) {
        api::FileMode::Executable
    } else {
        api::FileMode::Regular
    })
}

#[cfg(unix)]
fn target_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::fs::read_link(path)?.into_os_string().into_vec())
}

#[cfg(not(unix))]
fn target_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read_link(path)?
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

/// The file's mode and the bytes pyn stores for it: its content, or a symlink's target.
pub fn read(path: &Path) -> Result<(api::FileMode, Vec<u8>)> {
    let mode = mode_of(path)?;
    let bytes = if mode == api::FileMode::Symlink {
        target_bytes(path)
    } else {
        std::fs::read(path).map_err(Into::into)
    };
    Ok((
        mode,
        bytes.with_context(|| format!("reading {}", path.display()))?,
    ))
}

/// Uploads the file (or a symlink's target) and returns its mode and blob id.
pub fn upload(backend: &dyn Backend, path: &Path) -> Result<(api::FileMode, String)> {
    let mode = mode_of(path)?;
    let id = if mode == api::FileMode::Symlink {
        backend.upload_bytes(target_bytes(path)?)?
    } else {
        backend.upload(path)?
    };
    Ok((mode, id))
}

/// Gives a downloaded file its mode: sets or clears the executable bit, or turns it into the symlink it stands for.
#[cfg(unix)]
pub fn apply(path: &Path, link: &str, mode: api::FileMode) -> Result<()> {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    match mode {
        api::FileMode::Symlink => {
            let target = std::fs::read(path)?;
            if let Err(e) = pyn_gitwork::check_link_target(link.as_bytes(), &target) {
                let _ = std::fs::remove_file(path);
                return Err(e.into());
            }
            let target = std::ffi::OsString::from_vec(target);
            std::fs::remove_file(path)?;
            std::os::unix::fs::symlink(target, path)
                .with_context(|| format!("linking {}", path.display()))?;
        }
        _ => {
            let meta = std::fs::metadata(path)?;
            let mut perms = meta.permissions();
            let bits = perms.mode();
            perms.set_mode(if mode == api::FileMode::Executable {
                bits | 0o111
            } else {
                bits & !0o111
            });
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn apply(_: &Path, _: &str, _: api::FileMode) -> Result<()> {
    Ok(())
}

/// Whether the mode on disk differs from the recorded one; only systems with an executable bit can tell.
pub fn mode_differs(recorded: api::FileMode, on_disk: api::FileMode) -> bool {
    cfg!(unix) && recorded != on_disk
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn symlinks_are_read_as_their_target_and_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("l");
        std::os::unix::fs::symlink("missing/target", &link).unwrap();
        assert!(exists(&link) && is_symlink(&link));
        let (mode, bytes) = read(&link).unwrap();
        assert_eq!(
            (mode, bytes),
            (api::FileMode::Symlink, b"missing/target".to_vec())
        );
    }

    #[test]
    fn apply_sets_the_executable_bit_and_makes_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("run.sh");
        std::fs::write(&f, "#!/bin/sh\n").unwrap();
        apply(&f, "run.sh", api::FileMode::Executable).unwrap();
        assert_eq!(mode_of(&f).unwrap(), api::FileMode::Executable);
        apply(&f, "run.sh", api::FileMode::Regular).unwrap();
        assert_eq!(mode_of(&f).unwrap(), api::FileMode::Regular);

        let l = dir.path().join("l");
        std::fs::write(&l, "run.sh").unwrap();
        apply(&l, "l", api::FileMode::Symlink).unwrap();
        assert_eq!(mode_of(&l).unwrap(), api::FileMode::Symlink);
        assert_eq!(std::fs::read_link(&l).unwrap(), Path::new("run.sh"));
    }
}
