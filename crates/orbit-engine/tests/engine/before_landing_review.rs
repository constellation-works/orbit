//! The shipped PR pipeline's before-landing review run as one job graph
//! [ORB-14849]: the PR opens before the reviewer starts, a reviewer fix is
//! validated and pushed under a lease on the published head, completion is
//! pinned to the head the review settled, any other outcome stops before a
//! merge, and a DIRTY rebase of the reviewed head still routes through the
//! re-review steps. The claimed leaf's before-landing steps are run the same
//! way. Action-level tests cannot see broken step wiring or template
//! references; these graphs can.

use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ReviewerInvocationRequest, RuntimeHost, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};

use super::v2_runtime::{RECOVERY, REVIEWER, build_writer_and_sinks, job_asset, workspace_root};

const RUN: &str = "before-landing-run";

/// How the scripted before-landing reviewer and its settlement end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    Approve,
    Fix,
    Reject,
    Incomplete,
    Timeout,
}

/// An approving review: the PR is open and promoted before the reviewer
/// starts, the pre-push gate does not apply, and completion merges exactly
/// the published head the review settled.
#[test]
fn an_approved_before_landing_review_lands_the_head_it_settled() {
    let host = Host::new(Outcome::Approve, false);
    let result = run(&host, "done");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    assert_eq!(
        actions(&calls),
        [
            "test_stub_worktree",
            "test_stub_commit",
            "test_stub_prepare_branch",
            "test_stub_sync_base",
            "test_stub_review_gate_admit",
            "test_stub_review_gate_settle",
            "test_stub_push",
            "test_stub_pr_open",
            "test_stub_promote_tasks",
            "test_stub_review_gate_admit",
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_pr_complete",
            "test_stub_review_gate_admit",
            "test_stub_review_gate_settle",
        ],
        "the PR opens before the before-landing reviewer starts, and lands after it settles"
    );
    let admissions = inputs_of(&calls, "test_stub_review_gate_admit");
    assert!(admissions[0].get("before_landing").is_none());
    assert_eq!(admissions[1]["before_landing"], true);

    let completion = &inputs_of(&calls, "test_stub_pr_complete")[0];
    assert_eq!(completion["landing_reviewed_head_sha"], "candidate");
    assert_eq!(
        completion["reviewed_head_sha"], "",
        "no before-PR review settled anything"
    );
    assert_eq!(completion["published_head_sha"], "candidate");
    let outcome = result.expect("checked above");
    assert_eq!(
        outcome.pipeline["complete_pr"]["merge"]["merged_head"],
        outcome.pipeline["landing_review_gate_settle"]["reviewed_head_sha"],
        "the landed head is the settle's reviewed head"
    );
}

/// A reviewer that commits a fix: that commit is revalidated against the
/// implementation head, pushed to the PR branch under a lease on the
/// published head, and is the head completion merges.
#[test]
fn a_before_landing_reviewer_fix_is_validated_pushed_under_a_lease_and_lands() {
    let host = Host::new(Outcome::Fix, false);
    let result = run(&host, "done");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    let tail = actions(&calls)[9..].to_vec();
    assert_eq!(
        tail,
        [
            "test_stub_review_gate_admit",
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_candidate_validate",
            "test_stub_git_push",
            "test_stub_pr_complete",
            "test_stub_review_gate_admit",
            "test_stub_review_gate_settle",
        ]
    );
    let validation = &inputs_of(&calls, "test_stub_candidate_validate")[0];
    assert_eq!(validation["ownership_base_sha"], "candidate");
    assert_eq!(validation["base_sha"], "base-sha");

    let push = &inputs_of(&calls, "test_stub_git_push")[0];
    assert_eq!(push["branch"], "candidate-branch");
    assert_eq!(
        push["lease_remote_sha"], "candidate",
        "the fix goes onto the published head the review settled on"
    );

    let completion = &inputs_of(&calls, "test_stub_pr_complete")[0];
    assert_eq!(completion["landing_reviewed_head_sha"], "landing-fix");
    let outcome = result.expect("checked above");
    assert_eq!(
        outcome.pipeline["complete_pr"]["merge"]["merged_head"],
        "landing-fix"
    );
}

/// Every outcome other than an approve stops in the before-landing gate:
/// nothing is merged and no fix is pushed, and the failed step is a
/// completion-stage one, whose failure handoff keeps the PR open and the
/// task in review.
#[test]
fn a_rejected_incomplete_or_timed_out_before_landing_review_never_merges() {
    for (outcome, failed_action, reason) in [
        (
            Outcome::Reject,
            "test_stub_review_gate_settle",
            "review_gate_blocked: verdict reject",
        ),
        (
            Outcome::Incomplete,
            "test_stub_review_gate_settle",
            "review_gate_blocked: verdict incomplete",
        ),
        (
            Outcome::Timeout,
            "test_stub_agent_review_repair",
            "review_timeout_incomplete",
        ),
    ] {
        let host = Host::new(outcome, false);
        let result = run(&host, "done");

        let outcome_ok = matches!(&result, Ok(run) if run.success);
        assert!(!outcome_ok, "{outcome:?}: {result:?}");
        let calls = host.calls();
        assert!(
            inputs_of(&calls, "test_stub_pr_complete").is_empty(),
            "{outcome:?} must not reach completion"
        );
        assert!(
            inputs_of(&calls, "test_stub_git_push").is_empty(),
            "{outcome:?} pushes nothing"
        );
        assert!(
            actions(&calls).contains(&"test_stub_pr_open"),
            "{outcome:?}: the PR was already open"
        );
        let (last, input) = calls.last().expect("calls");
        assert_eq!(last, failed_action, "{outcome:?}");
        assert_eq!(
            input
                .pointer("/admission/attempt_id")
                .or(input.get("attempt_id")),
            Some(&json!("rvw-landing")),
            "{outcome:?} stops in the before-landing gate"
        );
        let failure = format!("{result:?}");
        assert!(failure.contains(reason), "{outcome:?}: {failure}");
    }
}

/// A review-only run reviews the PR before handing it off, and completion
/// does not run.
#[test]
fn a_review_only_run_still_reviews_the_open_pr() {
    let host = Host::new(Outcome::Approve, false);
    let result = run(&host, "review");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    assert_eq!(inputs_of(&calls, "test_stub_agent_review_repair").len(), 1);
    assert!(inputs_of(&calls, "test_stub_pr_complete").is_empty());
}

/// A DIRTY completion after a before-landing review rebases the reviewed
/// head for a re-review instead of merging it as rebased, unreviewed
/// content; the rebased head is reviewed, republished and merged.
#[test]
fn a_dirty_rebase_after_a_before_landing_review_routes_through_re_review() {
    let host = Host::new(Outcome::Approve, true);
    let result = run(&host, "done");

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    let admissions = inputs_of(&calls, "test_stub_review_gate_admit");
    assert_eq!(admissions[2]["re_review_after"], "complete_pr");
    let reviewers = inputs_of(&calls, "test_stub_agent_review_repair");
    assert_eq!(
        reviewers
            .iter()
            .map(|input| input["attempt_id"].clone())
            .collect::<Vec<_>>(),
        [json!("rvw-landing"), json!("rvw-re-review")]
    );
    let completions = inputs_of(&calls, "test_stub_pr_complete");
    assert_eq!(completions.len(), 2);
    assert_eq!(completions[0]["landing_reviewed_head_sha"], "candidate");
    assert_eq!(completions[0]["re_review_on_conflict"], true);
    assert_eq!(completions[1]["reviewed_head_sha"], "rebased-head");
    assert_eq!(completions[1]["published_head_sha"], "rebased-head");
    let outcome = result.expect("checked above");
    assert_eq!(
        outcome.pipeline["complete_reviewed_pr"]["merge"]["merged_head"],
        "rebased-head"
    );
}

fn run(host: &Host, completion: &str) -> Result<JobOutcome, DispatchError> {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), RUN);
    execute_job_with_resume(
        &shipped_job(),
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "completion": completion,
        }),
        RUN,
        writer,
        host,
        None,
    )
}

fn actions(calls: &[(String, Value)]) -> Vec<&str> {
    calls.iter().map(|(action, _)| action.as_str()).collect()
}

fn inputs_of(calls: &[(String, Value)], action: &str) -> Vec<Value> {
    calls
        .iter()
        .filter(|(called, _)| called == action)
        .map(|(_, input)| input.clone())
        .collect()
}

/// The shipped pipeline from base synchronization through the first
/// re-review round, with external activities resolved to the test host.
fn shipped_job() -> orbit_types::workflow::JobV2 {
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml"),
    )
    .expect("read the shipped PR pipeline");
    let shipped = load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec;
    let find_step = |id: &str| {
        shipped
            .steps
            .iter()
            .find(|step| step.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("the shipped PR pipeline has `{id}`"))
    };
    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": format!("test_stub_{id}"), "config": {} },
        })
    };
    let stubs = |ids: &[&str]| ids.iter().map(|id| stub(id)).collect::<Vec<_>>();
    let mut job = job_asset(json!(stubs(&[
        "worktree",
        "commit",
        "prepare_branch",
        "sync_base"
    ])));
    for id in [
        "review_gate_admit",
        "review",
        "review_gate_settle",
        "review_validate",
    ] {
        job.steps.push(find_step(id));
    }
    job.steps
        .extend(job_asset(json!(stubs(&["push", "pr_open", "promote_tasks"]))).steps);
    for id in [
        "landing_review_gate_admit",
        "landing_review",
        "landing_review_gate_settle",
        "landing_review_validate",
        "landing_push",
        "complete_pr",
        "re_review_gate_admit",
        "re_review",
        "re_review_gate_settle",
        "re_review_validate",
        "re_push",
        "complete_reviewed_pr",
    ] {
        job.steps.push(find_step(id));
    }

    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "review_gate_admit",
        REVIEWER,
        "review_gate_settle",
        "candidate_validate",
        "git_push",
        "pr_complete",
        "pr_conflict_recovery",
        RECOVERY,
    ] {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("test_stub_{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("resolve the shipped review and completion steps");
    job
}

/// Scripts a run whose captured admission is `review.before_landing`: the
/// pre-push gate never applies, the before-landing gate does, and its
/// reviewer ends as `outcome`. With `dirty`, the first completion finds the
/// PR conflicting and rebases it for a re-review, which approves.
struct Host {
    outcome: Outcome,
    dirty: bool,
    completions: Mutex<usize>,
    last_completion: Mutex<Option<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl Host {
    fn new(outcome: Outcome, dirty: bool) -> Self {
        Self {
            outcome,
            dirty,
            completions: Mutex::default(),
            last_completion: Mutex::default(),
            calls: Mutex::default(),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("call log").clone()
    }

    fn admit(&self, input: &Value) -> Value {
        let applies = |attempt: &str| {
            json!({
                "applies": true,
                "decision": "admitted",
                "first_task_id": "T-1",
                "attempt_id": attempt,
                "lineage_key": "lineage-1",
                "manifest_artifact": "review-manifest.json",
                "report_artifact": "review-report.json",
                "reviewer": { "crew": "reviewers" },
                "timing": "before-landing",
            })
        };
        if input.get("re_review_after").is_some() {
            let rebased = self
                .last_completion
                .lock()
                .expect("last completion")
                .as_ref()
                .is_some_and(|output| output["re_review_required"] == true);
            return if rebased {
                applies("rvw-re-review")
            } else {
                json!({ "applies": false, "reason": "re_review_not_required" })
            };
        }
        if input.get("before_landing") == Some(&json!(true)) {
            return applies("rvw-landing");
        }
        json!({ "applies": false, "reason": "review_before_landing", "timing": "before-landing" })
    }

    fn settle(&self, input: &Value) -> Result<Value, DispatchError> {
        if input.pointer("/admission/applies") != Some(&json!(true)) {
            return Ok(json!({
                "gate": "not_required",
                "reviewed_head_sha": "",
                "reviewed_base_sha": "",
                "reviewer_fixed": false,
                "review_fixes": "",
                "handoff_evidence": null,
            }));
        }
        if input.pointer("/admission/attempt_id") == Some(&json!("rvw-re-review")) {
            return Ok(json!({
                "gate": "passed",
                "reviewed_head_sha": "rebased-head",
                "reviewed_base_sha": "rebased-base",
                "implementation_head_sha": "rebased-head",
                "reviewer_fixed": false,
                "review_fixes": "",
            }));
        }
        let refused = |verdict: &str| DispatchError::DeterministicActionRefused {
            action: "test_stub_review_gate_settle".to_string(),
            message: format!(
                "review_gate_blocked: verdict {verdict} (decide); 1 finding(s) recorded; the pull \
                 request stays open and unmerged until a recorded decision lands it"
            ),
        };
        match self.outcome {
            Outcome::Approve => Ok(json!({
                "gate": "passed",
                "reviewed_head_sha": "candidate",
                "reviewed_base_sha": "base-sha",
                "implementation_head_sha": "candidate",
                "reviewer_fixed": false,
                "review_fixes": "",
            })),
            Outcome::Fix => Ok(json!({
                "gate": "passed",
                "reviewed_head_sha": "landing-fix",
                "reviewed_base_sha": "base-sha",
                "implementation_head_sha": "candidate",
                "reviewer_fixed": true,
                "review_fixes": "- fixed the stub",
            })),
            Outcome::Reject => Err(refused("reject")),
            Outcome::Incomplete => Err(refused("incomplete")),
            Outcome::Timeout => unreachable!("a timed-out reviewer never reaches settlement"),
        }
    }

    /// Completion merges the head its input pins, as `pr_complete` does: the
    /// before-landing pin when one is set, else the before-PR one.
    fn complete(&self, input: &Value) -> Value {
        let mut completions = self.completions.lock().expect("completions");
        let index = *completions;
        *completions += 1;
        let output = if self.dirty && index == 0 {
            assert_eq!(input["re_review_on_conflict"], true);
            json!({
                "re_review_required": true,
                "rebased": {
                    "head": "rebased-branch",
                    "head_sha": "rebased-head",
                    "base": "main",
                    "base_ref": "origin/main",
                    "base_sha": "rebased-base",
                    "remote_sha_before": "candidate",
                    "head_sha_before": "candidate",
                    "rewritten": true,
                },
                "completed_task_ids": [],
                "skipped_task_ids": [],
            })
        } else {
            let pinned = input["landing_reviewed_head_sha"]
                .as_str()
                .filter(|sha| !sha.is_empty())
                .or_else(|| input["reviewed_head_sha"].as_str())
                .unwrap_or_default();
            json!({
                "re_review_required": false,
                "merge": { "merged": true, "merged_head": pinned },
                "completed_task_ids": ["T-1"],
                "skipped_task_ids": [],
            })
        };
        *self.last_completion.lock().expect("last completion") = Some(output.clone());
        output
    }
}

impl RuntimeHost for Host {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        let output = match action {
            "test_stub_worktree" => json!({
                "job_run_id": RUN,
                "workspace_path": "/worktrees/before-landing-run",
            }),
            "test_stub_commit" => json!({ "skipped_no_diff_expected": false }),
            "test_stub_prepare_branch" | "test_stub_sync_base" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": "candidate",
                "remote_sha_before": null,
                "head_sha_before": "candidate",
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "test_stub_review_gate_admit" => self.admit(input),
            "test_stub_agent_review_repair" => {
                if self.outcome == Outcome::Timeout {
                    return Err(DispatchError::DeterministicActionRefused {
                        action: "test_stub_agent_review_repair".to_string(),
                        message: "review_timeout_incomplete: reviewer exceeded its wall clock; \
                                  partial report retained for continuation"
                            .to_string(),
                    });
                }
                json!({ "summary": "reviewed", "verdict": "accept" })
            }
            "test_stub_review_gate_settle" => return self.settle(input),
            "test_stub_push" => json!({ "local_sha": "candidate", "remote_sha_before": null }),
            "test_stub_pr_open" => {
                json!({ "pr_number": "41", "pr_url": "https://example.invalid/41" })
            }
            "test_stub_promote_tasks" => json!({ "promoted": true }),
            "test_stub_candidate_validate" => json!({ "decision": "passed" }),
            "test_stub_git_push" => {
                let rebased = input["branch"] == "rebased-branch";
                json!({
                    "decision": "performed_fast_forward",
                    "remote_sha_before": "candidate",
                    "local_sha": if rebased { "rebased-head" } else { "landing-fix" },
                })
            }
            "test_stub_pr_complete" => self.complete(input),
            other => panic!("unexpected action `{other}`"),
        };
        Ok(output)
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("system".to_string())
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn record_reviewer_invocation(
        &self,
        _request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }
}

/// The claimed leaf reviews its open pull request after `pr_open`: the
/// reviewer's fix is validated before it is pushed under a lease on the
/// published head, `pin_validation` pins that revalidation, and the handoff
/// carries the settled verdict as before-landing evidence. Without a fix the
/// pre-publication validation is carried through unchanged.
#[test]
fn a_claimed_leaf_reviews_its_open_pr_and_hands_off_the_head_it_settled() {
    for fix in [true, false] {
        let host = LeafHost {
            fix,
            calls: Mutex::default(),
        };
        let audit_root = tempfile::tempdir().expect("audit tempdir");
        let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), RUN);
        let result = execute_job_with_resume(
            &shipped_leaf(),
            json!({
                "task_ids": ["T-1"],
                "base_branch": "main",
                "base_sync": "remote",
            }),
            RUN,
            writer,
            &host,
            None,
        );
        assert!(
            matches!(&result, Ok(outcome) if outcome.success),
            "fix={fix}: {result:?}"
        );
        let calls = host.calls.lock().expect("calls").clone();
        let actions = actions(&calls);
        let opened = actions
            .iter()
            .position(|action| *action == "test_stub_pr_open")
            .expect("pr_open");
        let reviewer = actions
            .iter()
            .position(|action| *action == "test_stub_agent_review_repair")
            .expect("the before-landing reviewer");
        assert!(
            opened < reviewer,
            "the PR opens before the review: {actions:?}"
        );

        let validations = inputs_of(&calls, "test_stub_claim_validate");
        let landing = validations
            .iter()
            .find(|input| input.get("carry").is_some())
            .expect("the before-landing revalidation");
        assert_eq!(landing["revalidate"], fix);
        assert_eq!(landing["carry"]["tested_head"], "candidate");
        let pin = validations
            .iter()
            .find(|input| input.get("prevalidated").is_some())
            .expect("pin_validation");
        let pushes = inputs_of(&calls, "test_stub_git_push");
        let handoff = &inputs_of(&calls, "test_stub_claim_handoff")[0];
        assert_eq!(handoff["review_evidence"], Value::Null);
        if fix {
            assert_eq!(pin["prevalidated"]["tested_head"], "landing-fix");
            assert_eq!(pushes.len(), 2);
            assert_eq!(pushes[1]["lease_remote_sha"], "candidate");
            assert_eq!(
                handoff["landing_review_evidence"]["reviewed_head_sha"],
                "landing-fix"
            );
        } else {
            assert_eq!(pin["prevalidated"]["tested_head"], "candidate");
            assert_eq!(pushes.len(), 1);
            assert_eq!(
                handoff["landing_review_evidence"]["reviewed_head_sha"],
                "candidate"
            );
        }
    }
}

/// Every activity the claimed leaf names, resolved to the test host.
const LEAF_ACTIVITIES: &[&str] = &[
    "worktree_setup",
    "candidate_resume",
    "agent_implement",
    "git_commit",
    "pr_prepare",
    "git_rebase",
    "review_gate_admit",
    REVIEWER,
    "review_gate_settle",
    "claim_validate",
    "git_push",
    "pr_open",
    "claim_handoff",
    "claim_candidate_carry",
    "final_recovery",
    RECOVERY,
    "pr_conflict_recovery",
];

fn shipped_leaf() -> orbit_types::workflow::JobV2 {
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_claimed_pr_pipeline.yaml"),
    )
    .expect("read the shipped claimed leaf");
    let mut job = load_job_asset(&shipped)
        .expect("the shipped claimed leaf loads")
        .spec;
    let mut catalog = V2ActivityCatalog::new();
    for &name in LEAF_ACTIVITIES {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("test_stub_{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("every leaf activity resolves to a stand-in");
    job
}

/// A claimed leaf whose claim captured `review.before_landing`; the
/// reviewer commits a fix when `fix` is set.
struct LeafHost {
    fix: bool,
    calls: Mutex<Vec<(String, Value)>>,
}

impl RuntimeHost for LeafHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        let reviewed = if self.fix { "landing-fix" } else { "candidate" };
        let output = match action {
            "test_stub_worktree_setup" => json!({
                "job_run_id": RUN,
                "workspace_path": "/worktrees/leaf",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
            }),
            "test_stub_candidate_resume" => json!({ "implement": true, "repair": null }),
            "test_stub_agent_implement" => json!({ "summary": "implemented" }),
            "test_stub_git_commit" => json!({ "skipped_no_diff_expected": false }),
            "test_stub_pr_prepare" | "test_stub_git_rebase" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": null,
                "remote_sha_before": null,
                "head_sha_before": "candidate",
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "test_stub_review_gate_admit" if input["before_landing"] == true => json!({
                "applies": true,
                "decision": "admitted",
                "first_task_id": "T-1",
                "attempt_id": "rvw-landing",
                "lineage_key": "claim-1",
                "manifest_artifact": "review-manifest.json",
                "report_artifact": "review-report.json",
                "reviewer": { "crew": "reviewers" },
                "timing": "before-landing",
            }),
            "test_stub_review_gate_admit" => {
                json!({ "applies": false, "reason": "review_before_landing" })
            }
            "test_stub_agent_review_repair" => json!({ "summary": "reviewed" }),
            "test_stub_review_gate_settle" => {
                if input.pointer("/admission/applies") == Some(&json!(true)) {
                    json!({
                        "gate": "passed",
                        "reviewed_head_sha": reviewed,
                        "reviewed_base_sha": "base-sha",
                        "implementation_head_sha": "candidate",
                        "reviewer_fixed": self.fix,
                        "review_fixes": "",
                        "handoff_evidence": { "reviewed_head_sha": reviewed },
                    })
                } else {
                    json!({
                        "gate": "not_required",
                        "reviewed_head_sha": "",
                        "reviewed_base_sha": "",
                        "reviewer_fixed": false,
                        "review_fixes": "",
                        "handoff_evidence": null,
                    })
                }
            }
            // The carry step returns its input unless asked to revalidate,
            // as `claim_validate` does.
            "test_stub_claim_validate" if input.get("carry").is_some() => {
                if input["revalidate"] == true {
                    json!({ "decision": "passed", "publication": "pending", "tested_head": "landing-fix", "candidate": null, "validation": [] })
                } else {
                    input["carry"].clone()
                }
            }
            "test_stub_claim_validate" if input.get("prevalidated").is_some() => json!({
                "decision": "passed",
                "candidate": { "commit": input["prevalidated"]["tested_head"] },
                "validation": [],
            }),
            "test_stub_claim_validate" => json!({
                "decision": "passed",
                "publication": "pending",
                "tested_head": "candidate",
                "candidate": null,
                "validation": [],
            }),
            "test_stub_git_push" => json!({
                "branch": "candidate-branch",
                "local_sha": if input.get("lease_remote_sha").is_some() { "landing-fix" } else { "candidate" },
                "remote_sha_before": input.get("lease_remote_sha").cloned().unwrap_or(Value::Null),
            }),
            "test_stub_pr_open" => json!({ "pr_number": "41" }),
            "test_stub_claim_handoff" => json!({ "handed_off": true, "merged": false }),
            other => panic!("unexpected action `{other}`"),
        };
        Ok(output)
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("system".to_string())
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn record_reviewer_invocation(
        &self,
        _request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }
}
