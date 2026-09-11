//! [ORB-12102] Gated local delivery into a checkout with no push remote.
//!
//! `task_gate_pipeline` used to pin `auto_push: true` into the child run
//! input, which defeated `task_local_pipeline`'s own `auto_push: false`
//! default. On a local-ship workspace whose checkout has no usable `origin`
//! the leaf then ran `git_push`, the push failed, and the failed step
//! dispatched the `step_failure_recovery` agent — provider time spent on a
//! step that should never have executed (run `jrun-20260911-0152-3`).
//!
//! These tests drive the real gate job and the real leaf job together: the
//! host scripts the surrounding activities but leaves the pipeline wiring,
//! the `auto_push` threading, and the `push` step's condition untouched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use orbit_engine::{DispatchError, ResolvedCliExecutor, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::{
    git_in, resolved_job, retarget_engine_actions_for_scripted_host, seed_default_catalogs,
    seed_gate_task, test_runtime, try_execute_job, v2_events,
};
use crate::OrbitRuntime;

const BASE_BRANCH: &str = "agent-main";
const CHILD_RUN_ID: &str = "jrun-local-ship-child";

/// The implementation activity is an agent loop in production. Replace the
/// seeded asset with a deterministic stub so these tests exercise the
/// delivery tail without a provider.
fn stub_agent_implement(global_root: &Path) {
    std::fs::write(
        global_root.join("resources/activities/agent_implement.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: agent_implement
spec:
  type: deterministic
  description: Test stub for the bundle implementation step.
  input_schema_json:
    type: object
  output_schema_json:
    type: object
  action: test_agent_implement
  config: {}
"#,
    )
    .expect("stub agent implementation activity");
}

/// A checkout shaped like a local-ship workspace: a real repository on the
/// delivery base branch, with no remote to publish to.
fn init_remoteless_repo(repo_root: &Path) {
    git_in(repo_root, &["init"]);
    git_in(repo_root, &["config", "user.name", "Orbit Test"]);
    git_in(
        repo_root,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    std::fs::write(repo_root.join("README.md"), "base\n").expect("write initial file");
    git_in(repo_root, &["add", "README.md"]);
    git_in(repo_root, &["commit", "-m", "initial"]);
    git_in(repo_root, &["checkout", "-b", BASE_BRANCH]);

    let remotes = Command::new("git")
        .current_dir(repo_root)
        .arg("remote")
        .output()
        .expect("git remote");
    assert!(
        String::from_utf8_lossy(&remotes.stdout).trim().is_empty(),
        "the fixture checkout must have no push remote"
    );
}

/// Executes the real `task_local_pipeline` for the gate's `dispatch_child`
/// step, with the engine's own VCS and task activities scripted the way the
/// epic pipeline tests script them. The pipeline wiring under test — the
/// gate's `auto_push` threading and the leaf's `push` condition — stays
/// untouched.
struct LocalShipHost<'a> {
    runtime: &'a OrbitRuntime,
    repo_root: PathBuf,
    calls: Mutex<Vec<(String, Value)>>,
    child_run_input: Mutex<Option<Value>>,
}

impl<'a> LocalShipHost<'a> {
    fn new(runtime: &'a OrbitRuntime, repo_root: &Path) -> Self {
        Self {
            runtime,
            repo_root: repo_root.to_path_buf(),
            calls: Mutex::new(Vec::new()),
            child_run_input: Mutex::new(None),
        }
    }

    fn record(&self, action: &str, input: &Value) {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
    }

    fn inputs_for(&self, action: &str) -> Vec<Value> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .filter(|(recorded, _)| recorded == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .map(|(recorded, _)| recorded.clone())
            .collect()
    }

    fn child_run_input(&self) -> Value {
        self.child_run_input
            .lock()
            .expect("child run input")
            .clone()
            .expect("the gate dispatched a child pipeline")
    }

    fn run_child_pipeline(&self, input: &Value) -> Result<Value, DispatchError> {
        let job_name = input["job_name"]
            .as_str()
            .expect("child job name")
            .to_string();
        let run_input = input["run_input"].clone();
        *self.child_run_input.lock().expect("child run input") = Some(run_input.clone());

        let mut job = resolved_job(self.runtime, &job_name);
        retarget_engine_actions_for_scripted_host(&mut job);
        let outcome = try_execute_job(
            self.runtime,
            &self.repo_root,
            self,
            job,
            run_input,
            CHILD_RUN_ID,
        )?;

        Ok(json!({
            "run_id": CHILD_RUN_ID,
            "status": if outcome.success { "succeeded" } else { "failed" },
        }))
    }
}

impl RuntimeHost for LocalShipHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        let recorded = action.strip_prefix("scripted_").unwrap_or(action);
        self.record(recorded, input);
        match recorded {
            "reserve_locks" => Ok(json!({
                "reserved": true,
                "reservation_id": "reservation-local-ship",
            })),
            "release_locks" => Ok(json!({ "released": true })),
            "invoke_and_wait" => self.run_child_pipeline(input),
            "worktree_setup" => Ok(json!({
                "workspace_path": self.repo_root.display().to_string(),
                "job_run_id": CHILD_RUN_ID,
                "base_ref": format!("refs/heads/{BASE_BRANCH}"),
                "base_sha": "0000000000000000000000000000000000000000",
                "head_ref": BASE_BRANCH,
            })),
            "test_agent_implement" => Ok(json!({ "summary": "fixture implementation" })),
            "git_commit" => Ok(json!({ "committed": true, "decision": "committed" })),
            "git_merge" => Ok(json!({ "merged": true, "decision": "performed" })),
            "update_task" => Ok(json!({})),
            "git_push" => Ok(json!({
                "decision": "performed_create",
                "branch": BASE_BRANCH,
            })),
            _ => <OrbitRuntime as RuntimeHost>::run_deterministic(
                self.runtime,
                action,
                config,
                input,
                tool_context,
            ),
        }
    }

    /// Every agent activity in these pipelines — including the
    /// `step_failure_recovery` hook a failed step dispatches — resolves an
    /// executor first. Refusing here keeps the fixture from launching a
    /// provider, and records that something tried.
    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        self.record("resolve_cli_executor", &Value::Null);
        Err(DispatchError::JobExecution(format!(
            "fixture refuses to launch provider '{provider}'"
        )))
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

fn started_activities(runtime: &OrbitRuntime, run_id: &str) -> Vec<String> {
    v2_events(runtime, run_id, "activity.started")
        .iter()
        .map(|row| {
            let payload: Value = serde_json::from_str(&row.payload_json).expect("payload");
            payload["activity_name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[test]
fn gated_local_ship_without_a_remote_never_pushes_or_recovers() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_agent_implement(&global_root);
    init_remoteless_repo(&repo_root);
    let task_id = seed_gate_task(&runtime, &repo_root, TaskStatus::InProgress);
    let host = LocalShipHost::new(&runtime, &repo_root);

    let outcome = try_execute_job(
        &runtime,
        &repo_root,
        &host,
        resolved_job(&runtime, "task_gate_pipeline"),
        json!({
            "task_ids": [task_id.clone()],
            "mode": "local",
            "base_branch": BASE_BRANCH,
            "base_sync": "local",
        }),
        "jrun-local-ship-gate",
    )
    .expect("a local-ship bundle must deliver on a checkout with no remote");

    assert!(outcome.success);
    assert!(
        host.inputs_for("git_push").is_empty(),
        "local delivery has nothing to publish, yet the leaf pushed: {:?}",
        host.actions()
    );
    assert!(
        host.inputs_for("resolve_cli_executor").is_empty(),
        "no step failed, so no recovery agent may be dispatched"
    );
    assert!(
        !started_activities(&runtime, CHILD_RUN_ID).contains(&"step_failure_recovery".to_string()),
        "the child run must not enter step failure recovery"
    );
    assert_eq!(
        host.child_run_input()["auto_push"],
        json!(false),
        "the leaf skipped its push because the gate threaded its own auto_push value"
    );

    let reviewed = host.inputs_for("update_task");
    assert_eq!(reviewed.len(), 1, "the bundle still finishes its delivery");
    assert_eq!(reviewed[0]["task_id"], task_id);
    assert_eq!(reviewed[0]["status"], "review");
}

/// The complement of the regression above: threading the value must keep the
/// push reachable, not disable it. `git_push` is scripted like every other
/// engine action here, so what this observes is the leaf's `push` condition,
/// not git's publication behavior.
#[test]
fn gated_local_ship_still_pushes_when_the_caller_asks_for_it() {
    let (_root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    stub_agent_implement(&global_root);
    init_remoteless_repo(&repo_root);
    let task_id = seed_gate_task(&runtime, &repo_root, TaskStatus::InProgress);
    let host = LocalShipHost::new(&runtime, &repo_root);

    let outcome = try_execute_job(
        &runtime,
        &repo_root,
        &host,
        resolved_job(&runtime, "task_gate_pipeline"),
        json!({
            "task_ids": [task_id],
            "mode": "local",
            "base_branch": BASE_BRANCH,
            "base_sync": "local",
            "auto_push": true,
        }),
        "jrun-local-ship-gate-push",
    )
    .expect("an explicit push request must still deliver");

    assert!(outcome.success);
    assert_eq!(
        host.child_run_input()["auto_push"],
        json!(true),
        "an explicit request reaches the leaf as a typed boolean"
    );
    assert_eq!(
        host.inputs_for("git_push").len(),
        1,
        "the leaf must still publish when its caller asked it to"
    );
}
