//! Authenticated admission and host continuation of stopped rebases.

mod admission;
mod checkpoint;
mod continuation;
mod git;
mod intent;
mod metadata;

pub(super) use checkpoint::{RebaseContinuation, RebaseRecoveryCheckpoint};
pub(super) use git::git_mutation_output;
pub(super) use intent::ResolutionIntent;
pub(crate) use intent::allocate_resolution_intent;
