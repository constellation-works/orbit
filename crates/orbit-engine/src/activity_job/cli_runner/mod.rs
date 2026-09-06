mod argv;
mod envelope;
mod inspection;
mod orchestrator;
/// Sandbox-aware child creation. `pub(crate)` because the deterministic
/// `local_shell` action reuses the same spawn seam rather than growing a second
/// implementation of sandbox selection and process-group setup [ORB-11294].
pub(crate) mod spawn;
mod supervisor;

#[cfg(test)]
mod tests;

pub(super) use envelope::task_id_from_input;
pub use orchestrator::run_cli_backend;
