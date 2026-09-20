//! Direct v2 activity execution helper.
//!
//! Reads a YAML file from disk, parses it through the two-pass loader at
//! `orbit_engine::activity_job::load_activity_asset`, and invokes the dispatcher with
//! `OrbitRuntime` as the `V2RuntimeHost` (impl lives in
//! `orbit_core::runtime`'s v2 host).
//!
//! Loop + envelope audit sink construction is delegated to
//! `V2AuditWriter::with_disk_sinks` — this file never names orbit-agent types.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::{V2AuditWriter, V2DispatchInput, dispatch_v2_activity, load_activity_asset};
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::activity_job::{
    V2AuditEventKind, validate_activity_tool_allowlist_against_registered_tools,
};
use serde_json::Value;

use orbit_core::OrbitRuntime;
use orbit_core::application::SYSTEM_AUDIT_IDENTITY;

#[derive(Debug)]
pub struct V2ActivityRunResult {
    pub activity_name: String,
    pub activity_type: &'static str,
    pub success: bool,
    pub output: Value,
    pub message: Option<String>,
    pub run_id: String,
    pub events_emitted: u64,
}

/// Direct v2 activity execution surface for [`OrbitRuntime`] (extension
/// trait — the implementation moved out of orbit-core in [ORB-10016]).
pub trait ActivityV2Commands {
    /// Execute a v2 activity from a YAML path. Returns a structural result.
    /// Audit events for the run are queryable via `list_v2_audit_events` using
    /// the `run_id`.
    fn run_activity_v2_from_yaml(
        &self,
        yaml_path: &Path,
        input: Value,
    ) -> Result<V2ActivityRunResult, OrbitError>;
}

impl ActivityV2Commands for OrbitRuntime {
    fn run_activity_v2_from_yaml(
        &self,
        yaml_path: &Path,
        input: Value,
    ) -> Result<V2ActivityRunResult, OrbitError> {
        let yaml = std::fs::read_to_string(yaml_path).map_err(|err| {
            OrbitError::InvalidInput(format!("read {}: {err}", yaml_path.display()))
        })?;
        let asset = load_activity_asset(&yaml).map_err(|err| {
            OrbitError::InvalidInput(format!("load {}: {err}", yaml_path.display()))
        })?;
        let registered_tools = self.allowlist_known_tool_names();
        validate_activity_tool_allowlist_against_registered_tools(
            &asset.spec,
            registered_tools.iter().map(String::as_str),
        )
        .map_err(|err| {
            OrbitError::InvalidInput(format!(
                "activity `{}` tool allowlist invalid: {err}",
                asset.name
            ))
        })?;

        let run_id = format!(
            "activity-{}-{}",
            asset.name,
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f")
        );

        let audit_root = self.paths().audit_dir.clone();
        let workspace_path = self.paths().repo_root.clone();
        let writer = V2AuditWriter::with_disk_sinks(
            &audit_root,
            self.v2_audit_store()?,
            self.workspace_id()?,
            &run_id,
            SYSTEM_AUDIT_IDENTITY,
            Some(workspace_path.as_path()),
        )
        .map_err(|err| OrbitError::Execution(format!("audit sinks: {err}")))?;
        // Record the standard orbit-core activity-run lifecycle events so v2
        // runs appear in the same audit stream v1 runs use.
        self.record_event(OrbitEvent::ActivityRunStarted {
            id: asset.name.clone(),
        })?;
        let _ = writer.emit(V2AuditEventKind::RunStarted {
            job_name: format!("cli:{}", asset.name),
            retry_source_run_id: None,
        });

        let activity_type = match &asset.spec.spec {
            orbit_types::workflow::activity_job::ActivityV2Spec::AgentLoop(_) => "agent_loop",
            orbit_types::workflow::activity_job::ActivityV2Spec::Deterministic(_) => {
                "deterministic"
            }
        };

        let dispatch = dispatch_v2_activity(V2DispatchInput {
            activity_name: &asset.name,
            spec: &asset.spec.spec,
            fs_profile: asset.spec.fs_profile.as_deref(),
            input,
            audit: writer.clone(),
            run_id: &run_id,
            host: Some(self),
        });

        let (outcome_str, error_message) = match &dispatch {
            Ok(o) if o.success => ("success", None),
            Ok(o) => ("failed", o.message.clone()),
            Err(err) => ("error", Some(format!("v2 dispatch: {err}"))),
        };
        let _ = writer.emit(V2AuditEventKind::RunFinished {
            outcome: outcome_str.to_string(),
            error_message,
        });
        self.record_event(OrbitEvent::ActivityRunCompleted {
            id: asset.name.clone(),
            state: outcome_str.to_string(),
        })?;

        let events_count = writer.emitted_event_count();

        match dispatch {
            Ok(o) => Ok(V2ActivityRunResult {
                activity_name: asset.name,
                activity_type,
                success: o.success,
                output: o.output,
                message: o.message,
                run_id,
                events_emitted: events_count,
            }),
            Err(err) => Err(OrbitError::Execution(format!("v2 dispatch: {err}"))),
        }
    }
}
