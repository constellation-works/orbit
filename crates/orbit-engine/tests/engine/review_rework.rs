#![allow(missing_docs)]

//! The shipped PR pipeline's before-PR review rework loop [ORB-13891].
//!
//! Every activity the shipped `task_pr_pipeline` names resolves to a scripted
//! deterministic stand-in, so the job graph itself — the `review_gate` loop,
//! its `when:` guards, `break_when`, the template wiring between settlement,
//! rework, the pinned rework commit and the PR steps, and the terminal
//! failure handoff — runs exactly as shipped. The gate's own decisions are
//! covered at the gate boundary in `orbit-core`; here the settlement stub
//! answers from a script.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^review_rework::/)'`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ReviewerInvocationRequest, RuntimeHost, V2AuditWriter,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};

/// A `changes_required` verdict is sent back to the implementer with its
/// findings; the rework is committed on the head the findings were raised
/// on, validated, and a fresh reviewer passes the new head. Only that head
/// is published, and under `completion: done` the task is completed.
#[test]
fn changes_required_is_reworked_and_the_reworked_head_is_reviewed_and_completed() {
    let host = ScriptedHost::new([Settlement::Rework, Settlement::Pass]);
    let result = run_shipped_pipeline(&host, "done");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    assert_eq!(
        host.actions(),
        [
            "worktree_setup",
            "review_gate_admit",
            "agent_implement",
            "git_commit",
            "pr_prepare",
            "git_rebase",
            "candidate_validate",
            "review_gate_admit",
            "agent_review_repair",
            "review_gate_settle",
            "agent_rework",
            "git_commit",
            "candidate_validate",
            "review_gate_admit",
            "agent_review_repair",
            "review_gate_settle",
            "git_push",
            "pr_open",
            "pr_promote",
            "pr_complete",
            "review_gate_admit",
            "review_gate_settle",
        ],
        "one rework cycle, then a fresh review of the reworked head, then delivery"
    );

    let rework = host.inputs("agent_rework");
    assert_eq!(rework.len(), 1);
    assert_eq!(rework[0]["task_id"], "T-1");
    assert_eq!(rework[0]["workspace_path"], WORKSPACE);
    assert_eq!(rework[0]["review"]["attempt_id"], "rvw-1");
    assert_eq!(rework[0]["review"]["head_sha"], "candidate");
    assert_eq!(rework[0]["review"]["findings"][0]["id"], "F1");
    assert_eq!(
        rework[0]["review"]["findings"][0]["summary"],
        "finding 1 on candidate"
    );

    let commits = host.inputs("git_commit");
    assert_eq!(
        commits[1]["base_sha"], "candidate",
        "the rework commit is pinned to the head the findings were raised on"
    );
    let reviewers = host.inputs("agent_review_repair");
    assert_eq!(reviewers[0]["attempt_id"], "rvw-1");
    assert_eq!(reviewers[1]["attempt_id"], "rvw-2");
    let attempts = host
        .invocations()
        .into_iter()
        .map(|invocation| invocation.attempt_id)
        .collect::<Vec<_>>();
    assert_eq!(attempts, ["rvw-1", "rvw-1", "rvw-2", "rvw-2"]);

    let pr_open = &host.inputs("pr_open")[0];
    assert_eq!(pr_open["review_gate"], "passed");
    assert_eq!(pr_open["reviewed_head_sha"], "reworked-1");
    let complete = &host.inputs("pr_complete")[0];
    assert_eq!(complete["completion"], "done");
    assert_eq!(complete["reviewed_head_sha"], "reworked-1");
    assert_eq!(complete["published_head_sha"], "reworked-1");
    assert!(host.inputs("pr_failure_handoff").is_empty());
}

/// Each `changes_required` cycle the lineage can afford reaches the
/// implementer with that cycle's own findings and head. When the gate can no
/// longer afford a rework it refuses settlement: the run fails at the review
/// gate, nothing is pushed or opened, and the failure handoff receives the
/// gate's refusal to block the task.
#[test]
fn an_exhausted_rework_budget_stops_delivery_at_the_review_gate() {
    let host = ScriptedHost::new([Settlement::Rework, Settlement::Rework, Settlement::Block]);
    let result = run_shipped_pipeline(&host, "done");

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "an exhausted rework budget must not deliver: {result:?}"
    );
    let rework = host.inputs("agent_rework");
    assert_eq!(rework.len(), 2, "one rework per affordable cycle");
    for (cycle, (head, finding)) in [("candidate", "F1"), ("reworked-1", "F2")]
        .into_iter()
        .enumerate()
    {
        assert_eq!(rework[cycle]["review"]["head_sha"], head);
        assert_eq!(rework[cycle]["review"]["findings"][0]["id"], finding);
        assert_eq!(
            rework[cycle]["review"]["attempt_id"],
            format!("rvw-{}", cycle + 1)
        );
    }
    let admissions = host.inputs("review_gate_admit");
    assert_eq!(
        admissions
            .iter()
            .filter(|input| input["preflight"] != true)
            .count(),
        3,
        "the read-only preflight does not consume a reviewer start"
    );
    assert_eq!(
        admissions
            .iter()
            .filter(|input| input["preflight"] == true)
            .count(),
        1
    );
    for step in ["git_push", "pr_open", "pr_promote", "pr_complete"] {
        assert!(host.inputs(step).is_empty(), "{step} ran after a refusal");
    }
    assert!(
        host.inputs("step_failure_recovery").is_empty(),
        "a settled refusal is a decision, not a fault to recover"
    );

    let handoff = host.inputs("pr_failure_handoff");
    assert_eq!(handoff.len(), 1);
    assert_eq!(handoff[0]["failed_step_id"], "review_gate");
    let message = handoff[0]["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains("review_rework_exhausted"),
        "the handoff carries the gate's refusal: {message}"
    );
    assert_eq!(
        handoff[0]["pipeline"]["rework_commit"]["base_sha"], "reworked-1",
        "the last granted rework was committed before the refusal"
    );
}

/// A settlement asking for rework is never mistaken for a reviewed head:
/// without a later pass, the PR step sees the gate still asking for rework
/// and no reviewed head.
#[test]
fn a_loop_that_never_passes_hands_pr_open_the_unresolved_gate() {
    let host = ScriptedHost::new(std::iter::repeat_n(Settlement::Rework, 10));
    let result = run_shipped_pipeline(&host, "review");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    assert_eq!(host.inputs("agent_rework").len(), 10);
    let pr_open = &host.inputs("pr_open")[0];
    assert_eq!(pr_open["review_gate"], "rework_required");
    assert_eq!(pr_open["reviewed_head_sha"], "");
}

const STUB_PREFIX: &str = "test_stub_";
const RUN_ID: &str = "rework-run";
const WORKSPACE: &str = "/worktrees/rework-run";

/// Every activity the shipped PR pipeline dispatches, as scripted stand-ins.
const ACTIVITIES: &[&str] = &[
    "worktree_setup",
    "agent_implement",
    "step_failure_recovery",
    "git_commit",
    "pr_prepare",
    "git_rebase",
    "pr_conflict_recovery",
    "candidate_validate",
    "review_gate_admit",
    "agent_review_repair",
    "review_gate_settle",
    "agent_rework",
    "git_push",
    "pr_open",
    "pr_promote",
    "pr_complete",
    "pr_failure_handoff",
];

fn run_shipped_pipeline(
    host: &ScriptedHost,
    completion: &str,
) -> Result<JobOutcome, DispatchError> {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let sink = Arc::new(InMemorySink::new(audit_root.path().join("blobs")));
    let writer = Arc::new(V2AuditWriter::new(RUN_ID, "rework-agent", sink));
    execute_job_with_resume(
        &shipped_pipeline(),
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "completion": completion,
        }),
        RUN_ID,
        writer,
        host,
        None,
    )
}

/// The shipped `task_pr_pipeline`, every activity resolved to a scripted
/// deterministic action named after it. The prefix keeps the engine's own
/// built-in actions of the same names out of the way.
fn shipped_pipeline() -> orbit_types::workflow::JobV2 {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml");
    let shipped = std::fs::read_to_string(root).expect("read the shipped PR pipeline");
    let mut job = load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec;
    let mut catalog = V2ActivityCatalog::new();
    for &name in ACTIVITIES {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("{STUB_PREFIX}{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("every shipped activity resolves to a stand-in");
    job
}

/// What the scripted gate settles the next admitted attempt as.
#[derive(Clone, Copy)]
enum Settlement {
    /// `changes_required` the lineage can still afford: rework requested.
    Rework,
    Pass,
    /// `changes_required` with no rework left: the gate refuses.
    Block,
}

/// Plays the shipped pipeline's activities, keeping the candidate head a
/// rework commit advances and the attempt the gate last admitted.
struct ScriptedHost {
    settlements: Mutex<VecDeque<Settlement>>,
    state: Mutex<CandidateState>,
    calls: Mutex<Vec<(String, Value)>>,
    invocations: Mutex<Vec<ReviewerInvocationRequest>>,
}

struct CandidateState {
    head: String,
    reworks: usize,
    attempts: usize,
}

impl ScriptedHost {
    fn new(settlements: impl IntoIterator<Item = Settlement>) -> Self {
        Self {
            settlements: Mutex::new(settlements.into_iter().collect()),
            state: Mutex::new(CandidateState {
                head: "candidate".to_string(),
                reworks: 0,
                attempts: 0,
            }),
            calls: Mutex::default(),
            invocations: Mutex::default(),
        }
    }

    fn actions(&self) -> Vec<String> {
        let calls = self.calls.lock().expect("call log");
        calls.iter().map(|(action, _)| action.clone()).collect()
    }

    fn inputs(&self, action: &str) -> Vec<Value> {
        let calls = self.calls.lock().expect("call log");
        calls
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

    fn invocations(&self) -> Vec<ReviewerInvocationRequest> {
        self.invocations.lock().expect("invocations").clone()
    }

    fn settle(&self, input: &Value) -> Result<Value, DispatchError> {
        if input.pointer("/admission/applies") != Some(&json!(true)) {
            return Ok(json!({
                "gate": "not_required",
                "reviewed_head_sha": "",
                "reviewed_base_sha": "",
            }));
        }
        let attempt = input["admission"]["attempt_id"].clone();
        let head = self.state.lock().expect("candidate").head.clone();
        let cycle = self.state.lock().expect("candidate").attempts;
        let next = self
            .settlements
            .lock()
            .expect("settlements")
            .pop_front()
            .expect("a scripted settlement per admitted attempt");
        match next {
            Settlement::Pass => Ok(json!({
                "gate": "passed",
                "verdict": "passed_without_repairs",
                "attempt_id": attempt,
                "reviewed_head_sha": head,
                "reviewed_base_sha": "base-sha",
            })),
            Settlement::Rework => Ok(json!({
                "gate": "rework_required",
                "verdict": "changes_required",
                "attempt_id": attempt,
                "reviewed_head_sha": "",
                "reviewed_base_sha": "",
                "rework": {
                    "attempt_id": attempt,
                    "head_sha": head,
                    "base_sha": "base-sha",
                    "findings": [{
                        "id": format!("F{cycle}"),
                        "severity": "major",
                        "disposition": "open",
                        "summary": format!("finding {cycle} on {head}"),
                        "paths": ["src/lib.rs"],
                    }],
                    "escalation": null,
                    "certificate_artifact": "review/gate-certificate.json",
                },
            })),
            Settlement::Block => Err(DispatchError::DeterministicActionRefused {
                action: "review_gate_settle".to_string(),
                message: format!(
                    "review_gate_blocked: attempt {attempt} settled changes_required; \
                     review_rework_exhausted: review_repair_cycles_exhausted"
                ),
            }),
        }
    }
}

impl RuntimeHost for ScriptedHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        let action = action.strip_prefix(STUB_PREFIX).unwrap_or(action);
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        let head = self.state.lock().expect("candidate").head.clone();
        let output = match action {
            "worktree_setup" => json!({
                "job_run_id": RUN_ID,
                "workspace_path": WORKSPACE,
                "base_ref": "origin/main",
                "base_sha": "base-sha",
            }),
            "agent_implement" => json!({ "summary": "implemented" }),
            // The rework commit carries the pin; the first commit does not.
            "git_commit" if input.get("verify_already_landed").is_none() => {
                let mut state = self.state.lock().expect("candidate");
                state.reworks += 1;
                state.head = format!("reworked-{}", state.reworks);
                json!({ "head_sha": state.head, "base_sha": input["base_sha"] })
            }
            "git_commit" => json!({ "skipped_no_diff_expected": false }),
            "pr_prepare" | "git_rebase" => json!({
                "head": "candidate-branch",
                "head_sha": head,
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": head,
                "remote_sha_before": head,
                "head_sha_before": head,
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "candidate_validate" => json!({ "passed": true }),
            "review_gate_admit" if input["preflight"] == true => {
                json!({ "applies": true, "decision": "preflight_passed" })
            }
            "review_gate_admit" if input.get("re_review_after").is_some() => {
                json!({ "applies": false, "reason": "re_review_not_required" })
            }
            "review_gate_admit" => {
                let mut state = self.state.lock().expect("candidate");
                state.attempts += 1;
                json!({
                    "applies": true,
                    "first_task_id": "T-1",
                    "attempt_id": format!("rvw-{}", state.attempts),
                    "lineage_key": "lineage-1",
                    "manifest_artifact": "review-manifest.json",
                    "report_artifact": "review-report.json",
                    "reviewer": { "crew": "reviewers" },
                })
            }
            "agent_review_repair" => {
                json!({ "summary": "reviewed", "verdict": "changes_required" })
            }
            "review_gate_settle" => return self.settle(input),
            "agent_rework" => json!({ "summary": "reworked", "addressed": ["F"] }),
            "git_push" => json!({ "local_sha": head }),
            "pr_open" => json!({ "pr_number": "41", "pr_url": "https://example.invalid/41" }),
            "pr_promote" => json!({ "promoted": true }),
            "pr_complete" => json!({
                "re_review_required": false,
                "merge": { "merged": true },
                "completed_task_ids": ["T-1"],
                "skipped_task_ids": [],
            }),
            "pr_failure_handoff" => json!({ "decision": "blocked_review_gate" }),
            other => {
                return Err(DispatchError::DeterministicActionFailed {
                    action: other.to_string(),
                    message: "not scripted".to_string(),
                });
            }
        };
        Ok(output)
    }

    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<(), OrbitError> {
        self.invocations
            .lock()
            .expect("invocations")
            .push(request.clone());
        Ok(())
    }
}
