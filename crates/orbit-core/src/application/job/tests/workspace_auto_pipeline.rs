//! Pipeline-level coverage for `workspace_auto_pipeline`.
//!
//! These drive the shipped YAML through the real engine against a scripted
//! `RuntimeHost`, so what they pin is the wiring: how many times the loop
//! re-lists, what it dispatches, and which failures stop it. The classifier's
//! own decisions are tested where it lives, in
//! `adapter::engine_host::v2_host::tests::workspace_auto`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_engine::{DispatchError, ResolvedCliExecutor, RuntimeHost};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase, PipelineState};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::super::pipeline::workspace_auto_run_input;
use super::exec::{seed_default_catalogs, test_runtime, try_execute_named_job};

#[derive(Clone, Copy)]
enum WorkspaceAutoScenario {
    /// Two iterations, each offering fresh leaves, and no child ever finishes.
    KeepsDispatching,
    /// `invoke_detached` fails before a durable child exists.
    DispatchFailure,
    /// One iteration offering a single leaf *and* an epic root, so both
    /// detached dispatch shapes can be inspected in one run [ORB-11242].
    LeafAndEpic,
    /// The classifier itself fails.
    ClassifierFailure,
}

struct ScriptedWorkspaceAutoHost<'a> {
    runtime: &'a OrbitRuntime,
    scenario: WorkspaceAutoScenario,
    classify_calls: AtomicUsize,
    window_calls: AtomicUsize,
    calls: Mutex<Vec<(String, Value)>>,
    dispatch_state_lock: Mutex<()>,
}

impl<'a> ScriptedWorkspaceAutoHost<'a> {
    fn new(runtime: &'a OrbitRuntime, scenario: WorkspaceAutoScenario) -> Self {
        Self {
            runtime,
            scenario,
            classify_calls: AtomicUsize::new(0),
            window_calls: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            dispatch_state_lock: Mutex::new(()),
        }
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

    /// One task per dispatch, which is what the classifier now emits: the
    /// refill unit is a single leaf, so a slot reopens on its own child.
    fn classification(&self) -> Value {
        let iteration = self.classify_calls.fetch_add(1, Ordering::SeqCst);
        let task_ids: Vec<&str> = match self.scenario {
            WorkspaceAutoScenario::KeepsDispatching if iteration == 0 => {
                vec!["ORB-FIRST", "ORB-SECOND"]
            }
            WorkspaceAutoScenario::KeepsDispatching => vec!["ORB-LATER"],
            WorkspaceAutoScenario::DispatchFailure => vec!["ORB-DISPATCH-BROKEN"],
            WorkspaceAutoScenario::LeafAndEpic => vec!["ORB-LEAF"],
            WorkspaceAutoScenario::ClassifierFailure => {
                unreachable!("classifier failure returns before building a classification")
            }
        };
        json!({
            "loose_task_ids": task_ids,
            "loose_task_dispatches": task_ids
                .iter()
                .map(|task_id| json!({ "task_ids": [task_id] }))
                .collect::<Vec<_>>(),
            "has_leaves": true,
            "epic_task_id": matches!(self.scenario, WorkspaceAutoScenario::LeafAndEpic)
                .then_some("ORB-EPIC"),
            "has_epic": matches!(self.scenario, WorkspaceAutoScenario::LeafAndEpic),
            "idle": false,
            "sleep_seconds": 0,
            "pending_backlog": task_ids.len(),
            "active_leaf_runs": 0,
            "free_slots": 5,
            "active_epic_run_id": null,
            "active_epic_task_id": null,
        })
    }

    /// What the real `invoke_detached` leaves behind: a submitted, durably
    /// linked child that nobody is waiting on.
    fn record_submitted_child(&self, input: &Value, child_run_id: &str) {
        let Some(parent_run_id) = input.get("run_id").and_then(Value::as_str) else {
            return;
        };
        let _guard = self
            .dispatch_state_lock
            .lock()
            .expect("dispatch state lock");
        let Some(mut state) = self
            .runtime
            .read_run_state(parent_run_id)
            .expect("read scripted parent state")
        else {
            return;
        };
        state.record_child_dispatch(
            ChildDispatch::submitted(
                child_run_id.to_string(),
                "task_auto_pipeline".to_string(),
                "invoke_detached".to_string(),
                false,
                false,
                Utc::now(),
            )
            .with_parent_step_id(
                input
                    .get("step_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            ),
        );
        self.runtime
            .write_run_state(parent_run_id, &state)
            .expect("write scripted parent state");
    }

    fn detached_result(&self, input: &Value) -> Result<Value, DispatchError> {
        let task_id = input["run_input"]["task_ids"][0]
            .as_str()
            .or_else(|| input["run_input"]["epic_task_id"].as_str())
            .expect("scripted task id");
        if task_id == "ORB-DISPATCH-BROKEN" {
            return Err(DispatchError::DeterministicActionFailed {
                action: "invoke_detached".to_string(),
                message: "fixture dispatch failed before durable child creation".to_string(),
            });
        }
        let child_run_id = format!("jrun-scripted-{}", task_id.to_ascii_lowercase());
        self.record_submitted_child(input, &child_run_id);
        Ok(json!({
            "run_id": child_run_id,
            "job_name": input["job_name"].as_str().unwrap_or("task_auto_pipeline"),
            "queued": false,
        }))
    }
}

impl RuntimeHost for ScriptedWorkspaceAutoHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        match action {
            "resolve_workspace_ship_input" => Ok(json!({
                "mode": "pr",
                "base_branch": "agent-main",
            })),
            "drain_window" if input.get("deadline").is_none() => Ok(json!({
                "deadline": "2099-01-01T00:00:00Z",
                "expired": false,
                "remaining_seconds": 1,
            })),
            "drain_window" => {
                let reread = self.window_calls.fetch_add(1, Ordering::SeqCst);
                let expired = match self.scenario {
                    WorkspaceAutoScenario::KeepsDispatching => reread >= 1,
                    WorkspaceAutoScenario::DispatchFailure
                    | WorkspaceAutoScenario::ClassifierFailure
                    | WorkspaceAutoScenario::LeafAndEpic => true,
                };
                Ok(json!({
                    "deadline": "2099-01-01T00:00:00Z",
                    "expired": expired,
                    "remaining_seconds": usize::from(!expired),
                }))
            }
            "classify_workspace_auto_tasks"
                if matches!(self.scenario, WorkspaceAutoScenario::ClassifierFailure) =>
            {
                self.classify_calls.fetch_add(1, Ordering::SeqCst);
                Err(DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "fixture workspace classification failed".to_string(),
                })
            }
            "classify_workspace_auto_tasks" => Ok(self.classification()),
            "invoke_detached" => self.detached_result(input),
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

/// The throughput property: the drain re-lists and dispatches again while its
/// first children are still running. Nothing in this fixture ever completes a
/// child, and the second iteration still ships `ORB-LATER` — under the
/// wait-on-the-fan-out shape it could not have, because the loop would have
/// been blocked inside `ship_leaves` until every child of the first iteration
/// finished.
#[test]
fn workspace_auto_keeps_dispatching_while_earlier_leaves_are_still_running() {
    let (root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedWorkspaceAutoHost::new(&runtime, WorkspaceAutoScenario::KeepsDispatching);
    let input = json!({
        "max_tasks": 50,
        "for_seconds": 10,
        "poll_sleep_seconds": 0,
        "idle_sleep_seconds": 0,
    });
    // Keep the fixture's persisted job id unique. The engine uses the run id
    // as retry-jitter salt, and a hard-coded job id can reach that seed through
    // the run-id collision fallback.
    let fixture_job_id = root
        .path()
        .file_name()
        .expect("test tempdir has a final path component")
        .to_string_lossy()
        .into_owned();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(&fixture_job_id, 1, Utc::now(), Some(input.clone()), None)
        .expect("insert workspace auto run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        )
        .expect("write workspace auto run state");

    let outcome = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "workspace_auto_pipeline",
        input,
        &run.run_id,
    )
    .expect("detached leaf dispatch must not fail the workspace drain");

    assert!(outcome.success);
    assert_eq!(host.classify_calls.load(Ordering::SeqCst), 2);

    // [ORB-11253] The classifier's only handle on the drain whose live worker
    // ceiling it must read is the run id the engine injects into every activity
    // input. Pinned here, where the real engine renders the step, because
    // nothing in the asset itself passes it.
    for input in host.inputs_for("classify_workspace_auto_tasks") {
        assert_eq!(
            input["run_id"], run.run_id,
            "the classifier must be told which run it is admitting for: {input}"
        );
    }

    let dispatched: Vec<Value> = host
        .inputs_for("invoke_detached")
        .iter()
        .map(|input| input["run_input"]["task_ids"].clone())
        .collect();
    // The first iteration fills its two slots from parallel threads, so the
    // order in which `ORB-FIRST` and `ORB-SECOND` reach the host is whichever
    // thread wins; only the iteration boundary is a real ordering claim.
    assert_eq!(
        dispatched.len(),
        3,
        "one child per leaf, and the second iteration dispatched without waiting: {dispatched:?}"
    );
    let mut first_iteration = dispatched[..2].to_vec();
    first_iteration.sort_by_key(Value::to_string);
    assert_eq!(
        first_iteration,
        vec![json!(["ORB-FIRST"]), json!(["ORB-SECOND"])],
        "first iteration dispatched both leaves: {dispatched:?}"
    );
    assert_eq!(
        dispatched[2],
        json!(["ORB-LATER"]),
        "second iteration dispatched while the first two children were still running"
    );

    // Detached, but not lost: every child is durably linked to the parent as
    // submitted, which is the only handle this run keeps on it.
    let state = runtime
        .read_run_state(&run.run_id)
        .expect("read workspace auto run state")
        .expect("workspace auto run state exists");
    let mut linked: Vec<&str> = state
        .child_dispatches
        .iter()
        .map(|dispatch| dispatch.child_run_id.as_str())
        .collect();
    linked.sort_unstable();
    assert_eq!(
        linked,
        vec![
            "jrun-scripted-orb-first",
            "jrun-scripted-orb-later",
            "jrun-scripted-orb-second",
        ]
    );
    for dispatch in &state.child_dispatches {
        assert_eq!(dispatch.parent_step_id.as_deref(), Some("leaf_invoke"));
        assert_eq!(
            dispatch.phase,
            ChildDispatchPhase::Submitted,
            "the drain does not wait, so nothing here is terminal"
        );
    }
}

/// A dispatch that fails before a durable child exists has produced nothing to
/// re-observe, so it has to stop the drain rather than be counted as one more
/// detached leaf.
#[test]
fn workspace_auto_fails_promptly_when_leaf_dispatch_has_no_durable_child() {
    let (root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedWorkspaceAutoHost::new(&runtime, WorkspaceAutoScenario::DispatchFailure);
    let run_id = root
        .path()
        .file_name()
        .expect("test tempdir has a final path component")
        .to_string_lossy()
        .into_owned();

    let err = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "workspace_auto_pipeline",
        json!({"max_tasks": 50, "for_seconds": 0, "idle_sleep_seconds": 0}),
        &run_id,
    )
    .expect_err("pre-link dispatch failure must fail the workspace drain");

    let message = err.to_string();
    assert!(
        message.contains("fixture dispatch failed before durable child creation"),
        "{message}"
    );
    assert_eq!(host.classify_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn workspace_auto_preserves_concrete_workspace_step_failure() {
    let (root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedWorkspaceAutoHost::new(&runtime, WorkspaceAutoScenario::ClassifierFailure);
    let run_id = root
        .path()
        .file_name()
        .expect("test tempdir has a final path component")
        .to_string_lossy()
        .into_owned();

    let err = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "workspace_auto_pipeline",
        json!({"max_tasks": 50, "for_seconds": 0, "idle_sleep_seconds": 0}),
        &run_id,
    )
    .expect_err("workspace-level deterministic failure must fail the drain");

    let message = err.to_string();
    assert!(
        message.contains("fixture workspace classification failed"),
        "{message}"
    );
    assert_eq!(host.classify_calls.load(Ordering::SeqCst), 1);
    assert!(host.inputs_for("invoke_detached").is_empty());
}

/// [ORB-11242] The allowlist is only useful if it survives the hand-off: the
/// drain dispatches its leaves and its epic root *detached*, so a restriction
/// that stopped at this run's own input would leave every child unrestricted.
/// Driven through the shipped YAML so what is pinned is the forwarding the
/// job declares, not a Rust helper.
#[test]
fn workspace_auto_forwards_its_crew_allowlist_to_every_detached_child() {
    let (root, runtime, repo_root, global_root) = test_runtime();
    seed_default_catalogs(&global_root);
    let host = ScriptedWorkspaceAutoHost::new(&runtime, WorkspaceAutoScenario::LeafAndEpic);
    let input = json!({
        "max_tasks": 50,
        "for_seconds": 10,
        "poll_sleep_seconds": 0,
        "idle_sleep_seconds": 0,
        "allowed_crews": ["opus", "sonnet"],
    });
    // Keep the fixture's persisted job id unique. The engine uses the run id
    // as retry-jitter salt, and a hard-coded job id can reach that seed through
    // the run-id collision fallback.
    let fixture_job_id = root
        .path()
        .file_name()
        .expect("test tempdir has a final path component")
        .to_string_lossy()
        .into_owned();
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(&fixture_job_id, 1, Utc::now(), Some(input.clone()), None)
        .expect("insert workspace auto run");
    runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        )
        .expect("write workspace auto run state");

    let outcome = try_execute_named_job(
        &runtime,
        &repo_root,
        &host,
        "workspace_auto_pipeline",
        input,
        &run.run_id,
    )
    .expect("restricted drain must still ship");
    assert!(outcome.success);

    // The classifier needs it to decide what is admissible at all.
    let classified = host.inputs_for("classify_workspace_auto_tasks");
    assert_eq!(
        classified[0]["allowed_crews"],
        json!(["opus", "sonnet"]),
        "the classifier must see the window's restriction: {classified:?}"
    );

    // Both detached shapes carry it: the leaf job and the epic root.
    let dispatched = host.inputs_for("invoke_detached");
    let by_job: Vec<(String, Value)> = dispatched
        .iter()
        .map(|input| {
            (
                input["job_name"].as_str().unwrap_or_default().to_string(),
                input["run_input"]["allowed_crews"].clone(),
            )
        })
        .collect();
    assert_eq!(
        by_job,
        vec![
            ("task_auto_pipeline".to_string(), json!(["opus", "sonnet"])),
            ("epic_pipeline".to_string(), json!(["opus", "sonnet"])),
        ],
        "every detached child inherits the window: {dispatched:?}"
    );
}

/// [ORB-11242] The durable input the drain carries. Omission must stay
/// byte-identical to the pre-allowlist shape, because "the option was not
/// passed" and "the option was passed empty" have to mean the same thing all
/// the way down to the persisted run.
#[test]
fn workspace_auto_run_input_records_only_the_options_the_operator_passed() {
    use crate::application::workflow::CompletionPolicy;

    let bare = workspace_auto_run_input(None, None, CompletionPolicy::Review, &[])
        .expect("bare submission builds an input");
    assert_eq!(bare, json!({ "for_seconds": 0 }));

    let restricted = workspace_auto_run_input(
        Some(7200),
        Some(8),
        CompletionPolicy::Done,
        &["opus".to_string(), "sonnet".to_string()],
    )
    .expect("restricted submission builds an input");
    assert_eq!(
        restricted,
        json!({
            "for_seconds": 7200,
            "completion": "done",
            "max_active_leaf_runs": 8,
            "allowed_crews": ["opus", "sonnet"],
        })
    );

    assert!(
        workspace_auto_run_input(None, Some(0), CompletionPolicy::Review, &[]).is_err(),
        "zero concurrency is still refused before anything is submitted"
    );
}

/// The allowlist is canonicalized and validated against this host's registry
/// before a run record exists, so a typo is an immediate command error rather
/// than a live drain that quietly admits less than the operator asked for.
#[test]
fn auto_drain_crew_allowlist_is_validated_and_canonicalized_at_submission() {
    let (_root, runtime, _repo_root, _global_root) = test_runtime();

    assert!(
        runtime
            .canonical_allowed_crews(&["not-a-configured-crew".to_string()])
            .is_err(),
        "an unconfigured crew must fail the submission"
    );
    assert!(
        runtime
            .canonical_allowed_crews(&["  ".to_string()])
            .is_err(),
        "a blank crew name must fail the submission"
    );
    assert_eq!(
        runtime
            .canonical_allowed_crews(&[])
            .expect("omitting the option is valid"),
        Vec::<String>::new()
    );
}
