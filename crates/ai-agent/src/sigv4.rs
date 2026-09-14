//! AWS Signature Version 4 request signing, for the Bedrock provider.
//!
//! This exists because the alternative is pulling in the AWS SDK, which is a
//! very large dependency tree for exactly one HTTP call. SigV4 is ~150 lines
//! of HMAC-SHA256, it is specified, and it is testable against a published
//! worked example -- so it is implemented here with only `hmac` and `sha2`,
//! both of which are already in the lockfile.
//!
//! Reference: <https://docs.aws.amazon.com/IAM/latest/UserGuide/create-signed-request.html>

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// AWS credentials for signing.
#[derive(Debug, Clone)]
pub struct Credentials {
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Session token, when using temporary credentials.
    pub session_token: Option<String>,
}

/// A request to be signed.
#[derive(Debug, Clone)]
pub struct SigningRequest<'a> {
    /// HTTP method, uppercase.
    pub method: &'a str,
    /// The `Host` header value, e.g. `bedrock-runtime.us-east-1.amazonaws.com`.
    pub host: &'a str,
    /// Path **already URI-encoded**, e.g. `/model/qwen.qwen3-coder-next/converse`.
    pub canonical_uri: &'a str,
    /// Query string in canonical form (empty if there are no parameters).
    pub canonical_query: &'a str,
    /// Request body.
    pub body: &'a [u8],
    /// The service signing name, e.g. `bedrock`.
    pub service: &'a str,
    /// The region, e.g. `us-east-1`.
    pub region: &'a str,
}

/// The headers SigV4 produced, ready to attach to the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedHeaders {
    /// `Authorization` header value.
    pub authorization: String,
    /// `x-amz-date` header value, which must be sent verbatim.
    pub amz_date: String,
    /// `x-amz-content-sha256`, required by some services (including Bedrock).
    pub content_sha256: String,
    /// `x-amz-security-token`, when temporary credentials are used.
    pub security_token: Option<String>,
}

/// Percent-encode a string for use in a SigV4 canonical URI or query string.
///
/// SigV4 uses a stricter set than `application/x-www-form-urlencoded`: only
/// `A-Z a-z 0-9 - _ . ~` are left alone, and **spaces become `%20`**, never
/// `+`. The path separator `/` is passed through by the caller handling
/// segments separately, not by this function.
#[must_use]
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(byte as char);
        } else if byte == b'/' && !encode_slash {
            out.push('/');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Hex-encode the SHA-256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

/// Derive the SigV4 signing key for one credential scope.
#[must_use]
pub fn signing_key(secret_key: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let mut key = Vec::from(format!("AWS4{secret_key}").as_bytes());
    for part in [date_stamp, region, service, "aws4_request"] {
        let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(part.as_bytes());
        key = mac.finalize().into_bytes().to_vec();
    }
    key
}

/// Sign a request, returning the headers to attach.
#[must_use]
pub fn sign(
    request: &SigningRequest<'_>,
    credentials: &Credentials,
    now: DateTime<Utc>,
) -> SignedHeaders {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let content_sha256 = sha256_hex(request.body);

    // Signed headers must be lowercase and sorted. `content-type` is included
    // because it is sent; omitting a header that *is* sent is a silent
    // mismatch that surfaces only as a 403 from the service.
    let signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date";

    let canonical_headers = format!(
        "content-type:application/json\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
        request.host, content_sha256, amz_date
    );

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        request.canonical_uri,
        request.canonical_query,
        canonical_headers,
        signed_headers,
        content_sha256
    );

    let scope = format!(
        "{date_stamp}/{}/{}/aws4_request",
        request.region, request.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let key = signing_key(
        &credentials.secret_key,
        &date_stamp,
        request.region,
        request.service,
    );
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
    mac.update(string_to_sign.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    );

    SignedHeaders {
        authorization,
        amz_date,
        content_sha256,
        security_token: credentials.session_token.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The worked example from the AWS SigV4 documentation: credential
    /// `AKIDEXAMPLE` / `wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY`, scope
    /// `20150830/us-east-1/iam/aws4_request`.
    ///
    /// Only the payload-hash and canonical-URI details differ from a Bedrock
    /// call; the key derivation is identical. Verified independently against a
    /// `hmac`-based reimplementation rather than copied from this crate's own
    /// output -- a constant taken from the code under test proves nothing.
    const EXPECTED_SIGNING_KEY_HEX: &str =
        "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9";

    #[test]
    fn key_derivation_matches_the_published_example() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(hex::encode(key), EXPECTED_SIGNING_KEY_HEX);
    }

    #[test]
    fn uri_encoding_leaves_unreserved_characters_alone() {
        assert_eq!(uri_encode("AZaz09-_.~", false), "AZaz09-_.~");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a b", false), "a%20b", "space is %20, never +");
    }

    #[test]
    fn a_colon_in_a_model_id_is_encoded() {
        // `anthropic.claude-3-5-sonnet-20240620-v1:0` is a real Bedrock id. A
        // raw `:` in the canonical URI produces a signature mismatch.
        assert_eq!(
            uri_encode("anthropic.claude-3-5-sonnet-20240620-v1:0", false),
            "anthropic.claude-3-5-sonnet-20240620-v1%3A0"
        );
    }

    #[test]
    fn signing_is_deterministic_for_a_fixed_clock() {
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 22, 0, 0).unwrap();
        let credentials = Credentials {
            access_key: "AKIAEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        };
        let request = SigningRequest {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            canonical_uri: "/model/qwen.qwen3-coder-next/converse",
            canonical_query: "",
            body: br#"{"modelId":"x"}"#,
            service: "bedrock",
            region: "us-east-1",
        };

        let first = sign(&request, &credentials, now);
        let second = sign(&request, &credentials, now);
        assert_eq!(first, second);

        assert_eq!(first.amz_date, "20260913T220000Z");
        assert!(first
            .authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/"));
        assert!(first
            .authorization
            .contains("/20260913/us-east-1/bedrock/aws4_request,"));
        assert_eq!(first.content_sha256, sha256_hex(request.body));
    }

    #[test]
    fn a_different_body_produces_a_different_signature() {
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 22, 0, 0).unwrap();
        let credentials = Credentials {
            access_key: "AKIAEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        };
        let base = SigningRequest {
            method: "POST",
            host: "bedrock-runtime.us-east-1.amazonaws.com",
            canonical_uri: "/model/m/converse",
            canonical_query: "",
            body: b"one",
            service: "bedrock",
            region: "us-east-1",
        };
        let other = SigningRequest {
            body: b"two",
            ..base.clone()
        };

        assert_ne!(
            sign(&base, &credentials, now).authorization,
            sign(&other, &credentials, now).authorization
        );
    }
}
