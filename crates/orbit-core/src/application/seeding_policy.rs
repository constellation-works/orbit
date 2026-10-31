//! The workspace seeding policy over Orbit's shipped default routines and
//! auto-tasks (`seeding.routines`, `seeding.auto_tasks`; design
//! `docs/design/distributions/1_scope.md`).
//!
//! Every seeding path — `orbit workspace init`, re-init, `orbit workspace
//! sync` and the create-only seeding host `orbit init` runs after an upgrade —
//! reconciles through `workspace_sync`, which reads the policy here and hands
//! reconciliation only the defaults it includes. A shipped default the policy
//! leaves out is then retired exactly as a default a release dropped is: by
//! recorded digest, with an operator-edited copy preserved under
//! `.retired-managed/`.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_config::{
    ConfigRoots, RETIRED_DEFAULT_AUTO_TASKS, RETIRED_DEFAULT_ROUTINES, SHIPPED_DEFAULT_AUTO_TASKS,
    SHIPPED_DEFAULT_ROUTINES, SeedingPolicy, WorkspaceSeedingPolicies,
    load_workspace_seeding_policies,
};
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES;
use crate::application::auto_tasks::retired::{RETIRED_AUTO_TASK_FILES, RetiredAutoTask};
use crate::application::routines::seed::{DEFAULT_ROUTINE_FILES, RETIRED_ROUTINE_FILES};

/// The routine whose seeded-enabled default records the hourly
/// crew-performance snapshot.
pub const CREW_PERFORMANCE_REFRESH_ROUTINE: &str = "crew_performance_refresh";

// `orbit-config` admits policy names against its own lists; these keep them
// equal to the embedded asset tables, so a default added or retired here
// without updating config admission fails the build.
const _: () = assert!(
    asset_names_match(DEFAULT_ROUTINE_FILES, SHIPPED_DEFAULT_ROUTINES),
    "orbit_config::SHIPPED_DEFAULT_ROUTINES must list DEFAULT_ROUTINE_FILES"
);
const _: () = assert!(
    asset_names_match(RETIRED_ROUTINE_FILES, RETIRED_DEFAULT_ROUTINES),
    "orbit_config::RETIRED_DEFAULT_ROUTINES must list RETIRED_ROUTINE_FILES"
);
const _: () = assert!(
    asset_names_match(DEFAULT_AUTO_TASK_FILES, SHIPPED_DEFAULT_AUTO_TASKS),
    "orbit_config::SHIPPED_DEFAULT_AUTO_TASKS must list DEFAULT_AUTO_TASK_FILES"
);
const _: () = assert!(
    retired_auto_task_names_match(RETIRED_AUTO_TASK_FILES, RETIRED_DEFAULT_AUTO_TASKS),
    "orbit_config::RETIRED_DEFAULT_AUTO_TASKS must list RETIRED_AUTO_TASK_FILES"
);

const fn same_str(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn asset_names_match(files: &[(&str, &str)], names: &[&str]) -> bool {
    if files.len() != names.len() {
        return false;
    }
    let mut index = 0;
    while index < files.len() {
        if !same_str(files[index].0, names[index]) {
            return false;
        }
        index += 1;
    }
    true
}

const fn retired_auto_task_names_match(files: &[RetiredAutoTask], names: &[&str]) -> bool {
    if files.len() != names.len() {
        return false;
    }
    let mut index = 0;
    while index < files.len() {
        if !same_str(files[index].name, names[index]) {
            return false;
        }
        index += 1;
    }
    true
}

/// Resolve the seeding policies of the workspace whose `.orbit` directory is
/// `workspace_orbit_root`, layered over the global config in `global_root`.
pub fn workspace_seeding_policies(
    global_root: &Path,
    workspace_orbit_root: &Path,
) -> Result<WorkspaceSeedingPolicies, OrbitError> {
    load_workspace_seeding_policies(&ConfigRoots::new(global_root, workspace_orbit_root))
}

/// The entries of `files` that `policy` seeds, in shipped order.
pub(crate) fn seeded_defaults(
    files: &'static [(&'static str, &'static str)],
    policy: &SeedingPolicy,
) -> Vec<(&'static str, &'static str)> {
    files
        .iter()
        .filter(|(name, _)| policy.includes(name))
        .copied()
        .collect()
}

/// What a workspace's seeding policy leaves out, as `orbit workspace show`
/// and the doctor report it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeedingPolicyExclusions {
    /// Shipped default routines (file stems) the policy does not seed.
    pub routines: Vec<&'static str>,
    /// Shipped default auto-tasks the policy does not seed.
    pub auto_tasks: Vec<&'static str>,
}

impl SeedingPolicyExclusions {
    /// The shipped defaults `policies` leaves out, in shipped order.
    pub fn of(policies: &WorkspaceSeedingPolicies) -> Self {
        let excluded = |files: &'static [(&'static str, &'static str)], policy: &SeedingPolicy| {
            files
                .iter()
                .map(|(name, _)| *name)
                .filter(|name| !policy.includes(name))
                .collect()
        };
        Self {
            routines: excluded(DEFAULT_ROUTINE_FILES, &policies.routines),
            auto_tasks: excluded(DEFAULT_AUTO_TASK_FILES, &policies.auto_tasks),
        }
    }

    /// The excluded names of one kind as a set, for catalog filtering.
    pub(crate) fn routine_set(&self) -> BTreeSet<String> {
        self.routines
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    }

    /// The excluded auto-task names as a set.
    pub(crate) fn auto_task_set(&self) -> BTreeSet<String> {
        self.auto_tasks
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    }
}

impl OrbitRuntime {
    /// This workspace's seeding policies, read from the config layers this
    /// runtime resolves (`seeding.routines`, `seeding.auto_tasks`).
    pub fn workspace_seeding_policies(&self) -> Result<WorkspaceSeedingPolicies, OrbitError> {
        workspace_seeding_policies(&self.global_root(), &self.shared_root())
    }
}
