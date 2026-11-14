//! Server-Sent Events (SSE) parsing for streaming LLM APIs.
//!
//! The [`SseParser`] accumulates raw bytes and yields complete event blocks as
//! they arrive, tolerating arbitrary chunk boundaries from the network layer.

/// An incremental SSE byte-stream parser.
///
/// Feed it raw response bytes as they arrive; it returns every complete
/// event block (the text between blank-line separators). Call [`SseParser::finish`]
/// once the stream ends to flush any trailing block that was not terminated
/// by a blank line.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// Start of the unconsumed region: bytes before it belong to blocks
    /// already returned. Avoids an O(n) drain per block.
    start: usize,
}

impl SseParser {
    /// Maximum unconsumed bytes buffered while searching for the next event
    /// separator.
    pub const MAX_BUFFER_BYTES: usize = 1024 * 1024;

    /// Creates an empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds raw bytes and returns all event blocks completed by this write.
    ///
    /// Returns [`Error::Protocol`](crate::error::Error::Protocol) when the
    /// unconsumed buffer would exceed [`SseParser::MAX_BUFFER_BYTES`] without
    /// a separator — a hardening bound against hostile or broken streams.
    pub fn feed(&mut self, bytes: &[u8]) -> crate::error::Result<Vec<String>> {
        if self.buf.len() - self.start + bytes.len() > Self::MAX_BUFFER_BYTES {
            return Err(crate::error::Error::Protocol(format!(
                "sse buffer exceeded {} bytes without a separator",
                Self::MAX_BUFFER_BYTES
            )));
        }
        self.buf.extend_from_slice(bytes);
        let mut blocks = Vec::new();
        loop {
            let window = &self.buf[self.start..];
            let Some(rel) = find_separator(window) else {
                break;
            };
            let pos = self.start + rel;
            let sep_len = if self.buf[pos..].starts_with(b"\r\n\r\n") {
                4
            } else {
                2
            };
            let end = pos + sep_len;
            if let Some(text) = block_text(&self.buf[self.start..end]) {
                blocks.push(text);
            }
            self.start = end;
        }
        self.compact();
        Ok(blocks)
    }

    /// Flushes and returns a trailing unterminated block, if any.
    pub fn finish(&mut self) -> Vec<String> {
        if self.start >= self.buf.len() {
            self.buf.clear();
            self.start = 0;
            return Vec::new();
        }
        let block = &self.buf[self.start..];
        let out = match block_text(block) {
            Some(text) => vec![text],
            None => Vec::new(),
        };
        self.buf.clear();
        self.start = 0;
        out
    }

    /// Drops consumed bytes once they dominate the buffer, keeping the
    /// allocation bounded across a long stream.
    fn compact(&mut self) {
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > 64 * 1024 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

fn block_text(block: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn find_separator(buf: &[u8]) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        if buf[i] == b'\r' && buf[i..].starts_with(b"\r\n\r\n") {
            return Some(i);
        }
        // The SSE spec also allows bare-CR line endings, so `\r\r` is a
        // blank line (event separator) just like `\n\n`.
        if buf[i] == b'\r' && buf[i + 1] == b'\r' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Extracts the `data:` payload strings from a complete event block, zero-copy.
///
/// Lines that are not `data:` lines (event names, ids, comments) are ignored.
pub fn parse_data_events(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.strip_prefix("data:").map(|rest| rest.trim_start())
        })
        .collect()
}
