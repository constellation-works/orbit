//! Workspace-less update application.

mod admission;

pub mod channel;

pub mod converge;

mod environment;

pub mod flow;

pub mod local_candidate;

pub mod lock;

mod report;

pub mod source;

pub mod stage;

mod trust;

pub mod version;

pub use admission::{acquire_admissions, admission_authorities, candidate_preflight};
pub use environment::{UpdateEnvironment, UpdateWorkspace};
pub use flow::{UpdateRequest, run_update};
pub use local_candidate::{
    CandidateManifestRequest, LocalCandidateRequest, run_local_candidate_update,
    write_candidate_manifest,
};
pub use report::{
    EXIT_NEEDS_RECOVERY, EXIT_UPDATE_AVAILABLE, HandoverProcess, UpdateOutcome, UpdateReport,
};
pub use trust::trusted_keys_from_env;

/// An update request.
pub type Command = UpdateRequest;

/// The update result.
pub type Outcome = UpdateReport;

/// Errors returned by the update application.
pub type Error = orbit_common::OrbitError;

#[cfg(test)]
mod tests;
