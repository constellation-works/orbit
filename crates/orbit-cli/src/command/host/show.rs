use std::path::{Path, PathBuf};

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_registry::{load_host_identity, workspace_registry::global_orbit_dir};
use serde_json::json;

use crate::command::{CommandOut, Execute, Payload};

#[derive(Args)]
#[command(
    about = "Show this machine's local host identity",
    after_help = "Reports the stable machine_id, operator-facing host_id, and task-ID task_prefix."
)]
pub(crate) struct HostShowArgs {
    /// Emit the identity as JSON.
    #[arg(long)]
    pub json: bool,
}

impl HostShowArgs {
    pub(crate) fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        let global_root = selected_global_root(root_override)?;
        self.execute_at(&global_root)
    }

    fn execute_at(self, global_root: &Path) -> CommandOut {
        let identity = load_host_identity(global_root)?;
        let doc = json!({
            "machine_id": identity.machine_id,
            "host_id": identity.host_id,
            "task_prefix": identity.task_prefix,
        });
        let text = format!(
            "machine_id: {}\nhost_id: {}\ntask_prefix: {}",
            identity.machine_id, identity.host_id, identity.task_prefix
        );
        Ok(Payload::detail(doc, text).into())
    }
}

impl Execute for HostShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let global_root = runtime.global_root();
        self.execute_at(&global_root)
    }
}

fn selected_global_root(root_override: Option<&Path>) -> Result<PathBuf, orbit_core::OrbitError> {
    match root_override {
        Some(root) => Ok(root.to_path_buf()),
        None => global_orbit_dir(),
    }
}
