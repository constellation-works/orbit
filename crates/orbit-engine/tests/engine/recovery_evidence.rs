//! Built-in step recovery receives bounded evidence through the public job
//! executor, without asking the recovering agent to observe operator runs.

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{DispatchError, RuntimeHost, V2AuditWriter, execute_job_with_resume};
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, DeterministicSpec, JobV2, V2AuditEventKind,
};
use serde_json::{Value, json};

const RUN: &str = "jrun-recovery-evidence";
const ACTIVITIES: [&str; 2] = ["step_failure_recovery", "pr_conflict_recovery"];

struct Host {
    activity: &'static str,
    log: Result<Option<String>, String>,
    inputs: Mutex<Vec<Value>>,
}

impl RuntimeHost for Host {
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == self.activity {
            // The deterministic backend adds its own step_id after activity
            // dispatch; it is not part of the agent leaf's input schema.
            let mut leaf_input = input.clone();
            leaf_input.as_object_mut().unwrap().remove("step_id");
            self.inputs.lock().unwrap().push(leaf_input);
            return Ok(json!({}));
        }
        if self.activity == "pr_conflict_recovery" {
            return Err(DispatchError::RecoverableVcsConflict {
                operation: "git_rebase".to_string(),
                original_base_sha: "original".to_string(),
                target_base_sha: "target".to_string(),
                conflicting_paths: vec!["src/lib.rs".to_string()],
                diagnostic: "rebase stopped on an unmerged path".to_string(),
            });
        }
        Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "candidate validation failed".to_string(),
        })
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("fixture".to_string())
    }

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        assert_eq!(
            run_id, RUN,
            "never read a rendered stale or foreign run log"
        );
        self.log.clone().map_err(OrbitError::Execution)
    }
}

fn job(activity: &str) -> (JobV2, Value) {
    let yaml = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../orbit-core/assets/activities/{activity}.yaml")),
    )
    .unwrap();
    let mut recovery = load_activity_asset(&yaml).unwrap().spec;
    let schema = recovery.input_schema_json.clone();
    // Capture exactly what the production dispatcher passes to the leaf;
    // provider startup and response formatting are separate boundaries.
    recovery.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: activity.to_string(),
        config: json!({}),
    });
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "recovery_evidence_fixture"},
            "spec": {
                "state": "enabled", "kind": "workflow",
                "steps": [{
                    "id": "work", "recovery_activity": activity,
                    "spec": {"type": "deterministic", "action": "work", "config": {}},
                    "default_input": {
                        "task_id": "fixture-task",
                        "workspace_path": "/worktrees/fixture",
                        "repo_root": "/worktrees/fixture",
                        "run_id": "stale-rendered-run",
                    },
                }],
            },
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    job.steps[0].resolved_recovery_activity = Some(recovery);
    (job, schema)
}

fn prior_attempt(index: usize, diagnostic: String) -> V2AuditEventKind {
    V2AuditEventKind::StepPostRecoveryAttempt {
        step_id: format!("prior-{index}"),
        recovery_activity: "prior-recovery".to_string(),
        outcome: "error".to_string(),
        error_message: Some(diagnostic),
        output: Some(json!({"marker": index})),
    }
}

fn run(host: &Host, events: Vec<V2AuditEventKind>) -> (Value, Value) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(orbit_store::Store::open_in_memory().unwrap());
    let writer = V2AuditWriter::with_disk_sinks(
        dir.path(),
        store.clone(),
        "fixture",
        RUN,
        "test-agent",
        None,
    )
    .unwrap();
    let foreign = V2AuditWriter::with_disk_sinks(
        dir.path(),
        store,
        "fixture",
        "foreign-run",
        "test-agent",
        None,
    )
    .unwrap();
    foreign
        .emit(prior_attempt(99, "foreign diagnostic".to_string()))
        .unwrap();
    for event in events {
        writer.emit(event).unwrap();
    }
    let (job, schema) = job(host.activity);
    assert!(
        execute_job_with_resume(&job, json!({}), RUN, writer.clone(), host, None).is_err(),
        "the original failing step remains authoritative"
    );
    let attempt = writer
        .events_snapshot()
        .unwrap()
        .into_iter()
        .find_map(|event| {
            if let V2AuditEventKind::StepRecoveryAttempted {
                failure_phase,
                error_message,
                ..
            } = event.kind
            {
                Some(json!({"failure_phase": failure_phase, "error_message": error_message}))
            } else {
                None
            }
        })
        .unwrap();
    (schema, attempt)
}

#[test]
fn both_recovery_dispatches_inject_bounded_chronological_run_evidence() {
    for activity in ACTIVITIES {
        let host = Host {
            activity,
            log: Ok(Some(format!("{}own-log-end", "界".repeat(100_000)))),
            inputs: Mutex::new(Vec::new()),
        };
        let (schema, _) = run(
            &host,
            vec![
                prior_attempt(0, "historical failure".to_string()),
                prior_attempt(1, format!("start:{}:end", "界".repeat(100_000))),
            ],
        );
        let inputs = host.inputs.lock().unwrap();
        let [input] = inputs.as_slice() else {
            panic!("one recovery dispatch: {inputs:?}")
        };
        let validator = jsonschema::JSONSchema::compile(&schema).unwrap();
        assert!(
            validator.is_valid(input),
            "{activity} accepts its injected evidence input"
        );
        assert_eq!(input["run_id"], RUN);
        let attempts = input["step_recovery_attempts"].as_array().unwrap();
        assert_eq!(
            attempts.len(),
            2,
            "keep all own attempts and exclude foreign evidence"
        );
        assert_eq!(attempts[0]["failed_step_id"], "prior-0");
        assert_eq!(attempts[1]["failed_step_id"], "prior-1");
        assert_eq!(attempts[1]["output"]["marker"], 1);
        assert!(attempts[0]["attempted_at"].as_str() <= attempts[1]["attempted_at"].as_str());
        let diagnostic = attempts[1]["error_message"].as_str().unwrap();
        assert!(diagnostic.len() <= 8 * 1024);
        assert!(diagnostic.starts_with("start:") && diagnostic.ends_with(":end"));
        assert!(serde_json::to_vec(attempts).unwrap().len() <= 64 * 1024);
        let log = input["log_tail"].as_str().unwrap();
        assert!(log.len() <= 64 * 1024 && log.ends_with("own-log-end"));
    }
}

#[test]
fn both_recovery_dispatches_distinguish_missing_logs_from_unreadable_or_oversized_evidence() {
    for activity in ACTIVITIES {
        for case in ["missing", "unreadable", "oversized"] {
            let host = Host {
                activity,
                log: if case == "unreadable" {
                    Err("log read refused".to_string())
                } else {
                    Ok(None)
                },
                inputs: Mutex::new(Vec::new()),
            };
            let events = if case == "oversized" {
                (0..500)
                    .map(|i| prior_attempt(i, "failed ".repeat(100)))
                    .collect()
            } else {
                Vec::new()
            };
            let (_, attempt) = run(&host, events);
            let inputs = host.inputs.lock().unwrap();
            if case == "missing" {
                assert_eq!(inputs.len(), 1, "an absent log still permits recovery");
                assert_eq!(inputs[0]["log_tail"], "");
                assert_eq!(inputs[0]["step_recovery_attempts"], json!([]));
            } else {
                assert!(
                    inputs.is_empty(),
                    "never dispatch incomplete or unreadable evidence"
                );
                assert_eq!(attempt["failure_phase"], "input");
                let message = attempt["error_message"].as_str().unwrap();
                assert!(message.contains(if case == "unreadable" {
                    "log read refused"
                } else {
                    "step-recovery evidence exceeds"
                }));
            }
        }
    }
}
