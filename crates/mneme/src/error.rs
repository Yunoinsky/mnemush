//! Error types for mneme.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MnemeError {
    #[error("storage error: {0}")]
    Storage(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("scan blocked: {0}")]
    ScanBlocked(String),

    #[error("other: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MnemeError>;

/// Convert a [`MnemeError`] (e.g. from strict `parse_*` calls) into a
/// `rusqlite::Error` so it propagates through `query_row`/`query_map`
/// closures that must return `rusqlite::Result<_>`. The original
/// error is preserved as the inner `Box<dyn Error>`.
impl From<MnemeError> for rusqlite::Error {
    fn from(e: MnemeError) -> Self {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    }
}
