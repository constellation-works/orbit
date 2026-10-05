//! Guarded desktop authoring and evidence-bound review.

mod authorization;
mod execution;
mod review;
mod snapshot;
mod validation;
mod write;

pub(crate) use execution::HandoffPullRequest;

#[cfg(test)]
mod tests;
