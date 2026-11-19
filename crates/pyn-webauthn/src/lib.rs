//! WebAuthn Level 2 relying-party verification: registration with `none` attestation and
//! assertion. Pure functions over bytes; no I/O, no async, no HTTP. Algorithms: ES256 and EdDSA.

mod auth_data;
mod cbor;
mod client_data;
mod error;
mod key;

use ciborium::value::Value;
use client_data::Ceremony;
use sha2::{Digest, Sha256};

pub use auth_data::{AttestedCredential, AuthenticatorData};
pub use error::Error;
pub use key::{ALG_EDDSA, ALG_ES256, PublicKey};

/// What the relying party expects of a ceremony.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// The challenge the server issued (raw bytes, not base64url).
    pub challenge: &'a [u8],
    pub rp_id: &'a str,
    /// Allowed origins, compared exactly (for example `https://pyn.example`).
    pub origins: &'a [&'a str],
    pub require_user_verification: bool,
}

/// A verified registration, ready to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub credential_id: Vec<u8>,
    /// The COSE_Key bytes; keep them and pass them to [`verify_assertion`].
    pub cose_key: Vec<u8>,
    /// COSE algorithm of the key (-7 or -8).
    pub alg: i64,
    pub aaguid: [u8; 16],
    pub sign_count: u32,
    pub user_verified: bool,
    pub backup_eligible: bool,
    pub backup_state: bool,
    /// Transports as reported by the client, passed through.
    pub transports: Vec<String>,
}

/// Verdict on the signature counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterVerdict {
    /// The counter increased.
    Advanced,
    /// Both counters are zero: the authenticator keeps none.
    NotUsed,
    /// The counter did not increase while at least one value is non-zero: possible clone.
    Regressed,
}

/// A verified assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assertion {
    pub sign_count: u32,
    pub counter: CounterVerdict,
    pub user_verified: bool,
    pub backup_eligible: bool,
    pub backup_state: bool,
}

/// Verifies a registration response (`none` attestation only).
pub fn verify_registration(
    client_data_json: &[u8],
    attestation_object: &[u8],
    transports: &[&str],
    expected: &Expected<'_>,
) -> Result<Registration, Error> {
    client_data::verify(client_data_json, Ceremony::Create, expected)?;
    let mut obj = cbor::map_entries(cbor::decode_exact(attestation_object)?)?;
    let fmt = match cbor::take_text(&mut obj, "fmt") {
        Some(Value::Text(t)) => t,
        _ => return Err(Error::MalformedAttestation),
    };
    let Some(Value::Map(stmt)) = cbor::take_text(&mut obj, "attStmt") else {
        return Err(Error::MalformedAttestation);
    };
    let Some(Value::Bytes(auth)) = cbor::take_text(&mut obj, "authData") else {
        return Err(Error::MalformedAttestation);
    };
    if fmt != "none" {
        return Err(Error::UnsupportedAttestationFormat(fmt));
    }
    if !stmt.is_empty() {
        return Err(Error::MalformedAttestation);
    }
    let data = AuthenticatorData::parse(&auth)?;
    data.check(expected.rp_id, expected.require_user_verification)?;
    let att = data.attested.ok_or(Error::MissingCredentialData)?;
    let key = PublicKey::from_cose(&att.cose_key)?;
    Ok(Registration {
        credential_id: att.credential_id,
        cose_key: att.cose_key,
        alg: key.alg(),
        aaguid: att.aaguid,
        sign_count: data.sign_count,
        user_verified: data.user_verified,
        backup_eligible: data.backup_eligible,
        backup_state: data.backup_state,
        transports: transports.iter().map(|t| (*t).to_owned()).collect(),
    })
}

/// Verifies an assertion against the stored COSE key and signature counter.
pub fn verify_assertion(
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
    cose_key: &[u8],
    stored_sign_count: u32,
    expected: &Expected<'_>,
) -> Result<Assertion, Error> {
    client_data::verify(client_data_json, Ceremony::Get, expected)?;
    let data = AuthenticatorData::parse(authenticator_data)?;
    data.check(expected.rp_id, expected.require_user_verification)?;
    let key = PublicKey::from_cose(cose_key)?;
    let mut msg = authenticator_data.to_vec();
    msg.extend_from_slice(&Sha256::digest(client_data_json));
    key.verify(&msg, signature)?;
    let counter = if data.sign_count == 0 && stored_sign_count == 0 {
        CounterVerdict::NotUsed
    } else if data.sign_count > stored_sign_count {
        CounterVerdict::Advanced
    } else {
        CounterVerdict::Regressed
    };
    Ok(Assertion {
        sign_count: data.sign_count,
        counter,
        user_verified: data.user_verified,
        backup_eligible: data.backup_eligible,
        backup_state: data.backup_state,
    })
}
