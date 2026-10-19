//! Authenticated admission and host continuation of stopped rebases.

mod admission;
mod checkpoint;
mod continuation;
mod git;
mod metadata;

pub(super) use checkpoint::{RebaseContinuation, RebaseRecoveryCheckpoint};
pub(super) use git::git_mutation_output;
