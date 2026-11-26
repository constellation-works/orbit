mod args;
mod browser_kit;
mod execute;
mod report;
mod validate;

use std::path::PathBuf;

use orbit_types::workspace::WorkspaceCheckoutRole;

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
