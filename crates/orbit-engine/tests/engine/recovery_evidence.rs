//! Built-in step recovery receives bounded evidence through the public job
//! executor, without asking the recovering agent to observe operator runs.

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, DeterministicSpec, JobV2, JobV2StepBody, V2AuditEventKind,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

const RUN: &str = "jrun-recovery-evidence";
const ACTIVITIES: [&str; 2] = ["step_failure_recovery", "pr_conflict_recovery"];

struct Host {
    activity: &'static str,
    log: Result<Option<String>, String>,
    inputs: Mutex<Vec<Value>>,
    worker_run: Option<&'static str>,
}

impl orbit_engine::test_support::NullRuntimeHost for Host {
    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        self.worker_run
            .map(|run_id| orbit_types::tool::WorkerInvocation {
                owner_machine_id: "owner".into(),
                owner_workspace_id: "workspace".into(),
                owner_destination: "owner".into(),
                task_id: "fixture-task".into(),
                claim_id: "fixture-claim".into(),
                execution: orbit_types::task::ExecutionLocation {
                    machine_id: "follower".into(),
                    machine_name: None,
                },
                bound_run_id: run_id.into(),
            })
    }
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
            return Err(DispatchError::RecoverableVcsConflict(Box::new(
                orbit_common::RecoverableVcsConflict {
                    operation: "git_rebase".to_string(),
                    original_base_sha: "original".to_string(),
                    target_base_sha: "target".to_string(),
                    conflicting_paths: vec!["src/lib.rs".to_string()],
                    conflicting_files: Vec::new(),
                    diagnostic: "rebase stopped on an unmerged path".to_string(),
                },
            )));
        }
        Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "candidate validation failed".to_string(),
        })
    }

    fn get_task(&self, _: &str) -> Result<orbit_types::task::Task, OrbitError> {
        // Fault injection at the real git_rebase backend's task-read boundary
        // preserves resolved backend identity without starting a Git fixture.
        Err(OrbitError::RecoverableVcsConflict(Box::new(
            orbit_common::RecoverableVcsConflict {
                operation: "git_rebase".into(),
                original_base_sha: "original".into(),
                target_base_sha: "target".into(),
                conflicting_paths: vec!["src/lib.rs".into()],
                conflicting_files: Vec::new(),
                diagnostic: "injected rebase conflict".into(),
            },
        )))
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
            worker_run: None,
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
                worker_run: None,
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

#[test]
fn conflict_recovery_hands_over_only_managed_sync_validation() {
    for case in [
        "no-validator",
        "owner",
        "claim",
        "unbound-claim",
        "foreign-claim",
        "wrong-claim-task",
        "renamed",
        "renamed-claim-carry",
        "renamed-claim-carry-false",
        "renamed-claim-carry-true",
        "no-flag",
        "false-flag",
        "wrong-rebase",
        "wrong-validator",
        "disabled",
        "extra-guard",
        "shared-guard",
        "wrong-workspace",
        "wrong-tasks",
        "wrong-run",
        "wrong-base",
        "wrong-base-ref",
        "prevalidated",
        "compound-validator",
        "checkpointed",
        "validator-before",
        "nested-loop",
        "nested-fan-out",
        "nested-parallel",
        "shipped-owner",
        "shipped-claim",
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_str().unwrap();
        let host = Host {
            activity: "pr_conflict_recovery",
            log: Ok(None),
            inputs: Mutex::default(),
            worker_run: match case {
                "claim"
                | "prevalidated"
                | "shipped-claim"
                | "wrong-claim-task"
                | "renamed-claim-carry"
                | "renamed-claim-carry-false"
                | "renamed-claim-carry-true" => Some(RUN),
                "foreign-claim" => Some("foreign-run"),
                _ => None,
            },
        };
        let (mut job, schema) = job(host.activity);
        let mut resume = None;
        let input = json!({"task_ids": ["fixture-task"], "base_sync": "local"});
        if case.starts_with("shipped-") {
            let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-core/assets");
            let name = if case == "shipped-owner" {
                "task_pr_pipeline"
            } else {
                "task_claimed_pr_pipeline"
            };
            let recovery = job.steps[0].resolved_recovery_activity.clone();
            job = load_job_asset(
                &std::fs::read_to_string(assets.join(format!("jobs/{name}.yaml"))).unwrap(),
            )
            .unwrap()
            .spec;
            let mut catalog = V2ActivityCatalog::new();
            catalog.load_dir(&assets.join("activities")).unwrap();
            resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
            // Keep the loaded/resolved continuation, including the claimed
            // review steps. Seed only the already-completed preparation.
            let mut state = PipelineState::new(RUN.into(), name.into(), input.clone());
            state.compound_outputs.insert(
                0,
                std::collections::BTreeMap::from([("implement_one".into(), json!({}))]),
            );
            for (index, step) in job.steps.iter_mut().enumerate() {
                if step.id == "sync_base" {
                    step.resolved_recovery_activity = recovery;
                    break;
                }
                state.step_states.insert(index as u32, JobRunState::Success);
                state.step_outputs.insert(index as u32, match step.id.as_str() {
                    "worktree" => json!({"job_run_id": RUN, "workspace_path": workspace_path}),
                    "prepare_branch" => json!({"head": "candidate", "head_sha": "head", "base": "agent-main", "base_ref": "agent-main", "base_sha": "base", "remote_sha": null, "commits_behind": 1, "sync_required": true}),
                    "implement_one" => json!({}),
                    "commit" => json!({"skipped_no_diff_expected": false}),
                    _ => json!({}),
                });
            }
            // The failing step stays authoritative without unrelated final
            // recovery/provider dispatch after the captured recovery attempt.
            job.final_recovery_activity = None;
            job.resolved_final_recovery_activity = None;
            job.failure_activity = None;
            job.resolved_failure_activity = None;
            resume = Some(state);
        } else {
            job.steps[0].id = if matches!(
                case,
                "renamed"
                    | "renamed-claim-carry"
                    | "renamed-claim-carry-false"
                    | "renamed-claim-carry-true"
            ) {
                "custom_sync"
            } else {
                "sync_base"
            }
            .into();
            let sync_id = job.steps[0].id.clone();
            let JobV2StepBody::Target(target) = &mut job.steps[0].body else {
                panic!("target fixture")
            };
            target.activity_name = Some("git_rebase".into());
            target.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
                action: if case == "wrong-rebase" {
                    "work"
                } else {
                    "git_rebase"
                }
                .into(),
                config: json!({}),
            });
            let sync_input = target.default_input.as_mut().unwrap();
            sync_input["workspace_path"] = json!(workspace_path);
            sync_input["repo_root"] = json!(workspace_path);
            sync_input["job_run_id"] = json!(RUN);
            sync_input["completed_task_ids"] = json!(["fixture-task"]);
            if case == "wrong-claim-task" {
                sync_input["completed_task_ids"] = json!(["another-task"]);
            }
            if case != "no-flag" {
                sync_input["final_validation"] = json!(case != "false-flag");
            }
            let mut validator = load_job_asset(&json!({
                "schemaVersion": 2, "kind": "Job", "metadata": {"name": "validator"},
                "spec": {"state": "enabled", "steps": [{
                    "id": "validate", "spec": {"type": "deterministic", "action": if matches!(case, "claim" | "prevalidated" | "unbound-claim" | "foreign-claim" | "wrong-claim-task" | "renamed-claim-carry" | "renamed-claim-carry-false" | "renamed-claim-carry-true") {"claim_validate"} else {"candidate_validate"}, "config": {}},
                    "default_input": {
                        "workspace_path": workspace_path, "job_run_id": RUN,
                        "completed_task_ids": ["fixture-task"],
                        "base_sha": format!("{{{{ steps.{sync_id}.output.base_sha }}}}"),
                    },
                }]},
            }).to_string()).unwrap().spec.steps.remove(0);
            if case == "shared-guard" {
                job.steps[0].when = Some("{{ input.base_sync }} == local".into());
                validator.when = job.steps[0].when.clone();
            }
            if case == "disabled" {
                validator.when = Some("false".into());
            }
            if case == "extra-guard" {
                validator.when = Some("{{ input.base_sync }} == remote".into());
            }
            let JobV2StepBody::Target(target) = &mut validator.body else {
                panic!("validator fixture")
            };
            if case.starts_with("renamed-claim-carry") {
                let input = target.default_input.as_mut().unwrap();
                input["carry"] = json!({"decision": "passed", "candidate": {"commit": "prior"}});
                if case.ends_with("-false") {
                    input["revalidate"] = json!(false);
                } else if case.ends_with("-true") {
                    input["revalidate"] = json!(true);
                }
            }
            if case == "wrong-claim-task" {
                target.default_input.as_mut().unwrap()["completed_task_ids"] =
                    json!(["another-task"]);
            }
            if case == "wrong-validator" {
                target.activity_name = Some("candidate_validate".into());
                target.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: "work".into(),
                    config: json!({}),
                });
            }
            for (case_name, key, value) in [
                ("wrong-workspace", "workspace_path", json!("/another")),
                ("wrong-tasks", "completed_task_ids", json!(["another-task"])),
                ("wrong-run", "job_run_id", json!("foreign-run")),
                ("wrong-base", "base_sha", json!("old-base")),
                ("wrong-base-ref", "base_ref", json!("foreign-ref")),
                (
                    "prevalidated",
                    "prevalidated",
                    json!({"decision": "passed"}),
                ),
            ] {
                if case == case_name {
                    target.default_input.as_mut().unwrap()[key] = value;
                }
            }
            if case.starts_with("nested-") {
                let sync = serde_json::to_value(&job.steps[0]).unwrap();
                let body = match case {
                    "nested-loop" => json!({"loop": {"max_iterations": 1, "steps": [sync]}}),
                    "nested-fan-out" => {
                        json!({"fan_out": {"items": "[1]", "worker": sync}, "fan_in": {"join": {"mode": "all"}}})
                    }
                    _ => json!({"parallel": {"branches": [sync], "join": {"mode": "all"}}}),
                };
                let mut block = body;
                block["id"] = json!("nested");
                let mut parsed = load_job_asset(&json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "nested"}, "spec": {"state": "enabled", "steps": [block]}}).to_string()).unwrap().spec.steps.remove(0);
                let nested = match &mut parsed.body {
                    JobV2StepBody::Loop { loop_ } => &mut loop_.steps[0],
                    JobV2StepBody::FanOut { fan_out, .. } => &mut fan_out.worker,
                    JobV2StepBody::Parallel { parallel } => &mut parallel.branches[0],
                    _ => unreachable!(),
                };
                nested.resolved_recovery_activity = job.steps[0].resolved_recovery_activity.clone();
                job.steps[0] = parsed;
            }
            if case != "no-validator" {
                if case == "compound-validator" {
                    validator = load_job_asset(&json!({
                        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "compound"},
                        "spec": {"state": "enabled", "steps": [{
                            "id": "container", "loop": {"max_iterations": 1, "steps": [validator]},
                        }]},
                    }).to_string()).unwrap().spec.steps.remove(0);
                }
                job.steps.push(validator);
            }
            if case == "validator-before" {
                job.steps.swap(0, 1);
            }
            if case == "checkpointed" || case == "validator-before" {
                let mut state = PipelineState::new(RUN.into(), "fixture".into(), input.clone());
                let index = if case == "checkpointed" { 1 } else { 0 };
                state.step_states.insert(index, JobRunState::Success);
                state.step_outputs.insert(index, json!({}));
                resume = Some(state);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let writer = V2AuditWriter::with_disk_sinks(
            dir.path(),
            Arc::new(orbit_store::Store::open_in_memory().unwrap()),
            "fixture",
            RUN,
            "test-agent",
            None,
        )
        .unwrap();
        let outcome = execute_job_with_resume(&job, input, RUN, writer, &host, resume.as_ref());
        assert!(outcome.is_err(), "{case}: {outcome:?}");
        let inputs = host.inputs.lock().unwrap();
        let [input] = inputs.as_slice() else {
            panic!("{case}: one recovery: {inputs:?}; {outcome:?}")
        };
        assert_eq!(
            input["final_validation"] == true,
            matches!(
                case,
                "owner"
                    | "claim"
                    | "renamed"
                    | "renamed-claim-carry-true"
                    | "shared-guard"
                    | "shipped-owner"
                    | "shipped-claim"
            ),
            "{case}: only a proven active continuation owns the full gates"
        );
        assert!(
            jsonschema::JSONSchema::compile(&schema)
                .unwrap()
                .is_valid(input),
            "{case}: {input}"
        );
    }
}
