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
//! Both CRCs are CRC-32 (IEEE). Malformed frames (bad lengths or
//! CRCs) are errors, never panics — the parser is also a fuzz target.

use crate::error::{Error, Result};

/// CRC-32 (IEEE), table-driven, reflected.
///
/// The standard check value is `crc32(b"123456789") == 0xCBF43926`.
fn crc32(data: &[u8]) -> u32 {
    fn table() -> [u32; 256] {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
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
    if complete_frame_len(buf)?.is_none() {
        return Err(Error::Protocol("event-stream: truncated frame".into()));
    }
    if buf.len() < 16 {
        return Err(Error::Protocol(
            "event-stream: frame shorter than prelude".into(),
        ));
    }
    let total_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let headers_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    // bounds: minimum message is 16 bytes; lengths must fit the buffer and
    // leave room for the trailing CRC
    if !(16..=32 * 1024 * 1024).contains(&total_len) {
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
    if crc32(&frame[..8]) != prelude_crc {
        return Err(Error::Protocol("event-stream: prelude CRC mismatch".into()));
    }
    // message CRC covers everything except the trailing 4 bytes
    let msg_crc = u32::from_be_bytes([
        frame[total_len - 4],
        frame[total_len - 3],
        frame[total_len - 2],
        frame[total_len - 1],
    ]);
    if crc32(&frame[..total_len - 4]) != msg_crc {
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

/// Distinguishes incomplete input from an invalid prelude before buffering
/// more bytes. The 32 MiB implementation limit exceeds AWS's maximum payload
/// plus headers and prevents malformed lengths from growing memory forever.
pub(super) fn complete_frame_len(buf: &[u8]) -> Result<Option<usize>> {
    if buf.len() < 12 {
        return Ok(None);
    }
    let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let headers = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    if !(16..=32 * 1024 * 1024).contains(&total) || headers > total - 16 {
        return Err(Error::Protocol(
            "event-stream: invalid frame lengths".into(),
        ));
    }
    let expected = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if crc32(&buf[..8]) != expected {
        return Err(Error::Protocol("event-stream: prelude CRC mismatch".into()));
    }
    Ok((buf.len() >= total).then_some(total))
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
        if section.len() < 2 + name_len {
            return Err(Error::Protocol(
                "event-stream: truncated header name".into(),
            ));
        }
        let name = &section[1..1 + name_len];
        let value_type = section[1 + name_len];
        let mut rest = &section[2 + name_len..];
        // Only byte arrays (6) and strings (7) are length-prefixed;
        // other value types have fixed sizes
        let value_bytes: Option<&[u8]> = match value_type {
            6 | 7 => {
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
                let v = &rest[2..2 + vlen];
                rest = &rest[2 + vlen..];
                Some(v)
            }
            8 => {
                // timestamp: 8-byte big-endian; we don't use it
                if rest.len() < 8 {
                    return Err(Error::Protocol("event-stream: truncated timestamp".into()));
                }
                rest = &rest[8..];
                None
            }
            t => {
                let size = match t {
                    0 | 1 => 0, // bool is encoded in the type itself
                    2 => 1,     // byte
                    3 => 2,     // short
                    4 => 4,     // int
                    5 => 8,     // long
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
                None
            }
        };
        match name {
            b":event-type" => {
                event_type = value_bytes
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
            }
            b":message-type" => {
                message_type = value_bytes
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
            }
            b":exception-type" | b":error-code" | b":error-message" => {}
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
    headers.extend_from_slice(name);
    headers.push(7); // string value
    let value = event_type.as_bytes();
    headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
    headers.extend_from_slice(value);

    let total_len = 12 + headers.len() + payload.len() + 4;
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&(total_len as u32).to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&headers);
    out.extend_from_slice(payload);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_ieee_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
        assert_eq!(crc32(b""), 0x00000000);
        assert_eq!(crc32(b"a"), 0xE8B7BE43);
        assert_eq!(crc32(b"abcdefgh"), 0xAEEF2A50);
    }

    #[test]
    fn independently_encoded_aws_frame_and_every_prefix() {
        // AWS header layout + Python stdlib zlib.crc32, not encode_frame.
        let hex = "0000005d000000366a1b418a0d3a6d6573736167652d747970650700056576656e740b3a6576656e742d74797065070011636f6e74656e74426c6f636b44656c74617b2264656c7461223a7b2274657874223a224869227d7d384de711";
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect();
        for cut in 0..bytes.len() {
            assert_eq!(
                complete_frame_len(&bytes[..cut]).expect("valid prefix"),
                None
            );
        }
        let (frame, used) = decode_frame(&bytes).expect("AWS frame");
        assert_eq!(used, bytes.len());
        assert_eq!(frame.message_type, "event");
        assert_eq!(frame.event_type, "contentBlockDelta");
        assert_eq!(frame.payload, br#"{"delta":{"text":"Hi"}}"#);
        let mut corrupt = bytes;
        corrupt[8] ^= 1;
        assert!(
            complete_frame_len(&corrupt[..12]).is_err(),
            "invalid preludes fail immediately"
        );
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
