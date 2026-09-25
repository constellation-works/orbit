mod argv;
mod envelope;
mod inspection;
mod launcher;
mod orchestrator;
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

// `cli_agent_envelope_json` is the only place the provider's stdin frame is
// built, so the recovery-input bound is asserted against it rather than a
// second copy of the same shape [ORB-12467].
#[cfg(test)]
pub(super) use envelope::cli_agent_envelope_json;
pub(super) use envelope::task_id_from_input;
pub use launcher::locate_provider_launcher;
pub use orchestrator::run_cli_backend;
