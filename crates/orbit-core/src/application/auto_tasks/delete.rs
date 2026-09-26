//! Auto-task delete and restore: removing a definition for good, and the
//! durable opt-out that keeps a deleted shipped default from being reseeded.
//!
//! `toggle` pauses a definition and keeps it listed. Delete removes the file,
//! its scheduler cursor and, for a delivery trigger, the consumer state the
//! audited reset owns. Deleting a name Orbit ships also records an opt-out in
//! the managed-asset manifest, so `orbit workspace init --force` and `orbit
//! workspace sync` leave it absent. `restore` is the one way back for a
//! shipped default: it writes the shipped content and clears the opt-out.

use std::path::PathBuf;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_store::compose::auto_task::with_cursor_lock;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::AutoTaskDefinition;
use serde::Serialize;
use serde_json::{Value, json};

use crate::application::automation::{
    ConsumerTeardown, consumer_teardown_refusals, tear_down_auto_task_consumer,
};
use crate::application::managed_assets::{
    ManagedAssetLayout, record_managed_asset_opt_out, restore_managed_asset,
};
use crate::{AuditEventInsertParams, OrbitRuntime};

use super::loader::{auto_tasks_dir, definition_path};
use super::scheduler::open_auto_task_instances;
use super::state::cursor_state_path;
use super::{DEFAULT_AUTO_TASK_FILES, render_default_auto_task};

const ASSET_KIND: &str = "auto_task";
/// Reset reason recorded for a delivery consumer when the delete gave none.
const DEFAULT_DELETE_REASON: &str = "auto-task definition deleted";

/// Parameters for deleting a definition.
#[derive(Debug, Clone, Default)]
pub struct AutoTaskDeleteParams {
    pub name: String,
    /// Operator explanation, retained in the audit record.
    pub reason: Option<String>,
    /// Delete even while a minted task is open or a delivery action executes.
    pub force: bool,
}

/// What one delete removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoTaskDeleteReport {
    pub name: String,
    pub path: PathBuf,
    /// The name is a shipped default, so the delete recorded an opt-out that
    /// keeps reseeding from re-creating it.
    pub opted_out: bool,
    /// A scheduler cursor existed and was removed.
    pub cursor_removed: bool,
    /// Minted tasks still open when `force` overrode the refusal.
    pub open_tasks: Vec<String>,
    /// The delivery consumer torn down with the definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumer: Option<ConsumerTeardown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub deleted_by: String,
    pub deleted_at: String,
}

impl OrbitRuntime {
    /// Delete a definition, its scheduler cursor and any delivery consumer
    /// state, and write an audit record.
    ///
    /// Refuses while a task minted from the definition is still open, naming
    /// those tasks, unless `force` is set. A delivery consumer is torn down
    /// through the audited reset, so delete refuses whenever that reset would.
    pub fn auto_task_delete(
        &self,
        params: AutoTaskDeleteParams,
    ) -> Result<AutoTaskDeleteReport, OrbitError> {
        let name = params.name.as_str();
        let definition = self.require_validated_auto_task(name)?;
        let path = definition_path(&self.paths().local_dir, name);
        let reason = params
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(ToOwned::to_owned);
        let actor = self.actor().resolve_write_label(None, None)?;
        let now = Utc::now();

        let open_tasks = open_auto_task_instances(self, name)?;
        if !open_tasks.is_empty() && !params.force {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{name}' still has open minted tasks: {}; close them, or pass --force to delete anyway",
                open_tasks.join(", ")
            )));
        }
        let refusals = consumer_teardown_refusals(self, &definition, params.force, now)?;
        if !refusals.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{name}' has delivery consumer state its reset refuses to drop ({}); preview it with `orbit auto-task reset {name}`, or pass --force to abandon an executing action",
                refusals.join(", ")
            )));
        }

        let original = std::fs::read_to_string(&path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
        // The scheduler admits slots under this lock, so no pass sees the
        // definition gone while its cursor still exists.
        let cursor_removed =
            with_cursor_lock(&cursor_state_path(&self.paths().state_dir), |session| {
                std::fs::remove_file(&path).map_err(|error| {
                    OrbitError::Io(format!(
                        "delete auto-task '{name}' at {}: {error}",
                        path.display()
                    ))
                })?;
                if session.state.definitions.remove(name).is_none() {
                    return Ok(false);
                }
                session
                    .save()
                    .map_err(|error| restore_after_failure(&path, &original, error))?;
                Ok(true)
            })?;

        let finish = || -> Result<(Option<ConsumerTeardown>, bool), OrbitError> {
            let consumer = tear_down_auto_task_consumer(
                self,
                &definition,
                reason.as_deref().unwrap_or(DEFAULT_DELETE_REASON),
                params.force,
                now,
            )?;
            let shipped = is_shipped_default(name);
            if shipped {
                record_managed_asset_opt_out(
                    &auto_tasks_dir(&self.paths().local_dir),
                    ASSET_KIND,
                    ManagedAssetLayout::YamlStem,
                    name,
                )?;
            }
            Ok((consumer, shipped))
        };
        let (consumer, opted_out) =
            finish().map_err(|error| restore_after_failure(&path, &original, error))?;

        let report = AutoTaskDeleteReport {
            name: name.to_string(),
            path,
            opted_out,
            cursor_removed,
            open_tasks,
            consumer,
            reason,
            deleted_by: actor,
            deleted_at: now.to_rfc3339(),
        };
        self.record_auto_task_audit(
            "delete",
            name,
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(report)
    }

    /// Write a shipped default back with its shipped content and clear the
    /// opt-out a delete recorded for it. Refuses a name Orbit does not ship
    /// and a definition that already exists.
    pub fn auto_task_restore(&self, name: &str) -> Result<AutoTaskDefinition, OrbitError> {
        let (_, embedded) = DEFAULT_AUTO_TASK_FILES
            .iter()
            .find(|(shipped, _)| *shipped == name)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "auto-task '{name}' is not a shipped default; only shipped defaults can be restored"
                ))
            })?;
        let path = definition_path(&self.paths().local_dir, name);
        let collection = self.validated_auto_tasks(name)?;
        if path.exists()
            || collection
                .definitions
                .iter()
                .any(|loaded| loaded.definition.name == name)
        {
            return Err(OrbitError::InvalidInput(format!(
                "auto-task '{name}' already exists; delete it first to restore the shipped content"
            )));
        }

        let rendered = render_default_auto_task(embedded, self.workspace_base_branch());
        let definition = parse_auto_task_yaml(&rendered)?;
        atomic_write_text(&path, &rendered).map_err(|error| {
            OrbitError::Io(format!(
                "restore auto-task '{name}' at {}: {error}",
                path.display()
            ))
        })?;
        restore_managed_asset(
            &auto_tasks_dir(&self.paths().local_dir),
            ASSET_KIND,
            ManagedAssetLayout::YamlStem,
            name,
            sha256_hex(rendered.as_bytes()),
        )?;

        let actor = self.actor().resolve_write_label(None, None)?;
        self.record_auto_task_audit(
            "restore",
            name,
            json!({
                "name": name,
                "path": path,
                "restored_by": actor,
                "restored_at": Utc::now().to_rfc3339(),
            }),
        );
        Ok(definition)
    }

    /// Persist one definition-lifecycle audit event. The change it records is
    /// already on disk, so a failed audit write is logged, not returned.
    fn record_auto_task_audit(&self, subcommand: &str, name: &str, payload: Value) {
        let params = AuditEventInsertParams {
            execution_id: audit_execution_id(&format!("audit-auto-task-{subcommand}")),
            command: "auto_task".to_string(),
            subcommand: Some(subcommand.to_string()),
            tool_name: Some(format!("orbit.auto_task.{subcommand}")),
            target_type: Some(ASSET_KIND.to_string()),
            target_id: Some(name.to_string()),
            role: "admin".to_string(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: self.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(payload.to_string()),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: std::env::var("ORBIT_RUN_ID").ok().filter(|s| !s.is_empty()),
            activity_id: std::env::var("ORBIT_ACTIVITY_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            step_index: std::env::var("ORBIT_STEP_INDEX")
                .ok()
                .and_then(|s| s.parse().ok()),
        };
        if let Err(error) = self.record_audit_event(&params) {
            tracing::error!(
                target: "orbit.core.auto_tasks",
                auto_task = name,
                subcommand,
                %error,
                "failed to persist auto-task audit event"
            );
        }
    }
}

fn is_shipped_default(name: &str) -> bool {
    DEFAULT_AUTO_TASK_FILES
        .iter()
        .any(|(shipped, _)| *shipped == name)
}

/// Put the deleted definition back after a later step failed, so a refused
/// or failed delete leaves the definition in place.
fn restore_after_failure(path: &std::path::Path, original: &str, error: OrbitError) -> OrbitError {
    match atomic_write_text(path, original) {
        Ok(()) => error,
        Err(restore_error) => OrbitError::Io(format!(
            "{error}; restoring auto-task definition {} also failed: {restore_error}",
            path.display()
        )),
    }
}
