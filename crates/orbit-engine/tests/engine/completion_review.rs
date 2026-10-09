//! The shipped PR pipeline's completion stage run as one job graph: a
//! conflicting reviewed PR is rebased, re-reviewed, republished and completed,
//! for up to two re-review rounds, with conflict recovery between them. This
//! catches broken step wiring or template references that action-level tests
//! cannot see.

use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ReviewerInvocationRequest, RuntimeHost, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::ReviewerInvocationEvent;
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use serde_json::{Value, json};

use super::v2_runtime::{RECOVERY, REVIEWER, build_writer_and_sinks, job_asset, workspace_root};

const RUN: &str = "complete-review-run";
const CONFLICT_RECOVERY: &str = "test_stub_pr_conflict_recovery";

/// The deterministic completion stub reports that the published, reviewed
/// head needs rebasing; the fake reviewer then fixes and certifies the new
/// head, owner validation reruns on the reviewer commit [ORB-13989], and the
/// pipeline republishes and completes it.
#[test]
fn shipped_completion_rebases_re_reviews_and_completes_the_new_head() {
    let host = CompletionReviewHost::new(1, &[]);
    let result = run(&host);

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
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_push",
            "test_stub_pr_open",
            "test_stub_promote_tasks",
            "test_stub_review_gate_admit",
            "test_stub_review_gate_settle",
            "test_stub_pr_complete",
            "test_stub_review_gate_admit",
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_candidate_validate",
            "test_stub_git_push",
            "test_stub_pr_complete",
            "test_stub_review_gate_admit",
            "test_stub_review_gate_settle",
        ],
        "a completion conflict must enter the shipped re-review route before the second merge, \
         and a merged PR asks for no second round"
    );

    let admissions = inputs_of(&calls, "test_stub_review_gate_admit");
    assert_eq!(admissions.len(), 4);
    assert!(admissions[0].get("re_review_after").is_none());
    assert_eq!(
        admissions[1]["before_landing"], true,
        "the before-landing gate runs and does not apply to a before-PR run"
    );
    assert_eq!(admissions[2]["re_review_after"], "complete_pr");
    assert_eq!(admissions[3]["re_review_after"], "complete_reviewed_pr");

    let reviewer_inputs = inputs_of(&calls, "test_stub_agent_review_repair");
    assert_eq!(reviewer_inputs.len(), 2);
    assert_eq!(reviewer_inputs[0]["attempt_id"], "rvw-first");
    assert_eq!(reviewer_inputs[1]["attempt_id"], "rvw-re-review");

    let revalidations = inputs_of(&calls, "test_stub_candidate_validate");
    assert_eq!(
        revalidations.len(),
        1,
        "only the re-review's reviewer commit is revalidated"
    );
    assert_eq!(revalidations[0]["base_sha"], "rebased-base");
    assert_eq!(
        revalidations[0]["ownership_base_sha"],
        "rebased-implementation"
    );

    let completion_inputs = inputs_of(&calls, "test_stub_pr_complete");
    assert_eq!(completion_inputs.len(), 2);
    assert_eq!(completion_inputs[0]["reviewed_head_sha"], "candidate");
    assert_eq!(completion_inputs[1]["reviewed_head_sha"], "rebased-head");
    assert_eq!(completion_inputs[1]["published_head_sha"], "rebased-head");
    assert!(completion_inputs[0]["previous_published_head_sha"].is_null());
    assert_eq!(
        completion_inputs[1]["previous_published_head_sha"],
        "candidate"
    );
    let outcome = result.expect("checked above");
    assert_eq!(
        outcome.pipeline["complete_reviewed_pr"]["merge"]["merged"],
        true
    );

    let invocations = host.invocations();
    assert_eq!(
        invocations.len(),
        4,
        "both reviewer runs record their bounds"
    );
    assert_eq!(invocations[0].attempt_id, "rvw-first");
    assert_eq!(invocations[2].attempt_id, "rvw-re-review");
    assert!(matches!(
        invocations[0].event,
        ReviewerInvocationEvent::Started { .. }
    ));
    assert!(matches!(
        invocations[1].event,
        ReviewerInvocationEvent::Finished { .. }
    ));
    assert!(matches!(
        invocations[2].event,
        ReviewerInvocationEvent::Started { .. }
    ));
    assert!(matches!(
        invocations[3].event,
        ReviewerInvocationEvent::Finished { .. }
    ));
}

/// Both completions stop on a real rebase conflict, the second on two commits
/// of the candidate, and conflict recovery repairs every stop. The base moved
/// again under the first re-review, so a second round reviews, republishes
/// and completes that head: delivery continues without an operator and the
/// task is never left blocked.
#[test]
fn a_repaired_conflict_after_a_re_review_re_reviews_again_and_delivers() {
    let host = CompletionReviewHost::new(2, &[1, 2]);
    let result = run(&host);

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let outcome = result.expect("checked above");
    assert_eq!(
        outcome.pipeline["complete_reviewed_pr_2"]["merge"]["merged"],
        true
    );
    assert_eq!(
        outcome.pipeline["complete_reviewed_pr_2"]["completed_task_ids"],
        json!(["T-1"])
    );

    let calls = host.calls();
    let recoveries = inputs_of(&calls, CONFLICT_RECOVERY);
    let recovered_steps = recoveries
        .iter()
        .map(|input| input["failed_step_id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        recovered_steps,
        [
            "complete_pr",
            "complete_reviewed_pr",
            "complete_reviewed_pr"
        ],
        "each conflict stop gets its own recovery round"
    );

    let admissions = inputs_of(&calls, "test_stub_review_gate_admit");
    assert_eq!(admissions.len(), 4);
    assert_eq!(admissions[3]["re_review_after"], "complete_reviewed_pr");
    let reviewer_inputs = inputs_of(&calls, "test_stub_agent_review_repair");
    assert_eq!(
        reviewer_inputs
            .iter()
            .map(|input| input["attempt_id"].clone())
            .collect::<Vec<_>>(),
        [
            json!("rvw-first"),
            json!("rvw-re-review"),
            json!("rvw-re-review-2")
        ]
    );

    let revalidations = inputs_of(&calls, "test_stub_candidate_validate");
    assert_eq!(revalidations.len(), 2);
    assert_eq!(revalidations[1]["base_sha"], "rebased-base-2");
    assert_eq!(
        revalidations[1]["ownership_base_sha"],
        "rebased-implementation-2"
    );

    let pushes = inputs_of(&calls, "test_stub_git_push");
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[1]["branch"], "rebased-branch-2");
    assert_eq!(
        pushes[1]["expected_remote_sha"], "rebased-head",
        "the second republication leases the first one"
    );

    let completion_inputs = inputs_of(&calls, "test_stub_pr_complete");
    let last = completion_inputs.last().expect("a final completion");
    assert_eq!(last["reviewed_head_sha"], "rebased-head-2");
    assert_eq!(last["published_head_sha"], "rebased-head-2");
    assert_eq!(last["previous_published_head_sha"], "rebased-head");
    assert!(
        last.get("re_review_on_conflict").is_none(),
        "the last completion round cannot ask for another"
    );
    assert_eq!(
        completion_inputs
            .iter()
            .filter(|input| input["re_review_on_conflict"] == true)
            .count(),
        completion_inputs.len() - 1
    );
}

/// The base moving under the second re-review as well ends at the bound: the
/// last completion round refuses as `review_gate_stale` and nothing merges.
#[test]
fn a_conflict_after_the_second_re_review_stops_delivery() {
    let host = CompletionReviewHost::new(3, &[]);
    let result = run(&host);

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    assert_eq!(
        actions(&calls).last().copied(),
        Some("test_stub_pr_complete")
    );
    assert_eq!(inputs_of(&calls, "test_stub_pr_complete").len(), 3);
    assert_eq!(inputs_of(&calls, "test_stub_agent_review_repair").len(), 3);
    assert!(inputs_of(&calls, CONFLICT_RECOVERY).is_empty());
}

/// A rebase that keeps stopping on a new conflict gets a bounded number of
/// recovery rounds, then the step fails instead of looping.
#[test]
fn conflict_recovery_rounds_are_bounded_per_failure() {
    let host = CompletionReviewHost::new(2, &[0, 50]);
    let result = run(&host);

    assert!(
        !matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    let recoveries = inputs_of(&calls, CONFLICT_RECOVERY);
    assert_eq!(recoveries.len(), 4, "four rounds, then the step fails");
    assert!(
        recoveries
            .iter()
            .all(|input| input["failed_step_id"] == "complete_reviewed_pr")
    );
    assert_eq!(
        inputs_of(&calls, "test_stub_pr_complete").len(),
        1 + 1 + 4,
        "the first completion, the failure, and one retry per round"
    );
}

fn run(host: &CompletionReviewHost) -> Result<JobOutcome, DispatchError> {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), RUN);
    execute_job_with_resume(
        &shipped_completion_review_job(),
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "completion": "done",
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

/// The shipped pipeline slice that reaches completion and can enter both of
/// its re-review rounds, with all external activities resolved to the test
/// host.
fn shipped_completion_review_job() -> orbit_types::workflow::JobV2 {
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
    // Keep the graph boundary under test while replacing unrelated VCS and
    // task-store effects with deterministic stub activities.
    let prefix = stubs(&["worktree", "commit", "prepare_branch", "sync_base"]);
    let mut job = job_asset(json!(prefix));
    for id in [
        "review_gate_admit",
        "review",
        "review_gate_settle",
        "review_validate",
    ] {
        job.steps.push(find_step(id));
    }
    let middle = stubs(&["push", "pr_open", "promote_tasks"]);
    job.steps.extend(job_asset(json!(middle)).steps);
    for id in [
        "landing_review_gate_admit",
        "landing_review",
        "landing_review_gate_settle",
        "landing_review_validate",
        "landing_push",
        "complete_pr",
    ] {
        job.steps.push(find_step(id));
    }
    for id in [
        "re_review_gate_admit",
        "re_review",
        "re_review_gate_settle",
        "re_review_validate",
        "re_push",
        "complete_reviewed_pr",
        "re_review_gate_admit_2",
        "re_review_2",
        "re_review_gate_settle_2",
        "re_review_validate_2",
        "re_push_2",
        "complete_reviewed_pr_2",
    ] {
        job.steps.push(find_step(id));
    }

    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "worktree",
        "commit",
        "prepare_branch",
        "sync_base",
        "review_gate_admit",
        REVIEWER,
        "review_gate_settle",
        "candidate_validate",
        "push",
        "git_push",
        "pr_open",
        "promote_tasks",
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
        .expect("resolve the shipped completion and review steps");
    job
}

/// Scripts the completion stage. Completion `n` (in call order, counting only
/// the calls that get past their conflict stops) finds the PR conflicting and
/// rebases it for re-review while `n` is below `rebases`, and merges after
/// that. Before rebasing, completion `n` stops on `stops[n]` distinct typed
/// rebase conflicts, each repaired by a conflict recovery round.
struct CompletionReviewHost {
    rebases: usize,
    stops_left: Mutex<Vec<usize>>,
    completions: Mutex<usize>,
    last_completion: Mutex<Option<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
    invocations: Mutex<Vec<ReviewerInvocationRequest>>,
}

impl CompletionReviewHost {
    fn new(rebases: usize, stops: &[usize]) -> Self {
        Self {
            rebases,
            stops_left: Mutex::new(stops.to_vec()),
            completions: Mutex::default(),
            last_completion: Mutex::default(),
            calls: Mutex::default(),
            invocations: Mutex::default(),
        }
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("call log").clone()
    }

    fn invocations(&self) -> Vec<ReviewerInvocationRequest> {
        self.invocations.lock().expect("invocations").clone()
    }

    fn pr_complete(&self, input: &Value) -> Result<Value, DispatchError> {
        let mut completions = self.completions.lock().expect("completions");
        let index = *completions;
        if let Some(stops) = self.stops_left.lock().expect("stops").get_mut(index)
            && *stops > 0
        {
            *stops -= 1;
            return Err(DispatchError::RecoverableVcsConflict {
                operation: "git_rebase".to_string(),
                original_base_sha: "base-sha".to_string(),
                target_base_sha: format!("rebased-base{}", round_suffix(index)),
                conflicting_paths: vec!["src/lib.rs".to_string()],
                diagnostic: format!("rebase stopped; {stops} later stops remain"),
            });
        }
        *completions += 1;
        let output = if index < self.rebases {
            if input.get("re_review_on_conflict").and_then(Value::as_bool) != Some(true) {
                return Err(DispatchError::DeterministicActionRefused {
                    action: "test_stub_pr_complete".to_string(),
                    message: "review_gate_stale: pull request #41 has merge conflicts".to_string(),
                });
            }
            let suffix = round_suffix(index);
            let published = published_head(index);
            json!({
                "re_review_required": true,
                "rebased": {
                    "head": format!("rebased-branch{suffix}"),
                    "head_sha": format!("rebased-head{suffix}"),
                    "base": "main",
                    "base_ref": "origin/main",
                    "base_sha": format!("rebased-base{suffix}"),
                    "remote_sha_before": published,
                    "head_sha_before": published,
                    "rewritten": true,
                },
                "completed_task_ids": [],
                "skipped_task_ids": [],
            })
        } else {
            json!({
                "re_review_required": false,
                "merge": { "merged": true },
                "completed_task_ids": ["T-1"],
                "skipped_task_ids": [],
            })
        };
        *self.last_completion.lock().expect("last completion") = Some(output.clone());
        Ok(output)
    }

    /// Admission reads the named completion's checkpoint; here that is the
    /// most recent completion, which is the one each round follows.
    fn review_gate_admit(&self, input: &Value) -> Value {
        // A before-PR run: the gate after `pr_open` does not apply.
        if input.get("before_landing") == Some(&json!(true)) {
            return json!({ "applies": false, "reason": "reviewed_before_pr" });
        }
        let attempt_id = match input.get("re_review_after").and_then(Value::as_str) {
            None => "rvw-first",
            Some("complete_pr") => "rvw-re-review",
            Some(_) => "rvw-re-review-2",
        };
        let applies = input.get("re_review_after").is_none()
            || self
                .last_completion
                .lock()
                .expect("last completion")
                .as_ref()
                .is_some_and(|output| output["re_review_required"] == true);
        if !applies {
            return json!({ "applies": false, "reason": "re_review_not_required" });
        }
        json!({
            "applies": true,
            "decision": "admitted",
            "first_task_id": "T-1",
            "attempt_id": attempt_id,
            "lineage_key": "lineage-1",
            "manifest_artifact": "review-manifest.json",
            "report_artifact": "review-report.json",
            "reviewer": { "crew": "reviewers" },
        })
    }
}

/// Suffix of the names completion round `index` rebases to.
fn round_suffix(index: usize) -> String {
    if index == 0 {
        String::new()
    } else {
        format!("-{}", index + 1)
    }
}

/// The head published before completion round `index`.
fn published_head(index: usize) -> String {
    match index {
        0 => "candidate".to_string(),
        _ => format!("rebased-head{}", round_suffix(index - 1)),
    }
}

impl RuntimeHost for CompletionReviewHost {
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
                "workspace_path": "/worktrees/complete-review-run",
            }),
            "test_stub_commit" => json!({ "skipped_no_diff_expected": false }),
            "test_stub_prepare_branch" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": "candidate",
                "commits_behind": 0,
                "sync_required": false,
            }),
            "test_stub_sync_base" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": "candidate",
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "test_stub_review_gate_admit" => self.review_gate_admit(input),
            "test_stub_agent_review_repair" => {
                json!({ "summary": "reviewed", "verdict": "accept" })
            }
            "test_stub_review_gate_settle" => {
                if input.pointer("/admission/applies") != Some(&json!(true)) {
                    json!({
                        "gate": "not_required",
                        "reviewed_head_sha": "",
                        "reviewer_fixed": false,
                    })
                } else {
                    let attempt = input
                        .pointer("/admission/attempt_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let reviewed = |name: &str| match attempt {
                        "rvw-re-review" => format!("rebased-{name}"),
                        "rvw-re-review-2" => format!("rebased-{name}-2"),
                        _ => String::new(),
                    };
                    let re_review = attempt != "rvw-first";
                    json!({
                        "gate": "passed",
                        "reviewed_head_sha": if re_review { reviewed("head") } else { "candidate".to_string() },
                        "reviewed_base_sha": if re_review { reviewed("base") } else { "base-sha".to_string() },
                        "implementation_head_sha": if re_review { reviewed("implementation") } else { "candidate".to_string() },
                        "reviewer_fixed": re_review,
                        "review_fixes": "",
                    })
                }
            }
            "test_stub_push" => json!({ "local_sha": "candidate", "remote_sha_before": null }),
            "test_stub_git_push" => {
                let branch = input.get("branch").and_then(Value::as_str).unwrap_or("");
                json!({
                    "remote_sha_before": input.get("expected_remote_sha").cloned().unwrap_or(json!("candidate")),
                    "local_sha": branch
                        .strip_prefix("rebased-branch")
                        .map_or_else(|| "candidate".to_string(), |suffix| format!("rebased-head{suffix}")),
                })
            }
            "test_stub_pr_open" => {
                json!({ "pr_number": "41", "pr_url": "https://example.invalid/41" })
            }
            "test_stub_promote_tasks" => json!({ "promoted": true }),
            "test_stub_candidate_validate" => json!({ "decision": "passed" }),
            "test_stub_pr_complete" => return self.pr_complete(input),
            CONFLICT_RECOVERY => json!({}),
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
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        self.invocations
            .lock()
            .expect("invocations")
            .push(request.clone());
        Ok(None)
    }
}
