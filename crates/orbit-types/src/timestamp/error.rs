use thiserror::Error;

/// A timestamp could not be parsed or represented in the persisted date range.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UtcTimestampError {
    #[error("invalid RFC 3339 timestamp {0:?}")]
    Invalid(String),
    #[error("system time is outside the supported date range")]
    OutOfRange,
}
