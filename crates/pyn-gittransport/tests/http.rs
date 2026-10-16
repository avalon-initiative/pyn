mod common;

use std::collections::BTreeMap;

use common::*;
use pyn_gittransport::exchange::list_remote_refs;
use pyn_gittransport::{Env, Error};
use pyn_gitwire::{LsRefsOptions, ProtocolVersion, UploadPack};
use pyn_object::Algorithm;
use tempfile::tempdir;

struct Rig {
    tmp: tempfile::TempDir,
    bare: std::path::PathBuf,
}

fn rig(format: &'static str) -> Rig {
    let tmp = tempdir().unwrap();
    let bare = sample_bare(tmp.path(), format);
    Rig { tmp, bare }
}

impl Rig {
    fn root(&self) -> &std::path::Path {
        self.bare.parent().unwrap()
    }

    fn env(&self) -> Env {
        env(&self.tmp.path().join("home"))
    }
}

fn list(
    url: &str,
    config: &str,
    env: Env,
    prompt: Option<Scripted>,
) -> Result<BTreeMap<String, String>, Error> {
    let mut c = connector(config, env);
    if let Some(p) = prompt {
        c = c.with_prompt(Box::new(p));
    }
    let mut conn = c.connect(url, false)?;
    let opts = LsRefsOptions {
        peel: true,
        ..LsRefsOptions::default()
    };
    let (_, refs) = list_remote_refs(&mut *conn.transport, None, &opts)?;
    let mut out = BTreeMap::new();
    for r in refs {
        out.insert(r.name.clone(), r.id.unwrap().to_hex());
        if let Some(p) = r.peeled {
            out.insert(format!("{}^{{}}", r.name), p.to_hex());
        }
    }
    Ok(out)
}

fn authorization(log: &[Seen]) -> Vec<Option<String>> {
    log.iter()
        .map(|s| s.headers.get("authorization").cloned())
        .collect()
}

#[test]
fn smart_http_matches_git_and_negotiates_v2() {
    for format in FORMATS {
        let r = rig(format);
        let server = Server::start(r.root(), Opts::default());
        let url = format!("{}/served.git", server.url);
        assert_eq!(
            list(&url, "", r.env(), None).unwrap(),
            oracle_refs(&r.bare, &[]),
            "{format}"
        );
        let log = server.log();
        assert_eq!(log[0].method, "GET");
        assert_eq!(
            log[0].target,
            "/served.git/info/refs?service=git-upload-pack"
        );
        assert_eq!(log[0].headers["git-protocol"], "version=2");
        assert!(log[0].headers["user-agent"].starts_with("git/"));
        let post = log.iter().find(|s| s.method == "POST").unwrap();
        assert_eq!(post.target, "/served.git/git-upload-pack");
        assert_eq!(
            post.headers["content-type"],
            "application/x-git-upload-pack-request"
        );
        let mut conn = connector("", r.env()).connect(&url, false).unwrap();
        let up =
            UploadPack::open(&mut *conn.transport, Algorithm::from_name(format).unwrap()).unwrap();
        assert_eq!(up.version(), ProtocolVersion::V2);
    }
}

#[test]
fn smart_http_falls_back_to_v0() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            strip_protocol: true,
            ..Opts::default()
        },
    );
    let url = format!("{}/served.git", server.url);
    assert_eq!(
        list(&url, "", r.env(), None).unwrap(),
        oracle_refs(&r.bare, &[])
    );
    let mut conn = connector("", r.env()).connect(&url, false).unwrap();
    let up = UploadPack::open(&mut *conn.transport, Algorithm::Sha1).unwrap();
    assert_eq!(up.version(), ProtocolVersion::V0);
}

#[test]
fn credentials_in_the_url_answer_a_401_once() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("alice".into(), "s3cret".into())),
            ..Opts::default()
        },
    );
    let url = format!("http://alice:s3cret@{}/served.git", &server.url[7..]);
    assert_eq!(
        list(&url, "", r.env(), None).unwrap(),
        oracle_refs(&r.bare, &[])
    );
    let auth = authorization(&server.log());
    assert_eq!(auth[0], None, "the first request is anonymous");
    assert_eq!(auth[1].as_deref(), Some(basic("alice", "s3cret").as_str()));
    assert!(
        auth[2..]
            .iter()
            .all(|a| a.as_deref() == Some(basic("alice", "s3cret").as_str()))
    );
}

#[test]
fn a_bare_token_in_the_url_works_as_the_user_name() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("tok3n".into(), String::new())),
            ..Opts::default()
        },
    );
    let url = format!("http://tok3n@{}/served.git", &server.url[7..]);
    assert_eq!(
        list(&url, "", r.env(), None).unwrap(),
        oracle_refs(&r.bare, &[])
    );
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("x-access-token".into(), "tok3n".into())),
            ..Opts::default()
        },
    );
    let url = format!(
        "http://x-access-token:tok3n@{}/served.git",
        &server.url[7..]
    );
    assert_eq!(
        list(&url, "", r.env(), None).unwrap(),
        oracle_refs(&r.bare, &[])
    );
}

#[test]
fn wrong_credentials_fail_without_echoing_the_secret() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("alice".into(), "right".into())),
            ..Opts::default()
        },
    );
    let url = format!("http://alice:wr0ng-s3cret@{}/served.git", &server.url[7..]);
    let e = list(&url, "", r.env(), None).unwrap_err();
    let msg = format!("{e} / {e:?}");
    assert!(!msg.contains("wr0ng-s3cret"), "{msg}");
    assert!(
        msg.contains("terminal prompts disabled") || msg.contains("Authentication failed"),
        "{msg}"
    );
}

#[test]
fn the_store_helper_remembers_prompts_and_erases_rejections() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("alice".into(), "right".into())),
            ..Opts::default()
        },
    );
    let url = format!("{}/served.git", server.url);
    let file = r.tmp.path().join("creds");
    let config = format!("[credential]\n\thelper = store --file {}\n", file.display());

    let (prompt, asked) = Scripted::new(&["alice", "right"]);
    assert_eq!(
        list(&url, &config, r.env(), Some(prompt)).unwrap(),
        oracle_refs(&r.bare, &[])
    );
    assert_eq!(asked.lock().unwrap().len(), 2);
    let stored = std::fs::read_to_string(&file).unwrap();
    assert_eq!(stored, format!("http://alice:right@{}\n", &server.url[7..]));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let (silent, asked) = Scripted::new(&[]);
    assert!(list(&url, &config, r.env(), Some(silent)).is_ok());
    assert!(
        asked.lock().unwrap().is_empty(),
        "the stored credential is used"
    );

    std::fs::write(&file, format!("http://alice:stale@{}\n", &server.url[7..])).unwrap();
    let (prompt, asked) = Scripted::new(&["alice", "right"]);
    assert!(list(&url, &config, r.env(), Some(prompt)).is_ok());
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the stale entry was refused, so it prompted"
    );
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(!stored.contains("stale"), "{stored}");
    assert!(stored.contains("alice:right"), "{stored}");
}

#[test]
fn a_shell_helper_and_username_config_supply_the_credential() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("bob".into(), "hpw".into())),
            ..Opts::default()
        },
    );
    let url = format!("{}/served.git", server.url);
    let config = "[credential]\n\thelper = \"!f() { echo username=bob; echo password=hpw; }; f\"\n";
    assert!(list(&url, config, r.env(), None).is_ok());
    let config = "[credential]\n\tusername = bob\n\thelper = \"!f() { echo password=hpw; }; f\"\n";
    assert!(list(&url, config, r.env(), None).is_ok());
}

#[test]
fn askpass_supplies_credentials_when_nothing_else_does() {
    use std::os::unix::fs::PermissionsExt;
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            auth: Some(("carol".into(), "apw".into())),
            ..Opts::default()
        },
    );
    let ask = r.tmp.path().join("ask.sh");
    std::fs::write(
        &ask,
        "#!/bin/sh\ncase \"$1\" in Username*) echo carol;; *) echo apw;; esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&ask, std::fs::Permissions::from_mode(0o755)).unwrap();
    let url = format!("{}/served.git", server.url);
    let env = r.env().with("GIT_ASKPASS", ask.to_str().unwrap());
    assert!(list(&url, "", env, None).is_ok());
}

#[test]
fn a_redirect_is_followed_and_credentials_never_cross_hosts() {
    let r = rig("sha1");
    let target = Server::start(
        r.root(),
        Opts {
            auth: Some(("alice".into(), "s3cret".into())),
            ..Opts::default()
        },
    );
    let front = Server::start(
        r.root(),
        Opts {
            redirect_to: Some(target.url.clone()),
            ..Opts::default()
        },
    );
    let url = format!("http://alice:s3cret@{}/moved/served.git", &front.url[7..]);
    let e = list(&url, "", r.env(), None).unwrap_err();
    assert!(!format!("{e:?}").contains("s3cret"));
    assert!(
        authorization(&front.log()).iter().all(Option::is_none),
        "the first host saw credentials before it asked for any: {:?}",
        front.log()
    );
    assert!(
        authorization(&target.log()).iter().all(Option::is_none),
        "credentials from the URL must not reach another origin: {:?}",
        target.log()
    );

    // The same redirect with the target's credentials supplied by a helper for that origin works.
    let config = format!(
        "[credential \"{}\"]\n\thelper = \"!f() {{ echo username=alice; echo password=s3cret; }}; f\"\n",
        target.url
    );
    target.clear_log();
    assert_eq!(
        list(
            &format!("{}/moved/served.git", front.url),
            &config,
            r.env(),
            None
        )
        .unwrap(),
        oracle_refs(&r.bare, &[])
    );
    let posts: Vec<_> = target
        .log()
        .into_iter()
        .filter(|s| s.method == "POST")
        .collect();
    assert!(
        !posts.is_empty()
            && posts
                .iter()
                .all(|p| p.target == "/served.git/git-upload-pack")
    );
}

#[test]
fn dumb_http_is_refused_clearly() {
    let r = rig("sha1");
    let server = Server::start(
        r.root(),
        Opts {
            dumb: true,
            ..Opts::default()
        },
    );
    let e = list(&format!("{}/served.git", server.url), "", r.env(), None).unwrap_err();
    assert!(e.to_string().contains("dumb HTTP"), "{e}");
}

#[test]
fn missing_repositories_and_extra_headers() {
    let r = rig("sha1");
    let server = Server::start(r.root(), Opts::default());
    let e = list(&format!("{}/nope.git", server.url), "", r.env(), None).unwrap_err();
    assert!(
        matches!(e, Error::Wire(pyn_gitwire::Error::NotFound(_))),
        "{e:?}"
    );
    let config = "[http]\n\textraHeader = X-Pyn-Test: yes\n";
    list(&format!("{}/served.git", server.url), config, r.env(), None).unwrap();
    assert!(
        server
            .log()
            .iter()
            .any(|s| s.headers.get("x-pyn-test").map(String::as_str) == Some("yes"))
    );
}

#[test]
fn proxies_and_tls_settings_are_read_from_config() {
    let r = rig("sha1");
    let server = Server::start(r.root(), Opts::default());
    let config = "[http]\n\tproxy = http://127.0.0.1:1\n\tsslVerify = false\n";
    let e = list(&format!("{}/served.git", server.url), config, r.env(), None).unwrap_err();
    assert!(
        matches!(e, Error::Wire(pyn_gitwire::Error::Connection(_))),
        "{e:?}"
    );
    assert!(server.log().is_empty(), "the proxy setting was honoured");
}

/// Accepts connections, optionally answers with `reply` (headers and part of a body), then goes quiet.
fn stalling_server(reply: Option<&'static str>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            if let Some(r) = reply {
                let _ = s.write_all(r.as_bytes());
            }
            held.push(s);
        }
    });
    format!("http://{addr}/r.git")
}

#[test]
fn a_silent_server_fails_after_the_stall_timeout() {
    let r = rig("sha1");
    let config = "[http]\n\tlowSpeedTime = 1\n";
    for reply in [
        None,
        Some(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-git-upload-pack-advertisement\r\nContent-Length: 100000\r\n\r\n001e# service=git-upload-pack\n",
        ),
    ] {
        let url = stalling_server(reply);
        let started = std::time::Instant::now();
        let err = list(&url, config, r.env(), None).unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "{err}"
        );
        let text = err.to_string();
        assert!(
            text.contains("no response within") || text.contains("no data received"),
            "{text}"
        );
    }
}
