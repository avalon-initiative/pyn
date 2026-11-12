//! The LFS client's tus upload against the repository's own server, including a transfer that breaks off midway.

mod common;

use std::cell::Cell;
use std::fs;
use std::path::Path;

use pyn_git::Config;
use pyn_gittransport::credential::Prompt;
use pyn_gittransport::lfs::{Client, Object};
use pyn_gittransport::{Env, Error};
use reqwest::Url;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

struct NoPrompt;

impl Prompt for NoPrompt {
    fn ask(&mut self, _prompt: &str, _secret: bool) -> pyn_gittransport::Result<String> {
        Err(Error::Credential("no prompt".into()))
    }
}

fn client(env: &common::Env, home: &Path) -> Client {
    let url = format!("{}/v1/repos/alice/game/info/lfs", env.url);
    let config = Config::parse(b"[http]\n\textraheader = X-Pyn-User: alice\n").unwrap();
    let vars = Env::default()
        .with("HOME", home.to_str().unwrap())
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_TERMINAL_PROMPT", "0");
    Client::new(
        Url::parse(&url).unwrap(),
        &config,
        &vars,
        Box::new(NoPrompt),
    )
    .unwrap()
}

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

fn object(data: &[u8]) -> Object {
    Object {
        oid: hex::encode(Sha256::digest(data)),
        size: data.len() as u64,
    }
}

fn fetch(c: &mut Client, o: &Object, dir: &Path) -> Vec<u8> {
    let tmp = dir.join("dl");
    let got = dir.join("got");
    c.download(std::slice::from_ref(o), &tmp, &mut |_, p| {
        fs::copy(p, &got)?;
        Ok(())
    })
    .unwrap();
    fs::read(got).unwrap()
}

#[test]
fn an_object_uploads_through_tus_and_downloads_back() {
    let env = common::start();
    let dir = tempdir().unwrap();
    let data = content(3 << 20);
    let o = object(&data);
    fs::write(dir.path().join("src"), &data).unwrap();
    let mut c = client(&env, dir.path());
    let sent = c
        .upload(std::slice::from_ref(&o), &|_| {
            fs::File::open(dir.path().join("src"))
        })
        .unwrap();
    assert_eq!(sent, 1);
    assert_eq!(fetch(&mut c, &o, dir.path()), data);
    let sent = c
        .upload(std::slice::from_ref(&o), &|_| {
            unreachable!("the server has it")
        })
        .unwrap();
    assert_eq!(sent, 0);
}

#[test]
fn an_upload_cut_off_midway_resumes_where_the_server_stopped() {
    let env = common::start();
    let dir = tempdir().unwrap();
    let data = content(2 << 20);
    let o = object(&data);
    fs::write(dir.path().join("full"), &data).unwrap();
    fs::write(dir.path().join("short"), &data[..600_000]).unwrap();
    let opened = Cell::new(0);
    let mut c = client(&env, dir.path());
    let sent = c
        .upload(std::slice::from_ref(&o), &|_| {
            opened.set(opened.get() + 1);
            // The first attempt promises the whole object but can only supply part of it.
            let name = if opened.get() == 1 { "short" } else { "full" };
            fs::File::open(dir.path().join(name))
        })
        .unwrap();
    assert_eq!(sent, 1);
    assert!(opened.get() >= 2, "the upload was attempted again");
    assert_eq!(fetch(&mut c, &o, dir.path()), data);
}
