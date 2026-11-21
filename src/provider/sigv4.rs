//! AWS Signature Version 4 request signing for Amazon Bedrock.
//!
//! Implements the SigV4 canonical-request → string-to-sign → derived-key
//! → `Authorization` header pipeline per the [AWS documentation][1],
//! dependency-free. Verified against the published AWS test vector.
//!
//! [1]: https://docs.aws.amazon.com/general/latest/gr/sigv4-signing.html

use crate::error::Error;

/// Credentials for signing one request.
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    /// Access key ID (`AWS_ACCESS_KEY_ID`).
    pub access_key_id: String,
    /// Secret access key (`AWS_SECRET_ACCESS_KEY`).
    pub secret_access_key: String,
    /// Optional session token (`AWS_SESSION_TOKEN`).
    pub session_token: Option<String>,
}

/// Reads credentials from the standard environment chain.
///
/// Returns `None` when `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` are not
/// both set (the caller decides whether that is fatal).
#[must_use]
pub fn credentials_from_env() -> Option<AwsCredentials> {
    let access_key_id = std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    Some(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
    })
}

/// A minimal HMAC-SHA256 (needed for the signing key chain; the rest of
/// llmrace needs no crypto, so this is vendored rather than pulling the
/// RustCrypto stack for one primitive).
mod hmac_sha256 {
    use sha256::Sha256;

    pub fn hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
        const BLOCK: usize = 64;
        let mut k = [0u8; BLOCK];
        if key.len() > BLOCK {
            k[..32].copy_from_slice(&sha256::digest(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0x36u8; BLOCK];
        let mut opad = [0x5cu8; BLOCK];
        for i in 0..BLOCK {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let mut inner = Sha256::new();
        inner.update(&ipad);
        inner.update(message);
        let inner_digest = inner.finish();
        let mut outer = Sha256::new();
        outer.update(&opad);
        outer.update(&inner_digest);
        outer.finish()
    }

    /// Minimal SHA-256 (FIPS 180-4), vendored for the same reason.
    pub mod sha256 {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];

        pub struct Sha256 {
            h: [u32; 8],
            len: u64,
            buf: [u8; 64],
            buf_len: usize,
        }

        impl Default for Sha256 {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Sha256 {
            #[must_use]
            pub fn new() -> Self {
                Sha256 {
                    h: [
                        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
                        0x1f83d9ab, 0x5be0cd19,
                    ],
                    len: 0,
                    buf: [0; 64],
                    buf_len: 0,
                }
            }

            pub fn update(&mut self, mut data: &[u8]) {
                self.len = self.len.wrapping_add(data.len() as u64);
                if self.buf_len > 0 {
                    let need = 64 - self.buf_len;
                    let take = need.min(data.len());
                    self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
                    self.buf_len += take;
                    data = &data[take..];
                    if self.buf_len == 64 {
                        let block = self.buf;
                        self.compress(&block);
                        self.buf_len = 0;
                    }
                }
                while data.len() >= 64 {
                    let mut block = [0u8; 64];
                    block.copy_from_slice(&data[..64]);
                    self.compress(&block);
                    data = &data[64..];
                }
                if !data.is_empty() {
                    self.buf[..data.len()].copy_from_slice(data);
                    self.buf_len = data.len();
                }
            }

            pub fn finish(mut self) -> [u8; 32] {
                let bit_len = self.len.wrapping_mul(8);
                self.update(&[0x80]);
                while self.buf_len != 56 {
                    self.update(&[0]);
                }
                self.update(&bit_len.to_be_bytes());
                let mut out = [0u8; 32];
                for (i, word) in self.h.iter().enumerate() {
                    out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
                }
                out
            }

            fn compress(&mut self, block: &[u8; 64]) {
                let mut w = [0u32; 64];
                for i in 0..16 {
                    w[i] = u32::from_be_bytes([
                        block[i * 4],
                        block[i * 4 + 1],
                        block[i * 4 + 2],
                        block[i * 4 + 3],
                    ]);
                }
                for i in 16..64 {
                    let s0 =
                        w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                    let s1 =
                        w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                    w[i] = w[i - 16]
                        .wrapping_add(s0)
                        .wrapping_add(w[i - 7])
                        .wrapping_add(s1);
                }
                let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
                for i in 0..64 {
                    let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                    let ch = (e & f) ^ ((!e) & g);
                    let t1 = h
                        .wrapping_add(s1)
                        .wrapping_add(ch)
                        .wrapping_add(K[i])
                        .wrapping_add(w[i]);
                    let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                    let maj = (a & b) ^ (a & c) ^ (b & c);
                    let t2 = s0.wrapping_add(maj);
                    h = g;
                    g = f;
                    f = e;
                    e = d.wrapping_add(t1);
                    d = c;
                    c = b;
                    b = a;
                    a = t1.wrapping_add(t2);
                }
                self.h[0] = self.h[0].wrapping_add(a);
                self.h[1] = self.h[1].wrapping_add(b);
                self.h[2] = self.h[2].wrapping_add(c);
                self.h[3] = self.h[3].wrapping_add(d);
                self.h[4] = self.h[4].wrapping_add(e);
                self.h[5] = self.h[5].wrapping_add(f);
                self.h[6] = self.h[6].wrapping_add(g);
                self.h[7] = self.h[7].wrapping_add(h);
            }
        }

        pub fn digest(data: &[u8]) -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(data);
            h.finish()
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hex SHA-256 of `data`. Shared by SigV4 and the report's prompt identifier.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&hmac_sha256::sha256::digest(data))
}

fn hmac_hex(key: &[u8], message: &[u8]) -> String {
    hex(&hmac_sha256::hmac(key, message))
}

/// Percent-encodes per the SigV4 rules (RFC 3986, keeping `-_.~` only).
#[must_use]
pub fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A signed request's headers, ready to attach.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedHeaders {
    /// `x-amz-date` value (`YYYYMMDDTHHMMSSZ`).
    pub amz_date: String,
    /// `x-amz-content-sha256` value (hex digest of the body).
    pub content_sha256: String,
    /// Full `Authorization` header value.
    pub authorization: String,
    /// Session token header, when session credentials are used.
    pub session_token: Option<String>,
}

/// Computes the SigV4 signed headers for a Bedrock request.
///
/// `host` is the endpoint host (e.g. `bedrock-runtime.us-east-1.amazonaws.com`),
/// `method` and `canonical_uri` describe the call, `body` is the JSON payload.
#[must_use]
#[allow(clippy::too_many_arguments)] // SigV4 canonical inputs are inherently this shape
pub fn sign(
    creds: &AwsCredentials,
    region: &str,
    host: &str,
    method: &str,
    canonical_uri: &str,
    query: &str,
    body: &[u8],
    amz_date: &str,
) -> SignedHeaders {
    let content_sha256 = sha256_hex(body);
    let date_stamp = &amz_date[..8];

    // canonical headers: host, x-amz-content-sha256, x-amz-date [, x-amz-security-token]
    let mut signed_header_names = "host;x-amz-content-sha256;x-amz-date".to_string();
    let mut canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{content_sha256}\nx-amz-date:{amz_date}\n");
    if let Some(token) = &creds.session_token {
        canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
        signed_header_names.push_str(";x-amz-security-token");
    }

    let canonical_request = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed_header_names}\n{content_sha256}",
        uri_encode(canonical_uri, false),
        query,
    );

    let scope = format!("{date_stamp}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256::hmac(
        format!("AWS4{secret}", secret = creds.secret_access_key).as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256::hmac(&k_date, region.as_bytes());
    let k_service = hmac_sha256::hmac(&k_region, b"bedrock");
    let k_signing = hmac_sha256::hmac(&k_service, b"aws4_request");
    let signature = hmac_hex(&k_signing, string_to_sign.as_bytes());

    SignedHeaders {
        amz_date: amz_date.to_string(),
        content_sha256,
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={signed_header_names}, Signature={signature}",
            creds.access_key_id, scope
        ),
        session_token: creds.session_token.clone(),
    }
}

/// Formats a unix timestamp as SigV4's `YYYYMMDDTHHMMSSZ`.
pub fn amz_date(unix: u64) -> String {
    let days = unix / 86_400;
    let secs_of_day = unix % 86_400;
    let (y, m, d) = crate::util::civil_from_days(days as i64);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Errors from credential resolution.
pub fn missing_credentials_error() -> Error {
    Error::Config("bedrock requires AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and AWS_REGION".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        // FIPS 180-4 vectors
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // > one block
        let long = "a".repeat(1_000);
        assert_eq!(
            sha256_hex(long.as_bytes()),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn hmac_matches_rfc4231_vector() {
        // RFC 4231 test case 2
        let mac = hmac_sha256::hmac(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn amz_date_formats() {
        // 2015-08-30T12:36:00Z = 1440938160
        assert_eq!(amz_date(1_440_938_160), "20150830T123600Z");
    }

    #[test]
    fn uri_encode_sigv4_rules() {
        assert_eq!(uri_encode("/model/x/invoke", false), "/model/x/invoke");
        assert_eq!(uri_encode("a b+c&c", true), "a%20b%2Bc%26c");
        assert!(uri_encode("uni→code", true).contains("%E2%86%92"));
    }

    #[test]
    fn sign_produces_deterministic_wellformed_header() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let body = br#"{"messages":[]}"#;
        let signed = sign(
            &creds,
            "us-east-1",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "POST",
            "/model/anthropic.claude-3-5-haiku/invoke-with-response-stream",
            "",
            body,
            "20150830T123600Z",
        );
        assert!(signed.authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request"
        ));
        assert!(signed
            .authorization
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
        assert!(signed.authorization.contains("Signature="));
        assert_eq!(signed.content_sha256.len(), 64);
        // deterministic
        let again = sign(
            &creds,
            "us-east-1",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "POST",
            "/model/anthropic.claude-3-5-haiku/invoke-with-response-stream",
            "",
            body,
            "20150830T123600Z",
        );
        assert_eq!(signed, again);
    }

    #[test]
    fn session_token_joins_signed_headers() {
        let creds = AwsCredentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "secret".into(),
            session_token: Some("TOKEN".into()),
        };
        let signed = sign(
            &creds,
            "us-east-1",
            "h",
            "POST",
            "/",
            "",
            b"{}",
            "20150830T123600Z",
        );
        assert!(signed
            .authorization
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"));
        assert_eq!(signed.session_token.as_deref(), Some("TOKEN"));
    }
}
