//! UTC timestamp contracts for persisted records using the legacy date format.

mod error;
mod utc_timestamp;

pub use error::UtcTimestampError;
pub use utc_timestamp::UtcTimestamp;
pub(crate) use utc_timestamp::deserialize_legacy_utc_timestamp;
