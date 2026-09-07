use std::sync::Mutex;

use orbit_engine::{DispatchError, RuntimeHost};
use orbit_tools::ToolContext;
use orbit_types::workflow::{ActivityV2Spec, JobV2StepBody};
use serde_json::{Value, json};

use super::{resolved_job, seed_default_catalogs, test_runtime, try_execute_job};

/// Git/evidence rejection is exercised at the real engine boundary. Here the
/// shipped workflow must carry its accepted checkpoint intact and dispatch no
/// new branch, push, PR or merge work under either completion policy.
#[test]
fn already_landed_pipeline_routes_verified_evidence_without_a_new_delivery() {
    for completion in ["review", "done"] {
        let (_root, runtime, repo_root, global_root) = test_runtime();
        seed_default_catalogs(&global_root);
        let mut job = resolved_job(&runtime, "task_pr_pipeline");
        job.steps.retain(|step| step.id != "implement_bundle");
        for step in &mut job.steps {
            if let JobV2StepBody::Target(target) = &mut step.body
                && let ActivityV2Spec::Deterministic(spec) = &mut target.spec
            {
                spec.action = format!("scripted_{}", spec.action);
            }
        }
        let checkpoint = json!({
            "phase": "commit", "decision": "verified_already_landed",
            "committed": false, "skipped_no_diff_expected": true,
            "base_sha": "tested-head",
            "already_landed": {"covering_commit": "landed", "run_id": "source-run"},
            "validation_provenance": [{"command": "required-check", "exit_code": 0}],
        });
        let host = RoutingHost {
            workspace: repo_root.to_string_lossy().into_owned(),
            checkpoint: checkpoint.clone(),
            calls: Mutex::new(Vec::new()),
        };
        let outcome = try_execute_job(
            &runtime,
            &repo_root,
            &host,
            job,
            json!({"task_ids": ["T1"], "completion": completion}),
            "retry-run",
        )
        .unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.pipeline["commit"], checkpoint);
        let calls = host.calls.lock().unwrap();
        let mut expected = vec![
            "worktree_setup",
            "git_commit",
            "review_gate_admit",
            "review_gate_settle",
            "pr_promote",
        ];
        if completion == "done" {
            expected.push("pr_complete");
        }
        assert_eq!(*calls, expected);
    }
}

struct RoutingHost {
    workspace: String,
    checkpoint: Value,
    calls: Mutex<Vec<String>>,
}

impl RuntimeHost for RoutingHost {
    fn has_deterministic_action(&self, _action: &str) -> bool {
        true
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        let action = action
            .strip_prefix("scripted_")
            .expect("scripted routing action");
        self.calls.lock().unwrap().push(action.to_string());
        match action {
            "worktree_setup" => Ok(json!({"workspace_path": self.workspace,
                "job_run_id": "retry-run", "base_ref": "base", "base_sha": "tested-head"})),
            "git_commit" => Ok(self.checkpoint.clone()),
            "review_gate_admit" => {
                assert_eq!(input["skipped_no_diff_expected"], true);
                Ok(json!({"applies": false}))
            }
            "review_gate_settle" => Ok(json!({"applies": false})),
            "pr_promote" | "pr_complete" => {
                assert_eq!(input["no_diff_expected"], true);
                assert_eq!(input["already_landed_checkpoint"], self.checkpoint);
                Ok(json!({"decision": "verified_already_landed"}))
            }
            other => panic!("already-landed workflow dispatched unexpected action {other}"),
        }
    }
}
