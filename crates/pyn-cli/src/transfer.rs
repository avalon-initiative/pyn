//! Moving file content: chunked, resumable upload and verified, resumable download.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pyn_chunk::{MAX_CHUNK, SMALL_OBJECT, cut_point};
use pyn_proto as api;
use reqwest::Method;
use sha2::{Digest, Sha256};

use crate::client::Api;

const HASHES_PER_REQUEST: usize = 5000;

struct Piece {
    offset: u64,
    len: usize,
    hash: String,
}

fn put_blob(api: &Api, bytes: Vec<u8>) -> Result<String> {
    let put: api::PutObjectResponse = api
        .send(
            api.request(Method::PUT, &api.repo_route("/objects")?)
                .body(bytes),
        )?
        .json()?;
    Ok(put.content)
}

/// Cuts the file into chunks, reading it once.
fn pieces_of(file: &mut File) -> Result<Vec<Piece>> {
    let mut pieces = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut offset = 0u64;
    let mut eof = false;
    let mut block = vec![0u8; 256 << 10];
    while !(eof && buf.is_empty()) {
        while !eof && buf.len() < MAX_CHUNK {
            let n = file.read(&mut block)?;
            if n == 0 {
                eof = true;
            } else {
                buf.extend_from_slice(&block[..n]);
            }
        }
        let cut = cut_point(&buf);
        pieces.push(Piece {
            offset,
            len: cut,
            hash: hex::encode(Sha256::digest(&buf[..cut])),
        });
        offset += cut as u64;
        buf.drain(..cut);
    }
    Ok(pieces)
}

fn missing(api: &Api, hashes: &[&str]) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for batch in hashes.chunks(HASHES_PER_REQUEST) {
        let body = api::MissingObjectsRequest {
            hashes: batch.iter().map(|h| h.to_string()).collect(),
        };
        let found: api::MissingObjectsResponse = api
            .send(
                api.request(Method::POST, &api.repo_route("/objects/missing")?)
                    .json(&body),
            )?
            .json()?;
        out.extend(found.missing);
    }
    Ok(out)
}

/// Uploads `path` and returns its content hash. Small files go up whole; larger ones go up as chunks and only the
/// chunks the server lacks are sent, so repeating a failed upload continues where it stopped.
pub fn upload_file(api: &Api, path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let len = file.metadata()?.len();
    if len as usize <= SMALL_OBJECT {
        let mut bytes = Vec::with_capacity(len as usize);
        file.read_to_end(&mut bytes)?;
        return put_blob(api, bytes);
    }
    let pieces = pieces_of(&mut file)?;
    let hashes: Vec<&str> = pieces.iter().map(|p| p.hash.as_str()).collect();
    let mut need = missing(api, &hashes)?;
    for piece in &pieces {
        if !need.remove(&piece.hash) {
            continue;
        }
        file.seek(SeekFrom::Start(piece.offset))?;
        let mut bytes = vec![0u8; piece.len];
        file.read_exact(&mut bytes)?;
        let stored = put_blob(api, bytes)?;
        if stored != piece.hash {
            bail!(
                "the server stored a chunk as {stored}, expected {}",
                piece.hash
            );
        }
    }
    let body = api::AssembleObjectRequest {
        chunks: pieces.iter().map(|p| p.hash.clone()).collect(),
    };
    let put: api::PutObjectResponse = api
        .send(
            api.request(Method::POST, &api.repo_route("/objects/assemble")?)
                .json(&body),
        )?
        .json()?;
    Ok(put.content)
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_owned();
    name.push(".pyn-part");
    dest.with_file_name(name)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 256 << 10];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

/// Downloads `path` at `revision` (the head if `None`) to `dest` through a `.pyn-part` file, continuing a partial one,
/// and checks the content against the hash the server reports. Returns the revision and the content hash.
pub fn download(
    api: &Api,
    path: &str,
    revision: Option<u64>,
    dest: &Path,
) -> Result<(u64, String)> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let part = part_path(dest);
    for attempt in 0..2 {
        let have = std::fs::metadata(&part).map_or(0, |m| m.len());
        let mut req = api
            .get(&api.repo_route("/content")?)
            .query(&[("path", path)]);
        if let Some(r) = revision {
            req = req.query(&[("revision", r)]);
        }
        if have > 0 {
            req = req.header("range", format!("bytes={have}-"));
        }
        let mut resp = match api.send(req) {
            Ok(r) => r,
            Err(_) if have > 0 && attempt == 0 => {
                let _ = std::fs::remove_file(&part);
                continue;
            }
            Err(e) => return Err(e),
        };
        let found = resp
            .headers()
            .get(api::REVISION_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let expected = resp
            .headers()
            .get(api::CONTENT_HASH_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let appending = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(appending)
            .truncate(!appending)
            .open(&part)
            .with_context(|| format!("writing {}", part.display()))?;
        resp.copy_to(&mut out)?;
        out.flush()?;
        drop(out);
        let actual = hash_file(&part)?;
        if expected.as_deref().is_some_and(|e| e != actual) {
            let _ = std::fs::remove_file(&part);
            if attempt == 0 {
                continue;
            }
            bail!("the downloaded content of {path} does not match its hash");
        }
        std::fs::rename(&part, dest).with_context(|| format!("writing {}", dest.display()))?;
        return Ok((found, actual));
    }
    bail!("could not download {path}")
}
