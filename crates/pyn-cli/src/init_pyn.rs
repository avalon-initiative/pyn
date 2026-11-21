//! `pyn init`: starts a pyn server-home repository in a folder that is not tracked yet (#570, decision #562).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

use crate::client::Api;
use crate::repo_tool::{self, RepoTool};
use crate::workspace::{Settings, Workspace, resolve};
use crate::{RepoCommand, address, credentials, git_home, http_transport, userconfig};

/// The starter policy; a test keeps it equal to the server's `DEFAULT_INITIAL_POLICY`.
pub(crate) const POLICY: &str = r#"# Collaboration policy: which paths are shared (merged, edited concurrently) and which are exclusive
# (one editor at a time, through a lock). A path no entry covers uses `default`.
# Most specific wins: an exact file, then a glob, then a folder (the deepest folder wins).
# Between two globs, exclusive wins. See docs/concepts/pyn-toml.md.
[meta]
default = "shared"

# File kinds that cannot be merged are exclusive. Add or remove entries as your project needs.
[exclusive]
paths = [
  # Policy for this file itself: an exact entry beats the `**/*.toml` glob below.
  ".pyn/pyn.toml",

  # 3D models and art
  "**/*.obj",
  "**/*.fbx",
  "**/*.blend",
  "**/*.glb",
  "**/*.psd",
  "**/*.kra",
  "**/*.ai",

  # Images
  "**/*.png",
  "**/*.jpg",
  "**/*.jpeg",
  "**/*.tga",
  "**/*.exr",
  "**/*.tif",
  "**/*.tiff",

  # Audio and video
  "**/*.wav",
  "**/*.mp3",
  "**/*.ogg",
  "**/*.flac",
  "**/*.mp4",
  "**/*.mov",

  # Engine assets
  "**/*.uasset",
  "**/*.umap",

  # Archives and documents
  "**/*.zip",
  "**/*.7z",
  "**/*.rar",
  "**/*.docx",
  "**/*.xlsx",
  "**/*.pptx",
  "**/*.pdf",

  # Compiled
  "**/*.dll",
  "**/*.exe",
  "**/*.so",
  "**/*.dylib",
  "**/*.a",
  "**/*.lib",
]

# An example of a shared kind; `.pyn/pyn.toml` above is the exception to this glob.
[shared]
paths = [
  "**/*.toml",
]
"#;

/// The mode of paths no policy entry covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DefaultMode {
    Shared,
    Exclusive,
}

impl DefaultMode {
    fn word(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Exclusive => "exclusive",
        }
    }
}

/// The starter policy with `[meta] default` set; the first `default = ` line is the one in `[meta]`.
pub(crate) fn starter_policy(mode: DefaultMode) -> String {
    POLICY.replacen(
        "default = \"shared\"",
        &format!("default = \"{}\"", mode.word()),
        1,
    )
}

const ASK_ATTEMPTS: usize = 3;

/// The flag wins; without a terminal the answer is `shared` and nothing is asked.
pub(crate) fn choose_default(
    flag: Option<DefaultMode>,
    terminal: bool,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<DefaultMode> {
    if let Some(mode) = flag {
        return Ok(mode);
    }
    if !terminal {
        return Ok(DefaultMode::Shared);
    }
    for _ in 0..ASK_ATTEMPTS {
        write!(
            out,
            "Paths no entry covers are: shared (edits are merged, like git) or exclusive (a lock is needed)? [shared] "
        )?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(DefaultMode::Shared);
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "" | "s" | "shared" => return Ok(DefaultMode::Shared),
            "e" | "exclusive" => return Ok(DefaultMode::Exclusive),
            other => writeln!(out, "`{other}` is not shared or exclusive")?,
        }
    }
    anyhow::bail!("no valid answer; pass --default shared or --default exclusive")
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct InitArgs {
    /// The folder to start the repository in; created if missing. The current folder if omitted.
    directory: Option<PathBuf>,
    /// Create the repository named by `--repo` on the server without asking.
    #[arg(long)]
    create_on_server: bool,
    /// Never ask to create the repository on the server.
    #[arg(long)]
    no_create_on_server: bool,
}

/// The global options that say which server and repository to act on.
pub struct Remote {
    pub server: Option<String>,
    pub repo: Option<String>,
    pub token: Option<String>,
    pub user: Option<String>,
}

enum Create {
    Yes,
    No,
}

fn decide(args: &InitArgs, server: Option<&str>, repo: Option<&str>) -> Result<Create> {
    if args.create_on_server && args.no_create_on_server {
        crate::usage_error("`--create-on-server` and `--no-create-on-server` conflict");
    }
    if let Some(repo) = repo {
        address::split_repo(repo)?;
    }
    if args.create_on_server {
        return match (server, repo) {
            (Some(_), Some(_)) => Ok(Create::Yes),
            _ => crate::usage_error("`--create-on-server` needs a server and `--repo owner/name`"),
        };
    }
    match (server, repo) {
        (Some(server), Some(repo)) if !args.no_create_on_server && crate::interactive() => ask(
            &format!("Create {repo} on {server}? [y/N] "),
            &mut std::io::stdin().lock(),
            &mut std::io::stderr(),
        ),
        _ => Ok(Create::No),
    }
}

fn ask(question: &str, input: &mut impl BufRead, out: &mut impl Write) -> Result<Create> {
    write!(out, "{question}")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Create::Yes,
        _ => Create::No,
    })
}

pub fn init(args: &InitArgs, cwd: &Path, remote: Remote) -> Result<()> {
    let target = match &args.directory {
        Some(d) => cwd.join(d),
        None => cwd.to_path_buf(),
    };
    let yours = Settings::load(&userconfig::path()?).unwrap_or_default();
    let server = resolve(remote.server, None, yours.server.as_ref(), None);
    let create = decide(args, server.as_deref(), remote.repo.as_deref())?;
    if git_home::is_tracked(&target)? || target.join(".pyn").exists() {
        let hint = if target.ancestors().any(|p| p.join(".git").exists()) {
            "; `pyn convert` adopts a git repository"
        } else {
            ""
        };
        eprintln!(
            "fatal: {} is already a pyn workspace or git repository, or inside one{hint}",
            target.display()
        );
        std::process::exit(128);
    }
    std::fs::create_dir_all(&target)
        .with_context(|| format!("cannot mkdir {}", target.display()))?;
    let root = target.canonicalize()?;
    let ws = Workspace::create(&root)?;
    std::fs::write(ws.dir.join("pyn.toml"), POLICY)
        .with_context(|| format!("writing {}", ws.dir.join("pyn.toml").display()))?;
    repo_tool::set(&ws, RepoTool::Pyn)?;
    println!("Initialized empty pyn repository in {}", ws.dir.display());
    if let (Create::Yes, Some(server), Some(repo)) = (create, server, remote.repo) {
        create_on_server(
            &ws,
            &server,
            &repo,
            remote.token,
            remote.user.or(yours.user),
        )
        .context("the folder was initialized, but the repository was not created on the server")?;
    }
    Ok(())
}

fn create_on_server(
    ws: &Workspace,
    server: &str,
    repo: &str,
    token: Option<String>,
    user: Option<String>,
) -> Result<()> {
    let base = server.trim_end_matches('/').to_string();
    let api = Api {
        transport: Box::new(http_transport::HttpTransport::new(base.clone())),
        token: credentials::token_for(token, &base)?,
        base,
        user,
        repo: Some(repo.to_string()),
        branch: None,
        on_git: false,
    };
    crate::repo_command(
        &api,
        RepoCommand::Create {
            name: repo.to_string(),
            visibility: None,
            lease_hours: None,
            max_locks: None,
            policy: Some(ws.dir.join("pyn.toml")),
            no_policy: false,
            default: None,
        },
    )?;
    Settings {
        server: Some(server.to_string()),
        user: None,
        repo: Some(repo.to_string()),
        local_store: None,
        editor: None,
    }
    .save(&ws.config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offline_starter_equals_the_servers() {
        assert_eq!(POLICY, pyn_core::rules::DEFAULT_INITIAL_POLICY);
    }

    #[test]
    fn the_starter_takes_either_default_and_stays_valid_policy() {
        for mode in [DefaultMode::Shared, DefaultMode::Exclusive] {
            let text = starter_policy(mode);
            pyn_core::rules::Rules::from_toml(&text).unwrap();
            assert!(text.contains(&format!("default = \"{}\"", mode.word())));
            assert_eq!(text.matches("default = ").count(), 1);
            assert!(text.contains("\"**/*.png\""), "the exclusive list stays");
        }
        assert_eq!(starter_policy(DefaultMode::Shared), POLICY);
    }

    fn answer(text: &str, terminal: bool) -> (Result<DefaultMode>, String) {
        let mut out = Vec::new();
        let got = choose_default(None, terminal, &mut text.as_bytes(), &mut out);
        (got, String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_prompt_accepts_words_and_letters() {
        for (text, want) in [
            ("\n", DefaultMode::Shared),
            ("s\n", DefaultMode::Shared),
            ("Shared\n", DefaultMode::Shared),
            ("e\n", DefaultMode::Exclusive),
            (" EXCLUSIVE \n", DefaultMode::Exclusive),
            ("", DefaultMode::Shared),
            ("what\nexclusive\n", DefaultMode::Exclusive),
        ] {
            let (got, out) = answer(text, true);
            assert_eq!(got.unwrap(), want, "{text:?}");
            assert!(out.contains("[shared]"), "{out}");
        }
    }

    #[test]
    fn unrecognised_answers_are_bounded() {
        let (got, _) = answer("x\ny\nz\nexclusive\n", true);
        assert!(got.unwrap_err().to_string().contains("--default"));
    }

    #[test]
    fn no_terminal_or_a_flag_asks_nothing() {
        let (got, out) = answer("exclusive\n", false);
        assert_eq!((got.unwrap(), out.as_str()), (DefaultMode::Shared, ""));
        let mut out = Vec::new();
        let flagged = choose_default(
            Some(DefaultMode::Exclusive),
            true,
            &mut "".as_bytes(),
            &mut out,
        );
        assert_eq!(flagged.unwrap(), DefaultMode::Exclusive);
        assert!(out.is_empty());
    }

    #[test]
    fn only_yes_answers_create() {
        for (text, yes) in [
            ("y\n", true),
            ("YES\n", true),
            ("\n", false),
            ("n\n", false),
            ("", false),
        ] {
            let got = ask("?", &mut text.as_bytes(), &mut Vec::new()).unwrap();
            assert_eq!(matches!(got, Create::Yes), yes, "{text:?}");
        }
    }
}
