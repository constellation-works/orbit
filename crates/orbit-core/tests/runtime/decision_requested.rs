//! The `decision_requested` state trigger: an open decision request is
//! observed once per request (and binding), fires its notification
//! routine once, and its member settles when the request is answered or
//! superseded. Member state persisted before the variant existed still
//! loads and runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use orbit_core::application::automation::{consumer_key, evaluate_routine};
use orbit_core::application::task::{FinalRecoveryCompletion, FinalRecoveryRequest};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::members::MemberState;
use orbit_types::workflow::{JobRunState, RoutineDefinition};
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    runs: std::cell::Cell<u32>,
}

fn fixture() -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        "[decision_channel]\nenabled = true\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let status = crate::git_test_support::command()
            .args([
                "-c",
                "user.name=Orbit Test",
                "-c",
                "user.email=orbit-test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    Fixture {
        _root: root,
        global,
        repo,
        runtime,
        jobs,
        runs: std::cell::Cell::new(0),
    }
}

impl Fixture {
    fn automation_runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some("fixture-machine".into()))
    }

    fn human(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human("human:operator"))
    }

    fn task(&self) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Decide an escalated block",
                    "description": "Decision-requested fixture task.",
                    "acceptance_criteria": ["Recovered."],
                    "complexity": "low",
                    "type": "chore",
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        self.update(json!({"id": id, "plan": "1. Do it."}));
        self.update(json!({"id": id, "status": "backlog"}));
        id
    }

    fn update(&self, mut input: Value) {
        input["model"] = json!("codex");
        self.runtime.run_tool("orbit.task.update", input).unwrap();
    }

    fn escalate(&self, task: &str) {
        self.update(json!({"id": task, "status": "in_progress"}));
        self.runs.set(self.runs.get() + 1);
        let observed = self
            .runtime
            .final_recovery_revision(&self.runtime.get_task(task).unwrap())
            .unwrap();
        self.runtime
            .apply_final_recovery(
                &FinalRecoveryRequest {
                    task_id: task.to_string(),
                    run_id: format!("jrun-decision-{}", self.runs.get()),
                    observed,
                    repo_root: self.repo.clone(),
                    base_ref: "main".to_string(),
                    completion: FinalRecoveryCompletion::Review,
                    requeue_bound: Default::default(),
                },
                Some(&json!({
                    "decision": "escalate",
                    "diagnosis": "the worker crashed",
                    "human_action": "choose a scope",
                })),
            )
            .unwrap();
        assert_eq!(
            self.runtime.get_task(task).unwrap().status,
            TaskStatus::Blocked
        );
    }

    /// Raise one open decision request on `task`'s escalated episode.
    fn raise_request(&self, task: &str) -> orbit_types::task::DecisionRequest {
        let input = json!({
            "action": "propose",
            "id": task,
            "blocker": "criterion 2 contradicts the retired API",
            "options": [
                {"summary": "Drop criterion 2", "consequences": "the old API stays retired", "replacement": "- Recovered."},
                {"summary": "Keep the API", "consequences": "the retirement is reverted", "replacement": "- Recovered.\n- The API answers."},
            ],
            "recommended": 1,
            "model": "claude",
            "workspace": self.repo.to_string_lossy(),
        });
        self.runtime
            .run_tool("orbit.task.block_triage", input)
            .unwrap();
        let requests = self.runtime.get_task_decision_requests(task).unwrap();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(
            requests[0].state,
            orbit_types::task::DecisionRequestState::Open
        );
        requests[0].clone()
    }

    fn install_job(&self) {
        let jobs = self.global.join("resources/jobs");
        let activities = self.global.join("resources/activities");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(&activities).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/activities/pipeline_success_guard.yaml"),
            activities.join("pipeline_success_guard.yaml"),
        )
        .unwrap();
        std::fs::write(
            jobs.join("decision_request_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: decision_request_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  max_active_runs: 3\n  steps:\n    - id: notify\n      target: activity:pipeline_success_guard\n      default_input:\n        context: decision notification\n        result: {}\n",
        )
        .unwrap();
    }

    fn routine_state(&self) -> orbit_types::workflow::automation::AutomationState {
        let consumer = consumer_key(
            &self.automation_runtime(),
            "routine",
            "decision-request-fixture",
        )
        .unwrap();
        self.runtime
            .automation_store()
            .unwrap()
            .automation_state(&consumer)
            .unwrap()
            .unwrap()
    }

    fn members(&self) -> MemberState {
        self.routine_state().members.unwrap()
    }
}

fn decision_routine() -> RoutineDefinition {
    serde_json::from_value(json!({
        "schemaVersion": 1, "name": "decision-request-fixture", "enabled": true,
        "target": "job:decision_request_pipeline",
        "trigger": {"state": {
            "kind": "decision_requested", "owner_machine": "fixture-machine", "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
        }},
    }))
    .unwrap()
}

/// Observe `task`'s request, prove a due pass would fire it, then claim it
/// through the store's own transitions with a real run bound to the claim
/// (the same manual claim the task-pilot fixtures use, so no pipeline
/// worker is spawned). Returns the run id.
fn fire(fx: &Fixture, task: &str, now: chrono::DateTime<Utc>) -> String {
    use orbit_automation::delivery::digest;
    use orbit_store::contracts::KeyedJobRunAdmission;
    use orbit_types::workflow::automation::members::{MemberAttempt, StateTriggerKind};
    let runtime = fx.automation_runtime();
    let routine = decision_routine();
    let observed = evaluate_routine(&runtime, &routine, false, now).unwrap();
    assert_eq!(observed.reason, "debouncing", "{observed:?}");
    assert!(
        fx.members().pending.contains_key(task),
        "{:?}",
        fx.members()
    );
    let due = evaluate_routine(&runtime, &routine, true, now + Duration::minutes(3)).unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    assert_eq!(due.batch.len(), 1, "{due:?}");
    assert_eq!(due.batch[0].task_ids, vec![task.to_string()]);

    let store = fx.runtime.automation_store().unwrap();
    let state = fx.routine_state();
    let member = state.members.as_ref().unwrap().pending[task].clone();
    let id = digest(format!("attempt:{task}").as_bytes());
    let mut attempt = MemberAttempt {
        consumer: state.consumer.clone(),
        kind: StateTriggerKind::DecisionRequested,
        action_key: format!("automation:{id}:1"),
        id,
        member,
        members: Vec::new(),
        attempt: 1,
        max_attempts: 2,
        deadline: now + Duration::minutes(90),
        retry_after: now,
        action_id: None,
        exhausted: false,
    };
    attempt.members = vec![attempt.member.clone()];
    let mut claimed = state.clone();
    claimed.generation += 1;
    claimed.members.as_mut().unwrap().active = Some(attempt.clone());
    assert!(store.automation_commit(&state, &claimed, None).unwrap());
    let KeyedJobRunAdmission::Admitted(run) = fx
        .jobs
        .insert_automation_job_run(
            "decision_request_pipeline",
            json!({"state_automation": attempt}),
            &attempt.action_key,
        )
        .unwrap()
    else {
        panic!("a fresh fixture attempt must admit its own run");
    };
    attempt.action_id = Some(run.run_id.clone());
    let mut acknowledged = claimed.clone();
    acknowledged.generation += 1;
    acknowledged.members.as_mut().unwrap().active = Some(attempt);
    assert!(
        store
            .automation_commit(&claimed, &acknowledged, None)
            .unwrap()
    );
    run.run_id
}

/// Finish `run_id` with a conclusively stopped owner: a child that has
/// already exited supplies its pid, as the delivery settlement fixtures
/// do, so owner liveness reads stopped without timing guesses.
fn stop_run(fx: &Fixture, run_id: &str) {
    let mut owner = crate::git_test_support::command()
        .args(["hash-object", "--stdin"])
        .current_dir(&fx.repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    fx.jobs
        .mark_job_run_running(run_id, Utc::now(), owner.id())
        .unwrap();
    drop(owner.stdin.take());
    assert!(owner.wait().unwrap().success());
    fx.jobs
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
}

#[test]
fn an_open_request_fires_once_per_binding_and_does_not_refire() {
    let fx = fixture();
    fx.install_job();
    let task = fx.task();
    fx.escalate(&task);
    let request = fx.raise_request(&task);
    assert_eq!(
        serde_json::to_value(
            orbit_types::workflow::automation::members::StateTriggerKind::DecisionRequested
        )
        .unwrap(),
        json!("decision_requested")
    );

    let now = Utc::now();
    let run_id = fire(&fx, &task, now);
    assert_eq!(
        fx.members().active.as_ref().unwrap().member.fingerprint,
        request.id
    );

    // A later pass while the run is in flight starts no second run.
    let runtime = fx.automation_runtime();
    let pending = evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_eq!(pending.reason, "batch_pending", "{pending:?}");
    assert_eq!(
        fx.members().active.as_ref().unwrap().action_id.as_deref(),
        Some(run_id.as_str())
    );

    // The run finishing with the request still open settles the member as
    // notified at the request's fingerprint; re-observation at the same
    // fingerprint fires nothing again.
    stop_run(&fx, &run_id);
    evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(5),
    )
    .unwrap();
    let members = fx.members();
    assert!(members.active.is_none(), "{members:?}");
    assert!(members.pending.is_empty(), "{members:?}");
    assert!(members.failed.is_empty(), "{members:?}");
    assert_eq!(members.assessed[&task].resulting_fingerprint, request.id);
    let again = evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(6),
    )
    .unwrap();
    assert_ne!(again.reason, "would_fire", "{again:?}");
    assert!(
        fx.members().active.is_none(),
        "the same request never refires"
    );
}

#[test]
fn a_member_settles_when_its_request_is_superseded_before_it_fires() {
    let fx = fixture();
    fx.install_job();
    let task = fx.task();
    fx.escalate(&task);
    fx.raise_request(&task);
    let runtime = fx.automation_runtime();
    let now = Utc::now();
    evaluate_routine(&runtime, &decision_routine(), false, now).unwrap();
    assert!(fx.members().pending.contains_key(&task));

    // Leaving the block episode supersedes the request (slice 1's rule).
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "status": "backlog"}),
        )
        .unwrap();
    assert_eq!(
        fx.runtime.get_task_decision_requests(&task).unwrap()[0].state,
        orbit_types::task::DecisionRequestState::Superseded
    );
    evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    let members = fx.members();
    assert!(!members.pending.contains_key(&task), "{members:?}");
    assert!(members.active.is_none(), "a superseded request never fires");
}

#[test]
fn an_active_member_settles_when_its_request_is_answered() {
    let fx = fixture();
    fx.install_job();
    let task = fx.task();
    fx.escalate(&task);
    let request = fx.raise_request(&task);
    let now = Utc::now();
    fire(&fx, &task, now);

    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        &task,
        TaskAutomationUpdate {
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: Utc::now(),
                by: "human:operator".into(),
                event: orbit_types::task::DECISION_ANSWERED_EVENT.into(),
                note: serde_json::to_string(&orbit_types::task::DecisionResolution {
                    request_id: request.id.clone(),
                    answer_id: "1".into(),
                    answered_by: "human:operator".into(),
                    provenance: None,
                    note: None,
                    answered_at: Utc::now(),
                })
                .ok(),
                from_status: None,
                to_status: None,
                operation_id: Some(request.id.clone()),
                decision: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        fx.runtime.get_task_decision_requests(&task).unwrap()[0].state,
        orbit_types::task::DecisionRequestState::Answered
    );

    let runtime = fx.automation_runtime();
    evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let members = fx.members();
    assert!(members.active.is_none(), "{members:?}");
    assert!(members.pending.is_empty(), "{members:?}");
    assert!(members.failed.is_empty(), "{members:?}");
    assert!(
        !members.assessed.contains_key(&task),
        "an answered request is released, not marked notified: {members:?}"
    );
}

#[test]
fn an_active_member_settles_when_its_request_is_superseded() {
    let fx = fixture();
    fx.install_job();
    let task = fx.task();
    fx.escalate(&task);
    fx.raise_request(&task);
    let now = Utc::now();
    fire(&fx, &task, now);
    fx.human()
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "status": "backlog"}),
        )
        .unwrap();
    let runtime = fx.automation_runtime();
    evaluate_routine(
        &runtime,
        &decision_routine(),
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    let members = fx.members();
    assert!(members.active.is_none(), "{members:?}");
    assert!(members.pending.is_empty(), "{members:?}");
    assert!(members.failed.is_empty(), "{members:?}");
    assert!(!members.assessed.contains_key(&task), "{members:?}");
}

#[test]
fn member_state_from_before_the_variant_still_loads_and_runs() {
    let fx = fixture();
    fx.install_job();
    let runtime = fx.automation_runtime();
    let now = Utc::now();
    // Baseline the consumer so its epoch and branch match the routine.
    evaluate_routine(&runtime, &decision_routine(), false, now).unwrap();
    let state = fx.routine_state();
    let source = serde_json::to_value(&state.baseline).unwrap();
    // The pre-variant shape: pending/failed records naming only kinds and
    // fingerprints that existed before `decision_requested`.
    let old: MemberState = serde_json::from_value(json!({
        "pending": {
            "ORB-99999": {
                "key": "ORB-99999", "task_ids": ["ORB-99999"],
                "fingerprint": "old-episode", "source": source,
                "evidence": {"task_id": "ORB-99999"},
                "first_seen": "2020-01-01T00:00:00Z",
                "changed_at": "2020-01-01T00:00:00Z"
            }
        },
        "active": null,
        "failed": {
            "ORB-99999": {
                "consumer": state.consumer, "kind": "block_escalated",
                "id": "old-attempt",
                "member": {
                    "key": "ORB-99999", "task_ids": ["ORB-99999"],
                    "fingerprint": "older-episode", "source": source,
                    "evidence": {"task_id": "ORB-99999"},
                    "first_seen": "2020-01-01T00:00:00Z",
                    "changed_at": "2020-01-01T00:00:00Z"
                },
                "members": [], "attempt": 1, "max_attempts": 1,
                "deadline": "2020-01-02T00:00:00Z",
                "retry_after": "2020-01-01T00:00:00Z",
                "action_key": "automation:old-attempt:1",
                "action_id": null, "exhausted": true
            }
        },
        "assessed": {}, "withheld": {}, "scan_after": null
    }))
    .unwrap();
    assert_eq!(
        old.failed["ORB-99999"].kind,
        orbit_types::workflow::automation::members::StateTriggerKind::BlockEscalated,
        "a pre-variant failed attempt still deserializes"
    );
    let mut old = old;
    // A checkpoint may not invent failed records, so only the pending
    // pre-variant member is injected; the failed record above is loaded
    // and asserted, not rewritten.
    old.failed.clear();
    let mut next = state.clone();
    next.generation += 1;
    next.members = Some(old);
    let store = fx.runtime.automation_store().unwrap();
    assert!(store.automation_commit(&state, &next, None).unwrap());

    // The old pending member's task is gone, so evaluation runs and the
    // member retires instead of erroring or firing.
    let evaluated = evaluate_routine(&runtime, &decision_routine(), false, now).unwrap();
    assert!(evaluated.state.is_some(), "{evaluated:?}");
    assert!(
        !fx.members().pending.contains_key("ORB-99999"),
        "{:?}",
        fx.members()
    );
    assert!(fx.members().active.is_none());
}
