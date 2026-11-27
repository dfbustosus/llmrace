//! Typed error surface of the `llmrace` library.

use thiserror::Error;

/// Convenient result alias used across the library.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced while configuring or running provider benchmarks.
#[derive(Debug, Error)]
pub enum Error {
    /// Invalid configuration (unknown provider, missing env var…).
    #[error("configuration error: {0}")]
    Config(String),

    /// The provider endpoint answered with a non-success HTTP status.
    #[error("{provider} returned HTTP {status}: {body}")]
    Http {
        /// Provider that produced the failure.
        provider: String,
        /// HTTP status code.
        status: u16,
        /// Truncated response body for diagnostics.
        body: String,
        /// Parsed `Retry-After` delay, when the response carried one.
        ///
        /// `None` when the header was absent or not a delay in seconds
        /// (HTTP-date values are ignored; the caller falls back to jitter).
        retry_after: Option<std::time::Duration>,
    },

    /// The response stream failed or timed out.
    #[error("stream error: {0}")]
    Stream(String),

    /// The provider violated the expected streaming protocol.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Local I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Whether another attempt could succeed.
    ///
    /// Permanent client errors (authentication, unknown model, bad
    /// request) are not retried: repeating them only burns the budget.
    /// Timeouts, connection failures and the usual overload statuses are.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Http { status, .. } => {
                matches!(*status, 408 | 409 | 425 | 429 | 500 | 502..=599)
            }
            Error::Stream(_) => true,
            Error::Config(_) | Error::Protocol(_) | Error::Io(_) => false,
        }
    }

    /// Server-supplied delay from a `Retry-After` header, when captured.
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Error::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn http(status: u16) -> Error {
        Error::Http {
            provider: "t".into(),
            status,
            body: "x".into(),
            retry_after: None,
        }
    }

    #[test]
    fn permanent_client_errors_are_not_retried() {
        for status in [400, 401, 403, 404, 422, 501] {
            assert!(!http(status).is_retryable(), "{status}");
        }
        for status in [
            408, 409, 425, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 529, 599,
        ] {
            assert!(http(status).is_retryable(), "{status}");
        }
        assert!(Error::Stream("connection reset".into()).is_retryable());
        assert!(!Error::Protocol("empty response stream".into()).is_retryable());
        assert!(!Error::Config("missing env var".into()).is_retryable());
    }

    #[test]
    fn retry_after_is_readable_from_the_error() {
        let err = Error::Http {
            provider: "t".into(),
            status: 429,
            body: "slow down".into(),
            retry_after: Some(Duration::from_secs(2)),
        };
        assert_eq!(err.retry_after(), Some(Duration::from_secs(2)));
        assert!(http(429).retry_after().is_none());
    }
}
