//! Error type shared by STT and post-processing.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("brak klucza API dla {0}")]
    MissingKey(&'static str),
    #[error("dostawca odrzucił klucz API ({status}): {message}")]
    Unauthorized { status: u16, message: String },
    #[error("limit zapytań lub środków u dostawcy ({status}): {message}")]
    RateLimited { status: u16, message: String },
    #[error("na koncie u dostawcy skończyły się środki ({status}): {message}")]
    QuotaExceeded { status: u16, message: String },
    #[error("błąd HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("błąd sieci: {0}")]
    Network(String),
    #[error("przekroczono czas oczekiwania ({0:?})")]
    Timeout(Duration),
    #[error("nieprawidłowa odpowiedź dostawcy: {0}")]
    InvalidResponse(String),
    #[error("dostawca zgłosił błąd: {0}")]
    Provider(String),
    #[error("nie wykryto mowy")]
    NoSpeech,
    #[error("anulowano")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Map an HTTP error status and body to a typed error. The body is trimmed so
    /// a provider's HTML error page never floods the log.
    pub fn from_status(status: u16, body: &str) -> Self {
        let message = extract_error_message(body);
        if quota_exceeded(body) {
            return Error::QuotaExceeded { status, message };
        }
        match status {
            401 | 403 => Error::Unauthorized { status, message },
            402 | 429 => Error::RateLimited { status, message },
            _ => Error::Http { status, message },
        }
    }

    /// The provider refused this key or its account has no credits left, so a
    /// key of another account may still work.
    pub fn key_unusable(&self) -> bool {
        matches!(self, Error::Unauthorized { .. } | Error::QuotaExceeded { .. })
    }
}

/// Whether an error body says the account is out of credits: ElevenLabs
/// answers 401 with `detail.status`, OpenAI 429 with `error.code`.
fn quota_exceeded(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    ["/detail/status", "/error/code", "/error/type"]
        .into_iter()
        .filter_map(|p| v.pointer(p).and_then(serde_json::Value::as_str))
        .any(|s| matches!(s, "quota_exceeded" | "insufficient_quota"))
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Error::Timeout(Duration::ZERO)
        } else {
            Error::Network(e.to_string())
        }
    }
}

/// Pull a human-readable message out of a typical JSON error body
/// (`{"error":{"message":..}}`, `{"detail":..}`, `{"message":..}`), falling back
/// to the first 300 characters of the raw body.
pub fn extract_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let candidates = [
            v.pointer("/error/message"),
            v.pointer("/error"),
            v.pointer("/detail/message"),
            v.pointer("/detail"),
            v.pointer("/message"),
        ];
        for c in candidates.into_iter().flatten() {
            match c {
                serde_json::Value::String(s) => return truncate(s, 300),
                serde_json::Value::Null => {}
                other => return truncate(&other.to_string(), 300),
            }
        }
    }
    truncate(body.trim(), 300)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_auth_and_rate_limits() {
        assert!(matches!(
            Error::from_status(401, r#"{"error":{"message":"bad key"}}"#),
            Error::Unauthorized { status: 401, ref message } if message == "bad key"
        ));
        assert!(matches!(Error::from_status(429, "slow down"), Error::RateLimited { .. }));
        assert!(matches!(Error::from_status(500, ""), Error::Http { status: 500, .. }));
    }

    #[test]
    fn recognizes_used_up_credits() {
        let elevenlabs = r#"{"detail":{"status":"quota_exceeded","message":"This request exceeds your quota of 10000."}}"#;
        let e = Error::from_status(401, elevenlabs);
        assert!(matches!(e, Error::QuotaExceeded { status: 401, ref message } if message.starts_with("This request")));
        let openai = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        assert!(matches!(Error::from_status(429, openai), Error::QuotaExceeded { status: 429, .. }));
        let invalid_key = r#"{"detail":{"status":"invalid_api_key","message":"Invalid API key"}}"#;
        assert!(matches!(Error::from_status(401, invalid_key), Error::Unauthorized { .. }));
    }

    #[test]
    fn only_account_errors_make_a_key_unusable() {
        assert!(Error::from_status(401, "bad key").key_unusable());
        assert!(Error::QuotaExceeded { status: 401, message: String::new() }.key_unusable());
        for e in [
            Error::from_status(429, "slow down"),
            Error::from_status(500, ""),
            Error::Network("x".into()),
            Error::Timeout(Duration::ZERO),
            Error::NoSpeech,
        ] {
            assert!(!e.key_unusable(), "{e}");
        }
    }

    #[test]
    fn extracts_nested_detail() {
        assert_eq!(
            extract_error_message(r#"{"detail":{"status":"x","message":"quota"}}"#),
            "quota"
        );
        assert_eq!(extract_error_message("<html>oops</html>"), "<html>oops</html>");
    }
}
