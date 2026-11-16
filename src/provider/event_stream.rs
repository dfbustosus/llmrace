//! Decoder for the AWS `event-stream` wire format used by Bedrock
//! streaming responses (`invoke-with-response-stream` /
//! `converse-stream`).
//!
//! Each message is a CRC-validated frame:
//!
//! ```text
//! prelude:  total_len (u32) | headers_len (u32) | prelude_crc (u32)
//! section:  headers (headers_len bytes of TLV records) | payload | crc (u32)
//! ```
//!
//! Both CRCs are CRC-32C (Castagnoli). Malformed frames (bad lengths or
//! CRCs) are errors, never panics — the parser is also a fuzz target.

use crate::error::{Error, Result};

/// CRC-32C (Castagnoli), table-driven, reflected.
///
/// Verified against the iSCSI test vectors (e.g. `crc32c(b"123456789") == 0xE3069283`).
fn crc32c(data: &[u8]) -> u32 {
    fn table() -> [u32; 256] {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0x82F63B78 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    }
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(table);
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = t[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// One decoded event-stream frame.
#[derive(Debug, Clone, PartialEq)]
pub struct EventFrame {
    /// `:event-type` header value (e.g. `contentBlockDelta`).
    pub event_type: String,
    /// `:message-type` header value (`event` or `exception`).
    pub message_type: String,
    /// The JSON payload bytes.
    pub payload: Vec<u8>,
}

/// Decodes one complete frame from `buf`.
///
/// Returns the frame and the number of bytes consumed. Errors on
/// truncated input, impossible lengths, or CRC mismatches.
pub fn decode_frame(buf: &[u8]) -> Result<(EventFrame, usize)> {
    if buf.len() < 16 {
        return Err(Error::Protocol(
            "event-stream: frame shorter than prelude".into(),
        ));
    }
    let total_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let headers_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    // bounds: minimum message is 16 bytes; lengths must fit the buffer and
    // leave room for the trailing CRC
    if !(16..=16 * 1024 * 1024).contains(&total_len) {
        return Err(Error::Protocol(format!(
            "event-stream: impossible total length {total_len}"
        )));
    }
    if headers_len + 16 > total_len {
        return Err(Error::Protocol(format!(
            "event-stream: headers length {headers_len} exceeds message"
        )));
    }
    if buf.len() < total_len {
        return Err(Error::Protocol(format!(
            "event-stream: truncated frame ({}/{total_len})",
            buf.len()
        )));
    }
    let frame = &buf[..total_len];

    // prelude CRC covers the first 8 bytes
    let prelude_crc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if crc32c(&frame[..8]) != prelude_crc {
        return Err(Error::Protocol("event-stream: prelude CRC mismatch".into()));
    }
    // message CRC covers everything except the trailing 4 bytes
    let msg_crc = u32::from_be_bytes([
        frame[total_len - 4],
        frame[total_len - 3],
        frame[total_len - 2],
        frame[total_len - 1],
    ]);
    if crc32c(&frame[..total_len - 4]) != msg_crc {
        return Err(Error::Protocol("event-stream: message CRC mismatch".into()));
    }

    let headers_end = 12 + headers_len;
    let (event_type, message_type) = parse_headers(&frame[12..headers_end])?;
    let payload = frame[headers_end..total_len - 4].to_vec();
    Ok((
        EventFrame {
            event_type,
            message_type,
            payload,
        },
        total_len,
    ))
}

/// Walks the TLV header section for the headers we care about.
fn parse_headers(mut section: &[u8]) -> Result<(String, String)> {
    let mut event_type = String::new();
    let mut message_type = String::new();
    while !section.is_empty() {
        if section.len() < 2 {
            return Err(Error::Protocol("event-stream: truncated header".into()));
        }
        let name_len = section[0] as usize;
        let value_type = section[1];
        if section.len() < 2 + name_len {
            return Err(Error::Protocol(
                "event-stream: truncated header name".into(),
            ));
        }
        let name = String::from_utf8_lossy(&section[2..2 + name_len]).to_string();
        let mut rest = &section[2 + name_len..];
        // only string (7) and timestamp (8) values are length-prefixed;
        // other value types have fixed sizes
        let value = match value_type {
            7 => {
                if rest.len() < 2 {
                    return Err(Error::Protocol(
                        "event-stream: truncated string header".into(),
                    ));
                }
                let vlen = u16::from_be_bytes([rest[0], rest[1]]) as usize;
                if rest.len() < 2 + vlen {
                    return Err(Error::Protocol(
                        "event-stream: truncated string value".into(),
                    ));
                }
                let v = String::from_utf8_lossy(&rest[2..2 + vlen]).to_string();
                rest = &rest[2 + vlen..];
                v
            }
            8 => {
                // timestamp: 8-byte big-endian; we don't use it
                if rest.len() < 8 {
                    return Err(Error::Protocol("event-stream: truncated timestamp".into()));
                }
                rest = &rest[8..];
                String::new()
            }
            t => {
                let size = match t {
                    0 | 1 => 1, // bool
                    2 | 3 => 2, // byte/short
                    4 | 5 => 4, // int/float
                    6 => 8,     // long/double
                    9 => 16,    // uuid
                    _ => {
                        return Err(Error::Protocol(format!(
                            "event-stream: unknown header value type {t}"
                        )))
                    }
                };
                if rest.len() < size {
                    return Err(Error::Protocol(
                        "event-stream: truncated fixed header".into(),
                    ));
                }
                rest = &rest[size..];
                String::new()
            }
        };
        match name.as_str() {
            ":event-type" => event_type = value,
            ":message-type" => message_type = value,
            _ => {}
        }
        section = rest;
    }
    Ok((event_type, message_type))
}

/// Builds a well-formed frame (test/fuzz-corpus helper).
#[cfg(test)]
#[must_use]
pub fn encode_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    let name = b":event-type";
    headers.push(name.len() as u8);
    headers.push(7); // string value
    headers.extend_from_slice(name);
    let value = event_type.as_bytes();
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value);

    let total_len = 12 + headers.len() + payload.len() + 4;
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&(total_len as u32).to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32c(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&headers);
    out.extend_from_slice(payload);
    let crc = crc32c(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_iscsi_vectors() {
        assert_eq!(crc32c(b"123456789"), 0xE3069283);
        assert_eq!(crc32c(b""), 0x00000000);
        assert_eq!(crc32c(b"a"), 0xC1D04330);
        assert_eq!(crc32c(b"abcdefgh"), 0x0A9421B7);
    }

    #[test]
    fn roundtrip_frame() {
        #![allow(clippy::unwrap_used)] // tests may unwrap
        let frame = encode_frame("contentBlockDelta", br#"{"delta":{"text":"Hi"}}"#);
        let (decoded, used) = decode_frame(&frame).unwrap();
        assert_eq!(used, frame.len());
        assert_eq!(decoded.event_type, "contentBlockDelta");
        assert_eq!(decoded.message_type, "");
        assert_eq!(decoded.payload, br#"{"delta":{"text":"Hi"}}"#);
    }

    #[test]
    fn corrupted_payload_fails_crc() {
        let mut frame = encode_frame("messageStop", b"{}");
        let last = frame.len() - 5;
        frame[last] ^= 0xFF;
        assert!(decode_frame(&frame).is_err());
    }

    #[test]
    fn corrupted_prelude_fails() {
        let mut frame = encode_frame("messageStart", b"{}");
        frame[4] ^= 0xFF; // headers_len byte -> also breaks prelude CRC
        assert!(decode_frame(&frame).is_err());
    }

    #[test]
    fn truncated_input_errors_not_panics() {
        let frame = encode_frame("metadata", b"{}");
        for cut in 0..frame.len() {
            // any prefix must error or decode, never panic
            let _ = decode_frame(&frame[..cut]);
        }
    }

    #[test]
    fn impossible_lengths_rejected() {
        let mut tiny = vec![0u8; 16];
        tiny[0..4].copy_from_slice(&8u32.to_be_bytes()); // total < 16
        assert!(decode_frame(&tiny).is_err());
        let mut big_headers = vec![0u8; 16];
        big_headers[0..4].copy_from_slice(&64u32.to_be_bytes());
        big_headers[4..8].copy_from_slice(&60u32.to_be_bytes()); // 16+60 > 64
        assert!(decode_frame(&big_headers).is_err());
    }
}
