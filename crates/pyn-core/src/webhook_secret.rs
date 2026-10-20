use std::fmt;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use sha2::{Digest, Sha256};

use crate::error::{PynError, Result};
use crate::webhook::{NewSecret, WebhookSecret};

/// Prefix of a generated signing secret, so a leaked one is recognisable.
pub const SECRET_PREFIX: &str = "whsec_";

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const AAD: &[u8] = b"pyn-webhook-secret-v1";

/// `bytes` must be `NONCE_BYTES` long.
fn nonce_of(bytes: &[u8]) -> Nonce<U12> {
    let mut nonce = Nonce::<U12>::default();
    nonce.copy_from_slice(bytes);
    nonce
}

/// A fresh signing secret: `whsec_` and 64 hex characters (32 random bytes). The text is the HMAC key.
pub fn generate_secret() -> Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|e| PynError::Storage(format!("random source: {e}")))?;
    Ok(format!("{SECRET_PREFIX}{}", hex::encode(raw)))
}

/// The server key (`PYN_SECRET_KEY`) that encrypts signing secrets at rest with AES-256-GCM.
#[derive(Clone)]
pub struct SecretKey {
    cipher: Aes256Gcm,
    key_id: String,
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl SecretKey {
    /// Parses 32 bytes written as 64 hex characters.
    pub fn from_hex(text: &str) -> Result<Self> {
        let invalid = || {
            PynError::InvalidRequest(format!(
                "the secret key must be {KEY_BYTES} bytes written as {} hex characters",
                KEY_BYTES * 2
            ))
        };
        let bytes = hex::decode(text.trim()).map_err(|_| invalid())?;
        if bytes.len() != KEY_BYTES {
            return Err(invalid());
        }
        let cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| invalid())?;
        let digest = Sha256::digest(&bytes);
        Ok(Self {
            cipher,
            key_id: hex::encode(&digest[..4]),
        })
    }

    /// Identifies the key without revealing it; stored beside each secret it encrypts.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The secret as the store keeps it: encrypted under this key.
    pub fn seal(&self, secret: &str) -> Result<NewSecret> {
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce)
            .map_err(|e| PynError::Storage(format!("random source: {e}")))?;
        let ciphertext = self
            .cipher
            .encrypt(
                &nonce_of(&nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: AAD,
                },
            )
            .map_err(|_| PynError::Storage("could not encrypt a webhook secret".into()))?;
        let mut stored = nonce.to_vec();
        stored.extend_from_slice(&ciphertext);
        Ok(NewSecret {
            secret: stored,
            encrypted: true,
            key_id: Some(self.key_id.clone()),
        })
    }
}

/// The secret as stored: plaintext, or encrypted under `key`.
pub fn store_secret(key: Option<&SecretKey>, secret: &str) -> Result<NewSecret> {
    match key {
        Some(key) => key.seal(secret),
        None => Ok(NewSecret {
            secret: secret.as_bytes().to_vec(),
            encrypted: false,
            key_id: None,
        }),
    }
}

/// The HMAC key bytes of a stored secret, decrypting it when it was stored encrypted. Fails when no key is set,
/// when `key` is not the one that encrypted it, or when the data was altered.
pub fn reveal_secret(key: Option<&SecretKey>, stored: &WebhookSecret) -> Result<Vec<u8>> {
    if !stored.encrypted {
        return Ok(stored.secret.clone());
    }
    let key = key.ok_or_else(|| {
        PynError::Storage("a webhook secret is encrypted but no PYN_SECRET_KEY is set".into())
    })?;
    if stored.key_id.as_deref() != Some(key.key_id()) {
        return Err(PynError::Storage(
            "a webhook secret was encrypted under a different PYN_SECRET_KEY".into(),
        ));
    }
    if stored.secret.len() <= NONCE_BYTES {
        return Err(PynError::Storage("a webhook secret is truncated".into()));
    }
    let (nonce, ciphertext) = stored.secret.split_at(NONCE_BYTES);
    key.cipher
        .decrypt(
            &nonce_of(nonce),
            Payload {
                msg: ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| PynError::Storage("a webhook secret could not be decrypted".into()))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn row(new: NewSecret) -> WebhookSecret {
        WebhookSecret {
            id: 1,
            subscription_id: 1,
            secret: new.secret,
            encrypted: new.encrypted,
            key_id: new.key_id,
            created_at: Utc::now(),
            retired_at: None,
        }
    }

    #[test]
    fn a_generated_secret_has_the_prefix_and_32_random_bytes() {
        let a = generate_secret().unwrap();
        assert!(a.starts_with(SECRET_PREFIX));
        assert_eq!(hex::decode(&a[SECRET_PREFIX.len()..]).unwrap().len(), 32);
        assert_ne!(a, generate_secret().unwrap());
    }

    #[test]
    fn an_encrypted_secret_round_trips_and_hides_the_plaintext() {
        let key = SecretKey::from_hex(KEY).unwrap();
        let sealed = store_secret(Some(&key), "whsec_abc").unwrap();
        assert!(sealed.encrypted);
        assert!(!sealed.secret.windows(9).any(|w| w == b"whsec_abc"));
        assert_eq!(
            reveal_secret(Some(&key), &row(sealed)).unwrap(),
            b"whsec_abc"
        );
    }

    #[test]
    fn a_plaintext_secret_needs_no_key() {
        let plain = row(store_secret(None, "whsec_abc").unwrap());
        assert!(!plain.encrypted);
        assert_eq!(reveal_secret(None, &plain).unwrap(), b"whsec_abc");
    }

    #[test]
    fn the_wrong_key_none_or_tampering_fail() {
        let key = SecretKey::from_hex(KEY).unwrap();
        let other = SecretKey::from_hex(&"11".repeat(32)).unwrap();
        let stored = row(store_secret(Some(&key), "whsec_abc").unwrap());
        assert!(reveal_secret(Some(&other), &stored).is_err());
        assert!(reveal_secret(None, &stored).is_err());
        let mut flipped = stored.clone();
        *flipped.secret.last_mut().unwrap() ^= 1;
        assert!(reveal_secret(Some(&key), &flipped).is_err());
    }

    #[test]
    fn a_bad_key_is_refused() {
        for bad in ["", "zz", &"00".repeat(31), &"00".repeat(33)] {
            assert!(SecretKey::from_hex(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn debug_shows_no_key_material() {
        let key = SecretKey::from_hex(KEY).unwrap();
        assert!(!format!("{key:?}").contains("0102"));
    }
}
