//! Default routine seeding [ORB-10129].
//!
//! Routines are workspace-authored YAML under `.orbit/routines/` — unlike
//! activities and jobs there is no global routines directory, so defaults
//! are seeded per workspace on `orbit init`. Three placeholders are resolved
//! at seed time:
//!
//! - `__ORBIT_ROUTINE_NAME__` — routine names must be unique across all
//!   routine sources on a host, so the seeded name carries the registered
//!   workspace name as a suffix (`task-pilot-<workspace-name>`) to keep two
//!   seeded source workspaces from colliding fail-closed. The suffix comes
//!   from the workspace name the operator registered, never from the checkout
//!   directory: two checkouts whose directories share a basename would
//!   otherwise seed the same names on one host [ORB-12107].
//! - `__ORBIT_OWNER_MACHINE__` and `__ORBIT_BASE_BRANCH__` — a state trigger
//!   names the one machine that evaluates it and the branch it observes, so
//!   the seeded `task_pilot` definition renders this host's registered
//!   machine id and the workspace's registered base branch [ORB-12745].
//!
//! Cron definitions carry no host pin [ORB-12236]: two hosts initializing the
//! same workspace name write byte-identical cron definitions, and differ only
//! in the state trigger's owner. Seeded routines are disabled when written;
//! they exist so a fresh workspace gets reviewable, opt-in schedules without
//! silently enabling unattended work.
//!
//! Provenance is byte-exact first and shape-aware second. The manifest records
//! the digest Orbit last wrote; a file that still matches is Orbit's. A file
//! that differs only in what the operator owns — the `enabled` opt-in (the
//! documented lifecycle, and what the dashboard toggle edits), the retired
//! `hosts:` key the loader tells operators to drop, and comments — is still
//! Orbit's when its template-owned fields match a template this or a prior
//! release shipped for that stem ([`SUPERSEDED_ROUTINE_TEMPLATES`],
//! [`RETIRED_ROUTINE_FILES`]). Anything else is a local edit and is preserved.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use orbit_automation::routines::loader::declared_routine_names;
use orbit_common::OrbitError;

use super::template::sanitize_routine_name_part;
use crate::application::managed_assets::RoutineMaterializationBinding;

/// Shippable default routine assets, seeded under
/// `<workspace>/.orbit/routines/<file>.yaml` on `orbit init`. Every entry
/// must keep the `__ORBIT_ROUTINE_NAME__` placeholder parseable once
/// substituted — `seed_default_routines` validates each rendered document
/// fail-closed before writing.
pub(crate) const DEFAULT_ROUTINE_FILES: &[(&str, &str)] = &[
    (
        "ci_failure_sweep",
        include_str!("../../../assets/routines/ci_failure_sweep.yaml"),
    ),
    (
        "dependabot_alert_sweep",
        include_str!("../../../assets/routines/dependabot_alert_sweep.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../../assets/routines/task_pilot.yaml"),
    ),
    (
        "ship_sweep",
        include_str!("../../../assets/routines/ship_sweep.yaml"),
    ),
    (
        "worktree_gc",
        include_str!("../../../assets/routines/worktree_gc.yaml"),
    ),
];

/// Default routines a prior release seeded that this Orbit no longer ships,
/// each as the last template it shipped. `orbit workspace sync` retires an
/// on-disk copy whose template-owned fields still match one of these; the
/// loader skips a routine targeting its job as retired instead of failing it
/// on every clock tick (`RETIRED_ROUTINE_JOBS`).
///
/// Retiring a default: move its template here and add its target job to
/// `RETIRED_ROUTINE_JOBS` in the same change.
pub(crate) const RETIRED_ROUTINE_FILES: &[(&str, &str)] = &[
    (
        "auto_task_scheduler",
        include_str!("../../../assets/routines/retired/auto_task_scheduler.yaml"),
    ),
    (
        "task_triage",
        include_str!("../../../assets/routines/retired/task_triage.yaml"),
    ),
];

/// Earlier shapes of routines this Orbit still ships: every template whose
/// template-owned fields differed from the current one. A workspace seeded by
/// that release and since opted in (`enabled: true`) is refreshed onto the
/// current template with its opt-in kept, instead of being reported as
/// locally modified forever.
///
/// Changing a shipped template's fields (not its comments): copy the previous
/// version here as `<stem>.<last-shipped-date>.yaml` in the same change.
pub(crate) const SUPERSEDED_ROUTINE_TEMPLATES: &[(&str, &str)] = &[
    (
        "ci_failure_sweep",
        include_str!("../../../assets/routines/superseded/ci_failure_sweep.2026-08-30.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../../assets/routines/superseded/task_pilot.2026-08-15.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../../assets/routines/superseded/task_pilot.2026-09-21.yaml"),
    ),
    (
        "worktree_gc",
        include_str!("../../../assets/routines/superseded/worktree_gc.2026-07-12.yaml"),
    ),
];

// Widened to pub(crate) for test access in sibling tests/routine.rs.
pub(crate) const ROUTINE_NAME_PLACEHOLDER: &str = "__ORBIT_ROUTINE_NAME__";
/// The registered machine id a seeded state trigger names as its owner.
pub(crate) const OWNER_MACHINE_PLACEHOLDER: &str = "__ORBIT_OWNER_MACHINE__";
/// The registered base branch a seeded state trigger observes; the same
/// placeholder the delivery auto-task defaults use.
pub(crate) const BASE_BRANCH_PLACEHOLDER: &str =
    crate::application::auto_tasks::BASE_BRANCH_PLACEHOLDER;

/// The identity a workspace's default routines are materialized against: the
/// registered workspace name their names are suffixed with, plus the host
/// machine id and registered base branch a state trigger is bound to.
///
/// Construction validates the name, which is why the fields are private: an
/// existing value always renders a loadable routine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineSeedIdentity {
    name_suffix: String,
    owner_machine: String,
    base_branch: String,
}

impl RoutineSeedIdentity {
    /// Build the seed identity for `workspace_name` on the host registered
    /// as `machine_id`, observing `base_branch`. Rejects a workspace name
    /// with no characters usable in a routine name — that name would
    /// otherwise silently fall back to a host-wide unsuffixed routine — and
    /// a blank machine id or branch, which no state trigger validates with.
    pub fn new(
        workspace_name: &str,
        machine_id: &str,
        base_branch: &str,
    ) -> Result<Self, OrbitError> {
        let name_suffix = sanitize_routine_name_part(workspace_name);
        if name_suffix.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "workspace name '{workspace_name}' has no characters usable in a routine name; \
                 routine names must stay unique across every routine source on this host, so \
                 choose a workspace name containing letters or digits"
            )));
        }
        if machine_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "seeding default routines requires this host's registered machine id; run `orbit init` first".to_string(),
            ));
        }
        if base_branch.is_empty() || base_branch.chars().any(char::is_whitespace) {
            return Err(OrbitError::InvalidInput(format!(
                "workspace base branch '{base_branch}' cannot be observed by a seeded routine"
            )));
        }

        Ok(Self {
            name_suffix,
            owner_machine: machine_id.trim().to_string(),
            base_branch: base_branch.to_string(),
        })
    }

    /// The binding `template` renders against on this host: the workspace
    /// routine name, plus the owner and branch only when the template asks
    /// for them, so a cron default's provenance stays host-independent.
    pub(super) fn binding(&self, file_stem: &str, template: &str) -> RoutineMaterializationBinding {
        RoutineMaterializationBinding {
            name: self.routine_name(file_stem),
            owner_machine: template
                .contains(OWNER_MACHINE_PLACEHOLDER)
                .then(|| self.owner_machine.clone()),
            branch: template
                .contains(BASE_BRANCH_PLACEHOLDER)
                .then(|| self.base_branch.clone()),
        }
    }

    /// A recorded binding completed for `template`: a routine adopted or
    /// recorded before its template gained a state trigger has a name but no
    /// owner or branch, and takes this host's. A binding that already
    /// carries them keeps them — a recorded owner is preserved exactly as a
    /// recorded name is.
    pub(super) fn complete(
        &self,
        file_stem: &str,
        template: &str,
        binding: &RoutineMaterializationBinding,
    ) -> RoutineMaterializationBinding {
        let requested = self.binding(file_stem, template);
        RoutineMaterializationBinding {
            name: binding.name.clone(),
            owner_machine: binding.owner_machine.clone().or(requested.owner_machine),
            branch: binding.branch.clone().or(requested.branch),
        }
    }

    /// Compose a per-workspace routine name: `<stem>-<workspace-name>`, using
    /// the routine name charset (lowercase alphanumeric plus `-`/`_`, starting
    /// alphanumeric). Names must be unique across all routine sources on a
    /// host, so the workspace suffix is what lets two seeded sources coexist.
    pub(crate) fn routine_name(&self, file_stem: &str) -> String {
        format!("{}-{}", file_stem.replace('_', "-"), self.name_suffix)
    }

    /// Every name this identity would seed, in [`DEFAULT_ROUTINE_FILES`] order.
    pub fn seeded_routine_names(&self) -> Vec<String> {
        DEFAULT_ROUTINE_FILES
            .iter()
            .map(|(stem, _)| self.routine_name(stem))
            .collect()
    }
}

/// A name this workspace's default routines would take that another workspace
/// on the same host already declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineNameCollision {
    /// The routine name claimed twice.
    pub name: String,
    /// The already-existing definition file that claims it.
    pub declared_in: PathBuf,
}

/// Names that seeding for `identity` would take from routine definitions
/// already declared under `other_orbit_dirs` (every registered checkout on
/// this host except the one being initialized).
///
/// A name defined by more than one source is a load-time error that drops
/// *every* colliding definition, so init reports this instead of writing a
/// set of routines that can never fire [ORB-12107].
pub fn default_routine_name_collisions(
    identity: &RoutineSeedIdentity,
    other_orbit_dirs: &[PathBuf],
) -> Vec<RoutineNameCollision> {
    let seeded: BTreeSet<String> = identity.seeded_routine_names().into_iter().collect();
    let mut collisions: BTreeMap<String, PathBuf> = BTreeMap::new();

    for orbit_dir in other_orbit_dirs {
        for (name, path) in declared_routine_names(orbit_dir) {
            if seeded.contains(&name) {
                collisions.entry(name).or_insert(path);
            }
        }
    }

    collisions
        .into_iter()
        .map(|(name, declared_in)| RoutineNameCollision { name, declared_in })
        .collect()
}
