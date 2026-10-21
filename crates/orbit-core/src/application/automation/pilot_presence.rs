//! Whether a task pilot prepares this workspace's tasks on this machine.
//!
//! A drain admits a task that needs a pilot assessment only after a
//! `preparation_eligible` routine owned by this machine has prepared it. With
//! no such routine enabled here, the drain starts normally and those tasks
//! wait (`task_pilot_preparation_required`) for a pilot that never comes.
//! Drain start and every local admission pass report that, naming the
//! workspace, the machine and the fix.

use std::path::{Path, PathBuf};

use orbit_automation::routines::loader::declared_routine_names;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_types::workflow::automation::members::StateTriggerKind;

use crate::OrbitRuntime;

/// One task-pilot routine this workspace declares.
struct PilotRoutine {
    name: String,
    path: PathBuf,
    enabled: bool,
    owned_here: bool,
}

/// Why no task pilot prepares tasks on this machine.
enum PilotGap {
    /// The workspace declares no task-pilot routine.
    Missing,
    /// Every task-pilot routine names another owner machine.
    OwnedElsewhere(String),
    /// The routine this machine owns is disabled.
    Disabled { name: String, path: PathBuf },
    /// The routine this machine owns is paused on this host.
    Paused(String),
}

impl OrbitRuntime {
    /// The warning a local drain reports while no enabled, unpaused task-pilot
    /// routine of this workspace is owned by this machine; `None` when one is,
    /// or when this runtime carries no machine identity to judge ownership by.
    pub(crate) fn task_pilot_warning(&self) -> Option<String> {
        let machine_id = self.automation_machine_identity()?;
        let routines = declared_routine_names(&self.shared_root())
            .into_values()
            .filter_map(|path| {
                let definition = parse_routine_yaml(&std::fs::read_to_string(&path).ok()?).ok()?;
                let trigger = definition.trigger.state.as_ref()?;
                (trigger.kind == StateTriggerKind::PreparationEligible).then(|| PilotRoutine {
                    owned_here: trigger.owner_machine == machine_id,
                    name: definition.name,
                    path,
                    enabled: definition.enabled,
                })
            })
            .collect::<Vec<_>>();
        let gap = pilot_gap(&routines, || self.paused_routines())?;
        Some(self.describe_pilot_gap(machine_id, gap))
    }

    /// Routine names paused on this host. An unreadable store reports none:
    /// the warning is advisory and must not invent a pause.
    fn paused_routines(&self) -> Vec<String> {
        let read = || {
            let database = orbit_config::resolved_audit_db_path(
                &orbit_config::ConfigRoots::global_only(self.global_root()),
            )?;
            if !database.exists() {
                return Ok(Vec::new());
            }
            let store = orbit_store::Store::open_read_only(&database)?;
            Ok::<_, orbit_common::OrbitError>(store.routine_pauses()?.into_keys().collect())
        };
        read().unwrap_or_else(|error| {
            tracing::warn!(%error, "task-pilot check read no routine pauses");
            Vec::new()
        })
    }

    fn describe_pilot_gap(&self, machine_id: &str, gap: PilotGap) -> String {
        let machine = self
            .automation_execution_location()
            .and_then(|location| location.machine_name.as_deref())
            .unwrap_or(machine_id);
        let (cause, fix) = match gap {
            PilotGap::Missing => (
                "the workspace declares no task-pilot routine".to_string(),
                "Run `orbit workspace sync` to add `.orbit/routines/task_pilot.yaml`, then set \
                 `enabled: true` in it."
                    .to_string(),
            ),
            PilotGap::OwnedElsewhere(name) => (
                format!("its task-pilot routine `{name}` names another owner machine"),
                format!(
                    "Set `trigger.state.owner_machine` in `{name}` to `{machine_id}` and \
                     `enabled: true`."
                ),
            ),
            PilotGap::Disabled { name, path } => (
                format!("its task-pilot routine `{name}` is disabled"),
                format!(
                    "Set `enabled: true` in `{}`, or turn `{name}` on under Operations › \
                     Routines.",
                    self.workspace_relative(&path).display()
                ),
            ),
            PilotGap::Paused(name) => (
                format!("its task-pilot routine `{name}` is paused on this machine"),
                format!("Run `orbit routine resume {name}`."),
            ),
        };
        format!(
            "Task pilot is not enabled for workspace '{}' on machine '{machine}': {cause}, so \
             tasks that need a pilot assessment wait and are never prepared. {fix}",
            self.workspace_label()
        )
    }

    fn workspace_relative<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.paths().repo_root).unwrap_or(path)
    }
}

/// The gap, if any. `paused` is read only when an enabled routine is owned
/// here, so a workspace without one never opens the scheduler store.
fn pilot_gap(routines: &[PilotRoutine], paused: impl FnOnce() -> Vec<String>) -> Option<PilotGap> {
    let owned = routines
        .iter()
        .filter(|routine| routine.owned_here)
        .collect::<Vec<_>>();
    let Some(first_owned) = owned.first() else {
        return Some(routines.first().map_or(PilotGap::Missing, |routine| {
            PilotGap::OwnedElsewhere(routine.name.clone())
        }));
    };
    let enabled = owned
        .iter()
        .filter(|routine| routine.enabled)
        .collect::<Vec<_>>();
    let Some(first_enabled) = enabled.first() else {
        return Some(PilotGap::Disabled {
            name: first_owned.name.clone(),
            path: first_owned.path.clone(),
        });
    };
    let paused = paused();
    if enabled.iter().all(|routine| paused.contains(&routine.name)) {
        return Some(PilotGap::Paused(first_enabled.name.clone()));
    }
    None
}
