use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolError {
    #[error("{0}")]
    Invalid(String),
}

/// A managed worker's runtime binding, or a call that strays outside it.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkerBindingError {
    /// A binding field is empty or carries surrounding whitespace.
    #[error("managed worker invocation has an incomplete binding")]
    IncompleteBinding,
    /// Argument `key` names a different value than the binding.
    #[error("managed worker argument `{key}` conflicts with its binding")]
    ArgumentConflict { key: &'static str },
    /// A new task has no `spawned_from` relation to the claimed task.
    #[error("a claimed worker's new task must be spawned_from its claimed task")]
    NotSpawnedFromClaim,
    /// A new task relates to something other than the claimed task.
    #[error(
        "a claimed worker's new task may relate only to its claimed task, as spawned_from; only \
         a claimed review task's findings may also name regression_from"
    )]
    ForeignRelation,
    /// A review finding relates to something other than the claimed task
    /// and its culprit.
    #[error(
        "a claimed review worker's new task may relate only to its claimed task, as \
         spawned_from, and to the task that introduced its finding, as regression_from"
    )]
    ForeignFindingRelation,
}
