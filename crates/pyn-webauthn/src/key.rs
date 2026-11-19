use crate::{Error, cbor};
use ciborium::value::Value;

/// COSE algorithm identifier for ES256 (ECDSA P-256 with SHA-256).
pub const ALG_ES256: i64 = -7;
/// COSE algorithm identifier for EdDSA (Ed25519).
pub const ALG_EDDSA: i64 = -8;

/// A credential public key parsed from a COSE_Key.
#[derive(Clone)]
pub enum PublicKey {
    Es256(p256::ecdsa::VerifyingKey),
    Ed25519(ed25519_dalek::VerifyingKey),
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey(alg {})", self.alg())
    }
}

fn bytes(v: Option<Value>) -> Result<Vec<u8>, Error> {
    match v {
        Some(Value::Bytes(b)) => Ok(b),
        _ => Err(Error::MalformedPublicKey),
    }
}

impl PublicKey {
    /// COSE algorithm of this key.
    pub fn alg(&self) -> i64 {
        match self {
            Self::Es256(_) => ALG_ES256,
            Self::Ed25519(_) => ALG_EDDSA,
        }
    }

    /// Parses and validates a COSE_Key (ES256 or EdDSA only).
    pub fn from_cose(cose: &[u8]) -> Result<Self, Error> {
        let mut m = cbor::map_entries(cbor::decode_exact(cose)?)?;
        let alg = cbor::take_int(&mut m, 3)
            .as_ref()
            .and_then(cbor::as_i64)
            .ok_or(Error::MalformedPublicKey)?;
        let kty = cbor::take_int(&mut m, 1).as_ref().and_then(cbor::as_i64);
        let crv = cbor::take_int(&mut m, -1).as_ref().and_then(cbor::as_i64);
        match alg {
            ALG_ES256 => {
                if kty != Some(2) || crv != Some(1) {
                    return Err(Error::MalformedPublicKey);
                }
                let x = bytes(cbor::take_int(&mut m, -2))?;
                let y = bytes(cbor::take_int(&mut m, -3))?;
                if x.len() != 32 || y.len() != 32 {
                    return Err(Error::MalformedPublicKey);
                }
                let mut sec1 = Vec::with_capacity(65);
                sec1.push(0x04);
                sec1.extend_from_slice(&x);
                sec1.extend_from_slice(&y);
                p256::ecdsa::VerifyingKey::from_sec1_bytes(&sec1)
                    .map(Self::Es256)
                    .map_err(|_| Error::MalformedPublicKey)
            }
            ALG_EDDSA => {
                if kty != Some(1) || crv != Some(6) {
                    return Err(Error::MalformedPublicKey);
                }
                let x: [u8; 32] = bytes(cbor::take_int(&mut m, -2))?
                    .try_into()
                    .map_err(|_| Error::MalformedPublicKey)?;
                ed25519_dalek::VerifyingKey::from_bytes(&x)
                    .map(Self::Ed25519)
                    .map_err(|_| Error::MalformedPublicKey)
            }
            other => Err(Error::UnsupportedAlgorithm(other)),
        }
    }

    pub(crate) fn verify(&self, msg: &[u8], sig: &[u8]) -> Result<(), Error> {
        match self {
            Self::Es256(k) => {
                use p256::ecdsa::signature::Verifier;
                let s =
                    p256::ecdsa::Signature::from_der(sig).map_err(|_| Error::MalformedSignature)?;
                k.verify(msg, &s).map_err(|_| Error::SignatureInvalid)
            }
            Self::Ed25519(k) => {
                let s = ed25519_dalek::Signature::from_slice(sig)
                    .map_err(|_| Error::MalformedSignature)?;
                k.verify_strict(msg, &s)
                    .map_err(|_| Error::SignatureInvalid)
            }
        }
    }
}
