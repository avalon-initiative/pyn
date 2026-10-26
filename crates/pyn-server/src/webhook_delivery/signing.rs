use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn tag(key: &[u8], timestamp: i64, body: &[u8]) -> String {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// The `Pyn-Signature` value: `t=<unix>,v1=<hex HMAC-SHA256 of "t.body">`, one `v1` per key.
pub fn signature_header(keys: &[Vec<u8>], timestamp: i64, body: &[u8]) -> String {
    let mut out = format!("t={timestamp}");
    for key in keys {
        out.push_str(",v1=");
        out.push_str(&tag(key, timestamp, body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_the_hmac_of_the_timestamp_dot_body() {
        let header = signature_header(
            &[b"Jefe".to_vec()],
            1_700_000_000,
            b"what do ya want for nothing?",
        );
        let (t, v1) = header.split_once(',').unwrap();
        assert_eq!(t, "t=1700000000");
        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"1700000000.what do ya want for nothing?");
        assert_eq!(
            v1,
            format!("v1={}", hex::encode(mac.finalize().into_bytes()))
        );
    }

    #[test]
    fn every_active_secret_signs_and_a_new_time_changes_the_tag() {
        let keys = [b"one".to_vec(), b"two".to_vec()];
        let a = signature_header(&keys, 10, b"{}");
        assert_eq!(a.matches("v1=").count(), 2);
        let tags: Vec<_> = a.split(',').skip(1).collect();
        assert_ne!(tags[0], tags[1]);
        assert_ne!(a, signature_header(&keys, 11, b"{}"));
    }
}
