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

fn import_marker_key(workspace_id: &str) -> String {
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use orbit_types::workflow::{JobRun, JobRunState};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn import_is_marker_gated() {
        let temp = TempDir::new().expect("tempdir");
        let orbit = temp.path().join(".orbit");
        fs::create_dir_all(orbit.join("state/audit/v2_loop")).expect("audit dir");
        let event = serde_json::json!({
            "schemaVersion": 1,
            "event_type": "tool.denied",
            "event_id": "evt-1",
            "ts": "2026-01-01T00:00:00Z",
            "run_id": "run-1",
            "agent_identity": "codex",
            "body_kind": "tool_denied",
            "tool_name": "fs.read",
            "reason": "denied"
        });
        fs::write(
            orbit.join("state/audit/v2_loop/run-1.jsonl"),
            format!("{event}\n"),
        )
        .expect("write event");

        let run = JobRun {
            executed_on: None,
            run_id: "run-1".to_string(),
            job_id: "job-a".to_string(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            created_at: Utc::now(),
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        let run_dir = orbit.join("state/job-runs/job-a/run-1");
        fs::create_dir_all(&run_dir).expect("run dir");
        fs::write(
            run_dir.join("jrun.yaml"),
            serde_yaml::to_string(&serde_json::json!({"schema_version": 1, "run": run}))
                .expect("serialize"),
        )
        .expect("write run");
        let store = Store::open_in_memory().expect("store");
        let first = import_legacy_v2_state(&store, &orbit, "ws_a").expect("first");
        assert!(!first.skipped);
        assert_eq!(first.audit_events_inserted, 1);
        assert_eq!(first.job_runs_inserted, 1);

        let second = import_legacy_v2_state(&store, &orbit, "ws_a").expect("second");
        assert!(second.skipped);
        assert_eq!(
            store
                .count_v2_audit_events(&crate::V2AuditEventFilter {
                    workspace_id: "ws_a".to_string(),
                    ..Default::default()
                })
                .expect("count"),
            1
        );
    }

    #[test]
    fn import_sets_marker_even_when_audit_lines_are_skipped() {
        let temp = TempDir::new().expect("tempdir");
        let orbit = temp.path().join(".orbit");
        fs::create_dir_all(orbit.join("state/audit/v2_loop")).expect("audit dir");
        fs::write(orbit.join("state/audit/v2_loop/run-1.jsonl"), "not-json\n")
            .expect("write malformed event");

        let store = Store::open_in_memory().expect("store");
        let first = import_legacy_v2_state(&store, &orbit, "ws_a").expect("first");
        assert!(!first.skipped);
        assert_eq!(first.audit_events_inserted, 0);
        assert_eq!(first.audit_events_skipped, 1);
        assert!(first.skipped_records());
        assert!(
            store
                .schema_meta_value(&import_marker_key("ws_a"))
                .expect("marker read")
                .is_some()
        );

        let second = import_legacy_v2_state(&store, &orbit, "ws_a").expect("second");
        assert!(second.skipped);
    }

    #[test]
    fn import_skips_malformed_job_run_and_step_files() {
        let temp = TempDir::new().expect("tempdir");
        let orbit = temp.path().join(".orbit");

        let valid_run = JobRun {
            executed_on: None,
            run_id: "run-good".to_string(),
            job_id: "job-a".to_string(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: Utc::now(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            created_at: Utc::now(),
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        let valid_dir = orbit.join("state/job-runs/job-a/run-good");
        fs::create_dir_all(valid_dir.join("steps")).expect("valid run dir");
        fs::write(
            valid_dir.join("jrun.yaml"),
            serde_yaml::to_string(&serde_json::json!({"schema_version": 1, "run": valid_run}))
                .expect("serialize run"),
        )
        .expect("write valid run");
        fs::write(valid_dir.join("steps/000.json"), "not-json\n").expect("write bad step");

        let malformed_dir = orbit.join("state/job-runs/job-a/run-bad");
        fs::create_dir_all(&malformed_dir).expect("malformed run dir");
        fs::write(malformed_dir.join("jrun.yaml"), "run: [").expect("write bad run");

        let store = Store::open_in_memory().expect("store");
        let report = import_legacy_v2_state(&store, &orbit, "ws_a").expect("import");

        assert_eq!(report.job_runs_inserted, 1);
        assert_eq!(report.job_runs_skipped, 1);
        assert_eq!(report.job_run_steps_inserted, 0);
        assert_eq!(report.job_run_steps_skipped, 1);
        assert!(report.skipped_records());
        let loaded = store
            .get_job_run_for_workspace("ws_a", "run-good")
            .expect("read valid run")
            .expect("valid run inserted");
        assert_eq!(loaded.job_id, "job-a");
    }
}
