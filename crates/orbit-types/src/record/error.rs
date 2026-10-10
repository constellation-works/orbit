use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecordError {
    #[error("{0}")]
    Invalid(String),
}
