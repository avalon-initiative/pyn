//! A software authenticator that builds WebAuthn structures byte by byte, independent of the crate's parsers.
#![allow(dead_code)]

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::Signer as _;
use sha2::{Digest, Sha256};

pub const RP_ID: &str = "pyn.example";
pub const ORIGIN: &str = "https://pyn.example";
pub const CHALLENGE: &[u8] = b"0123456789abcdef0123456789abcdef";

pub const UP: u8 = 0x01;
pub const UV: u8 = 0x04;
pub const BE: u8 = 0x08;
pub const BS: u8 = 0x10;
pub const AT: u8 = 0x40;
pub const ED: u8 = 0x80;

fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    if n < 24 {
        vec![m | n as u8]
    } else if n < 256 {
        vec![m | 24, n as u8]
    } else if n < 65536 {
        let mut v = vec![m | 25];
        v.extend((n as u16).to_be_bytes());
        v
    } else {
        let mut v = vec![m | 26];
        v.extend((n as u32).to_be_bytes());
        v
    }
}
pub fn int(n: i64) -> Vec<u8> {
    if n >= 0 {
        head(0, n as u64)
    } else {
        head(1, (-1 - n) as u64)
    }
}
pub fn text(s: &str) -> Vec<u8> {
    let mut v = head(3, s.len() as u64);
    v.extend(s.as_bytes());
    v
}
pub fn bstr(b: &[u8]) -> Vec<u8> {
    let mut v = head(2, b.len() as u64);
    v.extend(b);
    v
}
pub fn map(pairs: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut v = head(5, pairs.len() as u64);
    for (k, val) in pairs {
        v.extend(k);
        v.extend(val);
    }
    v
}

pub enum Key {
    Es256(p256::ecdsa::SigningKey),
    Ed(ed25519_dalek::SigningKey),
}

pub struct Authenticator {
    pub key: Key,
    pub cred_id: Vec<u8>,
}

impl Authenticator {
    pub fn es256(seed: u8) -> Self {
        let mut s = [seed; 32];
        s[0] = 1;
        Self {
            key: Key::Es256(p256::ecdsa::SigningKey::from_slice(&s).unwrap()),
            cred_id: vec![seed; 16],
        }
    }
    pub fn ed(seed: u8) -> Self {
        Self {
            key: Key::Ed(ed25519_dalek::SigningKey::from_bytes(&[seed; 32])),
            cred_id: vec![seed; 20],
        }
    }

    pub fn cose_key(&self) -> Vec<u8> {
        match &self.key {
            Key::Es256(k) => {
                let pt = k.verifying_key().to_encoded_point(false);
                map(&[
                    (int(1), int(2)),
                    (int(3), int(-7)),
                    (int(-1), int(1)),
                    (int(-2), bstr(pt.x().unwrap())),
                    (int(-3), bstr(pt.y().unwrap())),
                ])
            }
            Key::Ed(k) => map(&[
                (int(1), int(1)),
                (int(3), int(-8)),
                (int(-1), int(6)),
                (int(-2), bstr(k.verifying_key().as_bytes())),
            ]),
        }
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        match &self.key {
            Key::Es256(k) => {
                let s: p256::ecdsa::Signature = k.sign(msg);
                s.to_der().as_bytes().to_vec()
            }
            Key::Ed(k) => k.sign(msg).to_bytes().to_vec(),
        }
    }

    pub fn reg_auth_data(&self, rp: &str, flags: u8, count: u32) -> Vec<u8> {
        let mut v = Sha256::digest(rp.as_bytes()).to_vec();
        v.push(flags | AT);
        v.extend(count.to_be_bytes());
        v.extend([0xA5; 16]);
        v.extend((self.cred_id.len() as u16).to_be_bytes());
        v.extend(&self.cred_id);
        v.extend(self.cose_key());
        v
    }
}

pub fn plain_auth_data(rp: &str, flags: u8, count: u32) -> Vec<u8> {
    let mut v = Sha256::digest(rp.as_bytes()).to_vec();
    v.push(flags);
    v.extend(count.to_be_bytes());
    v
}

pub fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"{kind}","challenge":"{}","origin":"{origin}","crossOrigin":false}}"#,
        URL_SAFE_NO_PAD.encode(challenge)
    )
    .into_bytes()
}

pub fn attestation_object(fmt: &str, att_stmt: Vec<u8>, auth_data: &[u8]) -> Vec<u8> {
    map(&[
        (text("fmt"), text(fmt)),
        (text("attStmt"), att_stmt),
        (text("authData"), bstr(auth_data)),
    ])
}

pub fn none_object(auth_data: &[u8]) -> Vec<u8> {
    attestation_object("none", map(&[]), auth_data)
}

pub fn assertion(a: &Authenticator, auth_data: &[u8], cd: &[u8]) -> Vec<u8> {
    let mut msg = auth_data.to_vec();
    msg.extend(Sha256::digest(cd));
    a.sign(&msg)
}
