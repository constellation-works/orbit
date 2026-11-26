//! Crew allowlists, discovery, task selection and run-input resolution.

mod allowlist;
mod host_os;
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
pub use run_input::{
    BundleCrews, ClaimedTaskSnapshot, TASK_PILOT_JOB, claimed_task_from_input, loaded_job_name,
    mixed_bundle_crew_error, normalized_task_crew, run_input_crew_override,
    task_ids_for_crew_resolution,
};

#[expect(unused_imports, reason = "preserve the crate-visible crew helper path")]
pub(crate) use run_input::job_dispatches_agent;
