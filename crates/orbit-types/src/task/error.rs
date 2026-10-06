use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TaskError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    StatusTransition(String),
    #[error("cannot drop system identity tag '{tag}'; pass override flag to confirm")]
    SystemIdentityTagDropped { tag: String },
}
