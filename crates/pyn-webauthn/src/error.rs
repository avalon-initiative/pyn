use std::fmt;

/// Why a registration or assertion was rejected. New variants may be added.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// clientDataJSON is not valid JSON of the expected shape.
    MalformedClientData,
    /// `type` is not `webauthn.create` / `webauthn.get` as the ceremony requires.
    WrongType,
    ChallengeMismatch,
    OriginNotAllowed,
    /// `crossOrigin` is true; cross-origin ceremonies are rejected.
    CrossOrigin,
    /// Invalid CBOR, or CBOR of the wrong shape.
    MalformedCbor,
    /// A map has the same key twice.
    DuplicateKey,
    /// authenticatorData is truncated or inconsistent with its flags.
    MalformedAuthData,
    /// Bytes remain after a structure that must end exactly.
    TrailingData,
    /// BS set without BE.
    InvalidFlags,
    /// The attestation object is missing a field or has the wrong types.
    MalformedAttestation,
    /// Only the `none` format is accepted.
    UnsupportedAttestationFormat(String),
    /// Registration authenticatorData carries no attested credential data.
    MissingCredentialData,
    /// Credential ids are at most 1023 bytes and not empty.
    BadCredentialIdLength,
    RpIdHashMismatch,
    UserNotPresent,
    UserNotVerified,
    /// COSE algorithm other than ES256 (-7) or EdDSA (-8).
    UnsupportedAlgorithm(i64),
    /// The COSE key is not a valid key for its algorithm.
    MalformedPublicKey,
    MalformedSignature,
    SignatureInvalid,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedClientData => f.write_str("malformed clientDataJSON"),
            Self::WrongType => f.write_str("wrong ceremony type"),
            Self::ChallengeMismatch => f.write_str("challenge mismatch"),
            Self::OriginNotAllowed => f.write_str("origin not allowed"),
            Self::CrossOrigin => f.write_str("cross-origin ceremony rejected"),
            Self::MalformedCbor => f.write_str("malformed CBOR"),
            Self::DuplicateKey => f.write_str("duplicate map key"),
            Self::MalformedAuthData => f.write_str("malformed authenticator data"),
            Self::TrailingData => f.write_str("trailing data"),
            Self::InvalidFlags => f.write_str("invalid authenticator flags"),
            Self::MalformedAttestation => f.write_str("malformed attestation object"),
            Self::UnsupportedAttestationFormat(fmt) => {
                write!(f, "unsupported attestation format {fmt:?}")
            }
            Self::MissingCredentialData => f.write_str("no attested credential data"),
            Self::BadCredentialIdLength => f.write_str("bad credential id length"),
            Self::RpIdHashMismatch => f.write_str("rpIdHash mismatch"),
            Self::UserNotPresent => f.write_str("user presence flag not set"),
            Self::UserNotVerified => f.write_str("user verification required but not performed"),
            Self::UnsupportedAlgorithm(a) => write!(f, "unsupported COSE algorithm {a}"),
            Self::MalformedPublicKey => f.write_str("malformed public key"),
            Self::MalformedSignature => f.write_str("malformed signature"),
            Self::SignatureInvalid => f.write_str("signature verification failed"),
        }
    }
}

impl std::error::Error for Error {}
