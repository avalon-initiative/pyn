//! Time-based one-time passwords (RFC 6238, over the HOTP of RFC 4226) with HMAC-SHA1, as authenticator apps expect.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::error::{PynError, Result};

pub const SECRET_BYTES: usize = 20;
pub const DIGITS: u32 = 6;
pub const PERIOD_SECS: i64 = 30;
/// Steps accepted on either side of the current one, for clock drift.
pub const SKEW_STEPS: i64 = 1;

pub fn generate_secret() -> Result<[u8; SECRET_BYTES]> {
    let mut secret = [0u8; SECRET_BYTES];
    getrandom::fill(&mut secret).map_err(|e| PynError::Storage(format!("random source: {e}")))?;
    Ok(secret)
}

pub fn step_at(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(PERIOD_SECS)
}

/// The HOTP value of `counter`, truncated to `digits` (RFC 4226 section 5.3).
pub fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("HMAC takes any key");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let word = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    word % 10u32.pow(digits)
}

/// The step `code` is valid for at `unix_secs` and after `after_step`, if any. Steps already used are refused, so a
/// code works once.
pub fn verify(secret: &[u8], code: &str, unix_secs: i64, after_step: Option<i64>) -> Option<i64> {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = step_at(unix_secs);
    let mut found = None;
    for step in (now - SKEW_STEPS)..=(now + SKEW_STEPS) {
        let Ok(counter) = u64::try_from(step) else {
            continue;
        };
        let expected = format!(
            "{:0width$}",
            hotp(secret, counter, DIGITS),
            width = DIGITS as usize
        );
        // Every step is compared, so timing does not say which one matched.
        let same = constant_time_eq(expected.as_bytes(), code.as_bytes());
        if same && after_step.is_none_or(|last| step > last) {
            found = Some(step);
        }
    }
    found
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 4648 base32 without padding, the form authenticator apps take as a manual key.
pub fn base32(data: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The `otpauth://` URI an authenticator app reads from a QR code.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={PERIOD_SECS}",
        percent(issuer),
        percent(account),
        base32(secret),
        percent(issuer),
    )
}

fn percent(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn hotp_matches_the_rfc_4226_vectors() {
        let expected = [
            755224, 287082, 359152, 969429, 338314, 254676, 287922, 162583, 399871, 520489,
        ];
        for (counter, want) in expected.into_iter().enumerate() {
            assert_eq!(
                hotp(RFC_SECRET, counter as u64, 6),
                want,
                "counter {counter}"
            );
        }
    }

    #[test]
    fn totp_matches_the_rfc_6238_sha1_vectors() {
        let vectors = [
            (59, 94287082),
            (1111111109, 7081804),
            (1111111111, 14050471),
            (1234567890, 89005924),
            (2000000000, 69279037),
            (20000000000, 65353130),
        ];
        for (time, want) in vectors {
            assert_eq!(
                hotp(RFC_SECRET, step_at(time) as u64, 8),
                want,
                "time {time}"
            );
        }
    }

    #[test]
    fn a_code_verifies_within_the_skew_and_not_beyond() {
        let code = |time: i64| format!("{:06}", hotp(RFC_SECRET, step_at(time) as u64, 6));
        let now = 1_700_000_000;
        assert_eq!(
            verify(RFC_SECRET, &code(now), now, None),
            Some(step_at(now))
        );
        assert!(verify(RFC_SECRET, &code(now - 30), now, None).is_some());
        assert!(verify(RFC_SECRET, &code(now + 30), now, None).is_some());
        assert!(verify(RFC_SECRET, &code(now - 90), now, None).is_none());
        assert!(verify(RFC_SECRET, &code(now + 90), now, None).is_none());
    }

    #[test]
    fn a_used_step_is_refused_and_malformed_codes_never_match() {
        let now = 1_700_000_000;
        let code = format!("{:06}", hotp(RFC_SECRET, step_at(now) as u64, 6));
        let step = step_at(now);
        assert_eq!(verify(RFC_SECRET, &code, now, Some(step - 1)), Some(step));
        assert_eq!(verify(RFC_SECRET, &code, now, Some(step)), None);
        assert_eq!(verify(RFC_SECRET, &code, now, Some(step + 1)), None);
        for bad in ["", "12345", "1234567", "12345a", "      ", "+12345"] {
            assert_eq!(verify(RFC_SECRET, bad, now, None), None, "{bad:?}");
        }
        assert!(verify(RFC_SECRET, &format!(" {code} "), now, None).is_some());
    }

    #[test]
    fn base32_matches_the_rfc_4648_vectors() {
        for (plain, want) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32(plain.as_bytes()), want);
        }
    }

    #[test]
    fn the_uri_carries_the_issuer_account_and_key() {
        let uri = otpauth_uri("pyn @ host", "alice", RFC_SECRET);
        assert_eq!(
            uri,
            "otpauth://totp/pyn%20%40%20host:alice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=pyn%20%40%20host&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn a_generated_secret_is_random_and_full_length() {
        let (a, b) = (generate_secret().unwrap(), generate_secret().unwrap());
        assert_eq!(a.len(), SECRET_BYTES);
        assert_ne!(a, b);
    }
}
