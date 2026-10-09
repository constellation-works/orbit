//! Explicit convergence of the managed definitions used by one workspace.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use serde::Serialize;

use super::job::DEFAULT_JOB_FILES;
use super::managed_assets::{
    ManagedAssetAction, ManagedAssetLayout, ManagedAssetOutcome, ManagedAssetReconcileMode,
    ManagedAssetReconciliation, reconcile_managed_assets_in_mode,
};
use super::routines::materialize::reconcile_default_routines;
use super::routines::seed::RoutineSeedIdentity;
use super::skill::{DEFAULT_SKILL_FILES, inject_skill_template_tokens};
use crate::application::auto_tasks::settings::migrate_settings_only_forks;
use crate::application::auto_tasks::{
    DEFAULT_AUTO_TASK_FILES, auto_tasks_dir, cursor_state_path, render_default_auto_task,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedArtifactScope {
    HostGlobal,
    WorkspaceLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedArtifactOutcome {
    Created,
    Refreshed,
    Retired,
    Migrated,
    Preserved,
    BindingDrift,
    Unchanged,
}

impl ManagedArtifactOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Refreshed => "refreshed",
            Self::Retired => "retired",
            Self::Migrated => "migrated",
            Self::Preserved => "preserved",
            Self::BindingDrift => "binding_drift",
            Self::Unchanged => "unchanged",
        }
    }

    pub fn requires_write(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Refreshed | Self::Retired | Self::Migrated
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedArtifactSyncAction {
    pub scope: ManagedArtifactScope,
    pub kind: String,
    pub name: String,
    pub path: PathBuf,
    pub outcome: ManagedArtifactOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkspaceManagedArtifactSyncReport {
    pub check: bool,
    pub actions: Vec<ManagedArtifactSyncAction>,
    /// Operator warnings no action's `detail` already carries: a manifest
    /// write skipped on a read-only or permission-denied catalog, or
    /// untracked legacy YAML preserved in place.
    pub warnings: Vec<String>,
}

impl WorkspaceManagedArtifactSyncReport {
    pub fn has_pending_changes(&self) -> bool {
        self.actions
            .iter()
            .any(|action| action.outcome.requires_write())
    }

    pub fn count(&self, outcome: ManagedArtifactOutcome) -> usize {
        self.actions
            .iter()
            .filter(|action| action.outcome == outcome)
            .count()
    }
}

/// Reconcile the host-global and workspace-local managed definitions used by
/// one already-initialized workspace. This use case deliberately knows
/// nothing about workspace registration, identity, role, config, or scheduler
/// cursor contents, so callers cannot accidentally turn convergence into bootstrap:
/// the caller supplies the registered `base_branch` the delivery defaults are
/// rendered against, exactly as it supplies the routine identity.
pub fn reconcile_workspace_managed_artifacts(
    global_root: &Path,
    workspace_orbit_root: &Path,
    routine_identity: Option<&RoutineSeedIdentity>,
    base_branch: &str,
    check: bool,
) -> Result<WorkspaceManagedArtifactSyncReport, OrbitError> {
    let mode = if check {
        ManagedAssetReconcileMode::Check
    } else {
        ManagedAssetReconcileMode::Apply
    };
    reconcile_managed_artifacts(
        global_root,
        workspace_orbit_root,
        routine_identity,
        base_branch,
        mode,
        true,
    )
}

/// Create the shipped workspace-local defaults (routines and auto-tasks) that
/// are absent from one workspace, and nothing else.
///
/// Host-level `orbit init` calls this for every registered workspace after it
/// has converged the host-global catalogs, so a release that ships a new
/// workspace default reaches each workspace without a manual sync. It is
/// create-only: a definition that exists — edited or user-authored — keeps its
/// bytes and recorded provenance, and nothing is retired. A created routine
/// takes the `enabled` value it ships with.
pub fn seed_absent_workspace_managed_artifacts(
    global_root: &Path,
    workspace_orbit_root: &Path,
    routine_identity: &RoutineSeedIdentity,
    base_branch: &str,
) -> Result<WorkspaceManagedArtifactSyncReport, OrbitError> {
    reconcile_managed_artifacts(
        global_root,
        workspace_orbit_root,
        Some(routine_identity),
        base_branch,
        ManagedAssetReconcileMode::CreateAbsent,
        false,
    )
}

fn reconcile_managed_artifacts(
    global_root: &Path,
    workspace_orbit_root: &Path,
    routine_identity: Option<&RoutineSeedIdentity>,
    base_branch: &str,
    mode: ManagedAssetReconcileMode,
    host_global: bool,
) -> Result<WorkspaceManagedArtifactSyncReport, OrbitError> {
    crate::bootstrap::product_profile::ProductProfile::Orbit
        .validate_roots(&[global_root, workspace_orbit_root])?;
    let mut report = WorkspaceManagedArtifactSyncReport {
        check: mode == ManagedAssetReconcileMode::Check,
        actions: Vec::new(),
        warnings: Vec::new(),
    };

    if host_global {
        let skills = reconcile_managed_assets_in_mode(
            &global_root.join("skills"),
            "skill",
            ManagedAssetLayout::RelativePath,
            &DEFAULT_SKILL_FILES,
            false,
            mode,
            |_, content| {
                Ok(Cow::Owned(inject_skill_template_tokens(
                    content,
                    global_root,
                )))
            },
        )?;
        append_actions(
            &mut report,
            ManagedArtifactScope::HostGlobal,
            "skill",
            skills,
        );

        let activities = reconcile_managed_assets_in_mode(
            &global_root.join("resources/activities"),
            "activity",
            ManagedAssetLayout::YamlStem,
            DEFAULT_ACTIVITY_FILES,
            false,
            mode,
            |_, content| Ok(Cow::Borrowed(content)),
        )?;
        append_actions(
            &mut report,
            ManagedArtifactScope::HostGlobal,
            "activity",
            activities,
        );

        let jobs = reconcile_managed_assets_in_mode(
            &global_root.join("resources/jobs"),
            "job",
            ManagedAssetLayout::YamlStem,
            DEFAULT_JOB_FILES,
            false,
            mode,
            |_, content| Ok(Cow::Borrowed(content)),
        )?;
        append_actions(&mut report, ManagedArtifactScope::HostGlobal, "job", jobs);
    }

    // A fork that differs from its bundled body only in operator settings is
    // moved into the settings table first, so reconciliation below finds the
    // restored body managed instead of preserving the fork [ORB-14909].
    let auto_tasks_root = auto_tasks_dir(workspace_orbit_root);
    let migrated = if mode.creates_only() {
        Vec::new()
    } else {
        let migrate = || migrate_settings_only_forks(&auto_tasks_root, base_branch, mode);
        if mode.writes() {
            // Serialize the whole settings read-modify-write and body restore
            // with CRUD, which uses this same workspace cursor sidecar.
            let state_path = cursor_state_path(&workspace_orbit_root.join("state"));
            with_exclusive_file_lock(&state_path, "auto-task cursor", migrate)?
        } else {
            migrate()?
        }
    };
    let mut auto_tasks = reconcile_managed_assets_in_mode(
        &auto_tasks_root,
        "auto_task",
        ManagedAssetLayout::YamlStem,
        DEFAULT_AUTO_TASK_FILES,
        false,
        mode,
        |name, content| {
            // Validate what lands on disk: the delivery defaults only become a
            // loadable definition once their branch placeholder is rendered.
            let rendered = render_default_auto_task(content, base_branch);
            let definition = parse_auto_task_yaml(&rendered).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "default auto-task `{name}` failed validation: {error}"
                ))
            })?;
            if definition.name != name || definition.enabled {
                return Err(OrbitError::InvalidInput(format!(
                    "default auto-task `{name}` must have the matching name and ship disabled"
                )));
            }
            Ok(rendered)
        },
    )?;
    // One row per migrated definition: in `Check` mode reconciliation still
    // sees the fork and would also report it preserved.
    let (superseded, mut kept): (Vec<_>, Vec<_>) = std::mem::take(&mut auto_tasks.actions)
        .into_iter()
        .partition(|action| {
            migrated
                .iter()
                .any(|migration| migration.name == action.name)
        });
    auto_tasks.warnings.retain(|warning| {
        !superseded
            .iter()
            .any(|action| action.detail.as_ref() == Some(warning))
    });
    kept.extend(migrated);
    auto_tasks.actions = kept;
    append_actions(
        &mut report,
        ManagedArtifactScope::WorkspaceLocal,
        "auto_task",
        auto_tasks,
    );

    if let Some(routine_identity) = routine_identity {
        let routines = reconcile_default_routines(
            &workspace_orbit_root.join("routines"),
            routine_identity,
            false,
            mode,
        )?;
        append_actions(
            &mut report,
            ManagedArtifactScope::WorkspaceLocal,
            "routine",
            routines,
        );
    }

    report.actions.sort_by(|left, right| {
        (
            scope_order(left.scope),
            left.kind.as_str(),
            left.path.as_path(),
            left.outcome.as_str(),
        )
            .cmp(&(
                scope_order(right.scope),
                right.kind.as_str(),
                right.path.as_path(),
                right.outcome.as_str(),
            ))
    });
    Ok(report)
}

fn append_actions(
    report: &mut WorkspaceManagedArtifactSyncReport,
    scope: ManagedArtifactScope,
    kind: &str,
    reconciliation: ManagedAssetReconciliation,
) {
    // Most warnings also describe a preserved action and already reach the
    // report as its detail; keep the rest, which have no action to ride on.
    report
        .warnings
        .extend(reconciliation.warnings.into_iter().filter(|warning| {
            !reconciliation
                .actions
                .iter()
                .any(|action| action.detail.as_ref() == Some(warning))
        }));
    report.actions.extend(
        reconciliation
            .actions
            .into_iter()
            .map(|action| map_action(scope, kind, action)),
    );
}

fn map_action(
    scope: ManagedArtifactScope,
    kind: &str,
    action: ManagedAssetAction,
) -> ManagedArtifactSyncAction {
    ManagedArtifactSyncAction {
        scope,
        kind: kind.to_string(),
        name: action.name,
        path: action.path,
        outcome: match action.outcome {
            ManagedAssetOutcome::Created => ManagedArtifactOutcome::Created,
            ManagedAssetOutcome::Refreshed => ManagedArtifactOutcome::Refreshed,
            ManagedAssetOutcome::Retired => ManagedArtifactOutcome::Retired,
            ManagedAssetOutcome::Migrated => ManagedArtifactOutcome::Migrated,
            ManagedAssetOutcome::Preserved => ManagedArtifactOutcome::Preserved,
            ManagedAssetOutcome::BindingDrift => ManagedArtifactOutcome::BindingDrift,
            ManagedAssetOutcome::Unchanged => ManagedArtifactOutcome::Unchanged,
        },
        detail: action.detail,
    }
}

fn scope_order(scope: ManagedArtifactScope) -> u8 {
    match scope {
        ManagedArtifactScope::HostGlobal => 0,
        ManagedArtifactScope::WorkspaceLocal => 1,
    }
}
