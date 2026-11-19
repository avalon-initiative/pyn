//! A software ES256 authenticator for tests of code built on this crate. Behind the `software` feature.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use sha2::{Digest, Sha256};

const UP: u8 = 0x01;
const UV: u8 = 0x04;
const AT: u8 = 0x40;

/// What a registration ceremony returns to the relying party.
pub struct Created {
    pub client_data_json: Vec<u8>,
    pub attestation_object: Vec<u8>,
}

/// What an authentication ceremony returns to the relying party.
pub struct Signed {
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
}

/// One credential. `counter` is the signature counter last reported; registration reports its starting value.
pub struct SoftwareAuthenticator {
    key: SigningKey,
    pub credential_id: Vec<u8>,
    pub counter: u32,
    pub user_verified: bool,
}

fn head(major: u8, n: usize) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        _ => [&[m | 25][..], &(n as u16).to_be_bytes()].concat(),
    }
}

fn bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len()), b.to_vec()].concat()
}

fn text(s: &str) -> Vec<u8> {
    [head(3, s.len()), s.as_bytes().to_vec()].concat()
}

impl SoftwareAuthenticator {
    /// A credential derived from `seed`, so a test can name it.
    pub fn new(seed: u8) -> Self {
        let mut secret = [seed; 32];
        secret[0] = 1;
        Self {
            key: SigningKey::from_slice(&secret).expect("a valid scalar"),
            credential_id: vec![seed; 16],
            counter: 1,
            user_verified: true,
        }
    }

    fn cose_key(&self) -> Vec<u8> {
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut out = head(5, 5);
        out.extend([0x01, 0x02, 0x03, 0x26, 0x20, 0x01]);
        out.push(0x21);
        out.extend(bytes(point.x().expect("uncompressed point")));
        out.push(0x22);
        out.extend(bytes(point.y().expect("uncompressed point")));
        out
    }

    fn flags(&self) -> u8 {
        UP | if self.user_verified { UV } else { 0 }
    }

    fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        format!(
            r#"{{"type":"{kind}","challenge":"{}","origin":"{origin}","crossOrigin":false}}"#,
            URL_SAFE_NO_PAD.encode(challenge)
        )
        .into_bytes()
    }

    /// A `none`-attestation registration for `rp_id` answering `challenge`.
    pub fn create(&self, rp_id: &str, origin: &str, challenge: &[u8]) -> Created {
        let mut data = Sha256::digest(rp_id.as_bytes()).to_vec();
        data.push(self.flags() | AT);
        data.extend(self.counter.to_be_bytes());
        data.extend([0u8; 16]);
        data.extend((self.credential_id.len() as u16).to_be_bytes());
        data.extend(&self.credential_id);
        data.extend(self.cose_key());
        let mut object = head(5, 3);
        object.extend(text("fmt"));
        object.extend(text("none"));
        object.extend(text("attStmt"));
        object.extend(head(5, 0));
        object.extend(text("authData"));
        object.extend(bytes(&data));
        Created {
            client_data_json: Self::client_data("webauthn.create", challenge, origin),
            attestation_object: object,
        }
    }

    /// An assertion answering `challenge`, with the counter advanced by one.
    pub fn get(&mut self, rp_id: &str, origin: &str, challenge: &[u8]) -> Signed {
        self.counter += 1;
        self.sign(rp_id, origin, challenge)
    }

    fn sign(&self, rp_id: &str, origin: &str, challenge: &[u8]) -> Signed {
        let mut data = Sha256::digest(rp_id.as_bytes()).to_vec();
        data.push(self.flags());
        data.extend(self.counter.to_be_bytes());
        let client_data_json = Self::client_data("webauthn.get", challenge, origin);
        let mut message = data.clone();
        message.extend(Sha256::digest(&client_data_json));
        let signature: Signature = self.key.sign(&message);
        Signed {
            client_data_json,
            authenticator_data: data,
            signature: signature.to_der().as_bytes().to_vec(),
        }
    }

    /// Like `get` but reporting exactly `counter`, to stage a regression or a counterless authenticator.
    pub fn get_with_counter(
        &mut self,
        rp_id: &str,
        origin: &str,
        challenge: &[u8],
        counter: u32,
    ) -> Signed {
        self.counter = counter;
        self.sign(rp_id, origin, challenge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Expected, verify_assertion, verify_registration};

    #[test]
    fn round_trips_through_the_verifiers() {
        let mut a = SoftwareAuthenticator::new(3);
        let origins = ["https://pyn.example"];
        let expected = Expected {
            challenge: b"chal",
            rp_id: "pyn.example",
            origins: &origins,
            require_user_verification: true,
        };
        let made = a.create("pyn.example", origins[0], b"chal");
        let reg = verify_registration(
            &made.client_data_json,
            &made.attestation_object,
            &[],
            &expected,
        )
        .unwrap();
        assert_eq!(reg.credential_id, a.credential_id);
        let got = a.get("pyn.example", origins[0], b"chal");
        verify_assertion(
            &got.client_data_json,
            &got.authenticator_data,
            &got.signature,
            &reg.cose_key,
            reg.sign_count,
            &expected,
        )
        .unwrap();
    }
}
