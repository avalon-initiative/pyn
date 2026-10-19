//! The `repo_tool` marker: which tool a checkout acts as, `git` or `pyn`. It lives in `.pyn/local_only/repo.toml`,
//! per checkout and never versioned. Absent means `pyn`.

use std::fmt;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoTool {
    Git,
    Pyn,
}

impl fmt::Display for RepoTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Git => "git",
            Self::Pyn => "pyn",
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerFile {
    repo_tool: String,
}

fn parse(text: &str) -> Result<RepoTool> {
    let file: MarkerFile = toml::from_str(text)?;
    match file.repo_tool.as_str() {
        "git" => Ok(RepoTool::Git),
        "pyn" => Ok(RepoTool::Pyn),
        other => bail!("unknown repo_tool {other:?}; it is git or pyn"),
    }
}

/// The marker, or `None` when the checkout has none.
pub fn read(ws: &Workspace) -> Result<Option<RepoTool>> {
    let path = ws.repo_tool_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text)
            .map(Some)
            .with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Records the marker, leaving the file alone when it already says so.
pub fn set(ws: &Workspace, tool: RepoTool) -> Result<()> {
    if read(ws)? == Some(tool) {
        return Ok(());
    }
    let path = ws.repo_tool_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("repo_tool = \"{tool}\"\n"))
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(dir: &std::path::Path) -> Workspace {
        Workspace {
            root: dir.to_path_buf(),
            dir: dir.join(".pyn"),
        }
    }

    #[test]
    fn no_file_means_no_marker() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read(&ws(tmp.path())).unwrap(), None);
    }

    #[test]
    fn a_marker_round_trips_and_can_change() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = ws(tmp.path());
        set(&ws, RepoTool::Git).unwrap();
        assert_eq!(read(&ws).unwrap(), Some(RepoTool::Git));
        assert_eq!(
            std::fs::read_to_string(ws.repo_tool_path()).unwrap(),
            "repo_tool = \"git\"\n"
        );
        set(&ws, RepoTool::Pyn).unwrap();
        assert_eq!(read(&ws).unwrap(), Some(RepoTool::Pyn));
    }

    #[test]
    fn an_unknown_value_or_key_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = ws(tmp.path());
        std::fs::create_dir_all(ws.dir.join("local_only")).unwrap();
        for text in [
            "repo_tool = \"svn\"\n",
            "tool = \"git\"\n",
            "repo_tool = 1\n",
        ] {
            std::fs::write(ws.repo_tool_path(), text).unwrap();
            let err = format!("{:#}", read(&ws).unwrap_err());
            assert!(err.contains("repo.toml"), "{err}");
        }
        std::fs::write(ws.repo_tool_path(), "repo_tool = \"svn\"\n").unwrap();
        assert!(
            format!("{:#}", read(&ws).unwrap_err())
                .contains("unknown repo_tool \"svn\"; it is git or pyn")
        );
    }
}
