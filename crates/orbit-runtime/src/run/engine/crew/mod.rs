//! Crew allowlists, discovery, task selection and run-input resolution.

mod allowlist;
mod projection;
mod resolution;
mod run_input;

pub use allowlist::{
    ALLOWED_CREWS_INPUT_KEY, CrewAllowlist, crew_allowlist_names_from_input, enforce_crew_allowlist,
};
pub use projection::{
    ConfiguredCrewProjection, ConfiguredCrewRegistryProjection, ResolvedCrewProjection,
    TaskCrewRead,
};
pub use run_input::{BundleCrews, normalized_task_crew};

#[expect(unused_imports, reason = "preserve the crate-visible crew helper path")]
pub(crate) use run_input::job_dispatches_agent;
