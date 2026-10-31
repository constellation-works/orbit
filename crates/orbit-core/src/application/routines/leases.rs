//! Lifetime maintenance is owner work, independent of scheduled definitions.

use std::collections::BTreeSet;
use std::time::Instant;

use super::loader::DiscoveredWorkspaces;
use crate::runtime::task::locks::lease;

pub(super) fn sweep_owner_lease_lifetimes(
    discovered: &DiscoveredWorkspaces,
    deadline: Instant,
    skipped_workspaces: &mut BTreeSet<String>,
) {
    for (workspace, runtime) in &discovered.entries {
        if Instant::now() >= deadline {
            skipped_workspaces.insert(workspace.name.clone());
            continue;
        }
        if let Err(error) = lease::sweep_lease_lifetimes(runtime, deadline) {
            tracing::warn!(workspace = %workspace.name, %error, "sweep.lease_lifetimes_failed");
        }
    }
}
