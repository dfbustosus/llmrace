//! Shared utility functions used across modules.

/// Escapes the HTML-significant characters in `s`.
///
/// Provider names, models and prompts are user input and are interpolated
/// into HTML reports; without escaping a model id like `<script>` would be
/// stored XSS in a shared report.
#[must_use]
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes XML-significant characters and strips characters illegal in XML 1.0.
///
/// XML 1.0 forbids control characters below `U+0020` other than `\t`, `\n`,
/// and `\r`, as well as the non-characters `U+FFFE` and `U+FFFF`.
#[must_use]
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if (c < '\u{20}' && !matches!(c, '\t' | '\n' | '\r'))
            || matches!(c, '\u{FFFE}' | '\u{FFFF}')
        {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Redacts userinfo, path, query string, and fragment from a URL for safe
/// logging and error messages.
///
/// Valid URLs with a host are formatted as `scheme://host[:port]/…`; unparseable
/// or hostless values return `"[redacted-url]"`.
#[must_use]
pub fn redact_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(parsed) => match parsed.host_str() {
            Some(host) => match parsed.port() {
                Some(port) => format!("{}://{host}:{port}/\u{2026}", parsed.scheme()),
                None => format!("{}://{host}/\u{2026}", parsed.scheme()),
            },
            None => "[redacted-url]".to_string(),
        },
        Err(_) => "[redacted-url]".to_string(),
    }
}

/// Escapes a value for safe interpolation into a Markdown table cell.
///
/// Replaces `\r`/`\n` with spaces, strips remaining control characters, and
/// escapes `|`, `` ` ``, `<`, and `>`.
#[must_use]
pub fn md_escape_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' | '\n' => out.push(' '),
            c if c.is_control() => {}
            '|' => out.push_str("\\|"),
            '`' => out.push_str("\\`"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Sanitizes untrusted text for terminal display by replacing `\r`, `\n`, and
/// `\t` with spaces and dropping all other control characters (such as ANSI
/// escape sequences' `\x1b` and bell `\x07`).
#[must_use]
pub(crate) fn sanitize_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' | '\n' | '\t' => out.push(' '),
            c if c.is_control() => {}
            _ => out.push(c),
        }
    }
    out
}

fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(buf.len());
    let take = chunk.len().min(remaining);
    if let Some(slice) = chunk.get(..take) {
        buf.extend_from_slice(slice);
    }
    chunk.len() >= remaining
}

/// Streams an HTTP response body up to `max_bytes`, stopping early so
/// unbounded or hostile responses cannot exhaust memory.
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<String, reqwest::Error> {
    let mut buf = Vec::with_capacity(max_bytes.min(8 * 1024));
    while buf.len() < max_bytes {
        let Some(chunk) = resp.chunk().await? else {
            break;
        };
        if append_capped(&mut buf, &chunk, max_bytes) {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Quotes a CSV field when it contains a comma, quote or newline
/// (RFC 4180).
#[must_use]
pub fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Truncates a string to at most `max` characters, appending an ellipsis
/// when the original was longer.
#[must_use]
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Converts a Unix timestamp (seconds) to a `YYYY-MM-DD` UTC date string.
#[must_use]
pub fn unix_to_date(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Current UTC date as `YYYY-MM-DD`.
#[must_use]
pub fn today_utc() -> String {
    unix_to_date(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// Howard Hinnant's `civil_from_days` algorithm for date conversion.
#[must_use]
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn xml_escape_escapes_entities_and_strips_illegal_xml10_chars() {
        let input = "a&b<c>d\"e'f\t\n\r\u{0000}\u{0007}\u{001B}\u{FFFE}\u{FFFF}ok";
        assert_eq!(xml_escape(input), "a&amp;b&lt;c&gt;d&quot;e&apos;f\t\n\rok");
    }

    #[test]
    fn redact_url_strips_userinfo_path_query_and_fragment() {
        assert_eq!(
            redact_url("https://user:secret@hooks.slack.com/services/T0/B0/XYZ?token=1#frag"),
            "https://hooks.slack.com/\u{2026}"
        );
        assert_eq!(
            redact_url("http://127.0.0.1:8080/v1/chat?key=abc"),
            "http://127.0.0.1:8080/\u{2026}"
        );
        assert_eq!(redact_url("not a url"), "[redacted-url]");
        assert_eq!(redact_url("file:///etc/passwd"), "[redacted-url]");
    }

    #[test]
    fn md_escape_cell_neutralizes_table_and_html_delimiters() {
        let input = "foo|bar `baz`\r\n<script>\x1b[31m\x07</script>";
        assert_eq!(
            md_escape_cell(input),
            "foo\\|bar \\`baz\\`  &lt;script&gt;[31m&lt;/script&gt;"
        );
    }

    #[test]
    fn sanitize_control_chars_replaces_whitespace_controls_and_drops_others() {
        let input = "hi\r\n\t\x1b[31mred\x07\x00end";
        assert_eq!(sanitize_control_chars(input), "hi   [31mredend");
    }

    #[test]
    fn append_capped_stops_at_limit_and_handles_zero_cap() {
        let mut buf = Vec::new();
        assert!(!append_capped(&mut buf, b"hello ", 10));
        assert_eq!(buf, b"hello ");
        assert!(append_capped(&mut buf, b"world-extra", 10));
        assert_eq!(buf, b"hello worl");

        // Zero cap immediately signals full without buffering anything.
        let mut zero = Vec::new();
        assert!(append_capped(&mut zero, b"abc", 0));
        assert!(zero.is_empty());

        // Invalid UTF-8 bytes truncated at cap convert lossily without panicking.
        let mut utf8 = Vec::new();
        assert!(append_capped(&mut utf8, "€€".as_bytes(), 2));
        let lossy = String::from_utf8_lossy(&utf8);
        assert_eq!(lossy, "\u{FFFD}");
    }
}
