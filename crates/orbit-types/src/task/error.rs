use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TaskError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    StatusTransition(String),
    #[error("cannot drop system identity tag '{tag}'; pass override flag to confirm")]
    SystemIdentityTagDropped { tag: String },
    /// A create or update surface supplied `unassessed`, which only automated
    /// creation may set.
    #[error(
        "complexity must be an assessed value (low, medium, hard, or xhard); unassessed is \
         reserved for automated creation"
    )]
    UnassessedComplexity,
}
