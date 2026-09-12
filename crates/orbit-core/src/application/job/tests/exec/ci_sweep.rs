//! CI sweep admission and collect-to-pilot input propagation.

use std::sync::{Arc, Mutex};

use orbit_engine::{DispatchError, ResolvedCliExecutor, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use serde_json::{Value, json};

use super::{test_runtime, try_execute_named_job};
use crate::OrbitRuntime;

// Keep the effective named job unchanged; replace only its external-query
// activity with a recording fixture. The collector's real GitHub ref
// classification is covered at its existing CiQueries boundary in Engine.
fn seed_ci_catalogs(global: &std::path::Path) {
    super::seed_default_catalogs(global);
    let activity = global.join("resources/activities/collect_ci_evidence.yaml");
    let yaml = std::fs::read_to_string(&activity).expect("collect activity");
    std::fs::write(
        activity,
        yaml.replace(
            "action: collect_ci_evidence",
            "action: test_collect_ci_evidence",
        ),
    )
    .expect("stub external CI reads");
}

struct ScriptedCiSweepHost<'a> {
    runtime: &'a OrbitRuntime,
    empty: bool,
    calls: Mutex<Vec<(String, Value)>>,
    invoked_task_ids: Mutex<Vec<String>>,
    applied_task_ids: Mutex<Vec<String>>,
}

impl<'a> ScriptedCiSweepHost<'a> {
    fn new(runtime: &'a OrbitRuntime) -> Self {
        Self {
            runtime,
            empty: false,
            calls: Mutex::new(Vec::new()),
            invoked_task_ids: Mutex::new(Vec::new()),
            applied_task_ids: Mutex::new(Vec::new()),
        }
    }

    fn empty(mut self) -> Self {
        self.empty = true;
        self
    }

    fn invoked_task_ids(&self) -> Vec<String> {
        self.invoked_task_ids
            .lock()
            .expect("invoked task ids")
            .clone()
    }

    fn applied_task_ids(&self) -> Vec<String> {
        self.applied_task_ids
            .lock()
            .expect("applied task ids")
            .clone()
    }
}

impl RuntimeHost for ScriptedCiSweepHost<'_> {
    fn repo_root(&self) -> Result<String, orbit_common::OrbitError> {
        Ok(self.runtime.paths().repo_root.display().to_string())
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("calls")
            .push((action.to_string(), input.clone()));
        match action {
            "test_collect_ci_evidence" => Ok(json!({
                "phase": "collected",
                "ci_evidence": {
                    "schema_version": 1,
                    "collected": true,
                    "capability": {},
                    "current_failures": [],
                }
            })),
            "file_ci_failure_tasks" => {
                if self.empty {
                    return Ok(json!({
                        "outcome": "no_current_failure",
                        "clusters": 0,
                        "filed_count": 0,
                        "filed": [],
                        "pilot_candidate_count": 0,
                        "pilot_candidates": [],
                        "skipped_existing": [],
                        "skipped_over_cap": [],
                        "deferred": [],
                        "audit": {},
                    }));
                }
                let candidate = |task_id: &str| {
                    json!({
                        "task_id": task_id,
                        "failure_key": format!("failure-{task_id}"),
                        "cluster_key": format!("cluster-{task_id}"),
                        "workflow": "CI",
                        "job": "test",
                        "step": "cargo test",
                        "tested_commit": "1111111111111111111111111111111111111111",
                        "run_ids": [1],
                        "run_urls": ["https://example.test/run/1"],
                        "ref_kinds": ["integration"],
                        "head_branches": ["agent-main"],
                    })
                };
                Ok(json!({
                    "outcome": "current_failures",
                    "clusters": 2,
                    "filed_count": 2,
                    "filed": [],
                    "pilot_candidate_count": 2,
                    "pilot_candidates": [
                        candidate("ORB-STALE"),
                        candidate("ORB-APPLIED"),
                    ],
                    "skipped_existing": [],
                    "skipped_over_cap": [],
                    "deferred": [],
                    "audit": {},
                }))
            }
            "invoke_and_wait" => {
                let task_id = input["run_input"]["task_ids"][0]
                    .as_str()
                    .expect("pilot task id")
                    .to_string();
                self.invoked_task_ids
                    .lock()
                    .expect("invoked task ids")
                    .push(task_id.clone());
                if task_id == "ORB-STALE" {
                    Ok(json!({
                        "run_id": "jrun-pilot-stale",
                        "status": "failed",
                        "error": "pilot apply skipped stale task status",
                    }))
                } else {
                    self.applied_task_ids
                        .lock()
                        .expect("applied task ids")
                        .push(task_id);
                    Ok(json!({
                        "run_id": "jrun-pilot-applied",
                        "status": "succeeded",
                        "error": Value::Null,
                    }))
                }
            }
            "pipeline_success_guard" => <OrbitRuntime as RuntimeHost>::run_deterministic(
                self.runtime,
                action,
                config,
                input,
                tool_context,
            ),
            other => Err(DispatchError::DeterministicActionNotRegistered(
                other.to_string(),
            )),
        }
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(self.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

#[test]
fn ci_sweep_parent_fails_for_stale_pilot_after_independent_pilot_applies() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_ci_catalogs(&global_root);
    let host = ScriptedCiSweepHost::new(&runtime);

    let error = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "ci_failure_sweep_pipeline",
        json!({
            "integration_branch": "agent-main",
            "workspace_path": repo_root,
        }),
        "jrun-ci-sweep-partial-pilot",
    )
    .expect_err("a stale pilot child must fail the parent sweep");

    let mut invoked = host.invoked_task_ids();
    invoked.sort();
    assert_eq!(invoked, ["ORB-APPLIED", "ORB-STALE"], "{error}");
    assert_eq!(host.applied_task_ids(), ["ORB-APPLIED"]);
    let message = error.to_string();
    assert!(
        message.contains("ci-failure sweep pilot child"),
        "{message}"
    );
    assert!(message.contains("jrun-pilot-stale"), "{message}");
    assert!(message.contains("status failed"), "{message}");
}

#[test]
fn ci_sweep_parent_keeps_a_genuinely_empty_pilot_batch_as_a_successful_no_op() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_ci_catalogs(&global_root);
    let host = ScriptedCiSweepHost::new(&runtime).empty();

    let outcome = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "ci_failure_sweep_pipeline",
        json!({
            "integration_branch": "agent-main",
            "workspace_path": repo_root,
        }),
        "jrun-ci-sweep-empty",
    )
    .expect("an empty candidate list is a successful no-op");

    assert!(outcome.success);
    assert!(host.invoked_task_ids().is_empty());
    assert!(host.applied_task_ids().is_empty());
    assert!(outcome.pipeline.get("require_pilot_success").is_none());
}

fn bound_runtime(
    branch: Option<&str>,
) -> (
    tempfile::TempDir,
    OrbitRuntime,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (root, runtime, repo, global) = test_runtime();
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let runtime = OrbitRuntime::from_roots_with_binding(
        &global,
        &repo.join(".orbit"),
        crate::WorkspaceRuntimeBinding {
            logical_workspace_id: workspace_id.clone(),
            task_partition_id: workspace_id,
            owner_machine_id: None,
            repo_root: repo.clone(),
            ship_mode: orbit_types::workflow::ShipMode::Pr,
            base_branch: branch.map(ToOwned::to_owned),
        },
    )
    .expect("bound runtime");
    seed_ci_catalogs(&global);
    (root, runtime, repo, global)
}

fn submit_input(runtime: &OrbitRuntime, input: Value) -> Value {
    use crate::application::job::pipeline::worker_command_override;

    worker_command_override::set(["sh", "-c", "sleep 2"]);
    let result =
        runtime.submit_pipeline_run("ci_failure_sweep_pipeline", input, None, Some("test"));
    worker_command_override::clear();
    let submitted = result.expect("submit named CI sweep");
    runtime
        .stores()
        .jobs()
        .get_job_run(&submitted.run_id)
        .expect("read run")
        .expect("persisted run")
        .input
        .expect("persisted input")
}

#[test]
fn ci_sweep_named_submission_carries_registered_integration_to_collect_and_pilots() {
    let (_root, runtime, repo, _global) = bound_runtime(Some("agent-main"));
    let shadow = repo.join(".orbit/resources/jobs/ci_failure_sweep_pipeline.yaml");
    std::fs::create_dir_all(shadow.parent().expect("shadow parent")).expect("shadow directory");
    std::fs::write(&shadow, "invalid: [workspace YAML must never be loaded")
        .expect("write ignored shadow");

    // This is the same empty input submitted by the scheduled routine. No
    // workspace YAML or GitHub default can supply integration authority.
    let input = submit_input(&runtime, json!({}));
    assert_eq!(input["integration_branch"], "agent-main");
    let host = ScriptedCiSweepHost::new(&runtime);
    let error = try_execute_named_job(
        &runtime,
        &repo,
        &host,
        "ci_failure_sweep_pipeline",
        input,
        "jrun-ci-branch",
    )
    .expect_err("scripted stale pilot fails parent");
    let calls = host.calls.lock().expect("calls");
    let collect = calls
        .iter()
        .find(|(action, _)| action == "test_collect_ci_evidence")
        .unwrap_or_else(|| panic!("collect call missing: {error}"));
    assert_eq!(collect.1["integration_branch"], "agent-main");
    let pilots: Vec<_> = calls
        .iter()
        .filter(|(action, _)| action == "invoke_and_wait")
        .collect();
    assert_eq!(pilots.len(), 2);
    for (_, pilot) in pilots {
        assert_eq!(pilot["run_input"]["base_branch"], "agent-main");
    }
}

#[test]
fn ci_sweep_named_submission_honors_explicit_run_and_trusted_job_overrides() {
    let (_root, runtime, _repo, global) = bound_runtime(Some("agent-main"));
    let job = global.join("resources/jobs/ci_failure_sweep_pipeline.yaml");
    let yaml = std::fs::read_to_string(&job).expect("read trusted job");
    std::fs::write(
        &job,
        yaml.replace(
            "integration_branch: \"\"",
            "integration_branch: trusted-integration",
        ),
    )
    .expect("trusted override");
    assert_eq!(
        submit_input(&runtime, json!({}))["integration_branch"],
        "trusted-integration"
    );
    assert_eq!(
        submit_input(&runtime, json!({"integration_branch": "run-integration"}))["integration_branch"],
        "run-integration"
    );
    assert_eq!(
        submit_input(&runtime, json!({"base_branch": "base-override"}))["integration_branch"],
        "base-override"
    );
    assert_eq!(
        submit_input(
            &runtime,
            json!({"integration_branch": " origin/run-integration "})
        )["integration_branch"],
        "run-integration"
    );
}

#[test]
fn ci_sweep_named_submission_reports_missing_or_invalid_authority() {
    for branch in [
        None,
        Some(""),
        Some("bad..branch"),
        Some("-invalid"),
        Some("HEAD"),
        Some("@{-1}"),
    ] {
        let (_root, runtime, _repo, _global) = bound_runtime(branch);
        let error = runtime
            .submit_pipeline_run("ci_failure_sweep_pipeline", json!({}), None, Some("test"))
            .expect_err("missing or invalid integration must refuse submission");
        assert!(error.to_string().contains("integration branch"), "{error}");
        assert!(
            runtime
                .stores()
                .jobs()
                .list_pending_or_running_job_runs("ci_failure_sweep_pipeline")
                .expect("runs")
                .is_empty()
        );
    }
    let (_root, runtime, _repo, _global) = bound_runtime(Some("agent-main"));
    for input in [
        json!({"integration_branch": 7}),
        json!({"integration_branch": "bad..branch"}),
    ] {
        let error = runtime
            .submit_pipeline_run("ci_failure_sweep_pipeline", input, None, Some("test"))
            .expect_err("invalid override must not fall back");
        assert!(error.to_string().contains("integration branch"), "{error}");
    }
}

#[test]
fn ci_sweep_named_submission_uses_explicit_config_without_registration() {
    let (_root, runtime, _repo, global) = super::test_runtime_with_workspace_config(
        "[workflow]\nbase_branch = \"configured-integration\"\n",
    );
    seed_ci_catalogs(&global);
    for input in [
        json!({}),
        json!({"integration_branch": ""}),
        json!({"integration_branch": null}),
    ] {
        assert_eq!(
            submit_input(&runtime, input)["integration_branch"],
            "configured-integration"
        );
    }
}
