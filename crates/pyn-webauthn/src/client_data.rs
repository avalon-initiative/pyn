use crate::{Error, Expected};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use subtle::ConstantTimeEq;

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin")]
    cross_origin: Option<bool>,
}

/// The ceremony a clientDataJSON belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ceremony {
    Create,
    Get,
}

impl Ceremony {
    fn type_str(self) -> &'static str {
        match self {
            Self::Create => "webauthn.create",
            Self::Get => "webauthn.get",
        }
    }
}

/// Checks type, challenge, origin and crossOrigin of a clientDataJSON.
pub(crate) fn verify(
    json: &[u8],
    ceremony: Ceremony,
    expected: &Expected<'_>,
) -> Result<(), Error> {
    let raw: Raw = serde_json::from_slice(json).map_err(|_| Error::MalformedClientData)?;
    if raw.kind != ceremony.type_str() {
        return Err(Error::WrongType);
    }
    let challenge = URL_SAFE_NO_PAD
        .decode(raw.challenge.as_bytes())
        .map_err(|_| Error::MalformedClientData)?;
    if !bool::from(challenge.ct_eq(expected.challenge)) {
        return Err(Error::ChallengeMismatch);
    }
    if !expected.origins.iter().any(|o| *o == raw.origin) {
        return Err(Error::OriginNotAllowed);
    }
    if raw.cross_origin == Some(true) {
        return Err(Error::CrossOrigin);
    }
    Ok(())
}
