//! Explicit convergence of the managed definitions used by one workspace.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use serde::Serialize;

use super::job::DEFAULT_JOB_FILES;
use super::routine::{RoutineSeedIdentity, reconcile_default_routines};
use super::skill::{DEFAULT_SKILL_FILES, inject_skill_template_tokens};
use super::{
    ManagedAssetAction, ManagedAssetLayout, ManagedAssetOutcome, ManagedAssetReconcileMode,
    ManagedAssetReconciliation, reconcile_managed_assets_in_mode,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;
use orbit_automation::auto_tasks::{DEFAULT_AUTO_TASK_FILES, auto_tasks_dir};

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
/// nothing about workspace registration, identity, role, config, or runtime
/// state, so callers cannot accidentally turn convergence into bootstrap.
pub fn reconcile_workspace_managed_artifacts(
    global_root: &Path,
    workspace_orbit_root: &Path,
    routine_identity: Option<&RoutineSeedIdentity>,
    check: bool,
) -> Result<WorkspaceManagedArtifactSyncReport, OrbitError> {
    let mode = if check {
        ManagedAssetReconcileMode::Check
    } else {
        ManagedAssetReconcileMode::Apply
    };
    let mut report = WorkspaceManagedArtifactSyncReport {
        check,
        actions: Vec::new(),
    };

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

    let auto_tasks = reconcile_managed_assets_in_mode(
        &auto_tasks_dir(workspace_orbit_root),
        "auto_task",
        ManagedAssetLayout::YamlStem,
        DEFAULT_AUTO_TASK_FILES,
        false,
        mode,
        |name, content| {
            let definition = parse_auto_task_yaml(content).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "default auto-task `{name}` failed validation: {error}"
                ))
            })?;
            if definition.name != name || definition.enabled {
                return Err(OrbitError::InvalidInput(format!(
                    "default auto-task `{name}` must have the matching name and ship disabled"
                )));
            }
            Ok(Cow::Borrowed(content))
        },
    )?;
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
