//! The Git LFS Batch client against a local server: endpoint resolution, verified and resumable downloads, uploads,
//! credentials.

mod common;

use std::fs;
use std::path::Path;

use common::lfssrv::{LfsServer, sha256_hex};
use pyn_git::Config;
use pyn_gittransport::credential::Prompt;
use pyn_gittransport::lfs::{Client, Object, endpoint};
use pyn_gittransport::{Env, Error};
use reqwest::Url;
use tempfile::tempdir;

fn env(home: &Path) -> Env {
    Env::default()
        .with("HOME", home.to_str().unwrap())
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_TERMINAL_PROMPT", "0")
}

struct Answers(Vec<String>);

impl Prompt for Answers {
    fn ask(&mut self, _prompt: &str, _secret: bool) -> pyn_gittransport::Result<String> {
        if self.0.is_empty() {
            return Err(Error::Credential("no more answers".into()));
        }
        Ok(self.0.remove(0))
    }
}

fn client(url: &str, config: &str, answers: &[&str], home: &Path) -> Client {
    Client::new(
        Url::parse(url).unwrap(),
        &Config::parse(config.as_bytes()).unwrap(),
        &env(home),
        Box::new(Answers(answers.iter().map(|s| (*s).into()).collect())),
    )
    .unwrap()
}

fn object(content: &[u8]) -> Object {
    Object {
        oid: sha256_hex(content),
        size: content.len() as u64,
    }
}

fn install_to(dir: &Path) -> impl FnMut(&Object, &Path) -> pyn_gittransport::Result<()> + '_ {
    move |o, p| {
        fs::rename(p, dir.join(&o.oid))?;
        Ok(())
    }
}

#[test]
fn endpoint_is_explicit_or_derived_from_the_remote() {
    let cfg = |s: &str| Config::parse(s.as_bytes()).unwrap();
    let e = |c: &str, remote: Option<&str>, url: Option<&str>, push: bool| {
        endpoint(&cfg(c), remote, url, push).map(|u| u.to_string())
    };
    assert_eq!(
        e(
            "",
            Some("origin"),
            Some("https://h.example/team/game"),
            false
        )
        .unwrap(),
        "https://h.example/team/game.git/info/lfs"
    );
    assert_eq!(
        e(
            "",
            Some("origin"),
            Some("https://h.example/team/game.git/"),
            false
        )
        .unwrap(),
        "https://h.example/team/game.git/info/lfs"
    );
    let c = "[lfs]\n\turl = https://lfs.example/a\n\tpushurl = https://push.example/a\n\
             [remote \"origin\"]\n\tlfsurl = https://lfs.example/origin\n\tlfspushurl = https://push.example/origin\n";
    assert_eq!(
        e(c, Some("origin"), Some("https://x/y"), false).unwrap(),
        "https://lfs.example/origin"
    );
    assert_eq!(
        e(c, Some("origin"), Some("https://x/y"), true).unwrap(),
        "https://push.example/origin"
    );
    assert_eq!(
        e(c, Some("other"), None, false).unwrap(),
        "https://lfs.example/a"
    );
    let rewritten = "[url \"https://real.example/\"]\n\tinsteadOf = short:\n";
    assert_eq!(
        e(rewritten, Some("origin"), Some("short:t/g"), false).unwrap(),
        "https://real.example/t/g.git/info/lfs"
    );
    assert!(e("", Some("origin"), Some("git@h.example:t/g.git"), false).is_err());
    assert!(e("", None, None, false).is_err());
    assert!(e("[lfs]\n\turl = ssh://h/x\n", None, None, false).is_err());
}

#[test]
fn downloads_are_verified_and_installed() {
    let srv = LfsServer::start();
    let content = vec![9u8; 200_000];
    let want = object(&content);
    srv.put(&content);
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let mut c = client(&srv.url("/r.git/info/lfs"), "", &[], tmp.path());
    c.download(
        std::slice::from_ref(&want),
        &tmp.path().join("tmp"),
        &mut install_to(&out),
    )
    .unwrap();
    assert_eq!(fs::read(out.join(&want.oid)).unwrap(), content);
    assert_eq!(srv.count("POST /r.git/info/lfs/objects/batch"), 1);
}

#[test]
fn a_corrupt_download_is_refused_and_not_installed() {
    let srv = LfsServer::start();
    let content = vec![3u8; 5000];
    srv.put(&content);
    srv.state.lock().unwrap().corrupt_downloads = true;
    let tmp = tempdir().unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], tmp.path());
    let err = c
        .download(
            &[object(&content)],
            &tmp.path().join("tmp"),
            &mut install_to(&out),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match"), "{err}");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    let part = format!("{}.part", sha256_hex(&content));
    assert!(!tmp.path().join("tmp").join(part).exists());
}

#[test]
fn a_missing_object_names_the_server_error() {
    let srv = LfsServer::start();
    let tmp = tempdir().unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], tmp.path());
    let err = c
        .download(
            &[object(b"absent")],
            &tmp.path().join("tmp"),
            &mut |_, _| Ok(()),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("does not exist") && err.contains("404"),
        "{err}"
    );
}

#[test]
fn a_partial_download_resumes_with_a_range_request() {
    let srv = LfsServer::start();
    let content: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let want = object(&content);
    srv.put(&content);
    let tmp = tempdir().unwrap();
    let work = tmp.path().join("tmp");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join(format!("{}.part", want.oid)), &content[..40_000]).unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], tmp.path());
    c.download(std::slice::from_ref(&want), &work, &mut install_to(&out))
        .unwrap();
    assert_eq!(fs::read(out.join(&want.oid)).unwrap(), content);
    assert_eq!(srv.state.lock().unwrap().ranges, ["bytes=40000-"]);
}

#[test]
fn a_stale_partial_file_with_wrong_content_is_discarded_and_retried() {
    let srv = LfsServer::start();
    let content = vec![5u8; 10_000];
    let want = object(&content);
    srv.put(&content);
    let tmp = tempdir().unwrap();
    let work = tmp.path().join("tmp");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join(format!("{}.part", want.oid)), vec![7u8; 4000]).unwrap();
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], tmp.path());
    c.download(std::slice::from_ref(&want), &work, &mut install_to(&out))
        .unwrap();
    assert_eq!(fs::read(out.join(&want.oid)).unwrap(), content);
}

#[test]
fn credentials_come_from_the_url_a_helper_or_a_prompt() {
    let srv = LfsServer::start();
    srv.require("ann", "s3cret");
    let content = b"payload".to_vec();
    srv.put(&content);
    let tmp = tempdir().unwrap();
    let mut clients = Vec::new();

    let url = format!("http://ann:s3cret@{}/lfs", srv.addr);
    clients.push(("url", client(&url, "", &[], tmp.path())));
    let helper =
        "[credential]\n\thelper = \"!f() { echo username=ann; echo password=s3cret; }; f\"\n";
    clients.push(("helper", client(&srv.url("/lfs"), helper, &[], tmp.path())));
    clients.push((
        "prompt",
        client(&srv.url("/lfs"), "", &["ann", "s3cret"], tmp.path()),
    ));
    for (how, mut c) in clients {
        let out = tmp.path().join(how);
        fs::create_dir_all(&out).unwrap();
        c.download(
            &[object(&content)],
            &tmp.path().join("tmp"),
            &mut install_to(&out),
        )
        .unwrap_or_else(|e| panic!("{how}: {e}"));
        assert_eq!(fs::read(out.join(sha256_hex(&content))).unwrap(), content);
    }
}

#[test]
fn wrong_credentials_fail_without_leaking_the_password() {
    let srv = LfsServer::start();
    srv.require("ann", "s3cret");
    let tmp = tempdir().unwrap();
    let url = format!("http://ann:wrong-pw@{}/lfs", srv.addr);
    let mut c = client(&url, "", &[], tmp.path());
    let err = c
        .download(&[object(b"x")], &tmp.path().join("tmp"), &mut |_, _| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(!err.contains("wrong-pw"), "{err}");
}

#[test]
fn credentials_are_not_sent_to_another_host() {
    let srv = LfsServer::start();
    srv.require("ann", "s3cret");
    let content = b"elsewhere".to_vec();
    srv.put(&content);
    {
        let mut st = srv.state.lock().unwrap();
        st.open_objects = true;
        st.href_host = Some(format!("localhost:{}", srv.addr.port()));
    }
    let tmp = tempdir().unwrap();
    let url = format!("http://ann:s3cret@{}/lfs", srv.addr);
    let mut c = client(&url, "", &[], tmp.path());
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    c.download(
        &[object(&content)],
        &tmp.path().join("tmp"),
        &mut install_to(&out),
    )
    .unwrap();
    let seen = srv.state.lock().unwrap().auth_seen.len();
    assert_eq!(seen, 1, "only the batch call carries credentials");
}

#[test]
fn uploads_send_what_the_server_lacks_and_verify_it() {
    let srv = LfsServer::start();
    let have = b"already there".to_vec();
    srv.put(&have);
    let fresh = vec![1u8; 70_000];
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(sha256_hex(&fresh)), &fresh).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], dir.path());
    let sent = c
        .upload(&[object(&have), object(&fresh)], &|o| {
            fs::File::open(dir.path().join(&o.oid))
        })
        .unwrap();
    assert_eq!(sent, 1);
    assert_eq!(srv.has(&sha256_hex(&fresh)).unwrap(), fresh);
    assert_eq!(srv.count("PUT"), 1);
    assert_eq!(srv.count("POST /lfs/objects/verify"), 1);
    let sent = c.upload(&[object(&fresh)], &|_| unreachable!()).unwrap();
    assert_eq!(sent, 0);
}

#[test]
fn a_refused_upload_and_a_failing_batch_report_why() {
    let srv = LfsServer::start();
    srv.state.lock().unwrap().refuse_uploads = true;
    let tmp = tempdir().unwrap();
    let data = b"nope".to_vec();
    fs::write(tmp.path().join(sha256_hex(&data)), &data).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], tmp.path());
    let err = c
        .upload(&[object(&data)], &|o| {
            fs::File::open(tmp.path().join(&o.oid))
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("403") && err.contains("read only"), "{err}");

    srv.state.lock().unwrap().batch_status = Some(500);
    let err = c
        .upload(&[object(&data)], &|_| unreachable!())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("500") && err.contains("batch refused"),
        "{err}"
    );
}

fn tus_upload_fixture(dir: &Path, size: usize) -> (Vec<u8>, Object) {
    let data: Vec<u8> = (0..size).map(|i| (i * 13 % 251) as u8).collect();
    fs::write(dir.join(sha256_hex(&data)), &data).unwrap();
    let object = object(&data);
    (data, object)
}

fn upload_one(c: &mut Client, dir: &Path, o: &Object) -> pyn_gittransport::Result<usize> {
    c.upload(std::slice::from_ref(o), &|o| {
        fs::File::open(dir.join(&o.oid))
    })
}

#[test]
fn a_server_that_offers_tus_gets_a_resumable_upload() {
    let srv = LfsServer::start();
    srv.state.lock().unwrap().tus = true;
    let dir = tempdir().unwrap();
    let (data, o) = tus_upload_fixture(dir.path(), 70_000);
    let mut c = client(&srv.url("/lfs"), "", &[], dir.path());
    assert_eq!(upload_one(&mut c, dir.path(), &o).unwrap(), 1);
    assert_eq!(srv.has(&o.oid).unwrap(), data);
    assert_eq!(
        srv.state.lock().unwrap().offered,
        [["tus".to_string(), "basic".to_string()]]
    );
    let log = srv.log();
    assert_eq!(log.len(), 5, "{log:?}");
    assert!(
        log[1].starts_with("HEAD ") && log[1].ends_with("/tus"),
        "{log:?}"
    );
    assert!(log[2].starts_with("POST "), "{log:?}");
    assert!(log[3].starts_with("PATCH "), "{log:?}");
    assert!(log[4].starts_with("POST /lfs/objects/verify"), "{log:?}");
    assert_eq!(srv.count("PUT"), 0);
}

#[test]
fn an_interrupted_tus_upload_continues_from_the_servers_offset() {
    let srv = LfsServer::start();
    let dir = tempdir().unwrap();
    let (data, o) = tus_upload_fixture(dir.path(), 50_000);
    {
        let mut st = srv.state.lock().unwrap();
        st.tus = true;
        st.partial.insert(o.oid.clone(), data[..20_000].to_vec());
        st.tus_lengths.insert(o.oid.clone(), data.len());
    }
    let mut c = client(&srv.url("/lfs"), "", &[], dir.path());
    assert_eq!(upload_one(&mut c, dir.path(), &o).unwrap(), 1);
    assert_eq!(srv.has(&o.oid).unwrap(), data);
    assert_eq!(
        srv.count("POST /lfs/objects/"),
        2,
        "a batch and a verify, no creation"
    );
    assert_eq!(srv.count("PATCH"), 1);
}

#[test]
fn a_server_that_accepts_part_of_a_patch_is_asked_again_from_its_new_offset() {
    let srv = LfsServer::start();
    let dir = tempdir().unwrap();
    let (data, o) = tus_upload_fixture(dir.path(), 25_000);
    {
        let mut st = srv.state.lock().unwrap();
        st.tus = true;
        st.tus_accept_limit = Some(10_000);
    }
    let mut c = client(&srv.url("/lfs"), "", &[], dir.path());
    assert_eq!(upload_one(&mut c, dir.path(), &o).unwrap(), 1);
    assert_eq!(srv.has(&o.oid).unwrap(), data);
    assert_eq!(srv.count("PATCH"), 3);
    assert_eq!(srv.count("HEAD"), 3);
}

#[test]
fn a_refused_tus_upload_is_reported_and_basic_is_used_when_tus_is_off() {
    let srv = LfsServer::start();
    let dir = tempdir().unwrap();
    let (data, o) = tus_upload_fixture(dir.path(), 4_000);
    srv.state.lock().unwrap().tus = true;
    let mut corrupt = data.clone();
    corrupt[0] ^= 1;
    fs::write(dir.path().join(&o.oid), &corrupt).unwrap();
    let mut c = client(&srv.url("/lfs"), "", &[], dir.path());
    let err = upload_one(&mut c, dir.path(), &o).unwrap_err().to_string();
    assert!(
        err.contains("422") && err.contains("digest mismatch"),
        "{err}"
    );
    assert!(srv.has(&o.oid).is_none());

    let srv = LfsServer::start();
    srv.state.lock().unwrap().tus = true;
    fs::write(dir.path().join(&o.oid), &data).unwrap();
    let mut c = client(
        &srv.url("/lfs"),
        "[lfs]\n\ttustransfers = false\n",
        &[],
        dir.path(),
    );
    assert_eq!(upload_one(&mut c, dir.path(), &o).unwrap(), 1);
    assert_eq!(srv.state.lock().unwrap().offered, [["basic".to_string()]]);
    assert_eq!(srv.count("PUT"), 1);
    assert_eq!(srv.count("PATCH"), 0);
}
