mod common;

use common::*;
use pyn_git::Config;
use pyn_gittransport::exchange::list_remote_refs;
use pyn_gittransport::{Env, Error, SshTransport};
use pyn_gitwire::{LsRefsOptions, ProtocolVersion, UploadPack};
use pyn_object::Algorithm;
use tempfile::tempdir;

fn alg(format: &str) -> Algorithm {
    Algorithm::from_name(format).unwrap()
}

fn refs_of(
    url: &str,
    env: Env,
    config: &str,
    algorithm: Option<Algorithm>,
) -> (Algorithm, std::collections::BTreeMap<String, String>) {
    let mut c = connector(config, env).connect(url, false).unwrap();
    let opts = LsRefsOptions {
        peel: true,
        symrefs: true,
        ..LsRefsOptions::default()
    };
    let (a, refs) = list_remote_refs(&mut *c.transport, algorithm, &opts).unwrap();
    let mut out = std::collections::BTreeMap::new();
    for r in refs {
        out.insert(r.name.clone(), r.id.unwrap().to_hex());
        if let Some(p) = r.peeled {
            out.insert(format!("{}^{{}}", r.name), p.to_hex());
        }
    }
    (a, out)
}

#[test]
fn ssh_negotiates_v2_and_matches_git() {
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let bare = sample_bare(tmp.path(), format);
        let shim = ssh_shim(tmp.path(), false);
        let env = env(&tmp.path().join("home")).with("GIT_SSH_COMMAND", shim.to_str().unwrap());
        let url = format!("ssh://git@example.invalid:2222{}", bare.display());
        let (a, refs) = refs_of(&url, env.clone(), "", None);
        assert_eq!(a, alg(format));
        assert_eq!(refs, oracle_refs(&bare, &[]), "{format}");

        let log = ssh_log(tmp.path());
        assert!(log.contains("GIT_PROTOCOL=version=2"), "{log}");
        assert!(log.contains("ARG -o\nARG SendEnv=GIT_PROTOCOL\nARG -p\nARG 2222\nARG --\nARG git@example.invalid\n"), "{log}");
        assert!(
            log.contains(&format!("ARG git-upload-pack '{}'", bare.display())),
            "{log}"
        );

        let mut c = connector("", env).connect(&url, false).unwrap();
        let up = UploadPack::open(&mut *c.transport, alg(format)).unwrap();
        assert_eq!(up.version(), ProtocolVersion::V2);
    }
}

#[test]
fn ssh_falls_back_to_v0_when_the_server_ignores_the_request() {
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let bare = sample_bare(tmp.path(), format);
        let shim = ssh_shim(tmp.path(), true);
        let env = env(&tmp.path().join("home")).with("GIT_SSH_COMMAND", shim.to_str().unwrap());
        let url = format!("example.invalid:{}", bare.display());
        let mut c = connector("", env.clone()).connect(&url, false).unwrap();
        let up = UploadPack::open(&mut *c.transport, alg(format)).unwrap();
        assert_eq!(up.version(), ProtocolVersion::V0);
        drop(up);
        let (_, refs) = refs_of(&url, env, "", Some(alg(format)));
        assert_eq!(refs, oracle_refs(&bare, &[]), "{format}");
    }
}

#[test]
fn awkward_paths_survive_the_remote_shell() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("it's a $HOME; !repo");
    std::fs::create_dir_all(&dir).unwrap();
    let bare = sample_bare(&dir, "sha1");
    let shim = ssh_shim(tmp.path(), false);
    let env = env(&tmp.path().join("home")).with("GIT_SSH_COMMAND", shim.to_str().unwrap());
    let (_, refs) = refs_of(&format!("host:{}", bare.display()), env, "", None);
    assert_eq!(refs, oracle_refs(&bare, &[]));
}

#[test]
fn core_ssh_command_and_git_ssh_pick_the_client() {
    let tmp = tempdir().unwrap();
    let bare = sample_bare(tmp.path(), "sha1");
    let shim = ssh_shim(tmp.path(), false);
    let base = env(&tmp.path().join("home"));
    let config = format!("[core]\n\tsshCommand = {}\n", shim.display());
    let url = format!("host:{}", bare.display());
    let (_, refs) = refs_of(&url, base.clone(), &config, None);
    assert_eq!(refs, oracle_refs(&bare, &[]));
    let direct = base.with("GIT_SSH", shim.to_str().unwrap());
    let (_, refs) = refs_of(&url, direct, "", None);
    assert_eq!(refs, oracle_refs(&bare, &[]));
}

#[test]
fn a_failing_ssh_is_a_typed_error() {
    let tmp = tempdir().unwrap();
    let base = env(&tmp.path().join("home"));
    for (script, expect) in [
        ("exit 255", "exit status 255"),
        ("exit 127", "exit status 127"),
        ("exit 128", "exit status 128"),
    ] {
        let env = base
            .clone()
            .with("GIT_SSH_COMMAND", &format!("{script}; #"));
        let mut c = connector("", env).connect("host:repo", false).unwrap();
        let Err(e) = list_remote_refs(&mut *c.transport, None, &LsRefsOptions::default()) else {
            panic!("expected an error")
        };
        let msg = e.to_string();
        assert!(msg.contains(expect), "{msg}");
        assert!(
            matches!(e, Error::Wire(pyn_gitwire::Error::Connection(_))),
            "{e:?}"
        );
    }
}

#[test]
fn the_variant_decides_the_port_flag() {
    let url = match pyn_gittransport::RemoteUrl::parse("ssh://h:2022/r").unwrap() {
        pyn_gittransport::RemoteUrl::Ssh(u) => u,
        _ => unreachable!(),
    };
    let t = SshTransport::new(
        url,
        &Config::default(),
        &Env::default().with("GIT_SSH", "plink"),
    );
    assert_eq!(t.variant(), pyn_gittransport::Variant::Plink);
}
