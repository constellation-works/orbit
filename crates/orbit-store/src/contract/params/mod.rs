mod desktop;
mod job_run_query;
mod leases;
mod local_pull;
mod task_mutations;
mod task_reservations;
mod workspace_claims;

pub use desktop::*;
pub use job_run_query::*;
pub use leases::*;
pub use local_pull::*;
pub use task_mutations::*;
pub use task_reservations::*;
pub use workspace_claims::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub schema_version: u32,
    pub workspace_id: String,
}
