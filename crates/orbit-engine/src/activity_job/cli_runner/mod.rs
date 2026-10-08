mod argv;
mod auth_probe;
mod envelope;
mod inspection;
mod inspection_tools;
mod launcher;
mod orchestrator;
mod plugin_broker;
mod response_diagnostics;
/// Sandbox-aware child creation. `pub(crate)` because the deterministic
/// `local_shell` action reuses the same spawn seam rather than growing a second
/// implementation of sandbox selection and process-group setup [ORB-11294].
pub(crate) mod spawn;
mod spawn_diagnostics;
mod stdout_preview;
mod supervisor;

#[cfg(test)]
mod tests;

pub use auth_probe::{AuthProbeOutcome, auth_credential_source, run_auth_probe};
pub(super) use envelope::task_id_from_input;
pub use inspection::is_source_inspection_checkout;
pub use launcher::{MissingLauncher, locate_provider_launcher, missing_launcher_in};
pub(crate) use orchestrator::run_cli_backend_for_step;
pub use orchestrator::{activity_tool_policy_env, run_cli_backend};
pub(crate) use supervisor::DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS;
