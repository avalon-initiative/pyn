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

fn many(n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|i| format!("object number {i}\n").repeat(50).into_bytes())
        .collect()
}

fn config_for(n: &str) -> String {
    format!("[lfs]\n\tconcurrenttransfers = {n}\n")
}

/// Downloads `contents`, with the server failing those at the `fail` positions, and returns the error text, the installed ids and
/// the most transfers the server saw at once.
fn download_all(
    contents: &[Vec<u8>],
    setting: &str,
    fail: &[usize],
) -> (String, Vec<String>, usize) {
    let srv = LfsServer::start();
    let ids: Vec<String> = contents.iter().map(|c| srv.put(c)).collect();
    {
        let mut st = srv.state.lock().unwrap();
        st.object_delay_ms = 150;
        st.fail_objects = fail.iter().map(|i| ids[*i].clone()).collect();
    }
    let home = tempdir().unwrap();
    let out = tempdir().unwrap();
    let mut c = client(&srv.url("/lfs"), &config_for(setting), &[], home.path());
    let objects: Vec<Object> = contents.iter().map(|c| object(c)).collect();
    let err = c
        .download(
            &objects,
            &out.path().join("tmp"),
            &mut install_to(out.path()),
        )
        .err()
        .map(|e| e.to_string().replace(&srv.addr.to_string(), "ADDR"))
        .unwrap_or_default();
    let mut got: Vec<String> = fs::read_dir(out.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() == 64)
        .collect();
    got.sort();
    let peak = srv.state.lock().unwrap().max_in_flight;
    (err, got, peak)
}

#[test]
fn downloads_overlap_with_concurrent_transfers_and_match_a_sequential_run() {
    let contents = many(12);
    let (err1, got1, peak1) = download_all(&contents, "1", &[]);
    let (err4, got4, peak4) = download_all(&contents, "4", &[]);
    assert_eq!((err1.as_str(), err4.as_str()), ("", ""));
    assert_eq!(got1.len(), 12);
    assert_eq!(got1, got4);
    assert_eq!(peak1, 1);
    assert!((2..=4).contains(&peak4), "{peak4}");
}

#[test]
fn a_failing_download_is_reported_the_same_at_any_concurrency() {
    let contents = many(10);
    let (err1, got1, _) = download_all(&contents, "1", &[2, 7]);
    let (err8, got8, _) = download_all(&contents, "8", &[2, 7]);
    assert!(
        err1.contains("500") && err1.contains("(and 1 more failed)"),
        "{err1}"
    );
    assert_eq!(err1, err8, "the first failure in object order is named");
    assert_eq!(got1.len(), 8);
    assert_eq!(got1, got8);
}

#[test]
fn uploads_overlap_and_report_failures_in_object_order() {
    for setting in ["1", "6"] {
        let srv = LfsServer::start();
        let contents = many(9);
        let dir = tempdir().unwrap();
        for c in &contents {
            fs::write(dir.path().join(sha256_hex(c)), c).unwrap();
        }
        let bad = sha256_hex(&contents[4]);
        {
            let mut st = srv.state.lock().unwrap();
            st.object_delay_ms = 150;
            st.fail_objects = vec![bad.clone()];
        }
        let home = tempdir().unwrap();
        let mut c = client(&srv.url("/lfs"), &config_for(setting), &[], home.path());
        let objects: Vec<Object> = contents.iter().map(|c| object(c)).collect();
        let err = c
            .upload(&objects, &|o| fs::File::open(dir.path().join(&o.oid)))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&bad) && err.contains("500"),
            "{setting}: {err}"
        );
        for (i, content) in contents.iter().enumerate() {
            assert_eq!(srv.has(&sha256_hex(content)).is_some(), i != 4, "{setting}");
        }
        let peak = srv.state.lock().unwrap().max_in_flight;
        assert_eq!(peak > 1, setting != "1", "{setting}: {peak}");
        assert_eq!(srv.count("POST /lfs/objects/verify"), 8);
    }
}

#[test]
fn credentials_are_asked_once_for_concurrent_transfers() {
    let srv = LfsServer::start();
    srv.require("ann", "pw");
    let contents = many(8);
    for c in &contents {
        srv.put(c);
    }
    srv.state.lock().unwrap().object_delay_ms = 100;
    let home = tempdir().unwrap();
    let out = tempdir().unwrap();
    let mut c = client(
        &srv.url("/lfs"),
        &config_for("8"),
        &["ann", "pw"],
        home.path(),
    );
    let objects: Vec<Object> = contents.iter().map(|c| object(c)).collect();
    c.download(
        &objects,
        &out.path().join("tmp"),
        &mut install_to(out.path()),
    )
    .unwrap();
    assert_eq!(fs::read_dir(out.path()).unwrap().count(), 9);
}

#[test]
fn the_concurrency_setting_must_be_a_positive_integer() {
    let home = tempdir().unwrap();
    for bad in ["0", "-3", "many", "", "2.5"] {
        let err = Client::new(
            Url::parse("http://127.0.0.1:1/lfs").unwrap(),
            &Config::parse(config_for(bad).as_bytes()).unwrap(),
            &env(home.path()),
            Box::new(Answers(vec![])),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(err.contains("lfs.concurrenttransfers"), "{bad}: {err}");
    }
}
