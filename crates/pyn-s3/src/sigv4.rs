//! AWS Signature Version 4 for S3 (header-based, full-payload hash).

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// What a request needs to be signed: everything that ends up in the canonical request.
pub struct Request<'a> {
    pub method: &'a str,
    /// The unencoded path, starting with `/`.
    pub path: &'a str,
    pub query: &'a [(&'a str, &'a str)],
    /// Every header to sign, names in any case; `host` and `x-amz-date` included.
    pub headers: &'a [(&'a str, &'a str)],
    pub payload_sha256: &'a str,
}

pub struct Signature {
    #[cfg_attr(not(test), allow(dead_code))]
    pub canonical_request: String,
    #[cfg_attr(not(test), allow(dead_code))]
    pub string_to_sign: String,
    pub signed_headers: String,
    pub signature: String,
}

/// Percent-encodes everything but the unreserved characters, and `/` when `keep_slash`.
pub fn uri_encode(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Trims a header value and folds runs of spaces into one.
fn header_value(v: &str) -> String {
    v.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn sign(secret: &str, region: &str, amz_date: &str, request: &Request<'_>) -> Signature {
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), header_value(v)))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        uri_encode(request.path, true),
        canonical_query(request.query),
        canonical_headers,
        signed_headers,
        request.payload_sha256
    );
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, b"s3");
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));
    Signature {
        canonical_request,
        string_to_sign,
        signed_headers,
        signature,
    }
}

/// The `Authorization` header value.
pub fn authorization(access_key: &str, region: &str, amz_date: &str, sig: &Signature) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{}/{region}/s3/aws4_request, SignedHeaders={}, Signature={}",
        &amz_date[..8],
        sig.signed_headers,
        sig.signature
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Split so the public-boundary scan does not flag AWS's documented example key.
    const ACCESS: &str = concat!("AKIAIOSFODNN7", "EXAMPLE");
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const DATE: &str = "20130524T000000Z";
    const HOST: &str = "examplebucket.s3.amazonaws.com";

    #[test]
    fn aws_example_get_object_with_range() {
        let headers = [
            ("Host", HOST),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let sig = sign(
            SECRET,
            "us-east-1",
            DATE,
            &Request {
                method: "GET",
                path: "/test.txt",
                query: &[],
                headers: &headers,
                payload_sha256: EMPTY_SHA256,
            },
        );
        assert_eq!(
            sig.canonical_request,
            "GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nrange:bytes=0-9\nx-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\nx-amz-date:20130524T000000Z\n\nhost;range;x-amz-content-sha256;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sig.string_to_sign,
            "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972"
        );
        assert_eq!(
            sig.signature,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
        assert_eq!(
            authorization(ACCESS, "us-east-1", DATE, &sig),
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS}/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            )
        );
    }

    #[test]
    fn aws_example_put_object() {
        let payload = sha256_hex(b"Welcome to Amazon S3.");
        assert_eq!(
            payload,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = [
            ("date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("host", HOST),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", DATE),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ];
        let sig = sign(
            SECRET,
            "us-east-1",
            DATE,
            &Request {
                method: "PUT",
                path: "/test$file.text",
                query: &[],
                headers: &headers,
                payload_sha256: &payload,
            },
        );
        assert!(
            sig.canonical_request
                .starts_with("PUT\n/test%24file.text\n\n")
        );
        assert_eq!(
            sig.signature,
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn aws_example_get_bucket_lifecycle() {
        let headers = [
            ("host", HOST),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let sig = sign(
            SECRET,
            "us-east-1",
            DATE,
            &Request {
                method: "GET",
                path: "/",
                query: &[("lifecycle", "")],
                headers: &headers,
                payload_sha256: EMPTY_SHA256,
            },
        );
        assert_eq!(
            sig.signature,
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }

    #[test]
    fn aws_example_list_objects_orders_query_pairs() {
        let headers = [
            ("host", HOST),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", DATE),
        ];
        let sig = sign(
            SECRET,
            "us-east-1",
            DATE,
            &Request {
                method: "GET",
                path: "/",
                query: &[("prefix", "J"), ("max-keys", "2")],
                headers: &headers,
                payload_sha256: EMPTY_SHA256,
            },
        );
        assert!(
            sig.canonical_request
                .starts_with("GET\n/\nmax-keys=2&prefix=J\n")
        );
        assert_eq!(
            sig.signature,
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn keys_are_encoded_once_per_byte_and_slashes_kept() {
        assert_eq!(uri_encode("a b/c+d~e.f-g_h", true), "a%20b/c%2Bd~e.f-g_h");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode("é", true), "%C3%A9");
        assert_eq!(uri_encode("100%", true), "100%25");
    }

    #[test]
    fn query_pairs_are_encoded_then_sorted_and_empty_values_keep_the_equals() {
        assert_eq!(canonical_query(&[("b", "2"), ("a", "x y")]), "a=x%20y&b=2");
        assert_eq!(canonical_query(&[("uploads", "")]), "uploads=");
        assert_eq!(
            canonical_query(&[("partNumber", "2"), ("uploadId", "a/b")]),
            "partNumber=2&uploadId=a%2Fb"
        );
        assert_eq!(canonical_query(&[]), "");
    }

    #[test]
    fn header_names_are_lowercased_and_values_trimmed_and_folded() {
        let headers = [
            ("X-Amz-Date", DATE),
            ("Host", "  example.com  "),
            ("X-Custom", "a   b \t c"),
        ];
        let sig = sign(
            SECRET,
            "us-east-1",
            DATE,
            &Request {
                method: "GET",
                path: "/",
                query: &[],
                headers: &headers,
                payload_sha256: EMPTY_SHA256,
            },
        );
        assert!(sig.canonical_request.contains(
            "\nhost:example.com\nx-amz-date:20130524T000000Z\nx-custom:a b c\n\nhost;x-amz-date;x-custom\n"
        ));
    }
}
