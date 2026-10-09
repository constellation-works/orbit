use std::path::PathBuf;

use orbit_types::workspace::WorkspaceCheckoutRole;

mod args;
mod execute;
mod report;
mod validate;

pub use args::WorkspaceInitArgs;

struct WorkspaceInitResult {
    id: String,
    name: String,
    root: PathBuf,
    orbit_dir: PathBuf,
    task_prefix: Option<String>,
    role: Option<WorkspaceCheckoutRole>,
    owner_machine_id: Option<String>,
}
