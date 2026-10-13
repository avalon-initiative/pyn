//! `pyn ls-remote` against real servers: ssh through a shim and smart HTTP through `git http-backend`.
//! Real git is the oracle for what the server holds and what `git ls-remote` prints.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::path::Path;
use std::process::{Command, Output};

use gitsrv::*;
use tempfile::tempdir;

struct Run {
    out: Output,
}

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.out.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.out.stderr).into_owned()
    }

    fn lines(&self) -> Vec<String> {
        let mut l: Vec<String> = self.stdout().lines().map(str::to_owned).collect();
        l.sort();
        l
    }

    fn code(&self) -> i32 {
        self.out.status.code().unwrap()
    }
}

fn pyn(cwd: &Path, home: &Path, envs: &[(&str, &str)], args: &[&str]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
    cmd.current_dir(cwd)
        .args(args)
        .env_clear()
        .env("PATH", path_env())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    Run {
        out: cmd.output().unwrap(),
    }
}

fn oracle(path: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["ls-remote"];
    args.extend_from_slice(extra);
    args.push(path.to_str().unwrap());
    let mut l: Vec<String> = run(Path::new("/"), &args)
        .lines()
        .map(str::to_owned)
        .collect();
    l.sort();
    l
}

#[test]
fn ls_remote_over_ssh_matches_git() {
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let bare = sample_bare(tmp.path(), format);
        let shim = ssh_shim(tmp.path(), false);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let cwd = tmp.path().join("nowhere");
        std::fs::create_dir_all(&cwd).unwrap();
        let url = format!("ssh://git@example.invalid{}", bare.display());
        let envs = [("GIT_SSH_COMMAND", shim.to_str().unwrap())];
        for flags in [
            vec![],
            vec!["--heads"],
            vec!["--tags"],
            vec!["--refs"],
            vec!["--symref"],
            vec!["--tags", "--refs"],
        ] {
            let mut args = vec!["ls-remote"];
            args.extend(&flags);
            args.push(&url);
            let got = pyn(&cwd, &home, &envs, &args);
            assert_eq!(got.code(), 0, "{flags:?}: {}", got.stderr());
            assert_eq!(got.lines(), oracle(&bare, &flags), "{format} {flags:?}");
        }
        let got = pyn(&cwd, &home, &envs, &["ls-remote", &url, "main", "v*"]);
        let want: Vec<String> = oracle(&bare, &[])
            .into_iter()
            .filter(|l| {
                let name = l.split_once('\t').unwrap().1;
                name.ends_with("/main") || name.contains("/v")
            })
            .collect();
        assert_eq!(got.lines(), want, "patterns");
        let none = pyn(
            &cwd,
            &home,
            &envs,
            &["ls-remote", "--exit-code", &url, "nothing-matches"],
        );
        assert_eq!((none.code(), none.stdout()), (2, String::new()));
        let none = pyn(&cwd, &home, &envs, &["ls-remote", &url, "nothing-matches"]);
        assert_eq!(none.code(), 0);
    }
}

#[test]
fn ls_remote_uses_origin_on_a_git_home_and_applies_instead_of() {
    let tmp = tempdir().unwrap();
    let bare = sample_bare(tmp.path(), "sha1");
    let shim = ssh_shim(tmp.path(), false);
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let work = tmp.path().join("work");
    init(&work, "sha1", false);
    run(
        &work,
        &[
            "remote",
            "add",
            "origin",
            &format!("fake:{}", bare.display()),
        ],
    );
    let envs = [("GIT_SSH_COMMAND", shim.to_str().unwrap())];
    let got = pyn(&work, &home, &envs, &["ls-remote", "--heads"]);
    assert_eq!(got.code(), 0, "{}", got.stderr());
    assert_eq!(got.lines(), oracle(&bare, &["--heads"]));
    assert!(got.stderr().contains("From fake:"), "{}", got.stderr());

    run(&work, &["config", "url.fake:.insteadOf", "alias:"]);
    run(
        &work,
        &[
            "remote",
            "add",
            "viaalias",
            &format!("alias:{}", bare.display()),
        ],
    );
    let got = pyn(&work, &home, &envs, &["ls-remote", "--tags", "viaalias"]);
    assert_eq!(got.lines(), oracle(&bare, &["--tags"]), "{}", got.stderr());
}

#[test]
fn ls_remote_over_http_with_credentials_and_redaction() {
    let tmp = tempdir().unwrap();
    let bare = sample_bare(tmp.path(), "sha256");
    let server = Server::start(
        tmp.path(),
        Opts {
            auth: Some(("alice".into(), "s3cret".into())),
            ..Opts::default()
        },
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let host = &server.url[7..];
    let ok = pyn(
        tmp.path(),
        &home,
        &[],
        &[
            "ls-remote",
            &format!("http://alice:s3cret@{host}/served.git"),
        ],
    );
    assert_eq!(ok.code(), 0, "{}", ok.stderr());
    assert_eq!(ok.lines(), oracle(&bare, &[]));

    let bad = pyn(
        tmp.path(),
        &home,
        &[],
        &[
            "ls-remote",
            &format!("http://alice:wr0ng-s3cret@{host}/served.git"),
        ],
    );
    assert_eq!(bad.code(), 128);
    assert!(!bad.stderr().contains("wr0ng-s3cret") && !bad.stdout().contains("wr0ng-s3cret"));
    assert!(bad.stderr().starts_with("fatal: "), "{}", bad.stderr());

    let ask = tmp.path().join("ask.sh");
    std::fs::write(
        &ask,
        "#!/bin/sh\ncase \"$1\" in Username*) echo alice;; *) echo s3cret;; esac\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ask, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let prompted = pyn(
        tmp.path(),
        &home,
        &[("GIT_ASKPASS", ask.to_str().unwrap())],
        &["ls-remote", "--refs", &format!("http://{host}/served.git")],
    );
    assert_eq!(prompted.code(), 0, "{}", prompted.stderr());
    assert_eq!(prompted.lines(), oracle(&bare, &["--refs"]));

    let denied = pyn(
        tmp.path(),
        &home,
        &[],
        &["ls-remote", &format!("http://{host}/served.git")],
    );
    assert_eq!(denied.code(), 128);
    assert!(
        denied.stderr().contains("terminal prompts disabled"),
        "{}",
        denied.stderr()
    );
}

#[test]
fn local_paths_and_bad_remotes_fail_clearly() {
    let tmp = tempdir().unwrap();
    let bare = sample_bare(tmp.path(), "sha1");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    for target in [
        bare.to_str().unwrap().to_owned(),
        format!("file://{}", bare.display()),
    ] {
        let got = pyn(tmp.path(), &home, &[], &["ls-remote", &target]);
        assert_eq!(got.code(), 128);
        assert!(
            got.stderr()
                .contains("local-path remotes are not supported yet"),
            "{}",
            got.stderr()
        );
    }
    let got = pyn(tmp.path(), &home, &[], &["ls-remote"]);
    assert_eq!(got.code(), 128);
    assert!(
        got.stderr().contains("no remote configured"),
        "{}",
        got.stderr()
    );
    let got = pyn(
        tmp.path(),
        &home,
        &[("PATH", "/nonexistent")],
        &["ls-remote", "git@host:r.git"],
    );
    assert_eq!(got.code(), 128);
    assert!(
        got.stderr().contains("system ssh client"),
        "{}",
        got.stderr()
    );
}
