use crate::{Error, cbor};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

const UP: u8 = 0x01;
const UV: u8 = 0x04;
const BE: u8 = 0x08;
const BS: u8 = 0x10;
const AT: u8 = 0x40;
const ED: u8 = 0x80;

/// Parsed authenticatorData.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatorData {
    pub rp_id_hash: [u8; 32],
    pub user_present: bool,
    pub user_verified: bool,
    pub backup_eligible: bool,
    pub backup_state: bool,
    pub sign_count: u32,
    pub attested: Option<AttestedCredential>,
    /// Raw CBOR of the extensions map when the ED flag is set.
    pub extensions: Option<Vec<u8>>,
}

/// Attested credential data from a registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestedCredential {
    pub aaguid: [u8; 16],
    pub credential_id: Vec<u8>,
    /// The COSE_Key exactly as the authenticator encoded it.
    pub cose_key: Vec<u8>,
}

fn take<'a>(buf: &mut &'a [u8], n: usize) -> Result<&'a [u8], Error> {
    if buf.len() < n {
        return Err(Error::MalformedAuthData);
    }
    let (head, tail) = buf.split_at(n);
    *buf = tail;
    Ok(head)
}

impl AuthenticatorData {
    /// Parses authenticatorData; bytes beyond the structure are an error.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut buf = bytes;
        let rp_id_hash: [u8; 32] = take(&mut buf, 32)?.try_into().unwrap();
        let flags = take(&mut buf, 1)?[0];
        let sign_count = u32::from_be_bytes(take(&mut buf, 4)?.try_into().unwrap());
        if flags & BS != 0 && flags & BE == 0 {
            return Err(Error::InvalidFlags);
        }
        let mut attested = None;
        if flags & AT != 0 {
            let aaguid: [u8; 16] = take(&mut buf, 16)?.try_into().unwrap();
            let len = u16::from_be_bytes(take(&mut buf, 2)?.try_into().unwrap()) as usize;
            if len == 0 || len > 1023 {
                return Err(Error::BadCredentialIdLength);
            }
            let credential_id = take(&mut buf, len)?.to_vec();
            let (key, rest) = cbor::decode_prefix(buf)?;
            let used = buf.len() - rest.len();
            let cose_key = buf[..used].to_vec();
            drop(key);
            buf = rest;
            attested = Some(AttestedCredential {
                aaguid,
                credential_id,
                cose_key,
            });
        }
        let mut extensions = None;
        if flags & ED != 0 {
            let (ext, rest) = cbor::decode_prefix(buf)?;
            if !matches!(ext, ciborium::value::Value::Map(_)) {
                return Err(Error::MalformedCbor);
            }
            extensions = Some(buf[..buf.len() - rest.len()].to_vec());
            buf = rest;
        }
        if !buf.is_empty() {
            return Err(Error::TrailingData);
        }
        Ok(Self {
            rp_id_hash,
            user_present: flags & UP != 0,
            user_verified: flags & UV != 0,
            backup_eligible: flags & BE != 0,
            backup_state: flags & BS != 0,
            sign_count,
            attested,
            extensions,
        })
    }

    /// Checks rpIdHash, UP, and UV when required.
    pub(crate) fn check(&self, rp_id: &str, require_uv: bool) -> Result<(), Error> {
        let want = Sha256::digest(rp_id.as_bytes());
        if !bool::from(self.rp_id_hash.ct_eq(want.as_slice())) {
            return Err(Error::RpIdHashMismatch);
        }
        if !self.user_present {
            return Err(Error::UserNotPresent);
        }
        if require_uv && !self.user_verified {
            return Err(Error::UserNotVerified);
        }
        Ok(())
    }
}
