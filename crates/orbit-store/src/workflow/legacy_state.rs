use std::fs;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::activity_job::V2AuditEvent;
use orbit_types::workflow::{JobRun, JobRunStep, PipelineState};
use serde::Deserialize;

use crate::Store;
use crate::contracts::V2AuditEventInsertParams;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub skipped: bool,
    pub audit_events_inserted: usize,
    pub audit_events_skipped: usize,
    pub job_runs_inserted: usize,
    pub job_runs_skipped: usize,
    pub job_run_steps_inserted: usize,
    pub job_run_steps_skipped: usize,
}

impl ImportReport {
    pub fn skipped() -> Self {
        Self {
            skipped: true,
            ..Self::default()
        }
    }

    pub fn skipped_records(&self) -> bool {
        self.audit_events_skipped > 0 || self.job_runs_skipped > 0 || self.job_run_steps_skipped > 0
    }
}

#[derive(Debug, Deserialize)]
struct JobRunFileDocument {
    run: JobRun,
}

#[derive(Debug, Deserialize)]
struct JobRunStepFileDocument {
    step: JobRunStep,
}

pub fn import_legacy_v2_state(
    store: &Store,
    orbit_root: &Path,
    workspace_id: &str,
) -> Result<ImportReport, OrbitError> {
    let marker_key = import_marker_key(workspace_id);
    if store.schema_meta_value(&marker_key)?.is_some() {
        return Ok(ImportReport::skipped());
    }

    let mut report = ImportReport::default();
    import_audit_dir(
        store,
        workspace_id,
        &orbit_root.join("state").join("audit").join("v2_loop"),
        "v2_envelope",
        &mut report,
    )?;
    import_audit_dir(
        store,
        workspace_id,
        &orbit_root.join("state").join("audit").join("loop"),
        "loop_event",
        &mut report,
    )?;
    import_job_runs(store, workspace_id, orbit_root, &mut report)?;

    store.set_schema_meta_value(&marker_key, &Utc::now().to_rfc3339())?;
    Ok(report)
}

// Widened to pub(crate) so sibling `workflow/tests/legacy_state.rs` can
// assert the import marker after the test-layout migration.
pub(crate) fn import_marker_key(workspace_id: &str) -> String {
    format!("v2_state_imported_at:{workspace_id}")
}

fn import_audit_dir(
    store: &Store,
    workspace_id: &str,
    dir: &Path,
    source: &str,
    report: &mut ImportReport,
) -> Result<(), OrbitError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(_) => {
                report.audit_events_skipped += 1;
                continue;
            }
        };
        let fallback_run_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown");
        for (index, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let params = match source {
                "v2_envelope" => v2_envelope_params(workspace_id, source, line),
                _ => loop_event_params(workspace_id, source, fallback_run_id, index, line),
            };
            match params {
                Ok(params) => {
                    store.insert_v2_audit_event(&params)?;
                    report.audit_events_inserted += 1;
                }
                Err(_) => report.audit_events_skipped += 1,
            }
        }
    }
    Ok(())
}

fn v2_envelope_params(
    workspace_id: &str,
    source: &str,
    line: &str,
) -> Result<V2AuditEventInsertParams, OrbitError> {
    let event: V2AuditEvent =
        serde_json::from_str(line).map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(V2AuditEventInsertParams {
        workspace_id: workspace_id.to_string(),
        event_id: event.envelope.event_id.clone(),
        source: source.to_string(),
        schema_version: event.envelope.schema_version,
        event_type: event.envelope.event_type.clone(),
        ts: event.envelope.ts,
        run_id: event.envelope.run_id.clone(),
        agent_identity: event.envelope.agent_identity.clone(),
        parent_event_id: event.envelope.parent_event_id.clone(),
        workspace_path: event.envelope.workspace_path.clone(),
        payload_json: line.to_string(),
    })
}

fn loop_event_params(
    workspace_id: &str,
    source: &str,
    fallback_run_id: &str,
    index: usize,
    line: &str,
) -> Result<V2AuditEventInsertParams, OrbitError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| OrbitError::Store(e.to_string()))?;
    let event_type = value
        .get("event_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("loop.event")
        .to_string();
    let ts = value
        .get("ts")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|ts| ts.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let run_id = value
        .get("run_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_run_id)
        .to_string();
    Ok(V2AuditEventInsertParams {
        workspace_id: workspace_id.to_string(),
        event_id: format!("loop:{run_id}:{index}"),
        source: source.to_string(),
        schema_version: 1,
        event_type,
        ts,
        run_id,
        agent_identity: value
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("loop")
            .to_string(),
        parent_event_id: None,
        workspace_path: None,
        payload_json: line.to_string(),
    })
}

fn import_job_runs(
    store: &Store,
    workspace_id: &str,
    orbit_root: &Path,
    report: &mut ImportReport,
) -> Result<(), OrbitError> {
    let runs_root = orbit_root.join("state").join("job-runs");
    let job_dirs = match fs::read_dir(&runs_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    for job_entry in job_dirs.filter_map(Result::ok) {
        let job_path = job_entry.path();
        if !job_path.is_dir()
            || job_path.file_name().and_then(|value| value.to_str()) == Some("archived")
        {
            continue;
        }
        let run_dirs = fs::read_dir(&job_path).map_err(|err| OrbitError::Io(err.to_string()))?;
        for run_entry in run_dirs.filter_map(Result::ok) {
            let run_path = run_entry.path();
            if !run_path.is_dir() {
                continue;
            }
            let jrun_path = run_path.join("jrun.yaml");
            let raw = match fs::read_to_string(&jrun_path) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            let doc = match serde_yaml::from_str::<JobRunFileDocument>(&raw) {
                Ok(doc) => doc,
                Err(err) => {
                    report.job_runs_skipped += 1;
                    orbit_common::tracing::warn!(
                        path = %jrun_path.display(),
                        error = %err,
                        "skipping malformed legacy job run"
                    );
                    continue;
                }
            };
            let pipeline_state = read_pipeline_state(&run_path)?;
            store.upsert_job_run_for_workspace(workspace_id, &doc.run, pipeline_state.as_ref())?;
            report.job_runs_inserted += 1;
            for step in read_steps(&run_path, report)? {
                store.upsert_job_run_step_for_workspace(workspace_id, &doc.run.run_id, &step)?;
                report.job_run_steps_inserted += 1;
            }
        }
    }
    Ok(())
}

fn read_pipeline_state(run_path: &Path) -> Result<Option<PipelineState>, OrbitError> {
    let state_path = run_path.join("state.json");
    if !state_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&state_path).map_err(|err| OrbitError::Io(err.to_string()))?;
    serde_json::from_str(&raw).map(Some).map_err(|err| {
        OrbitError::Store(format!(
            "invalid state.json '{}': {err}",
            state_path.display()
        ))
    })
}

fn read_steps(run_path: &Path, report: &mut ImportReport) -> Result<Vec<JobRunStep>, OrbitError> {
    let steps_dir = run_path.join("steps");
    let entries = match fs::read_dir(&steps_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(OrbitError::Io(err.to_string())),
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|ext| matches!(ext, "yaml" | "yml" | "json"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut steps = Vec::new();
    for path in paths {
        let raw = fs::read_to_string(&path).map_err(|err| OrbitError::Io(err.to_string()))?;
        let step = if path.extension().and_then(|value| value.to_str()) == Some("json") {
            match serde_json::from_str::<JobRunStep>(&raw) {
                Ok(step) => step,
                Err(err) => {
                    report.job_run_steps_skipped += 1;
                    orbit_common::tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "skipping malformed legacy job run step"
                    );
                    continue;
                }
            }
        } else {
            match serde_yaml::from_str::<JobRunStepFileDocument>(&raw) {
                Ok(doc) => doc.step,
                Err(err) => {
                    report.job_run_steps_skipped += 1;
                    orbit_common::tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "skipping malformed legacy job run step"
                    );
                    continue;
                }
            }
        };
        steps.push(step);
    }
    Ok(steps)
}
