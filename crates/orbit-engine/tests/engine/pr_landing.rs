// Drives a `#!/bin/sh` substitute `gh` from every test.
#![cfg(unix)]
#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! PR landing, merge gating and required validation through the engine's deterministic actions, with a substitute `gh` over a real bare remote (see below).
use chrono::Utc;
use orbit_common::{
    NotFoundKind, OrbitError, PrivateVcsFailureClass, RecoverableVcsConflict,
    process::run_bounded_capped, test_env,
};
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    BaseSyncMode, BaselineHoldStatus, ClaimExecutionContext, ClaimedKnownRed, DispatchError,
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, KnownRedUnconfirmed, PrAutoFixAdmission, PrAutoFixRequest,
    RebaseRecoveryAttemptScope, ReviewLandingRequest, ReviewReleaseRequest, RuntimeHost,
    TaskActivityUpdate, TaskAutomationUpdate, baseline_hold_status, execute_deterministic_action,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution, review_gate,
    run_private_vcs_operation, verify_claimed_known_red,
};
use orbit_exec::{LoginShell, ValidationEnvPolicy, ValidationEnvironment};
use orbit_types::task::{
    ContextWideningStep, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskArtifact,
    TaskComment, TaskDecision, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
    push_external_ref_if_missing,
};
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use orbit_types::workflow::handoff::{
    HandoffDelivery, HandoffReviewDisposition, HandoffValidationLog, TaskHandoff,
};
use orbit_types::workflow::{
    AgentBlocker, BASELINE_RED_HOLD_EVENT, BUBBLEWRAP_NAMESPACE_PROBE, BaselineRedHold,
    ClaimFailureClass, DEFERRED_SANDBOX_ARTIFACT, DeferralProbe, EvidenceHostOs,
    FLAKY_VALIDATION_EVENT, FlakyValidation, JobRun, JobV2, JobV2StepBody,
    KNOWN_RED_VALIDATION_EVENT, KnownRedValidation, NamespaceProbe, PR_AUTO_FIX_ADMITTED_EVENT,
    PR_AUTO_FIX_BUDGET, PR_AUTO_FIX_PUSHED_EVENT, PR_BASE_UPDATE_RESERVED_EVENT,
    PR_INFRASTRUCTURE_RESERVED_EVENT, PS_EXEC_PROBE, PipelineState, PrAutoFixAttempt,
    PrAutoFixBlocker, PrAutoFixClaim, PrAutoFixDecision, PrAutoFixPush, PrBaseUpdateAdmission,
    PrBaseUpdateReservation, PrInfrastructureAdmission, PrInfrastructureEvidence,
    ProviderFailureClass, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewBudget,
    ReviewConsumption, ReviewGateAdmissionOutput, ReviewGateAdmitted, ReviewGateDecision,
    ReviewGateRefusal, ReviewGateReviewer, ReviewTiming, ReviewVerdict, SEATBELT_APPLY_PROBE,
    decide_pr_auto_fix, decide_pr_base_update, decide_pr_infrastructure, is_baseline_red_failure,
    is_task_blocked_by_agent, is_validation_environment_failure, pr_auto_fix_attempts,
    provider_failure_text, task_blocked_by_agent_message,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tempfile::TempDir;
mod absent_context;
pub(super) use absent_context::adopted_policy_cases;
mod auto_fix;
mod check_annotations;
mod conventions;
mod flaky;
mod infrastructure;
mod landing_transport;
#[cfg(target_os = "linux")]
mod stable_build_mount;
use super::git_fixture;
use super::v2_runtime::{build_writer_and_sinks, job_asset, workspace_root};
const CHILD_ENV: &str = "ORBIT_PR_LANDING_CHILD";
/// The parent-owned directory holding the substitute `gh` and its pointer to the forge state of the case being run.
const SANDBOX_ENV: &str = "ORBIT_PR_LANDING_SANDBOX";
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;
const BASE: &str = "agent-main";
const BRANCH: &str = "orbit/landing-candidate";
const RUN_ID: &str = "jrun-landing";
type Res<T> = Result<T, OrbitError>;
const TASK_ID: &str = "T-LANDING";
const PR_NUMBER: &str = "42";
// Merge gating
fn failure_handoff_ok(
    fx: &Fixture,
    host: &DeliveryHost,
    step: &str,
    code: &str,
    msg: &str,
) -> Value {
    action_ok(
        host,
        "pr_failure_handoff",
        &failure_input(fx, step, code, msg),
    )
}
fn assert_held_without_pr(fx: &Fixture, handoff: &Value, remote_before: &str, case: &str) {
    assert_eq!(handoff["pr_created"], false, "{case}");
    assert_eq!(handoff["candidate_preserved"], true, "{case}");
    assert_eq!(fx.forge_state("pr-head"), None, "{case}: no PR is opened");
    assert_eq!(fx.remote_tip(BRANCH), remote_before, "{case}");
}
/// A held handoff that committed what the agent left and carried it to its durable ref, leaving the task status for run finalization.
fn assert_committed_held(
    fx: &Fixture,
    host: &DeliveryHost,
    handoff: &Value,
    head_before: &str,
    remote_before: &str,
    case: &str,
) {
    assert_held_without_pr(fx, handoff, remote_before, case);
    assert_ne!(fx.head(), head_before, "{case}: the candidate is committed");
    assert_eq!(handoff["head_sha"], fx.head(), "{case}");
    assert_eq!(fx.durable_candidate(handoff), fx.head(), "{case}");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress, "{case}");
}
fn sha256_hex(bytes: &[u8]) -> String {
    orbit_common::security::release::sha256_hex(bytes)
}
fn baseline_red(message: &str) -> bool {
    is_baseline_red_failure(None, Some(message))
}
fn env_failure(message: &str) -> bool {
    is_validation_environment_failure(None, Some(message))
}
/// `old` for `old_reads` reads, then `new`.
fn scripted_heads(old: &str, new: &str, old_reads: usize) -> Vec<String> {
    let mut heads = vec![old.to_string(); old_reads];
    heads.push(new.to_string());
    heads
}
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}
fn fx_host(sandbox: &Path) -> (Fixture, DeliveryHost) {
    let fx = Fixture::new(sandbox);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    (fx, host)
}
fn open_ok(fx: &Fixture, host: &DeliveryHost) -> Value {
    action_ok(host, "pr_open", &fx.open_input(&fx.candidate, &fx.base_sha))
}
fn fx_host_requiring(sandbox: &Path, commands: &[&str]) -> (Fixture, DeliveryHost) {
    let (fx, host) = fx_host(sandbox);
    host.require_commands(commands);
    (fx, host)
}
fn validate_err(fx: &Fixture, host: &DeliveryHost) -> String {
    action_err(host, "candidate_validate", &fx.validate_input()).to_string()
}
fn validate_ok(fx: &Fixture, host: &DeliveryHost) -> Value {
    action_ok(host, "candidate_validate", &fx.validate_input())
}
fn open_review(fx: &Fixture, host: &DeliveryHost) {
    open_ok(fx, host);
    host.set_status(TASK_ID, TaskStatus::Review);
}
/// The clean-refusal invariants: no merge was requested, the remote base did not move, the PR was not merged, no landing was recorded, and the task status is unchanged.
fn assert_nothing_landed(fx: &Fixture, host: &DeliveryHost, status: TaskStatus, case: &str) {
    assert!(fx.merge_requests().is_empty(), "{case}: no merge request");
    assert_eq!(fx.remote_tip(BASE), fx.base_sha, "{case}: base unchanged");
    assert_eq!(fx.forge_state("merged"), None, "{case}: PR not merged");
    assert!(host.landings().is_empty(), "{case}: no landing recorded");
    assert_eq!(host.status(TASK_ID), status, "{case}: status unchanged");
}
fn assert_no_merge_or_landing(fx: &Fixture, host: &DeliveryHost, status: TaskStatus, case: &str) {
    assert!(fx.merge_requests().is_empty(), "{case}: no merge request");
    assert!(host.landings().is_empty(), "{case}: no landing recorded");
    assert_eq!(host.status(TASK_ID), status, "{case}: status unchanged");
}
fn squash(head: &str) -> Vec<String> {
    vec![format!("sha={head} merge_method=squash")]
}
fn assert_landed_exactly(fx: &Fixture, host: &DeliveryHost, head: &str, case: &str) {
    assert_eq!(fx.merge_requests(), squash(head), "{case}");
    assert_eq!(host.status(TASK_ID), TaskStatus::Done, "{case}");
}
/// One table row per line: expands to an array of `(name, fields...)` tuples.
macro_rules! rows_by_name {
    ($($name:expr => ($($field:expr),+));+ $(;)?) => {
        [$(($name, $($field),+)),+]
    };
}
/// A `pr_failure_handoff` input for a failure in the candidate's worktree.
fn failure_input(fx: &Fixture, step: &str, code: &str, message: &str) -> Value {
    json!({
        "failed_step_id": step,
        "error_code": code,
        "error_message": message,
        "run_id": RUN_ID,
        "job_input": {"task_ids": [TASK_ID]},
        "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo}},
    })
}
/// Regression for ORB-14315: provider metadata may lag a verified lease-push, but both the merge mutation and review landing must keep the new exact SHA.
fn previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins(sandbox: &Path) {
    // Review presence and bounded reread count are independent: retain both routes and both counts without their Cartesian product.
    for (reviewed, stale_reads) in [(true, 2), (false, 1)] {
        let (fx, host) = fx_host(sandbox);
        open_review(&fx, &host);
        let mut input = fx.republished_completion_input(&host);
        let candidate = input["published_head_sha"].as_str().unwrap().to_string();
        if !reviewed {
            input.as_object_mut().unwrap().remove("reviewed_head_sha");
        }
        let mut heads = vec![fx.candidate.as_str(); stale_reads];
        heads.push(&candidate);
        fx.script_heads(&heads, Some(&candidate));
        let before = fx.status_reads();
        let completed = action_ok(&host, "pr_complete", &input);
        assert_eq!(completed["merge"]["stale_head_observations"], stale_reads);
        assert_eq!(completed["merge"]["waited_seconds"], stale_reads * 5);
        assert_eq!(fx.status_reads() - before, stale_reads + 2);
        assert_eq!(fx.merge_requests(), squash(&candidate));
        assert_eq!(
            completed["merge"]["delivery_evidence"]["head_sha"],
            candidate
        );
        assert_eq!(host.status(TASK_ID), TaskStatus::Done);
        if reviewed {
            assert_eq!(host.landings()[0].reviewed_head_sha, candidate);
        } else {
            assert!(host.landings().is_empty());
        }
    }
}
#[test]
fn previous_published_head_metadata_lag_refuses_unverified_or_persistent_heads() {
    isolated(
        "previous_published_head_metadata_lag_refuses_unverified_or_persistent_heads",
        |sandbox| {
            for case in "persistent review_only time_cap third_head third_after_old remote_missing remote_moved_after_old review_mismatch review_only_third no_checkpoint".split_whitespace() {
                let (fx, host) = fx_host(sandbox);
                open_review(&fx, &host);
                let mut input = fx.republished_completion_input(&host);
                let candidate = input["published_head_sha"].as_str().unwrap().to_string();
                let mut heads = vec![fx.candidate.as_str()];
                let mut remote = Some(candidate.as_str());
                let (expected_reads, error_code) = match case {
                    "persistent" => {
                        input["max_wait_seconds"] = json!(240);
                        (4, "delivery_evidence_stale")
                    }
                    "review_only" => {
                        input.as_object_mut().unwrap().remove("published_head_sha");
                        input["max_wait_seconds"] = json!(240);
                        (4, "review_gate_stale")
                    }
                    "time_cap" => {
                        input["max_wait_seconds"] = json!(8);
                        (2, "delivery_evidence_stale")
                    }
                    "third_head" => {
                        heads = vec![fx.base_sha.as_str()];
                        (1, "delivery_evidence_stale")
                    }
                    "third_after_old" => {
                        heads.push(&fx.base_sha);
                        (2, "delivery_evidence_stale")
                    }
                    "remote_missing" => {
                        remote = None;
                        (1, "delivery_evidence_stale")
                    }
                    "remote_moved_after_old" => {
                        heads.push(&candidate);
                        fs::write(
                            fx.forge.join("remote-heads"),
                            format!("{candidate}\n{}\n", fx.candidate),
                        )
                        .unwrap();
                        (2, "delivery_evidence_stale")
                    }
                    "review_mismatch" => {
                        heads = vec![candidate.as_str()];
                        input["reviewed_head_sha"] = json!(fx.candidate);
                        (1, "review_gate_stale")
                    }
                    "no_checkpoint" => {
                        input
                            .as_object_mut()
                            .unwrap()
                            .remove("previous_published_head_sha");
                        (1, "delivery_evidence_stale")
                    }
                    "review_only_third" => {
                        input.as_object_mut().unwrap().remove("published_head_sha");
                        heads = vec![fx.base_sha.as_str()];
                        (1, "review_gate_stale")
                    }
                    _ => unreachable!(),
                };
                fx.script_heads(&heads, remote);
                let before = fx.status_reads();
                let error = action_err(&host, "pr_complete", &input);
                assert!(error.to_string().contains(error_code), "{case}: {error}");
                assert_eq!(fx.status_reads() - before, expected_reads, "{case}");
                assert_nothing_landed(&fx, &host, TaskStatus::Review, case);
            }
        },
    );
}
/// Regression for ORB-14663: after a verified lease-push, `refs/pull/<N>/head` may briefly name the previous head too.
fn previous_published_head_pull_ref_lag_settles_with_exact_merge_pin(sandbox: &Path) {
    // Reads that still report the previous head: (metadata, pull ref).
    for (metadata_old_reads, pull_ref_old_reads) in [(2, 2), (1, 2)] {
        let (fx, host) = fx_host(sandbox);
        open_review(&fx, &host);
        let input = fx.republished_completion_input(&host);
        let candidate = input["published_head_sha"].as_str().unwrap().to_string();
        assert_eq!(fx.remote_tip(BRANCH), candidate, "branch ref is replaced");
        let metadata = scripted_heads(&fx.candidate, &candidate, metadata_old_reads);
        let pull_ref = scripted_heads(&fx.candidate, &candidate, pull_ref_old_reads);
        fx.script_heads(
            &metadata.iter().map(String::as_str).collect::<Vec<_>>(),
            Some(&fx.candidate),
        );
        fx.write_forge("remote-heads", pull_ref.join("\n") + "\n");
        let before = fx.status_reads();
        let completed = action_ok(&host, "pr_complete", &input);
        assert_eq!(
            completed["merge"]["stale_head_observations"],
            pull_ref_old_reads
        );
        assert_eq!(completed["merge"]["waited_seconds"], pull_ref_old_reads * 5);
        assert_eq!(fx.status_reads() - before, pull_ref_old_reads + 2);
        assert_landed_exactly(&fx, &host, &candidate, "candidate");
    }
}
/// The pull-ref wait is fail-closed: anything but the known previous head with the task branch at the candidate, inside the bound, refuses and leaves the task in review without a merge request.
fn pull_ref_refusal_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        for case in "pull_ref_third pull_ref_third_after_old branch_missing branch_at_previous previous_unknown previous_empty previous_is_candidate persistent persistent_after_metadata time_cap review_mismatch ls_remote_fails"
            .split_whitespace()
            .filter(|case| selected.contains(case))
        {
            let (fx, host) = fx_host(sandbox);
            open_review(&fx, &host);
            let mut input = fx.republished_completion_input(&host);
            let candidate = input["published_head_sha"].as_str().unwrap().to_string();
            let mut metadata = vec![fx.candidate.clone()];
            let mut pull_ref = vec![fx.candidate.clone()];
            let (expected_reads, error_code) = match case {
                "pull_ref_third" => {
                    pull_ref = vec![fx.base_sha.clone()];
                    (1, "delivery_evidence_stale")
                }
                "pull_ref_third_after_old" => {
                    pull_ref.push(fx.base_sha.clone());
                    metadata.push(fx.candidate.clone());
                    (2, "delivery_evidence_stale")
                }
                "branch_missing" => {
                    fx.remote_git(&["update-ref", "-d", &format!("refs/heads/{BRANCH}")]);
                    (1, "delivery_evidence_stale")
                }
                "branch_at_previous" => {
                    fx.remote_git(&["update-ref", &format!("refs/heads/{BRANCH}"), &fx.candidate]);
                    (1, "delivery_evidence_stale")
                }
                "previous_unknown" => {
                    input
                        .as_object_mut()
                        .unwrap()
                        .remove("previous_published_head_sha");
                    (1, "delivery_evidence_stale")
                }
                "previous_empty" => {
                    input["previous_published_head_sha"] = json!("");
                    (1, "delivery_evidence_stale")
                }
                "previous_is_candidate" => {
                    input["previous_published_head_sha"] = json!(candidate);
                    (1, "delivery_evidence_stale")
                }
                "persistent" => {
                    input["max_wait_seconds"] = json!(240);
                    (4, "delivery_evidence_stale")
                }
                "persistent_after_metadata" => {
                    input["max_wait_seconds"] = json!(240);
                    metadata.push(candidate.clone());
                    (4, "delivery_evidence_stale")
                }
                "time_cap" => {
                    input["max_wait_seconds"] = json!(8);
                    (2, "delivery_evidence_stale")
                }
                "review_mismatch" => {
                    metadata = vec![candidate.clone()];
                    input["reviewed_head_sha"] = json!(fx.candidate);
                    (1, "review_gate_stale")
                }
                "ls_remote_fails" => {
                    fx.git(&["remote", "set-url", "origin", "/nonexistent/orbit-remote.git"]);
                    (1, "delivery_evidence_stale")
                }
                _ => unreachable!(),
            };
            fx.script_heads(
                &metadata.iter().map(String::as_str).collect::<Vec<_>>(),
                Some(&fx.candidate),
            );
            fx.write_forge("remote-heads", pull_ref.join("\n") + "\n");
            let before = fx.status_reads();
            let error = action_err(&host, "pr_complete", &input);
            assert!(error.to_string().contains(error_code), "{case}: {error}");
            assert_eq!(fx.status_reads() - before, expected_reads, "{case}");
            assert_nothing_landed(&fx, &host, TaskStatus::Review, case);
        }
    });
}
fn previous_published_head_metadata_lag_after_in_run_conflict_repair(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    open_review(&fx, &host);
    fx.advance_base();
    fx.script_checks(&["dirty", "success"]);
    fx.script_heads(
        &[&fx.candidate, &fx.candidate, "current"],
        Some(&fx.candidate),
    );
    fx.write_forge("remote-heads", "current\n");
    let mut input = fx.complete_ungated_input();
    input["max_wait_seconds"] = json!(60);
    let completed = action_ok(&host, "pr_complete", &input);
    let repaired = fx.head();
    assert_ne!(repaired, fx.candidate);
    assert_eq!(completed["merge"]["stale_head_observations"], 1);
    assert_eq!(
        completed["merge"]["delivery_evidence"]["head_sha"],
        repaired
    );
    assert_landed_exactly(&fx, &host, &repaired, "repaired");
}
fn pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr(sandbox: &Path) {
    for (outcome, expected_calls, reused) in rows_by_name! {
        "gateway" => (vec!["pr list", "pr create", "pr list", "pr create", "pr view"], false);
        "applied_gateway" => (vec!["pr list", "pr create", "pr list", "pr view"], true)
    } {
        let fx = Fixture::new(sandbox);
        fx.script_creates(&[outcome, "success"]);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        let opened = open_ok(&fx, &host);
        assert_eq!(opened["pr_created"], !reused, "{outcome}");
        assert_eq!(opened["pr_reused"], reused, "{outcome}");
        assert_eq!(opened["pr_number"], PR_NUMBER);
        assert_eq!(fx.forge_calls(), expected_calls, "{outcome}");
        assert_eq!(fx.forge_lines("created-prs").len(), 1, "{outcome}");
        assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
        assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
        assert!(host.comments(TASK_ID).is_empty());
    }
}
fn pr_open_retries_http_499_pr_views_and_stops_at_the_existing_attempt_ceiling(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    fx.script_pr_views(&["HTTP 499: 499 (https://api.github.com/graphql)", "success"]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let opened = open_ok(&fx, &host);
    assert_eq!(opened["pr_created"], true);
    assert_eq!(
        fx.forge_calls(),
        ["pr list", "pr create", "pr view", "pr view"]
    );
    assert_eq!(fx.pr_view_reads(), 2);
    let fx = Fixture::new(sandbox);
    fx.script_pr_views(&["HTTP 499: 499 (https://api.github.com/graphql)"]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let error = action_err(
        &host,
        "pr_open",
        &fx.open_input(&fx.candidate, &fx.base_sha),
    );
    assert!(
        error
            .to_string()
            .contains("HTTP 499: 499 (https://api.github.com/graphql)"),
        "the provider diagnostic is preserved: {error}"
    );
    assert_eq!(
        error.private_vcs_failure().map(|failure| failure.class),
        Some(PrivateVcsFailureClass::TransientObservation)
    );
    assert_eq!(
        fx.forge_calls(),
        ["pr list", "pr create"]
            .into_iter()
            .chain(["pr view"].repeat(3))
            .collect::<Vec<_>>()
    );
    assert_eq!(fx.pr_view_reads(), 3, "the existing three-attempt bound");
}
fn pr_lookup_retry_classification_preserves_refusals_and_nonmatching_tokens(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let input = json!({"pr": "42", "workspace_path": path_str(&fx.repo)});
    for message in "HTTP 400: Bad Request;HTTP 401: Unauthorized;HTTP 403: Forbidden;HTTP 404: Not Found;HTTP 409: Conflict;HTTP 410: Gone;HTTP 422: Unprocessable Entity;HTTP 429: Too Many Requests;bad credentials;permission denied;not accessible by integration;authentication required;SAML authorization required;rate limit exceeded;HTTP 418: I'm a teapot;HTTP 4990: Not a 499 status token;https://api.github.com/graphql?status=499".split(';') {
        fx.script_pr_views(&[message]);
        let error = run_private_vcs_operation("pr.status", &input)
            .unwrap_err();
        let failure = error
            .private_vcs_failure()
            .unwrap();
        assert_eq!(failure.class, PrivateVcsFailureClass::Other);
        assert_eq!(fx.pr_view_reads(), 1, "{message}: no retry");
    }
    fx.script_pr_views(&["HTTP 499: Bad credentials"]);
    let error = run_private_vcs_operation("pr.status", &input).unwrap_err();
    let failure = error.private_vcs_failure().unwrap();
    assert_eq!(failure.class, PrivateVcsFailureClass::Transient);
    assert_eq!(fx.pr_view_reads(), 3);
    fx.script_pr_views(&["invalid_json"]);
    let error = run_private_vcs_operation("pr.status", &input).unwrap_err();
    assert!(matches!(&error, OrbitError::Execution(_)));
    assert!(error.private_vcs_failure().is_none());
    assert_eq!(fx.pr_view_reads(), 1, "an Orbit refusal is never retried");
    fx.script_merges(&["http_499"]);
    let error = run_private_vcs_operation(
        "pr.merge",
        &json!({
            "pr": "42",
            "workspace_path": path_str(&fx.repo),
            "reviewed_head_sha": fx.candidate,
        }),
    )
    .unwrap_err();
    assert!(error.to_string().contains("HTTP 499"));
    assert_eq!(fx.merge_requests().len(), 1, "merge is not retried");
    fx.script_pr_close_failure("HTTP 499: 499 (https://api.github.com/graphql)");
    let error = run_private_vcs_operation(
        "pr.close",
        &json!({
            "pr": "42",
            "comment": "superseded",
            "workspace_path": path_str(&fx.repo),
        }),
    )
    .unwrap_err();
    assert!(error.to_string().contains("HTTP 499"));
    assert_eq!(
        fx.forge_calls()
            .into_iter()
            .filter(|call| call == "pr close")
            .count(),
        1
    );
}
fn pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    fx.script_creates(&["gateway"]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let error = action_err(
        &host,
        "pr_open",
        &fx.open_input(&fx.candidate, &fx.base_sha),
    );
    assert_eq!(fx.forge_calls(), ["pr list", "pr create"].repeat(3));
    assert!(fx.forge_lines("created-prs").is_empty());
    assert_create_failure(&host, &error, "HTTP 502: 502 Bad Gateway");
}
fn pr_open_permanent_create_failures_never_retry_or_look_up_again(sandbox: &Path) {
    for (outcome, diagnostic) in rows_by_name! {
        "auth" => ("Resource not accessible by integration (HTTP 403)");
        "unknown_head" => ("Head sha can't be blank");
        "no_commits" => ("No commits between agent-main and orbit/landing-candidate");
        "body_too_long" => ("Body is too long (maximum is 65536 characters)");
        "rate_limit" => ("API rate limit exceeded (HTTP 403)")
    } {
        let fx = Fixture::new(sandbox);
        fx.script_creates(&[outcome, "success"]);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        let error = action_err(
            &host,
            "pr_open",
            &fx.open_input(&fx.candidate, &fx.base_sha),
        );
        assert_eq!(fx.forge_calls(), ["pr list", "pr create"], "{outcome}");
        assert!(fx.forge_lines("created-prs").is_empty(), "{outcome}");
        assert_create_failure(&host, &error, diagnostic);
    }
}
fn assert_create_failure(host: &DeliveryHost, error: &OrbitError, diagnostic: &str) {
    let expected = format!("private automation VCS PR create failed: {diagnostic}");
    assert_eq!(error.to_string(), format!("execution failed: {expected}"));
    let comments = host.comments(TASK_ID);
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0]
            .message
            .contains("[phase=automation.vcs.pr.create]"),
        "create must preserve its failing phase: {}",
        comments[0].message
    );
    assert!(comments[0].message.contains(&expected));
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
}
fn passing_checks_merge_the_reviewed_candidate_and_record_its_delivery(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    fx.script_checks(&["pending", "success"]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let opened = open_ok(&fx, &host);
    assert_eq!(opened["pr_created"], true);
    assert_eq!(opened["pr_number"], PR_NUMBER);
    assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
    assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
    // `pr_promote` hands the bundle to review before completion runs.
    host.set_status(TASK_ID, TaskStatus::Review);
    let completed = action_ok(&host, "pr_complete", &fx.complete_input());
    let merge = &completed["merge"];
    assert_eq!(merge["merged"], true);
    assert!(merge["waited_seconds"].as_u64().unwrap() > 0);
    assert_eq!(fx.merge_requests(), squash(&fx.candidate));
    assert!(fx.status_reads() >= 3);
    let landed = fx.remote_tip(BASE);
    assert_ne!(landed, fx.base_sha, "the remote base advanced");
    assert_eq!(merge["landed_commit"], landed.as_str());
    assert_eq!(merge["managed_merge"], true);
    assert_eq!(merge["delivery_evidence"]["merge_commit"], landed.as_str());
    assert_eq!(
        merge["delivery_evidence"]["head_sha"],
        fx.candidate.as_str()
    );
    let landings = host.landings();
    assert_eq!(landings.len(), 1, "one review landing is recorded");
    let landing = &landings[0];
    assert_eq!(landing.run_id, RUN_ID);
    assert_eq!(landing.task_ids, vec![TASK_ID.to_string()]);
    assert_eq!(landing.pr_number, PR_NUMBER);
    assert_eq!(landing.base, BASE);
    assert_eq!(landing.reviewed_head_sha, fx.candidate);
    assert!(landing.managed_merge);
    assert_eq!(landing.landed_commit.as_deref(), Some(landed.as_str()));
    // The recorded landing is what Core checks coverage against: the reviewed tree, squashed onto the reviewed base.
    review_gate::fetch_landed_commit(&fx.repo, &landed).unwrap();
    let landed_revision = review_gate::revision(&fx.repo, &landed).unwrap();
    let candidate_revision = review_gate::revision(&fx.repo, &fx.candidate).unwrap();
    let base_revision = review_gate::revision(&fx.repo, &fx.base_sha).unwrap();
    assert_eq!(landed_revision.tree, candidate_revision.tree);
    let facts = review_gate::landed_candidate_facts(
        &fx.repo,
        &landed_revision,
        &fx.candidate,
        &base_revision.tree,
        1,
    )
    .unwrap();
    assert_eq!(facts.base_at_landing, base_revision);
    assert!(!facts.is_candidate_commit);
    assert_eq!((facts.parents, facts.span_commits), (1, 1));
    assert_eq!(completed["completed_task_ids"], json!([TASK_ID]));
    assert_eq!(host.status(TASK_ID), TaskStatus::Done);
    let notes = host.completion_notes();
    assert_eq!(notes.len(), 1, "one guarded review -> done transition");
    assert!(
        notes[0].contains(&landed),
        "the done transition names the merge commit that permitted it: {}",
        notes[0]
    );
}
/// [ORB-14849] A before-landing review runs on the open PR.
fn a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    // The PR opens before any review settles.
    action_ok(&host, "pr_open", &fx.open_input("", ""));
    host.set_status(TASK_ID, TaskStatus::Review);
    fx.write_repo("src/feature.txt", "reviewed and fixed\n");
    fx.git(&["commit", "-am", "review: fix the change"]);
    let fix = fx.head();
    let push = |lease: &str| {
        action(
            &host,
            "git_push",
            &json!({
                "workspace_path": fx.repo,
                "job_run_id": RUN_ID,
                "completed_task_ids": [TASK_ID],
                "branch": BRANCH,
                "lease_remote_sha": lease,
            }),
        )
    };
    let lost = push(&fix).unwrap_err();
    assert!(lost.to_string().contains("push_lease_lost"), "{lost}");
    assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    let pushed = push(&fx.candidate).unwrap();
    assert_eq!(pushed["remote_sha_before"], fx.candidate.as_str());
    assert_eq!(fx.remote_tip(BRANCH), fix);
    // The shipped pipeline's pins: no before-PR review, the before-landing one, and `push`'s published head.
    let mut input = fx.complete_input();
    input["reviewed_head_sha"] = json!("");
    input["landing_reviewed_head_sha"] = json!(fix);
    let completed = action_ok(&host, "pr_complete", &input);
    assert_eq!(completed["merge"]["merged"], true);
    assert_eq!(fx.merge_requests(), squash(&fix));
    assert_eq!(host.landings()[0].reviewed_head_sha, fix);
    assert_eq!(host.status(TASK_ID), TaskStatus::Done);
    // A head pushed after the review settled is never merged.
    let (fx, host) = fx_host(sandbox);
    action_ok(&host, "pr_open", &fx.open_input("", ""));
    host.set_status(TASK_ID, TaskStatus::Review);
    fx.commit("src/later.txt", "pushed after the review\n");
    let mut input = fx.complete_input();
    input["reviewed_head_sha"] = json!("");
    input["landing_reviewed_head_sha"] = json!(fx.candidate);
    let refused = action_err(&host, "pr_complete", &input);
    assert!(refused.to_string().contains(&fx.candidate));
    assert_no_merge_or_landing(&fx, &host, TaskStatus::Review, "unreviewed");
}
fn assert_lease_lost(fx: &Fixture, host: &DeliveryHost, pipeline: &str, started: TaskStatus) {
    let error = run_shipped_landing_push(fx, host, pipeline).unwrap_err();
    assert!(error.contains("push_lease_lost"), "{pipeline}: {error}");
    assert!(
        !error.contains("review_repair_after_merge"),
        "{pipeline}: {error}"
    );
    assert!(remote_ref(fx, &format!("refs/heads/orbit-evidence/{BRANCH}")).is_none());
    assert_eq!(host.status(TASK_ID), started);
}
fn a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease(sandbox: &Path) {
    for pipeline in ["task_pr_pipeline.yaml", "task_claimed_pr_pipeline.yaml"] {
        for method in ["squash", "merge"] {
            let fx = Fixture::new(sandbox);
            let lease = fx.candidate.clone();
            let first = commit_local(&fx, "src/repair-a.txt", "repair a\n");
            let second = commit_local(&fx, "src/repair-b.txt", "repair b\n");
            merge_remote_head(&fx, &lease, method);
            delete_remote_branch(&fx, BRANCH);
            let host = landing_host(&fx, pipeline);
            let error = run_shipped_landing_push(&fx, &host, pipeline).unwrap_err();
            assert!(error.contains("review_repair_after_merge:"));
            assert!(!error.contains("push_lease_lost"));
            assert!(error.contains(&format!("pull request #{PR_NUMBER}")));
            assert!(error.contains(&first), "{pipeline} {method}: {error}");
            assert!(error.contains(&second), "{pipeline} {method}: {error}");
            let evidence = format!("refs/heads/orbit-evidence/{BRANCH}");
            assert!(error.contains(&evidence), "{pipeline} {method}: {error}");
            assert_eq!(remote_ref(&fx, &evidence).as_deref(), Some(second.as_str()));
            assert!(remote_ref(&fx, &format!("refs/heads/{BRANCH}")).is_none());
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
            let recipes: Vec<_> = host
                .comments(TASK_ID)
                .into_iter()
                .filter(|comment| comment.message.starts_with("review_repair_after_merge:"))
                .collect();
            assert_eq!(recipes.len(), 1, "{pipeline} {method}: one recipe comment");
            let recipe = &recipes[0].message;
            assert!(recipe.contains("git fetch origin"), "{recipe}");
            assert!(recipe.contains("git cherry-pick"), "{recipe}");
            assert!(recipe.contains("open a pull request"), "{recipe}");
            assert!(!recipe.contains(TASK_ID), "{recipe}");
        }
        let fx = Fixture::new(sandbox);
        let _repair = commit_local(&fx, "src/repair-a.txt", "repair a\n");
        open_pr_files(&fx);
        advance_remote_branch(&fx, BRANCH);
        let host = landing_host(&fx, pipeline);
        let started = host.status(TASK_ID);
        assert_lease_lost(&fx, &host, pipeline, started);
        let fx = Fixture::new(sandbox);
        let lease = fx.candidate.clone();
        let _repair = commit_local(&fx, "src/repair-a.txt", "repair a\n");
        let moved = advance_remote_branch(&fx, BRANCH);
        assert_ne!(moved, lease);
        merge_remote_head(&fx, &moved, "squash");
        delete_remote_branch(&fx, BRANCH);
        let host = landing_host(&fx, pipeline);
        let started = host.status(TASK_ID);
        assert_lease_lost(&fx, &host, pipeline, started);
    }
}
fn landing_host(fx: &Fixture, pipeline: &str) -> DeliveryHost {
    let status = if pipeline.contains("claimed") {
        TaskStatus::InProgress
    } else {
        TaskStatus::Review
    };
    let host = DeliveryHost::new(&fx.repo, status);
    host.publish_pr(TASK_ID);
    host
}
fn commit_local(fx: &Fixture, file: &str, contents: &str) -> String {
    let path = fx.repo.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
    fx.git(&["add", file]);
    fx.git(&["commit", "-m", &format!("review repair {file}")]);
    fx.head()
}
fn open_pr_files(fx: &Fixture) {
    fx.write_forge("pr-head", format!("{BRANCH}\n"));
    fx.write_forge("pr-base", format!("{BASE}\n"));
}
fn advance_remote_branch(fx: &Fixture, branch: &str) -> String {
    let parent = fx.remote_parse(&format!("refs/heads/{branch}"));
    let tree = fx.remote_parse(&format!("{parent}^{{tree}}"));
    let next = commit_tree(fx, &tree, &[&parent], "unreviewed move");
    let name = format!("refs/heads/{branch}");
    fx.remote_git(&["update-ref", &name, &next, &parent]);
    next
}
/// Land `head` on the remote base the way the forge does, and remember `head` as the merged pull request's head oid.
fn merge_remote_head(fx: &Fixture, head: &str, method: &str) {
    let base_ref = format!("refs/heads/{BASE}");
    let parent = fx.remote_parse(&base_ref);
    let tree = fx.remote_parse(&format!("{head}^{{tree}}"));
    let parents: Vec<&str> = if method == "merge" {
        vec![&parent, head]
    } else {
        vec![&parent]
    };
    let landed = commit_tree(fx, &tree, &parents, "Land pull request #42");
    fx.remote_git(&["update-ref", &base_ref, &landed, &parent]);
    open_pr_files(fx);
    fx.write_forge("merged", format!("{landed}\n"));
    fx.write_forge("merged-head", format!("{head}\n"));
}
fn delete_remote_branch(fx: &Fixture, branch: &str) {
    fx.remote_git(&["update-ref", "-d", &format!("refs/heads/{branch}")]);
}
fn commit_tree(fx: &Fixture, tree: &str, parents: &[&str], message: &str) -> String {
    let git_dir = path_str(&fx.forge.join("remote.git")).to_string();
    let mut args = vec![
        "-c",
        "user.name=Forge",
        "-c",
        "user.email=forge@example.invalid",
        "--git-dir",
        &git_dir,
        "commit-tree",
        tree,
    ];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    fx.git(&args)
}
fn remote_ref(fx: &Fixture, reference: &str) -> Option<String> {
    let output = crate::git_test_support::command()
        .args([
            "--git-dir",
            "remote.git",
            "rev-parse",
            "--verify",
            "--quiet",
            reference,
        ])
        .current_dir(&fx.forge)
        .output()
        .unwrap();
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
fn asset_text(dir: &str, file: &str) -> String {
    fs::read_to_string(workspace_root().join(dir).join(file)).unwrap()
}
fn activity_catalog(names: &[&str]) -> V2ActivityCatalog {
    let mut catalog = V2ActivityCatalog::new();
    for name in names {
        let yaml = asset_text(
            "crates/orbit-core/assets/activities",
            &format!("{name}.yaml"),
        );
        let asset = load_activity_asset(&yaml).unwrap();
        catalog.insert(asset.name, asset.spec);
    }
    catalog
}
fn shipped_job(pipeline: &str) -> JobV2 {
    let text = asset_text("crates/orbit-core/assets/jobs", pipeline);
    load_job_asset(&text).unwrap().spec
}
fn stub_step(id: &str) -> Value {
    json!({"id": id, "spec": {"type": "deterministic", "action": format!("test_stub_{id}"), "config": {}}})
}
fn run_job(host: &DeliveryHost, job: &JobV2) -> Result<(), String> {
    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), RUN_ID);
    match execute_job_with_resume(
        job,
        json!({"task_ids": [TASK_ID], "base_sync": "local"}),
        RUN_ID,
        writer,
        host,
        None,
    ) {
        Ok(outcome) if outcome.success => Ok(()),
        Ok(outcome) => Err(outcome.message.unwrap_or_default()),
        Err(error) => Err(error.to_string()),
    }
}
/// The shipped pipeline's real `landing_push` step, with the stubs its templates read, and that pipeline's real failure activity.
fn run_shipped_landing_push(
    fx: &Fixture,
    host: &DeliveryHost,
    pipeline: &str,
) -> Result<(), String> {
    host.script_all(&rows_by_name! {
        "test_stub_worktree" => (json!({"job_run_id": RUN_ID, "workspace_path": fx.repo, "base_sha": fx.base_sha}));
        "test_stub_sync_base" => (json!({"head": BRANCH, "base": BASE, "base_ref": BASE, "base_sha": fx.base_sha}));
        "test_stub_push" => (json!({"branch": BRANCH, "local_sha": fx.candidate, "phase": "push", "decision": "reused_current"}));
        "test_stub_pr_open" => (json!({"pr_number": PR_NUMBER}));
        "test_stub_landing_review_gate_settle" => (json!({"reviewer_fixed": true}));
        "test_stub_implement_one" => (json!({"summary": "implemented"}))
    });
    let shipped = shipped_job(pipeline);
    let mut landing = shipped
        .steps
        .iter()
        .find(|step| step.id == "landing_push")
        .cloned()
        .unwrap_or_else(|| panic!("{pipeline} has no landing_push"));
    landing.recovery_activity = None;
    let steps: Vec<_> = "worktree sync_base push pr_open landing_review_gate_settle implement_one"
        .split_whitespace()
        .map(stub_step)
        .collect();
    let mut job = job_asset(Value::Array(steps));
    job.steps.push(landing);
    job.failure_activity = shipped.failure_activity.clone();
    let catalog = activity_catalog(&["git_push", "pr_failure_handoff", "claim_candidate_carry"]);
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    run_job(host, &job)
}
fn pr_open_preserves_provider_number_representations(sandbox: &Path) {
    for reuse in [false, true] {
        for (number, expected) in [
            (json!(42), "42"),
            (json!(-1), "-1"),
            (json!("0042"), "0042"),
            (json!(u64::MAX), "18446744073709551615"),
        ] {
            let (fx, host) = fx_host(sandbox);
            let input = fx.open_input(&fx.candidate, &fx.base_sha);
            if reuse {
                action_ok(&host, "pr_open", &input);
            }
            fx.write_forge("pr-number-json", number.to_string());
            let opened = action(&host, "pr_open", &input);
            if reuse && expected == "-1" {
                let error = opened.unwrap_err();
                assert!(
                    matches!(error, OrbitError::InvalidInput(ref message) if message.contains("invalid private automation VCS PR selector '-1'"))
                );
                continue;
            }
            let opened = opened.unwrap();
            assert_eq!(opened["pr_number"], expected);
            assert_eq!(opened["pr_reused"], reuse);
        }
    }
}
/// [ORB-16342] A draft pull request stops completion before any merge request, even when GitHub reports `BLOCKED` or `UNKNOWN` instead of `DRAFT`.
#[test]
#[allow(clippy::print_stderr)] // Name the draft or ready case in isolated child failure output.
fn a_draft_pull_request_stops_completion_before_any_merge_request() {
    isolated(
        "a_draft_pull_request_stops_completion_before_any_merge_request",
        a_draft_pull_request_stops_completion_before_any_merge_request_scenario,
    );
}
#[allow(clippy::print_stderr)]
fn a_draft_pull_request_stops_completion_before_any_merge_request_scenario(sandbox: &Path) {
    for merge_state in ["BLOCKED", "UNKNOWN"] {
        eprintln!("scenario: draft {merge_state}");
        let fx = Fixture::new(sandbox);
        fx.script_checks(&["success"]);
        fx.write_forge("draft-merge-state", merge_state);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        action(
            &host,
            "pr_open",
            &fx.open_input(&fx.candidate, &fx.base_sha),
        )
        .unwrap_or_else(|error| panic!("{merge_state}: open: {error}"));
        host.set_status(TASK_ID, TaskStatus::Review);
        let mut input = fx.complete_input();
        input["max_wait_seconds"] = json!(0);
        let error = action_err(&host, "pr_complete", &input);
        let error = error.to_string();
        assert!(error.contains("still a draft"), "{merge_state}: {error}");
        assert!(
            !error.contains("could not request"),
            "{merge_state}: {error}"
        );
        assert_eq!(fx.status_reads(), 1, "{merge_state}: first read");
        assert_nothing_landed(&fx, &host, TaskStatus::Review, merge_state);
    }
    eprintln!("scenario: ready");
    let fx = Fixture::new(sandbox);
    fx.script_checks(&["success"]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    open_review(&fx, &host);
    let completed = action_ok(&host, "pr_complete", &fx.complete_input());
    assert_eq!(completed["merge"]["merged"], true);
    assert_landed_exactly(&fx, &host, &fx.candidate, "ready");
}
fn a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review(sandbox: &Path) {
    for (case, checks, refusal) in rows_by_name! {
        "failed required check" => (&["failure"][..], "status check 'test' failed");
        "pending checks that then fail" => (&["pending", "failure"][..], "status check 'test' failed");
        "required review outstanding" => (&["review_required"][..], "REVIEW_REQUIRED")
    } {
        let fx = Fixture::new(sandbox);
        fx.script_checks(checks);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        action(
            &host,
            "pr_open",
            &fx.open_input(&fx.candidate, &fx.base_sha),
        )
        .unwrap_or_else(|error| panic!("{case}: open: {error}"));
        host.set_status(TASK_ID, TaskStatus::Review);
        let error = action_err(&host, "pr_complete", &fx.complete_input());
        assert!(error.to_string().contains(refusal), "{case}: {error}");
        assert_eq!(fx.status_reads(), checks.len(), "{case}");
        assert_nothing_landed(&fx, &host, TaskStatus::Review, case);
        assert!(host.completion_notes().is_empty(), "{case}");
    }
}
/// [ORB-16339] Completion without auto-fix reads a freshly pushed head as BLOCKED before the provider has queued any check.
fn required_checks_the_head_has_not_reported_are_awaited_before_completion(sandbox: &Path) {
    for (case, checks, policy, max_wait, reads, refusal) in rows_by_name! {
        "appears" => (&["unreported", "unreported", "success"][..], true, 30, 4, None);
        "never" => (&["unreported"][..], true, 10, 2, Some("timed out after 10s"));
        "unreadable" => (&["unreported"][..], false, 60, 5, Some("required reviews or checks are not satisfied"))
    } {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_checks(checks);
        if policy {
            fx.write_forge("gates", "test");
        }
        let mut input = fx.complete_input();
        input["max_wait_seconds"] = json!(max_wait);
        let result = action(&host, "pr_complete", &input);
        assert_eq!(fx.status_reads(), reads, "{case}: {result:?}");
        match refusal {
            None => {
                let completed = result.unwrap_or_else(|error| panic!("{case}: {error}"));
                assert_eq!(completed["merge"]["waited_seconds"], 10, "{case}");
                assert_landed_exactly(&fx, &host, &fx.candidate, case);
            }
            Some(refusal) => {
                let error = result.expect_err(case).to_string();
                assert!(error.contains(refusal), "{case}: {error}");
                if case == "never" {
                    assert!(
                        error.contains("required checks the head has not reported: test"),
                        "{case}: the stop names the missing check: {error}"
                    );
                }
                assert_nothing_landed(&fx, &host, TaskStatus::Review, case);
            }
        }
    }
}
// Batch merge of the checked head [ORB-15701]
fn batch_merge_input(fx: &Fixture) -> Value {
    json!({
        "job_run_id": RUN_ID,
        "strategy": "pr_merge",
        "base": BASE,
        "workspace_path": fx.repo,
    })
}
/// Open the candidate's pull request, promote it into review, and approve it, as the pipeline leaves a batch before `git_merge` runs.
fn approve_batch(fx: &Fixture, host: &DeliveryHost) {
    let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
    let opened = action_ok(host, "pr_open", &input);
    input["pr_number"] = opened["pr_number"].clone();
    input["pr_url"] = opened["pr_url"].clone();
    action_ok(host, "pr_promote", &input);
    locked(&host.tasks).get_mut(TASK_ID).unwrap().pr_status = Some("APPROVED".into());
}
fn a_pr_merge_batch_lands_the_checked_head_at_its_pin(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    approve_batch(&fx, &host);
    let merged = action_ok(&host, "git_merge", &batch_merge_input(&fx));
    assert_eq!(merged, json!({"merged": true, "strategy": "squash"}));
    assert_eq!(fx.merge_requests(), squash(&fx.candidate));
    assert_ne!(fx.remote_tip(BASE), fx.base_sha, "the remote base advanced");
    assert_eq!(host.status(TASK_ID), TaskStatus::Done);
}
fn a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    approve_batch(&fx, &host);
    // Lands after the freshness check, while the merge strategy resolves.
    fx.write_forge("push-before-merge", "");
    action_err(&host, "git_merge", &batch_merge_input(&fx));
    assert_ne!(fx.remote_tip(BRANCH), fx.candidate);
    assert_eq!(fx.merge_requests(), squash(&fx.candidate));
    assert_eq!(fx.remote_tip(BASE), fx.base_sha);
    assert!(fx.forge_state("merged").is_none(), "the PR stays open");
    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
}
// Synchronous merge refused by a concurrent base landing [ORB-14205]
const MERGE_CALL: &str = "api repos/{owner}/{repo}/pulls/42/merge";
const BASE_MODIFIED: &str =
    "gh: Base branch was modified. Review and try the merge again. (HTTP 405)";
/// Opens the fixture's candidate and hands it to review, returning how many `gh` calls publication made so completion's own calls can be isolated.
fn open_for_completion(fx: &Fixture, host: &DeliveryHost) -> usize {
    action_ok(host, "pr_open", &fx.open_input(&fx.candidate, &fx.base_sha));
    host.set_status(TASK_ID, TaskStatus::Review);
    fx.forge_calls().len()
}
fn complete_with_logical_clock(host: &DeliveryHost, input: &Value) -> Res<Value> {
    let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
    let now = std::rc::Rc::clone(&clock);
    let waited = std::rc::Rc::clone(&clock);
    orbit_engine::test_support::with_prerequisite_clock(
        move || {
            let current = now.get();
            now.set(current.saturating_add(1));
            current
        },
        |_, _| {},
        || {
            orbit_engine::test_support::with_vcs_retry_wait(
                move |duration| {
                    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
                    waited.set(waited.get().saturating_add(millis));
                },
                || action(host, "pr_complete", input),
            )
        },
    )
}
fn a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence(sandbox: &Path) {
    for (case, reviewed, checks) in [
        ("reviewed", true, &["success"][..]),
        ("ungated but published", false, &["success"][..]),
        (
            "pending on refresh",
            true,
            &["success", "pending", "success"][..],
        ),
    ] {
        let (fx, host) = fx_host(sandbox);
        let published = open_for_completion(&fx, &host);
        fx.script_checks(checks);
        fx.script_merges(&["base_modified"]);
        let input = if reviewed {
            fx.complete_input()
        } else {
            fx.complete_ungated_input()
        };
        let completed = complete_with_logical_clock(&host, &input)
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        let condition = format!("sha={} merge_method=squash", fx.candidate);
        assert_eq!(
            fx.merge_requests(),
            vec![condition.clone(), condition],
            "{case}"
        );
        // Every read after the refusal, up to the one that permits the retry.
        let fresh_reads = checks.len().max(2) - 1;
        let mut expected = vec!["pr view", "repo view", "api graphql", MERGE_CALL];
        for check in checks.iter().skip(1) {
            expected.push("pr view");
            if *check == "pending" {
                expected.push("api graphql");
            }
        }
        if checks.len() == 1 {
            expected.push("pr view");
        }
        expected.extend(["repo view", "api graphql", MERGE_CALL, "pr view"]);
        assert_eq!(fx.forge_calls()[published..], expected, "{case}");
        let merge = &completed["merge"];
        let landed = fx.remote_tip(BASE);
        let concurrent = fx.remote_parent(&landed);
        assert_ne!(concurrent, fx.base_sha, "{case}: the base raced ahead");
        assert_eq!(fx.remote_parent(&concurrent), fx.base_sha, "{case}");
        assert_eq!(merge["merged"], true, "{case}");
        assert_eq!(merge["landed_commit"], landed.as_str(), "{case}");
        assert_eq!(merge["managed_merge"], true, "{case}");
        assert_eq!(merge["auto_merge_requested"], false, "{case}");
        assert_eq!(merge["waited_seconds"], 5 * fresh_reads as u64, "{case}");
        assert_eq!(
            merge["delivery_evidence"]["head_sha"],
            fx.candidate.as_str(),
            "{case}"
        );
        assert_eq!(host.status(TASK_ID), TaskStatus::Done, "{case}");
        let notes = host.completion_notes();
        assert_eq!(notes.len(), 1, "{case}");
        assert!(notes[0].contains(&landed), "{case}: {}", notes[0]);
        let landings = host.landings();
        if reviewed {
            assert_eq!(landings.len(), 1, "{case}");
            assert!(landings[0].managed_merge, "{case}");
            assert_eq!(landings[0].landed_commit.as_deref(), Some(landed.as_str()));
        } else {
            assert!(landings.is_empty(), "{case}: no review to land");
        }
    }
}
fn other_merge_refusals_fail_on_the_first_request(sandbox: &Path) {
    for (outcome, diagnostic) in rows_by_name! {
        "policy" => ("Merge commits are not allowed on this repository. (HTTP 405)");
        "queue" => ("Changes must be made through the merge queue (HTTP 405)");
        "protection" => ("At least 1 approving review is required");
        "auth" => ("Resource not accessible by integration (HTTP 403)");
        "head_modified" => ("Head branch was modified. Review and try the merge again. (HTTP 409)");
        "base_modified_422" => ("Base branch was modified. Review and try the merge again. (HTTP 422)");
        "mismatched_body" => (BASE_MODIFIED);
        "malformed_body" => (BASE_MODIFIED);
        "trailing_stderr" => ("retried after a proxy reset");
        "transport_timeout" => ("Client.Timeout exceeded");
        "server_error" => ("Server Error (HTTP 502)")
    } {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_merges(&[outcome]);
        let error = action_err(&host, "pr_complete", &fx.complete_input());
        let message = error.to_string();
        assert!(
            message.contains("could not request squash merge"),
            "{outcome}: {message}"
        );
        assert!(message.contains(diagnostic), "{outcome}: {message}");
        assert_eq!(fx.merge_requests().len(), 1, "{outcome}: no retry");
        assert_eq!(fx.status_reads(), 1, "{outcome}: no refresh read");
        assert_eq!(fx.forge_state("merged"), None, "{outcome}");
        assert!(host.landings().is_empty(), "{outcome}");
        assert!(host.completion_notes().is_empty(), "{outcome}");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{outcome}");
    }
}
/// Shared by the two refresh-refusal tests: after one base-modification refusal, the fresh read in each case must refuse without a second merge request, leaving the checkout, landings and task untouched.
fn assert_refresh_refuses(sandbox: &Path, cases: &[(&str, bool, &str, &str, &str)]) {
    for &(case, reviewed, outcome, checks, refusal) in cases {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_checks(&["success", checks]);
        fx.script_merges(&[outcome]);
        let input = if reviewed {
            fx.complete_input()
        } else {
            fx.complete_ungated_input()
        };
        let error = complete_with_logical_clock(&host, &input).unwrap_err();
        assert!(error.to_string().contains(refusal), "{case}: {error}");
        assert_eq!(fx.merge_requests().len(), 1, "{case}: no second mutation");
        assert_eq!(fx.status_reads(), 2, "{case}: one fresh read");
        assert_eq!(fx.head(), fx.candidate, "{case}: no local rewrite");
        assert!(host.landings().is_empty(), "{case}");
        assert!(host.completion_notes().is_empty(), "{case}");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
    }
}
fn a_base_race_refresh_refuses_a_changed_candidate_without_another_merge(sandbox: &Path) {
    assert_refresh_refuses(
        sandbox,
        &rows_by_name! {
            "moved head" => (true, "base_modified+head_moved", "success", "headRefOid");
            "moved head, ungated" => (false, "base_modified+head_moved", "success", "headRefOid");
            "repointed base" => (true, "base_modified+retargeted", "success", "baseRefName");
            "repointed base, ungated" => (false, "base_modified+retargeted", "success", "baseRefName");
            "other head merged" => (true, "base_modified+merged_other_head", "success", "headRefOid");
            "closed" => (true, "base_modified", "closed", "closed without being merged");
            "contradictory" => (true, "base_modified", "contradictory", "contradictory merge state")
        },
    );
}
fn a_base_race_refresh_refuses_unmet_gates_without_another_merge(sandbox: &Path) {
    assert_refresh_refuses(
        sandbox,
        &rows_by_name! {
            "failed checks" => (true, "base_modified", "failure", "status check 'test' failed");
            "review required" => (false, "base_modified", "review_required", "REVIEW_REQUIRED");
            "policy disallowed" => (true, "base_modified+policy_disallowed", "success", "no permitted merge method");
            "conflict on a reviewed head" => (true, "base_modified", "dirty", "review_gate_stale")
        },
    );
}
fn an_already_merged_refresh_reconciles_without_another_merge_request(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.script_merges(&["base_modified+merged"]);
    let completed = action_ok(&host, "pr_complete", &fx.complete_input());
    assert_eq!(fx.merge_requests().len(), 1, "no second mutation");
    let landed = fx.remote_tip(BASE);
    let merge = &completed["merge"];
    assert_eq!(merge["merged"], true);
    assert_eq!(merge["landed_commit"], landed.as_str());
    assert_eq!(merge["managed_merge"], false);
    assert_eq!(
        merge["delivery_evidence"]["head_sha"],
        fx.candidate.as_str()
    );
    let landings = host.landings();
    assert_eq!(landings.len(), 1);
    assert!(!landings[0].managed_merge);
    assert_eq!(landings[0].landed_commit.as_deref(), Some(landed.as_str()));
    assert_eq!(host.status(TASK_ID), TaskStatus::Done);
}
fn repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget(sandbox: &Path) {
    for (case, max_wait_seconds, requests, refusal) in [
        ("attempt ceiling", 30, 3, "attempt 3 of 3"),
        ("zero budget", 0, 1, "timed out after 0s"),
        ("budget spent by the wait", 5, 1, "timed out after 5s"),
    ] {
        let (fx, host) = fx_host(sandbox);
        open_for_completion(&fx, &host);
        fx.script_merges(&["base_modified"; 4]);
        let mut input = fx.complete_input();
        input["max_wait_seconds"] = json!(max_wait_seconds);
        let error = action_err(&host, "pr_complete", &input);
        let message = error.to_string();
        assert!(message.contains(refusal), "{case}: {message}");
        assert!(message.contains(BASE_MODIFIED), "{case}: {message}");
        assert!(
            message.contains(&format!("attempt {requests} of 3")),
            "{case}: {message}"
        );
        assert!(message.contains("stays in review"), "{case}: {message}");
        assert_eq!(fx.merge_requests().len(), requests, "{case}");
        assert_eq!(fx.status_reads(), requests, "{case}: one read per request");
        assert_eq!(fx.forge_state("merged"), None, "{case}");
        assert!(host.completion_notes().is_empty(), "{case}");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
    }
}
// Reviewed-candidate binding at publication
fn pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    fx.git(&["branch", "main", &fx.base_sha]);
    fx.git(&["push", "origin", "main"]);
    assert_eq!(fx.remote_tip(BASE), fx.remote_tip("main"));
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
    input["landing_branch"] = json!("main");
    let opened = action_ok(&host, "pr_open", &input);
    assert_eq!(opened["pr_created"], true);
    assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
    assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
    input["pr_number"] = json!(PR_NUMBER);
    action_ok(&host, "pr_promote", &input);
    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
    assert!(host.comments(TASK_ID).is_empty());
}
fn pr_promote_records_the_pull_request_page_with_its_number(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
    let opened = action_ok(&host, "pr_open", &input);
    let pr_url = opened["pr_url"].as_str().unwrap();
    assert!(pr_url.starts_with("https://"), "{opened}");
    input["pr_number"] = opened["pr_number"].clone();
    input["pr_url"] = json!(pr_url);
    action_ok(&host, "pr_promote", &input);
    assert_eq!(host.status(TASK_ID), TaskStatus::Review);
    assert_eq!(
        host.task(TASK_ID).external_refs,
        vec![ExternalRef {
            system: GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(),
            id: PR_NUMBER.to_string(),
            url: Some(pr_url.to_string()),
        }]
    );
    let unlinked = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    input["pr_url"] = json!("javascript:alert(1)");
    action_ok(&unlinked, "pr_promote", &input);
    let refs = unlinked.task(TASK_ID).external_refs;
    assert_eq!(refs.len(), 1, "{refs:?}");
    assert_eq!(refs[0].id, PR_NUMBER);
    assert_eq!(refs[0].url, None, "only an https page is recorded");
}
fn pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch(sandbox: &Path) {
    let mut fx = Fixture::new(sandbox);
    fx.git(&["branch", "main", &fx.base_sha]);
    fx.advance_base_and_rebase();
    fx.base_sha = fx.local_tip(BASE);
    fx.candidate = fx.head();
    fx.git(&["checkout", "main"]);
    fx.git(&["merge", "--no-ff", BASE, "-m", "Merge stacked base"]);
    fx.git(&["push", "origin", "main"]);
    fx.git(&["checkout", BRANCH]);
    assert_ne!(fx.base_sha, fx.remote_tip("main"));
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
    input["landing_branch"] = json!("main");
    input["pr_number"] = json!(PR_NUMBER);
    for phase in ["pr_open", "pr_promote"] {
        let error = action_err(&host, phase, &input);
        assert!(error.to_string().contains("already landed"), "{error}");
        assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
    }
    assert!(fx.forge_calls().is_empty());
    let comments = host.comments(TASK_ID);
    assert_eq!(comments.len(), 1, "the same handoff phase is recorded once");
    for comment in comments {
        assert!(
            comment.message.contains("[phase=obsolete-base]"),
            "{}",
            comment.message
        );
    }
}
fn pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate(sandbox: &Path) {
    for case in ["unreviewed commit on the head", "rebased onto another base"] {
        let (fx, host) = fx_host(sandbox);
        let (reviewed_head, reviewed_base) = match case {
            "unreviewed commit on the head" => {
                fx.commit("src/unreviewed.txt", "later edit\n");
                (fx.candidate.clone(), fx.base_sha.clone())
            }
            _ => {
                let reviewed_base = fx.base_sha.clone();
                fx.advance_base_and_rebase();
                (fx.head(), reviewed_base)
            }
        };
        let base_sha = fx.local_tip(BASE);
        let mut input = fx.open_input(&reviewed_head, &reviewed_base);
        input["base_sha"] = json!(base_sha);
        let error = action_err(&host, "pr_open", &input);
        assert!(
            error.to_string().contains("review_gate_stale"),
            "{case}: {error}"
        );
        assert!(fx.forge_calls().is_empty(), "{case}: forge never asked");
        assert_eq!(fx.forge_state("pr-head"), None, "{case}: no PR exists");
        let comments = host.comments(TASK_ID);
        assert_eq!(comments.len(), 1, "{case}: one failed-handoff record");
        assert!(
            comments[0].message.contains("[phase=stale-review-gate]"),
            "{case}: {}",
            comments[0].message
        );
        assert_eq!(host.status(TASK_ID), TaskStatus::InProgress, "{case}");
        let mut current = fx.open_input(&fx.head(), &base_sha);
        current["base_sha"] = json!(base_sha);
        let opened = action(&host, "pr_open", &current)
            .unwrap_or_else(|error| panic!("{case}: current candidate: {error}"));
        assert_eq!(opened["pr_created"], true, "{case}");
        assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
        assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
    }
}
/// [ORB-13989] The settled review's "Review fixes" section is published with the PR it explains: appended to a body the run supplied, and to the body Orbit generates when none was supplied.
fn pr_open_appends_the_review_fixes_section_to_the_published_body(sandbox: &Path) {
    let section = "## Review fixes\n\n- `F1` [high] Missing guard — added the guard";
    for supplied in [Some("Operator-written body."), None] {
        let (fx, host) = fx_host(sandbox);
        let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
        input["review_fixes"] = json!(section);
        if let Some(body) = supplied {
            input["body"] = json!(body);
        }
        let opened = action_ok(&host, "pr_open", &input);
        assert_eq!(opened["pr_created"], true);
        let body = fx.forge_state("pr-body").unwrap();
        assert!(body.trim_end().ends_with(section));
        if let Some(supplied) = supplied {
            assert!(body.starts_with(supplied), "{body}");
        } else {
            assert!(body.len() > section.len() + 2);
        }
    }
}
// Required validation before publication
fn required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    // Comparable base-pass control: both executions declare the same selection and a counted check; candidate validation still fails.
    let format_check = r#"printf '%s' '{"schema_version":1,"selection":{"packages":["fixture"]},"tests_run":1}' > "$ORBIT_VALIDATION_SUMMARY";
         test ! -e src/feature.txt || grep -qx formatted src/feature.txt ||
         { echo 'src/feature.txt: not formatted' >&2; exit 3; }"#;
    host.require_commands(&["echo suite-ok", format_check]);
    let remote_before = fx.remote_tip(BRANCH);
    let message = validate_err(&fx, &host);
    assert!(
        message.contains("required validation")
            && message.contains(&fx.candidate)
            && message.contains("exit 3")
            && message.contains(&format!("validation/{RUN_ID}/1.json")),
        "the refusal names the candidate, exit code and log: {message}"
    );
    assert!(
        !baseline_red(&message)
            && message.contains("failure was not reproduced")
            && !message.contains("candidate introduced"),
        "a failure the base does not share stays the candidate's: {message}"
    );
    let passed = host.run_log("0.json");
    assert_eq!(passed["command"], "echo suite-ok");
    assert_eq!(passed["exit_code"], 0);
    assert_eq!(passed["output"], "suite-ok");
    let failed = host.run_log("1.json");
    assert_eq!(failed["exit_code"], 3);
    assert_eq!(failed["tested_head"], fx.candidate.as_str());
    assert_eq!(failed["base_sha"], fx.base_sha.as_str());
    assert_eq!(failed["baseline"]["decision"], "passed");
    let base_log = host.run_log("1.baseline.json");
    assert_eq!(base_log["exit_code"], 0, "the base ran the same command");
    assert_eq!(
        base_log["summary"]["selection"],
        failed["summary"]["selection"]
    );
    assert_eq!(base_log["summary"]["tests_run"], 1);
    assert_eq!(failed["failure_kind"], "candidate");
    assert_eq!(
        failed["output"],
        format!(
            "src/feature.txt: not formatted\nRequired validation PATH={}",
            std::env::var("PATH").unwrap()
        )
    );
    assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
    fx.write_repo("src/feature.txt", "formatted\n");
    let dirty = action_err(&host, "candidate_validate", &fx.validate_input());
    assert!(
        dirty
            .to_string()
            .contains("staged, tracked, or untracked changes")
    );
    fx.git(&["commit", "-am", "format feature"]);
    let repaired = fx.head();
    let validated = validate_ok(&fx, &host);
    assert_eq!(validated["decision"], "passed");
    assert_eq!(validated["tested_head"], repaired.as_str());
    assert_eq!(
        validated["commands"],
        json!(["echo suite-ok", format_check])
    );
    assert_eq!(validated["validation"].as_array().unwrap().len(), 2);
    let log = host.run_log("1.json");
    assert_eq!(log["exit_code"], 0, "the retry replaces the failing log");
    assert_eq!(log["tested_head"], repaired.as_str());
    let attempt = &log["timing"]["attempts"][0];
    assert!(attempt["process_ms"].is_u64());
    assert_eq!(attempt["admission_waits"]["deadline_extension_ms"], 0);
    assert_eq!(
        attempt["phase_timings"],
        json!({"compile_ms": null, "test_ms": null, "package_timings": null}),
        "a counted check without phase measurements must not fabricate timings"
    );
    assert!(host.run_log("0.json")["timing"]["attempts"][0]["phase_timings"].is_null());
    let measured = r#"printf '%s' '{"schema_version":1,"selection":{"packages":["fixture"]},"tests_run":1,"compile_ms":11,"test_ms":22,"package_timings":[{"package":"fixture","compile_ms":11,"test_ms":22}]}' > "$ORBIT_VALIDATION_SUMMARY""#;
    host.require_commands(&[measured]);
    validate_ok(&fx, &host);
    let measured_log = host.run_log("0.json");
    assert_eq!(
        measured_log["timing"]["attempts"][0]["phase_timings"]["compile_ms"],
        11
    );
    assert_eq!(
        measured_log["timing"]["attempts"][0]["phase_timings"]["test_ms"],
        22
    );
    let original_bytes = serde_json::to_vec(&failed).unwrap();
    let archive = format!(
        "validation/{RUN_ID}/attempts/{}.json",
        sha256_hex(&original_bytes)
    );
    assert_eq!(host.validation_log(TASK_ID, &archive), failed);
}
fn required_validation_retains_reported_evidence_after_scratch_lifetime(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let script = sandbox.join("report-evidence.py");
    let source = sandbox.join("raw-failure.log");
    let observed = sandbox.join("evidence-directory");
    let raw = format!(
        "{}        FAIL [0.001s] fixture::suite broken\n  scenario: middle-failure\n  assertion: durable-detail\n{}",
        "héád noise\n".repeat(20000),
        "tail noise\n".repeat(20000)
    );
    fs::write(&source, &raw).unwrap();
    fs::write(&script, r#"import hashlib, json, os, sys
from pathlib import Path
mode, source, observed = sys.argv[1:]
if not Path('src/feature.txt').exists() and mode not in ('both', 'partial-both'):
    sys.exit(0)
if 'ORBIT_COMMAND_EVIDENCE_DIR' not in os.environ:
    Path(observed).write_text('unavailable-evidence-directory')
    sys.stdout.write(Path(source).read_text())
    sys.exit(3)
directory = Path(os.environ['ORBIT_COMMAND_EVIDENCE_DIR'])
Path(observed).write_text(str(directory))
raw = Path(source).read_bytes()
if mode == 'oversized': raw = b'x' * (1048576 + 1)
if mode == 'invalid-utf8': raw = b'\xff\xfe'
if mode == 'redacted': raw = b'Authorization: Bearer fixture-secret-token\n'
name = 'runner.log'
path = directory / name
path.write_bytes(raw)
if mode == 'symlink':
    path.unlink()
    path.symlink_to(source)
if mode == 'fifo':
    path.unlink()
    os.mkfifo(path)
entry = dict(name=name, source='logs/runner.log', bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest(), complete=not mode.startswith('partial'))
manifest = dict(schema_version=1, invocation=os.environ['ORBIT_COMMAND_EVIDENCE_INVOCATION'], files=[entry])
if mode == 'foreign': manifest['invocation'] = 'another-command'
if mode == 'outside': entry['name'] = source
if mode == 'outside-source': entry['source'] = '../runner.log'
if mode == 'digest': entry['sha256'] = '0' * 64
if mode == 'size': entry['bytes'] += 1
if mode == 'malformed': manifest['unexpected'] = True
if mode == 'malformed-secret': manifest['ghp_' + 'Q' * 34] = True
if mode == 'missing-complete': del entry['complete']
if mode == 'total-oversized':
    raw = b'x' * 600000
    path.write_bytes(raw)
    entry.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
    (directory / 'another.log').write_bytes(raw)
    manifest['files'].append(dict(entry, name='another.log'))
(directory / 'manifest.json').write_text(json.dumps(manifest))
if mode == 'manifest-oversized':
    (directory / 'manifest.json').write_text(' ' * (16384 + 1))
if mode == 'parent-swap':
    directory.rename(directory.with_name('anchored-renamed'))
    directory.symlink_to(Path(source).parent, target_is_directory=True)
if mode == 'manifest-symlink':
    (directory / 'manifest.json').unlink()
    (directory / 'manifest.json').symlink_to(source)
if mode == 'both' or mode.startswith(('partial-', 'secret-')):
    print('test broken ... FAILED')
else:
    sys.stdout.write(Path(source).read_text())
sys.exit(3)
"#).unwrap();
    let mut measurements = Vec::new();
    for route in ["candidate_validate", "claim_validate"] {
        for mode in concat!(
            "complete both partial-ghp partial redacted partial-token partial-key partial-assignment partial-bearer partial-utf8 partial-both secret-complete malformed malformed-secret missing-complete ",
            "foreign outside outside-source symlink fifo manifest-symlink manifest-oversized total-oversized invalid-utf8 parent-swap oversized digest size unknown-write foreign-owner",
        )
        .split_whitespace()
        {
            // Synthetic bytes only: incomplete source payloads are withheld regardless of credential shape or where UTF-8 was cut.
            let reported: Vec<u8> = match mode {
                "partial-ghp" | "partial-both" => {
                    format!("{}ghp_{}", " ".repeat(1048576 - 38), "Q".repeat(34))
                        .into_bytes()
                }
                "partial-token" => b"github_pat_QQQQQQQ".to_vec(),
                "partial-key" => {
                    b"sk-QQQQQQQ\nglpat-QQQQQQQ\nAIzaQQQQQQQ\nAKIAQQQQQQQ\nnpm_QQQQQQQ\nxoxb-QQQQ".to_vec()
                }
                "partial-assignment" => {
                    b"aws_secret_access_key=QQQQ\n{\"api_key\":\"QQQQ\nQQQQ".to_vec()
                }
                "partial-bearer" => {
                    b"Authorization: Bearer QQQQ\nBearer [REDACTED_AUTH]".to_vec()
                }
                "partial-utf8" => b"{\"api_key\":\"QQQQ\nQQQQ\xf0\x9f".to_vec(),
                "secret-complete" => {
                    format!(
                        "{}ghp_{}\nsk-{}\nglpat-{}\ngithub_pat_{}\nAIza{}\nAKIA{}\nnpm_{}\nxoxb-{}\naws_secret_access_key=QQQQ\n{{\"api_key\":\"QQQQ\"}}\nBearer QQQQ\nBearer [REDACTED_AUTH]\n",
                        " ".repeat(16384 - 38), "Q".repeat(36),
                        "Q".repeat(20), "Q".repeat(20), "Q".repeat(22),
                        "Q".repeat(35), "Q".repeat(16), "Q".repeat(36), "Q".repeat(10),
                    ).into_bytes()
                }
                "redacted" => b"Authorization: Bearer fixture-secret-token\n".to_vec(),
                _ => raw.as_bytes().to_vec(),
            };
            let boundary_case = mode.starts_with("partial-")
                || matches!(mode, "secret-complete" | "malformed-secret");
            let boundary_source = sandbox.join("boundary-source.log");
            fs::write(&boundary_source, &reported).unwrap();
            let command_source = if boundary_case {
                &boundary_source
            } else {
                &source
            };
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            *locked(&host.fail_evidence_writes) = mode == "unknown-write";
            if mode == "foreign-owner" {
                locked(&host.tasks).get_mut(TASK_ID).unwrap().job_run_id =
                    Some("foreign-run".into());
            }
            let command = format!(
                "python3 '{}' {mode} '{}' '{}'",
                script.display(),
                command_source.display(),
                observed.display()
            );
            host.require_commands(&[&command]);
            let input = if route == "candidate_validate" { fx.validate_input() } else { fx.claim_input() };
            let outcome = action(&host, route, &input);
            if mode == "foreign-owner" && route == "candidate_validate" {
                assert!(outcome.unwrap_err().to_string().contains("does not own"));
                assert!(locked(&host.artifacts).is_empty());
                continue;
            }
            if matches!(mode, "both" | "partial-both") {
                assert_eq!(outcome.unwrap()["decision"], "passed_with_known_red");
            } else {
                let failure = outcome.unwrap_err().to_string();
                assert!(failure.contains("exit 3"), "{route}/{mode}: {failure}");
            }
            if mode == "partial-both" {
                let replay = action_ok(&host, route, &input);
                assert_eq!(replay["decision"], "passed_with_known_red");
            }
            let ephemeral = fs::read_to_string(&observed).unwrap();
            assert!(!Path::new(&ephemeral).exists());
            let read = |path: &str| -> Value {
                if route == "candidate_validate" {
                    host.validation_log(TASK_ID, path)
                } else {
                    host.claim_log(path)
                }
            };
            let log_path = if route == "candidate_validate" {
                format!("validation/{RUN_ID}/0.json")
            } else {
                "validation/claim-landing/0.failed.json".into()
            };
            let log = read(&log_path);
            assert_eq!(log["exit_code"], 3);
            if mode == "partial-both" {
                assert_eq!(log["baseline"]["cached"], true);
            }
            let evidence = &log["timing"]["attempts"][0]["command_evidence"];
            assert!(log["timing"]["attempts"][0]["evidence_capture_ms"].is_u64());
            measurements.push(json!({"route": route, "mode": mode,
                "raw_bytes": reported.len(), "bounded_bytes": log["output"].as_str().unwrap().len(),
                "capture_ms": log["timing"]["attempts"][0]["evidence_capture_ms"],
                "retention_ms": log["timing"]["evidence_retention_ms"],
                "status": evidence["status"]}));
            match mode {
                "complete" | "both" | "partial" | "redacted" | "foreign-owner" | "parent-swap"
                | "partial-ghp" | "partial-token" | "partial-key" | "partial-assignment"
                | "partial-bearer" | "partial-utf8" | "partial-both" | "secret-complete" => {
                    assert_eq!(evidence["status"], "captured", "{route}/{mode}: {evidence}");
                    let file = &evidence["files"][0];
                    let reference = &file["manifests"][0];
                    let manifest = read(reference["path"].as_str().unwrap());
                    assert_eq!(manifest["task_id"], TASK_ID);
                    assert_eq!(manifest["source_workspace"], fx.repo.to_string_lossy().as_ref());
                    assert_eq!(manifest["provenance"]["command"], command);
                    assert_eq!(
                        manifest["provenance"]["command_sha256"],
                        sha256_hex(command.as_bytes())
                    );
                    assert_eq!(manifest["provenance"]["run_id"], RUN_ID);
                    assert_eq!(manifest["provenance"]["tested_head"], fx.candidate);
                    assert_eq!(manifest["provenance"]["role"], "candidate");
                    assert_eq!(manifest["file"]["source"], "logs/runner.log");
                    let mut retained = String::new();
                    for chunk in manifest["file"]["chunks"].as_array().unwrap() {
                        let stored = read(chunk["path"].as_str().unwrap());
                        let data = stored["content"].as_str().unwrap();
                        assert_eq!(chunk["offset"], retained.len());
                        assert_eq!(chunk["sha256"], sha256_hex(data.as_bytes()));
                        assert_eq!(
                            chunk["artifact_sha256"],
                            sha256_hex(
                                &serde_json::to_vec(&stored).unwrap()
                            )
                        );
                        retained.push_str(data);
                    }
                    assert_eq!(file["retained_sha256"], sha256_hex(retained.as_bytes()));
                    assert_eq!(file["retained_bytes"], retained.len());
                    assert_eq!(file["source_bytes"], reported.len());
                    assert_eq!(file["source_sha256"], sha256_hex(&reported));
                    let partial = mode.starts_with("partial");
                    assert_eq!(file["complete"], !partial);
                    assert_eq!(evidence["complete"], !partial);
                    assert_eq!(
                        file["raw_complete"],
                        !partial && !matches!(mode, "redacted" | "secret-complete")
                    );
                    if partial {
                        assert!(retained.is_empty(), "{route}/{mode}");
                        assert_eq!(file["partial_source_withheld"], true);
                        assert_eq!(file["redacted"], true);
                    } else if matches!(mode, "redacted" | "secret-complete") {
                        assert!(!retained.contains("fixture-secret-token"));
                        assert_eq!(file["redacted"], true);
                    } else {
                        assert_eq!(retained, raw);
                    }
                    if matches!(mode, "both" | "partial-both") {
                        let base_path = if route == "candidate_validate" {
                            format!("validation/{RUN_ID}/0.baseline.json")
                        } else {
                            "validation/claim-landing/0.baseline.json".into()
                        };
                        let base = read(&base_path);
                        let base_ref = &base["timing"]["attempts"][0]["command_evidence"]["files"]
                            [0]["manifests"][0];
                        let base_manifest = read(base_ref["path"].as_str().unwrap());
                        assert_eq!(base_manifest["provenance"]["tested_head"], fx.base_sha);
                        assert_eq!(base_manifest["provenance"]["role"], "baseline");
                        if partial {
                            assert_eq!(base_manifest["file"]["retained_bytes"], 0);
                            let cache_dir = fx.repo.join(".git/orbit-baseline");
                            for entry in fs::read_dir(cache_dir).unwrap().flatten() {
                                if entry.path().extension().is_some_and(|ext| ext == "json") {
                                    let cached = fs::read_to_string(entry.path()).unwrap();
                                    assert!(!cached.contains("ghp_Q"));
                                }
                            }
                        }
                    } else if !boundary_case {
                        assert!(log["output"].as_str().unwrap().contains("durable-detail"));
                    }
                }
                "unknown-write" => {
                    assert_eq!(evidence["status"], "attachment_unknown");
                    assert_eq!(evidence["complete"], false);
                    assert_eq!(evidence["captured_files"][0]["source"], "logs/runner.log");
                    assert_eq!(evidence["captured_files"][0]["complete"], false);
                    let paths: Vec<_> = if route == "candidate_validate" {
                        locked(&host.artifacts)
                            .keys()
                            .map(|(_, path)| path.clone())
                            .collect()
                    } else {
                        locked(&host.claim_logs).clone()
                    };
                    assert_eq!(
                        paths
                            .iter()
                            .filter(|path| path.contains("/evidence/"))
                            .count(),
                        1,
                        "unknown write is never retried"
                    );
                    assert!(!paths.iter().any(|path| path.ends_with(".manifest.json")));
                }
                _ => {
                    assert_eq!(evidence["status"], "refused", "{route}/{mode}: {evidence}");
                    assert_eq!(evidence["complete"], false);
                    assert!(evidence.get("files").is_none());
                }
            }
            if boundary_case {
                let contents: Vec<Vec<u8>> = if route == "candidate_validate" {
                    locked(&host.artifacts).values().cloned().collect()
                } else {
                    locked(&host.claim_log_contents).values().cloned().collect()
                };
                for bytes in contents {
                    let persisted = String::from_utf8(bytes).unwrap();
                    for fragment in "ghp_Q github_pat_Q sk-QQ glpat-QQ AIzaQQ AKIAQQ npm_QQ xoxb-QQ fixture-secret-token QQQQ".split_whitespace() {
                        assert!(!persisted.contains(fragment), "{route}/{mode}");
                    }
                }
            }
        }
    }
    if let Some(path) = std::env::var_os("EVIDENCE_SCENARIO_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&measurements).unwrap()).unwrap();
    }
}
fn required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let output_path = sandbox.join("validation-output");
    let output = format!(
        "foo_bar: passing test stdout\n{}\ntest actual_failure ... FAILED\n",
        "é evidence\n".repeat(12000)
    );
    fs::write(&output_path, &output).unwrap();
    let command = format!(
        "test ! -e src/feature.txt || {{ cat '{}'; exit 3; }}",
        output_path.display()
    );
    host.require_commands(&[&command]);
    let message = action(&host, "candidate_validate", &fx.validate_input())
        .unwrap_err()
        .to_string();
    let candidate_path = format!("validation/{RUN_ID}/0.json");
    let base_path = format!("validation/{RUN_ID}/0.baseline.json");
    assert!(
        message.len() <= 16 * 1024,
        "diagnostic bound: {}",
        message.len()
    );
    for field in [
        &command,
        "exit 3",
        "actual_failure",
        "Base verdict:",
        "failure was not reproduced",
        &candidate_path,
        &base_path,
    ] {
        assert!(message.contains(field));
    }
    assert!(
        message.contains("Neither run declared a selection")
            && message.contains("source causation is unconfirmed")
            && !message.contains("candidate introduced"),
        "[F2026-10-378] a no-summary base that skips the failing guard proves no source causation: {message}"
    );
    assert!(!message.contains("foo_bar:"));
    let log = host.validation_log(TASK_ID, &candidate_path);
    assert!(log["output"].as_str().unwrap().starts_with(&output));
    assert_eq!(log["baseline"]["log"], base_path);
    let base = host.validation_log(TASK_ID, &base_path);
    assert_eq!(base["exit_code"], 0);
    assert_eq!(base["command"], log["command"]);
    assert_eq!(base["base_sha"], fx.base_sha);
    assert_eq!(log["tested_head"], fx.candidate);
    assert_eq!(log["failure_kind"], "candidate");
    assert!(log["summary"].is_null() && base["summary"].is_null());
    assert_eq!(base["output"], "", "the base skipped the failing guard");
    let input = fx.claim_input();
    let message = action(&host, "claim_validate", &input)
        .unwrap_err()
        .to_string();
    assert!(message.len() <= 16 * 1024);
    assert!(
        message.contains("failure was not reproduced")
            && message.contains("Neither run declared a selection")
            && message.contains("source causation is unconfirmed")
            && !message.contains("candidate introduced"),
        "[F2026-10-378] claimed validation retains uncertainty about the skipped guard: {message}"
    );
    for field in [
        &fx.candidate,
        &fx.base_sha,
        "actual_failure",
        "exit 3",
        "validation/claim-landing/0.failed.json",
        "validation/claim-landing/0.baseline.json",
    ] {
        assert!(message.contains(field));
    }
    let log = host.claim_log("validation/claim-landing/0.failed.json");
    assert!(log["output"].as_str().unwrap().starts_with(&output));
    assert_eq!(log["failure_kind"], "candidate");
    assert_eq!(log["tested_head"], fx.candidate);
    let base = host.claim_log("validation/claim-landing/0.baseline.json");
    assert_eq!(base["base_sha"], fx.base_sha);
    assert_eq!(base["command"], log["command"]);
    assert_eq!(base["exit_code"], 0);
    assert_eq!(base["output"], "");
    // Shared exit status without failure identities proves no shared failure, even when the output looks the same on both sides.
    let output = format!("{}\nbase failure evidence\n", "é evidence\n".repeat(12000));
    fs::write(&output_path, &output).unwrap();
    let command = format!("cat '{}'; exit 3", output_path.display());
    host.require_commands(&[&command]);
    for (action_name, input) in [
        ("candidate_validate", fx.validate_input()),
        ("claim_validate", input),
    ] {
        let message = action(&host, action_name, &input).unwrap_err().to_string();
        assert!(message.len() <= 16 * 1024);
        assert!(!baseline_red(&message), "{message}");
        assert!(message.contains("failure identities are missing"));
        assert!(!message.contains("failed identically"), "{message}");
        assert!(!message.contains(&output));
    }
    host.require_commands(&[
        "if test -e src/feature.txt; then exit 3; else orbit-missing-base-tool; fi",
    ]);
    for (route, input) in [
        ("candidate_validate", fx.validate_input()),
        (
            "claim_validate",
            json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha}),
        ),
    ] {
        let message = action(&host, route, &input).unwrap_err().to_string();
        assert!(
            message.contains("could not be judged")
                && message.contains("source causation is unconfirmed")
                && !message.contains("candidate introduced"),
            "an inconclusive base cannot establish source causation: {message}"
        );
        assert!(!baseline_red(&message), "{message}");
    }
}
/// A missing validation tool is refused with the full searched PATH, both in the action error and the durable command log [ORB-13963], even when the resolved PATH differs from the launcher or exceeds 1024 bytes.
fn missing_required_validation_tool_reports_its_search_path(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let launcher_path = std::env::var("PATH").unwrap();
    // Deterministically exercise long, multibyte paths on short-PATH hosts too, without changing the fixture's own tool lookup.
    let long_path = format!("{MINIMAL_PATH}{}", ":/orbit-missing-é".repeat(100));
    for resolved_path in [None, Some(MINIMAL_PATH), Some(long_path.as_str())] {
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        if let Some(path) = resolved_path {
            host.resolve_validation_env(ValidationEnvironment::launcher(vec![(
                "PATH".to_string(),
                path.to_string(),
            )]));
        }
        host.require_commands(&["orbit_missing_validation_tool_13963"]);
        let error = action_err(&host, "candidate_validate", &fx.validate_input());
        let searched_path = resolved_path.unwrap_or(&launcher_path);
        let diagnostic = format!("Required validation PATH={searched_path}");
        assert!(error.to_string().contains(&diagnostic), "{error}");
        let log = host.run_log("0.json");
        assert_eq!(log["exit_code"], 127);
        assert_eq!(log["validation_env"]["path"], searched_path);
        assert!(log["output"].as_str().unwrap().contains(&diagnostic));
    }
}
const MINIMAL_PATH: &str = "/usr/bin:/bin";
const PROFILE_TOOL: &str = "orbit-profile-lint-13987";
/// A drain launched from a minimal PATH still validates with the owner's login-shell toolchain, and the log records that the login shell decided PATH [ORB-13987].
fn validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let (launcher, shell, tools) = login_shell_toolchain(sandbox);
    host.resolve_validation_env(ValidationEnvironment::resolve(
        launcher,
        &ValidationEnvPolicy::default(),
        &LoginShell::new(shell, Duration::from_secs(10)),
    ));
    host.require_commands(&[PROFILE_TOOL]);
    let validated = validate_ok(&fx, &host);
    assert_eq!(validated["decision"], "passed");
    assert_eq!(validated["validation_env"]["source"], "login_shell");
    assert_eq!(
        validated["validation_env"]["probe_mode"],
        "interactive_login"
    );
    assert_eq!(
        validated["validation_env"]["fallback_reason"],
        serde_json::Value::Null
    );
    let log = host.run_log("0.json");
    assert_eq!(log["output"], "lint-ok");
    assert_eq!(log["validation_env"]["source"], "login_shell");
    let path = log["validation_env"]["path"].as_str().unwrap();
    assert!(path.starts_with(&format!("{}:", tools.display())) && path.ends_with(MINIMAL_PATH));
    assert_eq!(log["failure_kind"], Value::Null);
}
fn a_missing_validation_tool_is_an_environment_failure(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let (launcher, _shell, _tools) = login_shell_toolchain(sandbox);
    host.resolve_validation_env(ValidationEnvironment::launcher(launcher));
    host.require_commands(&[PROFILE_TOOL]);
    let remote_before = fx.remote_tip(BRANCH);
    let message = validate_err(&fx, &host);
    assert!(env_failure(&message));
    assert!(
        message.contains(&format!("`{PROFILE_TOOL}`"))
            && message.contains(&format!("PATH={MINIMAL_PATH}"))
            && message.contains("source: launcher_fallback"),
        "the failure reports the tool, PATH and its source: {message}"
    );
    let log = host.run_log("0.json");
    assert_eq!(log["exit_code"], 127);
    assert_eq!(log["failure_kind"], "environment");
    assert_eq!(log["missing_tool"], PROFILE_TOOL);
    assert_eq!(log["validation_env"]["source"], "launcher_fallback");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
    // The run's failure handoff keeps the unjudged candidate for `orbit job resume`: no `[BLOCKED]` PR, no push, no repair.
    let handoff = failure_handoff_ok(&fx, &host, "validate", "validation_environment", &message);
    assert_eq!(handoff["decision"], "blocked_validation_environment");
    assert_eq!(handoff["head_sha"], fx.candidate.as_str());
    assert_held_without_pr(&fx, &handoff, &remote_before, "missing tool");
    assert_eq!(fx.durable_candidate(&handoff), fx.candidate);
    assert_eq!(fx.head(), fx.candidate);
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    let comments = host.comments(TASK_ID);
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains(&format!("PATH={MINIMAL_PATH}"))),
        "the block reports the PATH the tool was missing from: {comments:?}"
    );
}
/// The owner validates outside any agent sandbox, so a required command that exits zero after printing a `DEFERRED:` notice never ran its sandbox-confined path before delivery [ORB-15287].
fn a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let notice = orbit_exec::bwrap_deferral_notice(
        "sandboxed_path",
        "bwrap: No permissions to create new namespace",
    );
    let remote_before = fx.remote_tip(BRANCH);
    host.require_commands(&[&format!("echo '{notice}' >&2")]);
    let message = validate_err(&fx, &host);
    assert!(env_failure(&message) && message.contains(&notice) && message.contains(&fx.candidate));
    let log = host.run_log("0.json");
    assert_eq!(log["exit_code"], 0, "the process itself succeeded");
    assert_eq!(log["failure_kind"], "environment");
    assert_eq!(log["deferred_notice"], notice.as_str());
    assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
    // The claimed path validates before publication the same way.
    host.ship_pr();
    let input = fx.claim_input();
    let claim = action_err(&host, "claim_validate", &input);
    assert!(
        is_validation_environment_failure(None, Some(&claim.to_string()))
            && claim.to_string().contains(&notice),
        "{claim}"
    );
    // A real failure alongside the notice stays the candidate's: the base lacks src/feature.txt, so it passes the same command.
    host.require_commands(&[&format!("echo '{notice}' >&2; test ! -e src/feature.txt")]);
    let failed = action_err(&host, "candidate_validate", &fx.validate_input());
    let failed = failed.to_string();
    assert!(
        !env_failure(&failed)
            && failed.contains("failure was not reproduced")
            && !failed.contains("candidate introduced"),
        "a failing gate with a notice is the candidate's failure: {failed}"
    );
    let log = host.run_log("0.json");
    assert_eq!(log["failure_kind"], "candidate");
    // A notice that does not lead its line is not a deferral.
    host.require_commands(&[&format!("echo 'saw {notice}'")]);
    let validated = validate_ok(&fx, &host);
    assert_eq!(validated["decision"], "passed");
}
// Deferred sandbox handoff [ORB-15287]
/// An implementer whose affected-test gate passed with only Bubblewrap deferrals hands the gate off instead of failing [ORB-15287].
fn a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let gate = Gate::new(sandbox);
    host.gate_commands(&gate);
    let record = deferral(&fx, &gate.command());
    let handoff = json!({
        "summary": "done",
        "deferred_sandbox_validation": handoff_for(&fx, &record),
    });
    run_implementation(&fx, &host, Some(handoff)).unwrap();
    let kept = host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT);
    assert_eq!(kept["run_id"], RUN_ID);
    assert_eq!(kept["deferred"], record);
    assert_eq!(
        host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
        record,
        "the derived record replaces the handoff in the step's output"
    );
    assert_eq!(
        host.probes(),
        vec![(
            BUBBLEWRAP_NAMESPACE_PROBE.to_string(),
            None,
            fx.repo.clone()
        )],
        "Orbit ran the notices' probe once, in the implementer's checkout and lane"
    );
    assert_eq!(gate.runs(), 1, "the owner ran the deferred gate itself");
    let replay = host.run_log("1.json");
    assert_eq!(replay["command"], gate.command().as_str());
    assert_eq!(replay["deferred_sandbox_replay"], true);
    assert_eq!(replay["exit_code"], 0);
    assert_eq!(replay["summary"]["tests_run"], 3);
    run_implementation(&fx, &host, None).unwrap();
    assert_eq!(gate.runs(), 2);
    gate.defers();
    let held = run_implementation(&fx, &host, None).unwrap_err();
    assert!(env_failure(&held) && held.contains("deferred a sandbox-confined path"));
    gate.executes_nothing();
    let refused = run_implementation(&fx, &host, None).unwrap_err();
    assert!(refused.contains("replayed nothing"), "{refused}");
    gate.fails();
    let red = run_implementation(&fx, &host, None).unwrap_err();
    assert!(!baseline_red(&red), "{red}");
    assert!(red.contains("failure identities are missing"), "{red}");
    gate.executes();
    let runs = gate.runs();
    run_implementation(&fx, &host, Some(json!({ "summary": "done" }))).unwrap();
    assert_eq!(gate.runs(), runs, "nothing is left to replay");
    assert_eq!(
        host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT)["deferred"],
        Value::Null
    );
}
/// Every handoff whose derivation is not the sanctioned case, only notices of sandboxes whose probe the lane refuses (Bubblewrap [ORB-15287], Seatbelt [ORB-15409], or both [ORB-15692]), fails the implement step before commit: nothing is validated, replayed or kept, and the failure names what is wrong [ORB-15516].
fn a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let gate = Gate::new(sandbox);
    let valid = deferral(&fx, &gate.command());
    let with = |field: &str, value: Value| {
        let mut record = valid.clone();
        record[field] = value;
        record
    };
    let seatbelt = seatbelt_deferral(&fx, &gate.command());
    let seatbelt_with = |field: &str, value: Value| {
        let mut record = seatbelt.clone();
        record[field] = value;
        record
    };
    let alongside = |line: &str| json!([bwrap_notice(), line]);
    let probe = |exit_code: i32, output: &str| json!({ "command": BUBBLEWRAP_NAMESPACE_PROBE, "exit_code": exit_code, "output": output });
    let seatbelt_probe = |exit_code: i32, output: &str| json!({ "command": SEATBELT_APPLY_PROBE, "exit_code": exit_code, "output": output });
    let none = Value::Null;
    let (not_sanctioned, no_seatbelt, no_ns) = (
        "is not a Bubblewrap, Seatbelt or ps-denied deferral",
        "no refused Seatbelt apply probe",
        "no failing namespace probe",
    );
    // Rows: name => (gate-summary record, the lane's answer to the record's probe, hand-written fields beside `summary`, reason).
    let cases = rows_by_name! {
        "a SKIP line" => (with("notices", alongside("SKIP: shell_child: sandbox unavailable")), none.clone(), none.clone(), not_sanctioned);
        "a notice of neither sandbox" => (with("notices", json!(["DEFERRED: landlock unavailable: confined_child: unsupported"])), none.clone(), none.clone(), not_sanctioned);
        "a Seatbelt denial that is not the guard's notice" => (seatbelt_with("notices", json!(["SKIP: seatbelt_child: sandbox_apply: Operation not permitted"])), none.clone(), none.clone(), not_sanctioned);
        "a Seatbelt notice beside a Bubblewrap one, in a lane that applies Seatbelt" => (seatbelt_with("notices", alongside(&seatbelt_notice())), seatbelt_probe(0, ""), none.clone(), no_seatbelt);
        "a Bubblewrap notice beside a Seatbelt one, in a lane that creates namespaces" => (with("notices", json!([seatbelt_notice(), bwrap_notice()])), probe(0, ""), none.clone(), no_ns);
        "a non-Seatbelt skip beside a Seatbelt notice" => (seatbelt_with("notices", json!([seatbelt_notice(), "SKIP: the sandbox denied running ps: Operation not permitted (os error 1)"])), none.clone(), none.clone(), not_sanctioned);
        "a ps-denied notice in an owner run" => (with("notices", json!([ps_notice()])), none.clone(), none.clone(), "only a claimed leaf on macOS may defer");
        "a lane that answers the Seatbelt probe with the namespace one" => (seatbelt.clone(), valid["probe"].clone(), none.clone(), "names the probe");
        "a lane that applies Seatbelt" => (seatbelt.clone(), seatbelt_probe(0, ""), none.clone(), no_seatbelt);
        "a Seatbelt probe that failed otherwise" => (seatbelt.clone(), seatbelt_probe(1, "sandbox-exec: sandbox_apply: Operation not permitted"), none.clone(), no_seatbelt);
        "a Seatbelt probe carrying the namespace refusal" => (seatbelt.clone(), seatbelt_probe(71, "bwrap: No permissions to create new namespace"), none.clone(), no_seatbelt);
        "a Seatbelt gate that failed" => (seatbelt_with("exit_code", json!(1)), none.clone(), none.clone(), "exited 1");
        "a Seatbelt handoff that claims a pass" => (seatbelt.clone(), none.clone(), json!({ "passed": true }), "unknown field `passed`");
        "no notice" => (with("notices", json!([])), none.clone(), none.clone(), "lists no deferral notice");
        "zero tests" => (with("tests_run", json!(0)), none.clone(), none.clone(), "executed no tests");
        "the wrong base" => (with("base", json!(fx.candidate)), none.clone(), none.clone(), "not the pinned delivery base");
        "a nonzero exit" => (with("exit_code", json!(101)), none.clone(), none.clone(), "exited 101");
        "a lane that creates namespaces" => (valid.clone(), probe(0, ""), none.clone(), no_ns);
        "a probe that never reached the kernel" => (valid.clone(), probe(-1, "the probe could not run: No such file or directory"), none.clone(), no_ns);
        "a hand-written probe that is not Orbit's" => (valid.clone(), none.clone(), json!({ "probe": probe(1, "bwrap: setting up uid map: Permission denied") }), "hands `probe` as");
        "a hand-written count that is not the summary's" => (valid.clone(), none.clone(), json!({ "tests_run": 1 }), "hands `tests_run` as 1");
        "a command no host replays" => (with("command", json!("scripts/codeql-rust-local.sh")), none.clone(), none.clone(), "not one of the owner's validation gates");
        "a host-required criterion" => (with("command", json!("cargo test -p orbit-cli --test mcp")), none.clone(), none.clone(), "not one of the owner's validation gates");
        "a gate that records no command" => (with("command", Value::Null), none.clone(), none.clone(), "names no gate `command`");
        "a handoff that claims a coverage" => (valid.clone(), none.clone(), json!({ "coverage": "not_run" }), "unknown field `coverage`")
    };
    let refuse = |case: &str, deferral: Value, answered: &Value, probe: Value, reason: &str| {
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        host.gate_commands(&gate);
        if !probe.is_null() {
            locked(&host.deferral_answers).insert(
                answered.as_str().unwrap().to_string(),
                serde_json::from_value(probe).unwrap(),
            );
        }
        let handoff = json!({ "summary": "done", "deferred_sandbox_validation": deferral });
        let refusal = run_implementation(&fx, &host, Some(handoff)).expect_err(case);
        assert!(
            refusal.contains("`deferred_sandbox_validation`") && refusal.contains(reason),
            "{case}: {refusal}"
        );
        assert!(
            !host.has_artifact(TASK_ID, &format!("validation/{RUN_ID}/0.json")),
            "{case}"
        );
        assert!(
            !host.has_artifact(TASK_ID, DEFERRED_SANDBOX_ARTIFACT),
            "{case}"
        );
    };
    for (case, record, probe, handed, reason) in cases {
        let mut handoff = handoff_for(&fx, &record);
        if let Some(handed) = handed.as_object() {
            handoff.as_object_mut().unwrap().extend(handed.clone());
        }
        refuse(case, handoff, &record["probe"]["command"], probe, reason);
    }
    refuse(
        "a hand-written record that names no summary",
        valid.clone(),
        &none,
        none.clone(),
        "names no gate summary",
    );
    refuse(
        "a summary outside the scratch directory",
        json!({ "summary": "src/feature.txt" }),
        &none,
        none.clone(),
        "is not a file in the checkout's `.orbit/tmp`",
    );
    refuse(
        "a summary that escapes the checkout",
        json!({ "summary": format!(".orbit/tmp/../../../{}", fx.repo.file_name().unwrap().to_string_lossy()) }),
        &none,
        none.clone(),
        "is not a file in the checkout's `.orbit/tmp`",
    );
    assert_eq!(gate.runs(), 0, "no refused handoff reaches a replay");
}
fn claim_deferral_input(fx: &Fixture, record: Value) -> Value {
    let mut input = fx.claim_input();
    input["implementation"] = json!({"deferred_sandbox_validation": record});
    input
}
/// Replay the deferred gate before publication and pin the native pass: the pending validation replays exactly the gate once, the pin runs nothing, and the pin fields are removed from `input` afterwards.
fn claim_replay_pinned(host: &DeliveryHost, input: &mut Value, gate: &Gate, case: &str) -> Value {
    let before = gate.runs();
    let pending = action_ok(host, "claim_validate", input);
    assert_eq!(pending["replayed"], json!([gate.command()]), "{case}");
    assert_eq!(gate.runs(), before + 1, "{case}");
    input["pull_request"] = json!(PR_NUMBER);
    input["prevalidated"] = pending.clone();
    let pinned = action_ok(host, "claim_validate", input);
    assert_eq!(pinned["validation"].as_array().unwrap().len(), 2, "{case}");
    assert_eq!(gate.runs(), before + 1, "{case}: the pin runs nothing");
    let fields = input.as_object_mut().unwrap();
    fields.remove("pull_request");
    fields.remove("prevalidated");
    pending
}
fn attach_deferred(host: &DeliveryHost, record: &Value) {
    RuntimeHost::attach_task_validation_log(
        host,
        TASK_ID,
        RUN_ID,
        DEFERRED_SANDBOX_ARTIFACT,
        serde_json::to_vec(&json!({"schema_version": 1, "run_id": RUN_ID, "deferred": record}))
            .unwrap(),
    )
    .unwrap();
}
/// A claimed leaf always runs its implementer, so its validation replays the gate from this attempt's output before publication, and the pin expects the replay among the results [ORB-15287].
fn a_claimed_leaf_replays_its_implementers_deferral_before_publication(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    host.ship_pr();
    let gate = Gate::new(sandbox);
    host.gate_commands(&gate);
    let mut input = claim_deferral_input(&fx, deferral(&fx, &gate.command()));
    let pending = claim_replay_pinned(&host, &mut input, &gate, "claimed leaf");
    assert_eq!(pending["commands"], json!(["true", gate.command()]));
    gate.defers();
    let held = action_err(&host, "claim_validate", &input).to_string();
    assert!(env_failure(&held), "{held}");
    input["implementation"]["deferred_sandbox_validation"]["command"] = json!("make goldens");
    let refused = action_err(&host, "claim_validate", &input).to_string();
    assert!(
        refused.contains("not one of this host's validation gates")
            && refused.contains("from this host's configuration"),
        "[ORB-15622] the refusal names the gates it checked and their source: {refused}"
    );
}
/// A macOS implementer whose gate passed with only the Seatbelt guard's skips hands it off like a Bubblewrap deferral [ORB-15409].
fn a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let gate = Gate::new(sandbox);
    host.gate_commands(&gate);
    let record = seatbelt_deferral(&fx, &gate.command());
    run_implementation(
        &fx,
        &host,
        Some(json!({
            "summary": "done",
            "deferred_sandbox_validation": handoff_for(&fx, &record),
        })),
    )
    .unwrap();
    let kept = host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT);
    assert_eq!(kept["deferred"], record);
    let replay = host.run_log("1.json");
    assert_eq!(replay["deferred_sandbox_replay"], true);
    assert_eq!(replay["exit_code"], 0);
    assert_eq!(gate.runs(), 1);
    host.ship_pr();
    let mut input = claim_deferral_input(&fx, record);
    claim_replay_pinned(&host, &mut input, &gate, "seatbelt");
    gate.skips_seatbelt();
    let held = action_err(&host, "claim_validate", &input).to_string();
    assert!(env_failure(&held) && held.contains(&seatbelt_notice()));
    gate.fails();
    let failed = action_err(&host, "claim_validate", &input).to_string();
    assert!(failed.contains(&gate.command()));
}
/// A macOS claimed leaf whose agent sandbox refuses to exec `ps` hands off the tests that compare against it like a nested-Seatbelt skip [ORB-15587].
fn a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    host.ship_pr();
    host.set_os(EvidenceHostOs::Macos);
    let gate = Gate::new(sandbox);
    host.gate_commands(&gate);
    let record = ps_deferral(&fx, &gate.command());
    let handoff = |fx: &Fixture| {
        json!({
            "summary": "done",
            "deferred_sandbox_validation": handoff_for(fx, &record),
        })
    };
    run_claimed_implementation(&fx, &host, handoff(&fx)).unwrap();
    assert_eq!(
        host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
        record,
        "Orbit derived the record from the gate summary and its own `ps` probe"
    );
    assert_eq!(
        host.probes().first().map(|probe| probe.0.as_str()),
        Some(PS_EXEC_PROBE)
    );
    assert!(!host.has_artifact(TASK_ID, DEFERRED_SANDBOX_ARTIFACT));
    assert_eq!(gate.runs(), 1);
    gate.fails();
    let failed = run_claimed_implementation(&fx, &host, handoff(&fx)).unwrap_err();
    assert!(failed.contains(&gate.command()) && !env_failure(&failed));
    gate.denies_ps();
    let held = run_claimed_implementation(&fx, &host, handoff(&fx)).unwrap_err();
    assert!(env_failure(&held) && held.contains(&ps_notice()));
    gate.executes();
    host.set_os(EvidenceHostOs::Linux);
    let runs = gate.runs();
    let refused = run_claimed_implementation(&fx, &host, handoff(&fx)).unwrap_err();
    assert!(
        refused.contains("`deferred_sandbox_validation`")
            && refused.contains("only a claimed leaf on macOS may defer"),
        "{refused}"
    );
    assert_eq!(gate.runs(), runs, "a refused handoff reaches no replay");
}
/// A macOS claimed leaf's broad gate prints Seatbelt skips and ps-denied deferrals together, and one record defers both once Orbit's probe of each refuses in the lane [ORB-15692].
fn a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    host.ship_pr();
    host.set_os(EvidenceHostOs::Macos);
    let gate = Gate::new(sandbox);
    host.gate_commands(&gate);
    let record = mixed_deferral(&fx, &gate.command());
    let handoff = json!({
        "summary": "done",
        "deferred_sandbox_validation": handoff_for(&fx, &record),
    });
    run_claimed_implementation(&fx, &host, handoff).unwrap();
    assert_eq!(
        host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
        record,
        "Orbit derived one record naming both sandboxes' probes"
    );
    assert_eq!(
        host.probes()
            .iter()
            .map(|probe| probe.0.as_str())
            .collect::<Vec<_>>(),
        vec![SEATBELT_APPLY_PROBE, PS_EXEC_PROBE],
        "Orbit ran each sandbox's probe in the implementer's lane"
    );
    assert_eq!(gate.runs(), 1, "the claim host replayed the gate once");
    let mut input = claim_deferral_input(&fx, record);
    claim_replay_pinned(&host, &mut input, &gate, "mixed");
    for (still, notice) in [
        (Gate::skips_seatbelt as fn(&Gate), seatbelt_notice()),
        (Gate::denies_ps, ps_notice()),
    ] {
        still(&gate);
        let held = action_err(&host, "claim_validate", &input).to_string();
        assert!(env_failure(&held) && held.contains(&notice));
    }
    let owner = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    owner.gate_commands(&gate);
    attach_deferred(&owner, &mixed_deferral(&fx, &gate.command()));
    gate.executes();
    let runs = gate.runs();
    run_implementation(&fx, &owner, None).unwrap();
    assert_eq!(gate.runs(), runs + 1);
    gate.skips_seatbelt();
    let held = run_implementation(&fx, &owner, None).unwrap_err();
    assert!(env_failure(&held), "{held}");
}
/// The handed-off gate, run by path so a test can change what it does between runs while its command, and so the kept record, stays the same.
struct Gate {
    script: PathBuf,
    runs: PathBuf,
}
impl Gate {
    fn new(sandbox: &Path) -> Self {
        let gate = Self {
            script: sandbox.join("affected-gate.sh"),
            runs: sandbox.join("affected-gate.runs"),
        };
        gate.executes();
        gate
    }
    /// The gate as the owner's configuration names it.
    fn command(&self) -> String {
        format!("sh '{}'", self.script.display())
    }
    fn set(&self, body: &str) {
        fs::write(
            &self.script,
            format!("echo ran >> '{}'\n{body}\n", self.runs.display()),
        )
        .unwrap();
    }
    /// Runs its tests and reports them, as `make ci-test-affected` does.
    fn executes(&self) {
        self.set(&reported_tests(3));
    }
    fn defers_with(&self, notice: String) {
        self.set(&format!("echo '{}' >&2\n{}", notice, reported_tests(3)));
    }
    fn defers(&self) {
        self.defers_with(bwrap_notice());
    }
    fn skips_seatbelt(&self) {
        self.defers_with(seatbelt_notice());
    }
    fn denies_ps(&self) {
        self.defers_with(ps_notice());
    }
    /// Passes without reporting an executed test, as a lint gate does.
    fn executes_nothing(&self) {
        self.set("echo lint-ok");
    }
    /// Fails wherever it runs, the base included.
    fn fails(&self) {
        self.set(&format!(
            "{}\necho 'gate is red' >&2\nexit 3",
            reported_tests(3)
        ));
    }
    fn runs(&self) -> usize {
        fs::read_to_string(&self.runs).map_or(0, |runs| runs.lines().count())
    }
}
fn reported_tests(tests: u64) -> String {
    format!(
        "printf '{{\"schema_version\":1,\"selection\":{{\"packages\":[\"fixture\"]}},\
         \"tests_run\":{tests}}}' > \"$ORBIT_VALIDATION_SUMMARY\""
    )
}
fn bwrap_notice() -> String {
    orbit_exec::bwrap_deferral_notice(
        "spawn_under_linux_bwrap",
        "bwrap: No permissions to create new namespace",
    )
}
fn seatbelt_notice() -> String {
    orbit_exec::seatbelt_deferral_notice(
        "cli_ctrl_c_during_proc_spawn_reports_interrupt",
        "permissive profile exited with Some(71): sandbox-exec: sandbox_apply: Operation not \
         permitted",
    )
}
fn ps_notice() -> String {
    orbit_common::test_env::ps_denied_deferral_notice(
        "run_observation::cancelling_owners_with_pre_change_ps_tokens_still_signals_them",
        "the sandbox denied running ps: Operation not permitted (os error 1)",
    )
}
const GATE_SUMMARY: &str = ".orbit/tmp/gate-summary.json";
/// Write the summary of the gate run `record` describes, as `make ci-test-affected` writes it, and return the handoff that names it [ORB-15516].
fn handoff_for(fx: &Fixture, record: &Value) -> Value {
    let mut summary = record.clone();
    let fields = summary.as_object_mut().unwrap();
    fields.remove("probe");
    fields.remove("other_probes");
    fields.insert("schema_version".to_string(), json!(1));
    fields.insert("selection".to_string(), json!({ "packages": ["fixture"] }));
    let path = fx.repo.join(GATE_SUMMARY);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&summary).unwrap()).unwrap();
    let exclude = fx.repo.join(".git/info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    let excluded = fs::read_to_string(&exclude).unwrap_or_default();
    if !excluded.lines().any(|line| line == "/.orbit/") {
        fs::write(&exclude, format!("{excluded}/.orbit/\n")).unwrap();
    }
    json!({ "summary": GATE_SUMMARY })
}
fn probe(command: &str, exit_code: i64, output: &str) -> Value {
    json!({"command": command, "exit_code": exit_code, "output": output})
}
fn deferral_with(
    fx: &Fixture,
    command: &str,
    tests_run: u64,
    notices: Value,
    probe: Value,
) -> Value {
    json!({"command": command, "exit_code": 0, "tests_run": tests_run, "base": fx.base_sha, "notices": notices, "probe": probe})
}
/// The accepted macOS record for this run's gate and base [ORB-15409].
fn seatbelt_deferral(fx: &Fixture, command: &str) -> Value {
    deferral_with(
        fx,
        command,
        1650,
        json!([seatbelt_notice()]),
        probe(
            SEATBELT_APPLY_PROBE,
            71,
            "sandbox-exec: sandbox_apply: Operation not permitted",
        ),
    )
}
/// The accepted macOS claimed-leaf record for ps-denied tests [ORB-15587].
fn ps_deferral(fx: &Fixture, command: &str) -> Value {
    deferral_with(
        fx,
        command,
        1650,
        json!([ps_notice()]),
        probe(
            PS_EXEC_PROBE,
            71,
            "sandbox-exec: execvp() of '/bin/ps' failed: Operation not permitted",
        ),
    )
}
/// The accepted broad-gate record printing both sandboxes' notices [ORB-15692].
fn mixed_deferral(fx: &Fixture, command: &str) -> Value {
    let mut record = seatbelt_deferral(fx, command);
    record["notices"] = json!([seatbelt_notice(), ps_notice(), seatbelt_notice()]);
    record["other_probes"] = json!([ps_deferral(fx, command)["probe"].clone()]);
    record
}
fn deferral(fx: &Fixture, command: &str) -> Value {
    deferral_with(
        fx,
        command,
        2164,
        json!([bwrap_notice()]),
        probe(
            BUBBLEWRAP_NAMESPACE_PROBE,
            1,
            "bwrap: No permissions to create new namespace",
        ),
    )
}
/// Run the shipped PR pipeline's `implement_bundle` and `validate` steps once, between scripted worktree, resume, commit and base-sync steps.
fn run_implementation(
    fx: &Fixture,
    host: &DeliveryHost,
    implementation: Option<Value>,
) -> Result<(), String> {
    run_shipped_implementation(fx, host, implementation, false)
}
/// [`run_implementation`] through the claimed PR pipeline, whose implementer runs in claimed mode and whose `validate` is the engine's own `claim_validate`, on this claim host before publication [ORB-15587].
fn run_claimed_implementation(
    fx: &Fixture,
    host: &DeliveryHost,
    implementation: Value,
) -> Result<(), String> {
    run_shipped_implementation(fx, host, Some(implementation), true)
}
fn run_shipped_implementation(
    fx: &Fixture,
    host: &DeliveryHost,
    implementation: Option<Value>,
    claimed: bool,
) -> Result<(), String> {
    let (pipeline, validate) = if claimed {
        ("task_claimed_pr_pipeline.yaml", "claim_validate")
    } else {
        ("task_pr_pipeline.yaml", "candidate_validate")
    };
    let implemented = implementation.is_some();
    host.script_all(&rows_by_name! {
        "test_stub_worktree" => (json!({"job_run_id": RUN_ID, "workspace_path": fx.repo, "base_sha": fx.base_sha}));
        "test_stub_resume_candidate" => (json!({"implement": implemented, "repair": null}));
        "test_stub_implement" => (implementation.unwrap_or(Value::Null));
        "test_stub_commit" => (json!({"skipped_no_diff_expected": false}));
        "test_stub_sync_base" => (json!({"base_sha": fx.base_sha, "base_ref": BASE}))
    });
    let shipped = shipped_job(pipeline);
    let step = |id: &str| {
        let mut step = shipped
            .steps
            .iter()
            .find(|step| step.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("the shipped PR pipeline has `{id}`"));
        step.recovery_activity = None;
        if let JobV2StepBody::Loop { loop_ } = &mut step.body {
            for nested in &mut loop_.steps {
                nested.recovery_activity = None;
            }
        }
        step
    };
    let mut job = job_asset(json!([
        stub_step("worktree"),
        stub_step("resume_candidate")
    ]));
    job.steps.push(step("implement_bundle"));
    let mut commit = stub_step("commit");
    if implemented {
        // What the commit step sees of the implementer's output.
        commit["default_input"] = json!({ "implementation": "{{ steps.implement_one.output }}" });
    }
    job.steps
        .extend(job_asset(json!([commit, stub_step("sync_base")])).steps);
    job.steps.push(step("validate"));
    let mut catalog = V2ActivityCatalog::new();
    for (name, action) in [
        ("agent_implement", "test_stub_implement"),
        (validate, validate),
    ] {
        catalog.insert(
            name.to_string(),
            ActivityV2 {
                description: format!("`{name}` in the deferral graph"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: action.to_string(),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    run_job(host, &job)
}
fn an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let head_before = fx.head();
    let remote_before = fx.remote_tip(BRANCH);
    fx.write_repo("src/wip.txt", "still drafting\n");
    let message = task_blocked_by_agent_message(&AgentBlocker {
        kind: "environment".to_string(),
        evidence: "the toolchain the task needs is not installed".to_string(),
    });
    let handoff = failure_handoff_ok(
        &fx,
        &host,
        "implement_bundle",
        "task_blocked_by_agent",
        &message,
    );
    assert_eq!(handoff["decision"], "blocked_by_agent");
    assert_eq!(handoff["blocker_kind"], "environment");
    assert_eq!(handoff["head_sha"], head_before);
    assert_held_without_pr(&fx, &handoff, &remote_before, "blocker");
    assert_eq!(fx.head(), head_before, "the dirty tree is not committed");
    assert_eq!(
        fs::read_to_string(fx.repo.join("src/wip.txt")).unwrap(),
        "still drafting\n"
    );
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    let (_, event, note) = locked(&host.status_events).last().cloned().unwrap();
    assert_eq!(event.as_deref(), Some("task_blocked_by_agent"));
    let note = note.unwrap();
    assert!(note.contains("kind=environment") && is_task_blocked_by_agent(None, Some(&note)));
    assert!(
        host.comments(TASK_ID)
            .iter()
            .any(|comment| comment.message.contains("kind=environment")),
        "the task comment records the kind"
    );
}
/// [ORB-14266] A run its provider failed did not judge the candidate.
fn a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let head_before = fx.head();
    let remote_before = fx.remote_tip(BRANCH);
    fx.write_repo("src/wip.txt", "half written\n");
    let message = format!(
        "step `implement_one`: {}",
        provider_failure_text(
            ProviderFailureClass::Refusal,
            "codex",
            "cli subprocess exited with code 1: codex provider refused the request: \
             This content was flagged for possible cybersecurity risk.",
        )
    );
    let handoff = failure_handoff_ok(&fx, &host, "implement_one", "provider_refusal", &message);
    assert_eq!(handoff["decision"], "held_provider_failure");
    assert_eq!(handoff["provider_failure"], "provider_refusal");
    assert_eq!(fx.git(&["show", "HEAD:src/wip.txt"]), "half written");
    assert_committed_held(
        &fx,
        &host,
        &handoff,
        &head_before,
        &remote_before,
        "provider",
    );
}
fn an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let head_before = fx.head();
    let remote_before = fx.remote_tip(BRANCH);
    fx.write_repo("src/wip.txt", "reviewer fix in progress\n");
    let blocker = AgentBlocker {
        kind: "orbit.generation_switch_pending".to_string(),
        evidence: "upgrade admission refused: cannot record the pending generation \
                   switch: Operation not permitted (os error 1)"
            .to_string(),
    };
    let message = format!(
        "{}\noriginal error before recovery: the review report was never recorded",
        blocker.step_failure_message()
    );
    let handoff = failure_handoff_ok(&fx, &host, "review", "upgrade_pending", &message);
    assert_eq!(handoff["decision"], "held_upgrade_pending");
    assert_eq!(handoff["blocker_kind"], "orbit.generation_switch_pending");
    assert_committed_held(
        &fx,
        &host,
        &handoff,
        &head_before,
        &remote_before,
        "upgrade",
    );
}
/// A delivery its gate stopped waiting for [ORB-15643] is cancelled mid-validation.
fn a_child_its_parent_stopped_waiting_for_holds_its_candidate(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let head_before = fx.head();
    let remote_before = fx.remote_tip(BRANCH);
    fs::write(
        fx.repo.join("src/wip.txt"),
        "implementation under validation\n",
    )
    .unwrap();
    let handoff = failure_handoff_ok(
        &fx,
        &host,
        "validate",
        "child_timeout",
        "child_timeout: parent run jrun-parent stopped waiting for child run jrun-child after 14400 s",
    );
    assert_eq!(handoff["decision"], "held_child_timeout");
    assert_committed_held(&fx, &host, &handoff, &head_before, &remote_before, "child");
}
// A red base and network flakes [ORB-14258]
const RED_LINT: &str = "make ci-lint";
/// Normal child output cannot impersonate a supervisor stop [ORB-15507]. The synchronized base prints the same note but passes the command.
fn printed_supervisor_notes_remain_candidate_failures_on_both_streams(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    for note in [
        "process output capture limit exceeded on stderr",
        "process interrupted by signal SIGTERM",
        "process terminated with signal: 9 (SIGKILL)",
    ] {
        for redirect in ["", " >&2"] {
            let command = format!(
                "printf '%s\\n' '{note}'{redirect}; \
                 if test -f src/feature.txt; then exit 1; else exit 0; fi"
            );
            host.require_commands(&[&command]);
            let message = action_err(&host, "candidate_validate", &fx.validate_input()).to_string();
            assert!(!message.contains("[validation_environment]") && !baseline_red(&message));
            let log = host.run_log("0.json");
            assert_eq!(log["exit_code"], 1, "{command}: {log}");
            assert_eq!(log["timed_out"], false, "{command}: {log}");
            assert_eq!(log["failure_kind"], "candidate", "{command}: {log}");
            assert_eq!(log["baseline"]["decision"], "passed", "{command}: {log}");
            let base = host.run_log("0.baseline.json");
            assert_eq!(base["exit_code"], 0, "the healthy base ran: {base}");
            assert_eq!(base["base_sha"], fx.base_sha, "{base}");
            let claim_input = fx.claim_input();
            let message = action_err(&host, "claim_validate", &claim_input).to_string();
            assert!(!message.contains("[validation_environment]") && !baseline_red(&message));
            let claim = host.claim_log("validation/claim-landing/0.failed.json");
            assert_eq!(claim["exit_code"], 1, "{command}: {claim}");
            assert_eq!(claim["timed_out"], false, "{command}: {claim}");
            assert_eq!(claim["failure_kind"], "candidate", "{command}: {claim}");
            assert_eq!(claim["baseline"]["decision"], "passed");
        }
    }
    assert_eq!(fx.head(), fx.candidate);
}
fn supervisor_stops_are_environment_failures_and_never_poison_the_base_cache(sandbox: &Path) {
    // The supervisor restores and re-raises an incoming SIGTERM after cleanup. A harmless prior handler keeps this isolated fixture alive to inspect the returned classification; exec resets the caught disposition in the actual shell child.
    extern "C" fn retain_fixture_process(_signal: libc::c_int) {}
    let previous = unsafe {
        libc::signal(
            libc::SIGTERM,
            retain_fixture_process as *const () as libc::sighandler_t,
        )
    };
    assert_ne!(previous, libc::SIG_ERR);
    // Pin the supervisor, not just the command's environment: the drain may have raised this ceiling as an operational mitigation.
    unsafe { std::env::set_var("ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES", "4096") };
    // A capture stop still names the first failure before it, so a flood of one spawn failure says what failed [ORB-15870].
    let first_failure = "spawn /tmp/orbit-build/debug/orbit: No such file or directory";
    for (index, (stop, cause)) in [
        (
            "printf '%s\\n' \"thread 'spawns_orbit' (7) panicked at tests/cli.rs:4:29:\" \
             'spawn /tmp/orbit-build/debug/orbit: No such file or directory (os error 2)' >&2\n\
             printf '%8192s' '' >&2",
            "process output capture limit exceeded on stderr",
        ),
        ("kill -KILL $$", "process terminated with"),
        (
            "kill -TERM $PPID; sleep 60",
            "process interrupted by signal SIGTERM",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for base_only in [false, true] {
            let root = sandbox.join(format!("stop-{index}-{base_only}"));
            fs::create_dir_all(&root).unwrap();
            let runs = root.join("runs");
            let mut fx = Fixture::new(&root);
            fx.commit_on_base(
                "stop.sh",
                &format!("#!/bin/sh\necho ran >> '{}'\n{stop}\n", runs.display()),
            );
            fx.rebase_candidate();
            let candidate = if base_only {
                fx.commit(
                    "stop.sh",
                    &format!("#!/bin/sh\necho ran >> '{}'\nexit 137\n", runs.display()),
                )
            } else {
                fx.candidate.clone()
            };
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&["exec sh stop.sh"]);
            for attempt in 1..=2 {
                let message =
                    action_err(&host, "candidate_validate", &fx.validate_input()).to_string();
                assert!(message.contains("[validation_environment]") && message.contains(cause));
                assert_eq!(message.contains(first_failure), index == 0 && !base_only);
                assert!(!baseline_red(&message), "{message}");
                let log = host.run_log("0.json");
                assert_eq!(log["failure_kind"], "environment", "{log}");
                if base_only {
                    assert_eq!(log["baseline"]["decision"], "inconclusive", "{log}");
                    assert_eq!(log["baseline"]["cached"], false, "{log}");
                } else {
                    assert!(log["baseline"].is_null());
                }
                assert_eq!(
                    fs::read_to_string(&runs).unwrap().lines().count(),
                    attempt * if base_only { 2 } else { 1 },
                    "a stopped base must rerun instead of answering from a poisoned cache"
                );
            }
            assert_eq!(fx.head(), candidate, "validation preserves the candidate");
        }
    }
}
fn a_required_command_red_on_the_base_holds_the_task_without_a_pr(sandbox: &Path) {
    let runs = sandbox.join("lint-runs");
    let fx = Fixture::with_red_lint(sandbox, &runs);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    host.require_commands(&[RED_LINT]);
    let remote_before = fx.remote_tip(BRANCH);
    let mut input = fx.validate_input();
    input["base_ref"] = json!(BASE);
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| action(&host, "candidate_validate", &input));
        let second = scope.spawn(|| action(&host, "candidate_validate", &input));
        [first.join().unwrap(), second.join().unwrap()]
    });
    let messages = results
        .into_iter()
        .map(|result| result.unwrap_err().to_string())
        .collect::<Vec<_>>();
    for message in &messages {
        assert!(baseline_red(message));
    }
    assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 3);
    let hold = BaselineRedHold::from_text(&messages[0]).unwrap();
    assert_eq!(
        hold.failures.identities,
        ["README.md:1:1: error: lint is red"]
    );
    assert!(
        hold.failures
            .tail
            .iter()
            .all(|line| !line.starts_with("Required validation PATH=")),
        "{hold:?}"
    );
    assert!(
        messages[0].contains("failing:") && messages[0].contains("lint is red"),
        "the failure names what fails on the base: {}",
        messages[0]
    );
    assert_eq!(
        hold,
        BaselineRedHold {
            base_ref: BASE.to_string(),
            base_sha: fx.base_sha.clone(),
            command: RED_LINT.to_string(),
            run_id: RUN_ID.to_string(),
            selection: None,
            failures: hold.failures.clone(),
        }
    );
    let log = host.run_log("0.json");
    assert_eq!(log["exit_code"], 2);
    assert_eq!(log["failure_kind"], "baseline_red");
    assert_eq!(log["baseline"]["decision"], "failed");
    assert_eq!(log["baseline"]["exit_code"], 2);
    let base_log = host.run_log("0.baseline.json");
    assert_eq!(base_log["base_sha"], fx.base_sha.as_str());
    assert_eq!(base_log["passed"], false);
    assert!(
        base_log["output"]
            .as_str()
            .is_some_and(|output| output.contains("error: lint is red")),
        "{base_log}"
    );
    let handoff = failure_handoff_ok(&fx, &host, "validate", "baseline_red", &messages[0]);
    assert_eq!(handoff["decision"], "held_baseline_red");
    assert_held_without_pr(&fx, &handoff, &remote_before, "baseline-red");
    assert_eq!(fx.head(), fx.candidate, "the candidate is kept");
    assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
    let (_, event, note) = locked(&host.status_events).last().cloned().unwrap();
    assert_eq!(event.as_deref(), Some(BASELINE_RED_HOLD_EVENT));
    assert_eq!(
        note.as_deref().and_then(BaselineRedHold::from_text),
        Some(hold.clone())
    );
    match baseline_hold_status(&host, &fx.repo, &hold) {
        BaselineHoldStatus::Holding(reason) => assert!(
            reason.contains("lint is red"),
            "the verdict names what fails on the base: {reason}"
        ),
        lifted => panic!("the base ref still points at the red commit: {lifted:?}"),
    }
    fx.commit_on_base("Makefile", "ci-lint:\n\t@echo lint-ok\n");
    assert!(
        matches!(
            baseline_hold_status(&host, &fx.repo, &hold),
            BaselineHoldStatus::Lifted(_)
        ),
        "the base moved to a commit where the command passes"
    );
}
const RED_TESTS: &str = "make ci-test";
/// A known-red case: its name, the base's tests, the candidate's tests (`None` fails exactly as the base), and the tests it proceeds with as known red (`None` requires candidate repair).
type KnownRedCase<'a> = (
    &'a str,
    &'a [&'a str],
    Option<&'a [&'a str]>,
    Option<&'a [&'a str]>,
);
// Reuse the existing runner and suite identities for the observed six-vs-one shape and mixed failures. Both delivery routes must make the same decision.
const BOTH_RED: &[&str] = &["suite::broken", "suite::flaky"];
const SIX_RED: &[&str] = &[
    "orbit-core suite::broken",
    "orbit-core suite::flaky",
    "orbit-core suite::added",
    "orbit-engine suite::broken",
    "orbit-engine suite::flaky",
    "orbit-engine suite::added",
];
const KNOWN_RED_CASES: &[KnownRedCase<'_>] = &rows_by_name! {
    "equal" => (BOTH_RED, None, Some(BOTH_RED));
    "subset" => (BOTH_RED, Some(&["suite::flaky"]), Some(&["suite::flaky"]));
    "shared-plus-new" => (BOTH_RED, Some(&["suite::broken", "suite::flaky", "suite::added"]), None);
    "disjoint-six-vs-one" => (&["suite::broken"], Some(SIX_RED), None);
    "candidate-unknown" => (&["suite::broken"], Some(&[]), None);
    "base-unknown" => (&[], Some(&["suite::added"]), None);
    "both-unknown" => (&[], None, None)
};
fn named_tests(tests: &[&str]) -> Vec<String> {
    tests
        .iter()
        .map(|test| format!("test orbit-core::runtime {test}"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn nextest_failures(tests: &[&str]) -> String {
    tests
        .iter()
        .enumerate()
        .map(|(index, test)| {
            format!(
                "        FAIL [   0.512s] ({}/{}) orbit-core::runtime {test}\n",
                index + 1,
                tests.len()
            )
        })
        .collect()
}
/// [ORB-15375] A candidate whose failing tests all fail on a red base — the same tests, or some of them — introduced none of them: validation passes as `passed_with_known_red`, and the run's output, the log, the task's history and the pull request name the known-red tests.
fn a_candidate_failing_only_known_red_tests_proceeds_and_records_them(sandbox: &Path) {
    let mut legacy_history = false;
    for &(case, base, candidate_tests, known_red) in KNOWN_RED_CASES {
        let fx = Fixture::with_red_tests(sandbox, base, candidate_tests);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        host.require_commands(&[RED_TESTS]);
        let mut input = fx.validate_input();
        input["base_ref"] = json!(BASE);
        let outcome = action(&host, "candidate_validate", &input);
        let log = host.run_log("0.json");
        let recorded = host
            .history(TASK_ID)
            .into_iter()
            .filter(|entry| entry.event == KNOWN_RED_VALIDATION_EVENT)
            .map(|entry| match entry.decision {
                Some(TaskDecision::KnownRedValidation(known)) => {
                    assert!(
                        entry
                            .note
                            .as_deref()
                            .is_some_and(|note| note.starts_with(&known.text(""))),
                        "{case}: the note still carries the record: {:?}",
                        entry.note
                    );
                    known
                }
                other => panic!("{case}: the known red is a typed decision: {other:?}"),
            })
            .collect::<Vec<_>>();
        let Some(known_red) = known_red else {
            let message = outcome
                .expect_err(&format!("{case}: candidate failures need repair"))
                .to_string();
            assert!(
                !baseline_red(&message) && !message.contains("failed identically"),
                "different or unknown failures cannot hold for a red base: {case}: {message}"
            );
            assert_eq!(log["failure_kind"], "candidate", "{case}");
            assert_eq!(log["baseline_reproduces"], false, "{case}");
            assert_eq!(
                log["failures"],
                json!(named_tests(candidate_tests.unwrap_or(base))),
                "{case}"
            );
            let baseline =
                host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.baseline.json"));
            assert_eq!(
                baseline["failures"],
                json!(
                    named_tests(base)
                        .into_iter()
                        .chain(base.first().map(|test| format!("test {test}")))
                        .collect::<Vec<_>>()
                ),
                "{case}"
            );
            assert_eq!(
                log["baseline"]["log"],
                format!("validation/{RUN_ID}/0.baseline.json"),
                "{case}"
            );
            assert!(recorded.is_empty(), "{case}: {recorded:?}");
            continue;
        };
        let output = outcome.unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(output["decision"], "passed_with_known_red", "{case}");
        assert_eq!(output["commands"], json!([]), "{case}");
        let record = KnownRedValidation {
            command: RED_TESTS.to_string(),
            base_sha: fx.base_sha.clone(),
            tested_head: fx.candidate.clone(),
            run_id: RUN_ID.to_string(),
            failures: recorded[0].failures.clone(),
        };
        assert_eq!(record.failures.identities, named_tests(known_red), "{case}");
        assert_eq!(output["known_red"], json!([record]), "{case}");
        assert_eq!(log["failure_kind"], "known_red", "{case}");
        assert_eq!(
            log["known_red"]["identities"],
            json!(named_tests(known_red)),
            "{case}"
        );
        assert_eq!(recorded, vec![record], "{case}");
        // Every other case reads history an older binary wrote: the record in the note alone, which `pr_open` still lists.
        legacy_history = !legacy_history;
        if legacy_history {
            for entry in locked(&host.history).values_mut().flatten() {
                entry.decision = None;
            }
        }
        action(
            &host,
            "pr_open",
            &fx.open_input(&fx.candidate, &fx.base_sha),
        )
        .unwrap_or_else(|error| panic!("{case}: open the PR: {error}"));
        let body = fx.forge_state("pr-body").unwrap();
        assert!(body.contains("## Known-red validation"), "{case}: {body}");
        assert!(
            body.contains(&format!("`{RED_TESTS}` fails on base `{}`", fx.base_sha)),
            "{case}: {body}"
        );
        for test in known_red {
            assert!(
                body.contains(&format!("`orbit-core::runtime {test}`")),
                "{case}: {body}"
            );
        }
    }
}
/// [ORB-15548] Shared failed tests do not waive unexecuted host coverage.
fn known_red_coverage_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        let base = ["suite::broken", "suite::flaky"];
        let (su, ss) = (Some("sandbox_unavailable"), Some("self_skipped"));
        let s = |text: &str| Some(text.to_string());
        let cases = rows_by_name! {
            "bubblewrap" => (Some(bwrap_notice()), false, false, Some(3), su);
            "seatbelt" => (Some(seatbelt_notice()), false, false, Some(3), su);
            "deferred" => (s("DEFERRED: sandbox fixture did not run"), false, false, Some(3), ss);
            "skip" => (s("SKIP: sandbox fixture did not run"), false, false, Some(3), ss);
            "skipping-colon" => (s("skipping: sandbox fixture"), false, false, Some(3), ss);
            "skipping-space" => (s("skipping sandbox fixture"), false, false, Some(3), ss);
            "raw-refusal" => (s("bwrap: No permissions to create new namespace"), false, false, Some(3), su);
            "dropped-deferral" => (Some(bwrap_notice()), true, false, Some(3), su);
            "dropped-skip" => (s("SKIP: sandbox fixture did not run"), true, false, Some(3), ss);
            "replay-with-notice" => (Some(bwrap_notice()), false, true, Some(3), su);
            "replay-zero-tests" => (None, false, true, Some(0), None);
            "replay-missing-count" => (None, false, true, None, None);
            "replay-missing-tool" => (None, false, true, None, None);
            "replay-executed" => (None, false, true, Some(3), None)
        };
        for (case, notice, dropped, replay, tests_run, reason) in
            cases.into_iter().filter(|row| selected.contains(&row.0))
        {
            let fx = Fixture::with_red_tests(sandbox, &base, Some(&base));
            let padding = if dropped {
                "padding line that names no failure\n".repeat(8 * 1024)
            } else {
                String::new()
            };
            // Alternate complete stdout and stderr, including notices in the dropped middle, without depending on sandbox availability.
            let stream = if case == "skip" || dropped {
                ""
            } else {
                " >&2"
            };
            let notice_script = if dropped {
                "cat failing.txt\n".to_string()
            } else {
                notice.as_ref().map_or_else(String::new, |notice| {
                    format!("printf '%s\\n' '{notice}'{stream}\n")
                })
            };
            if dropped {
                fx.commit(
                    "failing.txt",
                    &format!("{padding}{}\n{padding}", notice.as_ref().unwrap()),
                );
            }
            let gate = Gate::new(sandbox);
            gate.set(&format!(
                "{notice_script}{}\n{}\nexit 2",
                nextest_failures(&base)
                    .lines()
                    .map(|line| format!("echo '{line}'\n"))
                    .collect::<String>(),
                tests_run.map_or_else(String::new, |count| {
                    if case == "replay-executed" {
                        format!(
                            "selection=\"$ORBIT_VALIDATION_SELECTION\"; [ -n \"$selection\" ] || selection='{{\"packages\":[\"fixture\"]}}'; \
                             printf '{{\"schema_version\":1,\"selection\":%s,\"tests_run\":{count},\"executed_tests\":[\"orbit-core::runtime suite::broken\",\"orbit-core::runtime suite::flaky\"]}}' \
                             \"$selection\" > \"$ORBIT_VALIDATION_SUMMARY\""
                        )
                    } else {
                        reported_tests(count)
                    }
                }),
            ));
            if case == "replay-missing-tool" {
                gate.set(&format!(
                    "'{}'",
                    sandbox.join("missing-required-tool").display()
                ));
            }
            let command = gate.command();
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&[&command]);
            let record = deferral(&fx, &command);
            if replay {
                attach_deferred(&host, &record);
            }
            let outcome = action(&host, "candidate_validate", &fx.validate_input());
            let log = host.run_log("0.json");
            assert_eq!(
                log["exit_code"],
                if case == "replay-missing-tool" {
                    127
                } else {
                    2
                },
                "{case}"
            );
            assert_eq!(log["deferred_sandbox_replay"], replay, "{case}");
            let mut claim_input = fx.claim_input();
            if replay {
                claim_input["implementation"] = json!({ "deferred_sandbox_validation": record });
            }
            let claim = action(&host, "claim_validate", &claim_input);
            if notice.is_none() && tests_run == Some(3) {
                assert_eq!(
                    outcome.unwrap()["decision"],
                    "passed_with_known_red",
                    "{case}"
                );
                assert_eq!(
                    claim.unwrap()["decision"],
                    "passed_with_known_red",
                    "{case}"
                );
                continue;
            }
            let failure = outcome
                .expect_err(&format!("{case}: unresolved coverage is refused"))
                .to_string();
            let claim_failure = claim
                .expect_err(&format!("{case}: claimed coverage is refused"))
                .to_string();
            if let Some(reason) = reason {
                assert!(baseline_red(&failure), "{case}: {failure}");
                assert!(baseline_red(&claim_failure), "{case}: {claim_failure}");
                assert_eq!(log["host_output_refusal"]["reason"], reason, "{case}");
                assert_eq!(
                    log["host_output_refusal"]["detail"],
                    notice.as_deref().unwrap(),
                    "{case}"
                );
                let claim_log = host.claim_log("validation/claim-landing/0.failed.json");
                assert_eq!(
                    claim_log["host_output_refusal"], log["host_output_refusal"],
                    "{case}"
                );
                if dropped {
                    let captured = log["output"].as_str().unwrap();
                    assert!(
                        captured.contains("truncated")
                            && !captured.contains(notice.as_deref().unwrap()),
                        "{case}: the retained refusal comes from the dropped middle"
                    );
                }
            } else if case == "replay-missing-tool" {
                assert!(env_failure(&failure), "{case}: {failure}");
                assert!(env_failure(&claim_failure), "{case}: {claim_failure}");
                assert_eq!(log["failure_kind"], "environment", "{case}");
            } else {
                assert!(failure.contains("replayed nothing"), "{case}: {failure}");
                assert!(
                    claim_failure.contains("replayed nothing"),
                    "{case}: {claim_failure}"
                );
            }
            assert!(log["known_red"].is_null(), "{case}: no known-red admission");
            assert!(
                host.history(TASK_ID)
                    .iter()
                    .all(|entry| entry.event != KNOWN_RED_VALIDATION_EVENT),
                "{case}: no known-red history"
            );
            assert!(host.handoffs().is_empty(), "{case}");
        }
    });
}
/// [ORB-15427] A claimed leaf applies the owner's known-red rule.
fn claimed_known_red_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        for &(case, base, candidate_tests, known_red) in KNOWN_RED_CASES
            .iter()
            .filter(|row| selected.contains(&row.0))
        {
            let fx = Fixture::with_red_tests(sandbox, base, candidate_tests);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&[RED_TESTS]);
            let mut input = fx.claim_input();
            let outcome = action(&host, "claim_validate", &input);
            // The owner's own base run, not the one the leaf cached.
            let cache =
                PathBuf::from(fx.git(&["rev-parse", "--git-common-dir"])).join("orbit-baseline");
            let owner_check = |base_sha: &str, failures: &BTreeSet<String>| {
                if cache.exists() {
                    fs::remove_dir_all(&cache).unwrap();
                }
                verify_claimed_known_red(
                    &host,
                    &fx.repo,
                    &ClaimedKnownRed {
                        base_branch: BASE,
                        base_sync: BaseSyncMode::Local,
                        base_sha,
                        command: RED_TESTS,
                        selection: None,
                        // make's status for the recipe's failure
                        exit_code: 2,
                        failures,
                    },
                    std::time::Instant::now() + Duration::from_secs(120),
                )
            };
            let Some(known_red) = known_red else {
                let message = outcome
                    .expect_err(&format!("{case}: candidate failures need repair"))
                    .to_string();
                assert!(
                    !baseline_red(&message) && !message.contains("failed identically"),
                    "different or unknown failures cannot hold for a red base: {case}: {message}"
                );
                assert!(host.handoffs().is_empty(), "{case}");
                let logs = locked(&host.claim_log_contents);
                let log: Value =
                    serde_json::from_slice(&logs["validation/claim-landing/0.failed.json"])
                        .unwrap();
                let baseline: Value =
                    serde_json::from_slice(&logs["validation/claim-landing/0.baseline.json"])
                        .unwrap();
                assert_eq!(log["failure_kind"], "candidate", "{case}");
                assert_eq!(log["baseline_reproduces"], false, "{case}");
                assert_eq!(
                    log["failures"],
                    json!(named_tests(candidate_tests.unwrap_or(base))),
                    "{case}"
                );
                assert_eq!(
                    baseline["failures"],
                    json!(
                        named_tests(base)
                            .into_iter()
                            .chain(base.first().map(|test| format!("test {test}")))
                            .collect::<Vec<_>>()
                    ),
                    "{case}"
                );
                assert_eq!(
                    log["baseline"]["log"], "validation/claim-landing/0.baseline.json",
                    "{case}"
                );
                drop(logs);
                if base.is_empty() || candidate_tests.is_some_and(|tests| tests.is_empty()) {
                    continue;
                }
                let claimed = named_tests(candidate_tests.unwrap()).into_iter().collect();
                let refusal = owner_check(&fx.base_sha, &claimed)
                    .expect_err(&format!("{case}: the owner refuses the report"));
                assert!(
                    matches!(&refusal, KnownRedUnconfirmed::Contradicted(reason)
                            if reason.contains("does not fail `test orbit-core::runtime")),
                    "{case}: the owner's base run contradicts it: {refusal:?}"
                );
                continue;
            };
            let output = outcome.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert_eq!(output["decision"], "passed_with_known_red", "{case}");
            assert_eq!(output["commands"], json!([]), "{case}");
            let records: Vec<KnownRedValidation> =
                serde_json::from_value(output["known_red"].clone()).unwrap();
            assert_eq!(records.len(), 1, "{case}: {records:?}");
            assert_eq!(records[0].command, RED_TESTS, "{case}");
            assert_eq!(records[0].base_sha, fx.base_sha, "{case}");
            assert_eq!(records[0].tested_head, fx.candidate, "{case}");
            assert_eq!(
                records[0].failures.identities,
                named_tests(known_red),
                "{case}"
            );
            let path = "validation/claim-landing/0.json";
            let log: HandoffValidationLog =
                serde_json::from_slice(&locked(&host.claim_log_contents)[path]).unwrap();
            assert_eq!(log.exit_code, 2, "{case}: the log keeps the exit status");
            let report = log.known_red.unwrap();
            assert_eq!(report.base_sha, fx.base_sha, "{case}");
            assert!(
                named_tests(known_red)
                    .iter()
                    .all(|test| report.failures.contains(test)),
                "{case}: {report:?}"
            );
            assert_eq!(
                owner_check(&fx.base_sha, &report.failures),
                Ok(report.failures.clone()),
                "{case}: the owner's base run fails every reported test"
            );
            // A base off the owner's own base branch is no base: here the candidate itself, which fails the same tests. Nothing runs.
            let off_base = owner_check(&fx.candidate, &report.failures)
                .expect_err(&format!("{case}: a base off the owner's branch is refused"));
            assert!(
                matches!(&off_base, KnownRedUnconfirmed::Inconclusive(reason)
                if reason.contains(&format!(
                    "base {} is not in the history of this owner's {BASE}",
                    fx.candidate
                ))),
                "{case}: nothing ran, so nothing is contradicted: {off_base:?}"
            );
            assert!(!cache.exists(), "{case}: the owner ran nothing");
            // Before publication the results carry the report, and the pin attaches the same log for the published candidate.
            host.ship_pr();
            locked(&host.claim_log_contents).clear();
            let pending = action(&host, "claim_validate", &input)
                .unwrap_or_else(|error| panic!("{case}: validate before push: {error}"));
            assert_eq!(pending["publication"], "pending", "{case}");
            assert_eq!(pending["decision"], "passed_with_known_red", "{case}");
            assert_eq!(pending["known_red"], output["known_red"], "{case}");
            action(&host, "pr_open", &{
                let mut open = fx.open_input(&fx.candidate, &fx.base_sha);
                open["claim_validation"] = pending.clone();
                open
            })
            .unwrap_or_else(|error| panic!("{case}: open the PR: {error}"));
            let body = fx.forge_state("pr-body").unwrap();
            assert!(body.contains("## Known-red validation"), "{case}: {body}");
            for test in known_red {
                assert!(
                    body.contains(&format!("`orbit-core::runtime {test}`")),
                    "{case}: {body}"
                );
            }
            input["pull_request"] = json!(PR_NUMBER);
            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input)
                .unwrap_or_else(|error| panic!("{case}: pin: {error}"));
            assert_eq!(pinned["decision"], "passed_with_known_red", "{case}");
            let pinned_log: HandoffValidationLog =
                serde_json::from_slice(&locked(&host.claim_log_contents)[path]).unwrap();
            assert_eq!(pinned_log.exit_code, 2, "{case}");
            assert_eq!(pinned_log.known_red, Some(report), "{case}");
        }
    });
}
/// [ORB-15375] Captured output keeps only a head and a tail, so a failure in the middle of a long run never reaches the log.
fn a_failure_only_the_truncated_middle_names_still_requires_candidate_repair(sandbox: &Path) {
    let base = ["suite::broken", "suite::flaky"];
    let fx = Fixture::with_red_tests(sandbox, &base, Some(&base));
    // About 590 KB: past the 256 KiB recorded budget, under the 1 MiB supervisor capture limit that stops a run as an environment failure.
    let padding = "padding line that names no failure\n".repeat(8 * 1024);
    let middle_failure = format!(
        "{}--- STDOUT: orbit-core::runtime suite::added ---\n\
         scenario completion1335: cancellation during nested execution\n\
         --- STDERR: orbit-core::runtime suite::added ---\n\
         thread 'suite::added' panicked at fixture.rs:1335:9:\n\
         nested assertion failed: child completion must retain its cause\n\
         left: cancelled\nright: completed\n\
         PASS [   0.001s] orbit-core::runtime unrelated\n",
        nextest_failures(&["suite::added"])
    );
    let candidate = fx.commit(
        "failing.txt",
        &format!(
            "{}{padding}{middle_failure}{padding}{}",
            nextest_failures(&base[..1]),
            nextest_failures(&base[1..]),
        ),
    );
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    host.require_commands(&[RED_TESTS]);
    let message = action_err(&host, "candidate_validate", &fx.validate_input()).to_string();
    assert!(!baseline_red(&message), "{message}");
    let log = host.run_log("0.json");
    assert_eq!(log["tested_head"], candidate.as_str());
    let captured = log["output"].as_str().unwrap_or_default();
    assert!(captured.contains("truncated") && captured.len() <= 256 * 1024 + 1024);
    for detail in [
        "suite::added",
        "scenario completion1335",
        "nested assertion failed: child completion must retain its cause",
        "left: cancelled",
        "right: completed",
    ] {
        assert!(captured.contains(detail), "middle failure lost {detail}");
    }
    assert_eq!(log["base_sha"], fx.base_sha);
    let claim_input = json!({
        "workspace_path": fx.repo,
        "base_sync": "local",
        "base_sha": fx.base_sha,
    });
    let message = action_err(&host, "claim_validate", &claim_input).to_string();
    assert!(!baseline_red(&message), "{message}");
    let logs = locked(&host.claim_log_contents);
    let claim: Value =
        serde_json::from_slice(&logs["validation/claim-landing/0.failed.json"]).unwrap();
    assert_eq!(claim["tested_head"], candidate.as_str());
    assert_eq!(claim["base_sha"], fx.base_sha);
    assert_eq!(claim["failure_kind"], log["failure_kind"]);
    assert_eq!(claim["failures"], log["failures"]);
    assert_eq!(claim["output"], log["output"]);
    drop(logs);
    assert!(log["baseline"]["log"].as_str().is_some());
    assert_eq!(log["failure_kind"], "candidate");
    assert!(
        log["failures"]
            .as_array()
            .unwrap()
            .contains(&json!("test orbit-core::runtime suite::added"))
    );
    assert!(
        host.history(TASK_ID)
            .iter()
            .all(|entry| entry.event != KNOWN_RED_VALIDATION_EVENT)
    );
}
fn a_red_base_during_pr_revalidation_holds_the_task(sandbox: &Path) {
    let runs = sandbox.join("lint-runs");
    let fx = Fixture::with_red_lint(sandbox, &runs);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    host.require_commands(&[RED_LINT]);
    host.publish_pr(TASK_ID);
    host.set_status(TASK_ID, TaskStatus::Review);
    let remote_before = fx.remote_tip(BRANCH);
    let hold = BaselineRedHold {
        base_ref: BASE.to_string(),
        base_sha: fx.base_sha.clone(),
        command: RED_LINT.to_string(),
        run_id: RUN_ID.to_string(),
        selection: None,
        failures: Default::default(),
    };
    let diagnostic = hold.text("the rebased candidate shares the base failure");
    let handoff = failure_handoff_ok(
        &fx,
        &host,
        "re_review_validate",
        "baseline_red",
        &diagnostic,
    );
    assert_eq!(handoff["decision"], "held_baseline_red");
    assert_eq!(handoff["pr_created"], false);
    assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
    assert_eq!(fx.remote_tip(BRANCH), remote_before);
    let (_, event, note) = locked(&host.status_events).last().cloned().unwrap();
    assert_eq!(event.as_deref(), Some(BASELINE_RED_HOLD_EVENT));
    assert_eq!(
        note.as_deref().and_then(BaselineRedHold::from_text),
        Some(hold.clone())
    );
    assert!(
        host.comments(TASK_ID).last().is_some_and(|comment| comment
            .message
            .contains(&format!("existing PR #{PR_NUMBER} remains"))),
        "the existing PR is described as preserved at its last published head"
    );
    fx.commit_on_base("Makefile", "ci-lint:\n\t@echo lint-ok\n");
    assert!(matches!(
        baseline_hold_status(&host, &fx.repo, &hold),
        BaselineHoldStatus::Lifted(_)
    ));
}
fn a_network_flake_is_rerun_before_the_candidate_is_judged(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let attempts = sandbox.join("attempts");
    let flaky = format!(
        "echo try >> '{0}'; [ $(wc -l < '{0}') -gt 1 ] || \
         {{ echo 'curl: (6) Could not resolve host: forge.invalid' >&2; exit 6; }}",
        attempts.display()
    );
    host.require_commands(&[&flaky]);
    let validated = validate_ok(&fx, &host);
    assert_eq!(validated["decision"], "passed");
    assert_eq!(fs::read_to_string(&attempts).unwrap().lines().count(), 2);
    let log = host.run_log("0.json");
    assert_eq!(log["network_retries"], 1);
    assert_eq!(log["exit_code"], 0);
    // Only the candidate has src/feature.txt, so the base passes.
    let offline = sandbox.join("offline");
    let down = format!(
        "test ! -e src/feature.txt || {{ echo try >> '{}'; \
         echo 'curl: (7) Failed to connect to forge.invalid port 443' >&2; exit 7; }}",
        offline.display()
    );
    host.require_commands(&[&down]);
    let message = validate_err(&fx, &host);
    assert!(!baseline_red(&message), "{message}");
    assert_eq!(fs::read_to_string(&offline).unwrap().lines().count(), 3);
    let log = host.run_log("0.json");
    assert_eq!(log["network_retries"], 2);
    assert_eq!(log["baseline"]["decision"], "passed");
}
fn a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs(sandbox: &Path) {
    let fx = Fixture::with_red_lint(sandbox, &sandbox.join("lint-runs"));
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    host.require_commands(&[RED_LINT]);
    let input = fx.claim_input();
    let error = action_err(&host, "claim_validate", &input);
    let message = error.to_string();
    let hold = BaselineRedHold::from_text(&message).unwrap();
    assert_eq!(hold.base_ref, BASE);
    assert_eq!(hold.base_sha, fx.base_sha);
    assert_eq!(hold.command, RED_LINT);
    assert_eq!(
        *locked(&host.claim_logs),
        [
            "validation/claim-landing/0.failed.json",
            "validation/claim-landing/0.baseline.json"
        ]
    );
    assert!(host.handoffs().is_empty());
}
/// [ORB-14257] A claimed leaf's required command that still cannot reach the network after its reruns, on a base that passes, says nothing about the candidate: it fails typed `transient`, so the leaf releases its claim instead of blocking the task, and the owner is still sent both logs.
fn a_claimed_candidate_that_cannot_reach_the_network_fails_transient(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    // Only the candidate has src/feature.txt, so the base passes.
    host.require_commands(&["test ! -e src/feature.txt || \
         { echo 'curl: (6) Could not resolve host: forge.invalid' >&2; exit 6; }"]);
    let input = fx.claim_input();
    let error = action_err(&host, "claim_validate", &input);
    let message = error.to_string();
    assert_eq!(
        ClaimFailureClass::of_step_failure(None, Some(&message)),
        Some(ClaimFailureClass::Transient),
        "{message}"
    );
    assert!(!baseline_red(&message), "{message}");
    assert_eq!(
        *locked(&host.claim_logs),
        [
            "validation/claim-landing/0.failed.json",
            "validation/claim-landing/0.baseline.json"
        ]
    );
    assert!(host.handoffs().is_empty());
}
fn a_pr_claim_validates_before_publication_and_pins_after_it(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    host.ship_pr();
    let runs = sandbox.join("runs");
    let command = format!("echo ran >> '{}'; echo suite-ok", runs.display());
    host.require_commands(&[&command]);
    let mut input = fx.claim_input();
    let pending = action_ok(&host, "claim_validate", &input);
    assert_eq!(pending["decision"], "passed");
    assert_eq!(pending["publication"], "pending");
    assert_eq!(pending["candidate"], Value::Null);
    assert_eq!(pending["tested_head"], fx.candidate.as_str());
    assert!(locked(&host.claim_logs).is_empty(), "nothing attached");
    input["pull_request"] = json!(PR_NUMBER);
    let mut moved = pending.clone();
    moved["tested_head"] = json!(fx.base_sha);
    input["prevalidated"] = moved;
    let refused = action_err(&host, "claim_validate", &input);
    assert!(refused.to_string().contains("rerun validation"));
    input["prevalidated"] = pending;
    let pinned = action_ok(&host, "claim_validate", &input);
    assert_eq!(pinned["decision"], "passed");
    assert_eq!(pinned["validation"].as_array().unwrap().len(), 1);
    assert_eq!(locked(&host.claim_logs).len(), 1);
    assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 1);
    let handoff = json!({
        "workspace_path": fx.repo,
        "base_sync": "local",
        "base_sha": fx.base_sha,
        "pull_request": PR_NUMBER,
        "candidate": pinned["candidate"],
        "validation": pinned["validation"],
    });
    action_ok(&host, "claim_handoff", &handoff);
    assert_eq!(locked(&host.handoffs).len(), 1);
}
/// A launcher environment with only [`MINIMAL_PATH`], plus a substitute login shell whose profile prints a banner and prepends a toolchain directory holding [`PROFILE_TOOL`].
fn login_shell_toolchain(sandbox: &Path) -> (Vec<(String, String)>, PathBuf, PathBuf) {
    let home = sandbox.join("owner-home");
    let tools = home.join(".toolchain/bin");
    fs::create_dir_all(&tools).unwrap();
    let tool = tools.join(PROFILE_TOOL);
    fs::write(&tool, "#!/bin/sh\necho lint-ok\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let shell = home.join("login-shell");
    fs::write(
        &shell,
        format!(
            "#!/bin/sh\n[ \"$1\" = -i ] && shift\n\
             [ \"$1\" = -l ] && [ \"$2\" = -c ] || exit 64\n\
             echo 'Welcome back'\nPATH=\"{}:$PATH\"; export PATH\neval \"$3\"\n",
            tools.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    let launcher = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("PATH".to_string(), MINIMAL_PATH.to_string()),
    ];
    (launcher, shell, tools)
}
// Conflicting reviewed head at completion [ORB-13890]
fn a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    open_review(&fx, &host);
    fx.advance_base();
    fx.script_checks(&["dirty"]);
    let error = action_err(&host, "pr_complete", &fx.complete_input());
    assert!(error.to_string().contains("review_gate_stale"), "{error}");
    assert_eq!(fx.head(), fx.candidate, "the checkout is untouched");
    let mut input = fx.complete_input();
    input["re_review_on_conflict"] = json!(true);
    let rebased = action_ok(&host, "pr_complete", &input);
    assert_eq!(rebased["re_review_required"], true);
    assert_eq!(rebased["completed_task_ids"], json!([]));
    assert_eq!(rebased["merge"]["merged"], false);
    let checkpoint = &rebased["rebased"];
    let rebased_head = checkpoint["head_sha"].as_str().unwrap().to_string();
    assert_eq!(rebased_head, fx.head());
    assert_ne!(rebased_head, fx.candidate);
    assert_eq!(checkpoint["rewritten"], true);
    assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    assert_no_merge_or_landing(&fx, &host, TaskStatus::Review, "rebase");
    // The pipeline reviews the rebased head, then republishes it under the lease the rebase checkpoint carries.
    let pushed = action_ok(
        &host,
        "git_push",
        &json!({ "workspace_path": fx.repo, "job_run_id": RUN_ID, "completed_task_ids": [TASK_ID], "branch": checkpoint["head"], "rewrite_performed": checkpoint["rewritten"], "rewrite_head_before": checkpoint["head_sha_before"], "expected_remote_sha": checkpoint["remote_sha_before"], }),
    );
    assert_eq!(pushed["local_sha"], rebased_head.as_str());
    assert_eq!(fx.remote_tip(BRANCH), rebased_head);
    fx.script_checks(&["success"]);
    let mut reviewed = fx.complete_input();
    reviewed["published_head_sha"] = json!(rebased_head);
    reviewed["reviewed_head_sha"] = json!(rebased_head);
    let completed = action_ok(&host, "pr_complete", &reviewed);
    assert_eq!(completed["re_review_required"], false);
    assert_eq!(completed["merge"]["merged"], true);
    assert_eq!(fx.merge_requests(), squash(&rebased_head));
    assert_eq!(host.landings().len(), 1);
    assert_eq!(host.landings()[0].reviewed_head_sha, rebased_head);
    assert_eq!(host.status(TASK_ID), TaskStatus::Done);
}
// A base that advances after completion's conflict recovery [ORB-14393]
const COMPLETE_STEP: &str = "complete_pr";
const FEATURE: &str = "src/feature.txt";
/// ORB-14344's shape: completion's rebase conflicts on base B, recovery certifies the candidate on B, and the base advances to B' before the retry, which re-pins to B'.
fn a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.commit_on_base(FEATURE, "base change\n");
    let base = fx.local_tip(BASE);
    fx.script_checks(&["dirty", "dirty", "success"]);
    let input = completion_with_step(&fx);
    let conflict = completion_conflict(action(&host, "pr_complete", &input));
    assert_eq!(conflict.target_base_sha, base);
    let recovered = recover_completion(&fx, &host, &conflict, "resolved\n");
    fx.commit_on_base("other.txt", "advanced\n");
    let advanced = fx.local_tip(BASE);
    let forged = json!({
        "workspace_path": fx.repo,
        "job_run_id": RUN_ID,
        "completed_task_ids": [TASK_ID],
        "step_id": COMPLETE_STEP,
        "head": BRANCH,
        "head_sha": fx.candidate,
        "base": BASE,
        "base_ref": BASE,
        "base_sha": advanced,
        "remote_sha": "0".repeat(40),
        "commits_behind": 2,
        "sync_required": true,
    });
    let error = action_err(&host, "git_rebase", &forged);
    assert!(error.to_string().contains("provenance does not match"));
    assert_eq!(fx.head(), recovered, "the refusal touches nothing");
    let completed = action_ok(&host, "pr_complete", &input);
    let landed = fx.head();
    assert_ne!(landed, recovered);
    assert!(is_ancestor(&fx.repo, &advanced, &landed));
    assert_eq!(
        fs::read_to_string(fx.repo.join(FEATURE)).unwrap(),
        "resolved\n"
    );
    assert_eq!(completed["merge"]["merged"], true);
    assert_landed_exactly(&fx, &host, &landed, "landed");
    let certificates = host.recovery_certificates();
    let [_, (_, step, chased)] = certificates.as_slice() else {
        panic!("expected the recovery and its chase, got {certificates:#?}");
    };
    assert_eq!(step, COMPLETE_STEP);
    assert_eq!(chased["head_sha"], landed);
    assert_eq!(chased["head_sha_before"], fx.candidate);
    assert_eq!(chased["target_base_sha"], advanced);
    assert_eq!(chased["base_sha"], advanced);
    assert_eq!(chased["chased_from"], recovered);
    assert_eq!(chased["recovery_attempt"], 2);
}
fn conflicting_base_advances_after_recovery_are_chased_until_the_bound(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    open_for_completion(&fx, &host);
    fx.commit_on_base(FEATURE, "base change 0\n");
    fx.script_checks(&["dirty"]);
    let input = completion_with_step(&fx);
    let conflict = completion_conflict(action(&host, "pr_complete", &input));
    let mut recovered = recover_completion(&fx, &host, &conflict, "resolved 0\n");
    let mut chased = 0;
    let exhausted = loop {
        assert!(chased <= 5, "the chase must be bounded");
        fx.commit_on_base(FEATURE, &format!("base change {}\n", chased + 1));
        let advanced = fx.local_tip(BASE);
        let conflict = match action(&host, "pr_complete", &input) {
            Err(OrbitError::RecoverableVcsConflict(conflict)) => conflict,
            Err(error) => break error,
            Ok(output) => panic!("a conflicting chase cannot merge: {output:#}"),
        };
        assert_eq!(conflict.target_base_sha, advanced);
        assert_eq!(rebase_state(&fx.repo, "orig-head"), fx.candidate);
        assert!(conflict.diagnostic.contains(&recovered), "{conflict:?}");
        chased += 1;
        recovered = recover_completion(&fx, &host, &conflict, &format!("resolved {chased}\n"));
    };
    assert!(exhausted.to_string().contains("base_chase_exhausted:"));
    assert!(chased >= 1, "at least one more bounded recovery ran");
    assert_eq!(fx.head(), recovered, "the last certified recovery is kept");
    assert_eq!(fx.git(&["status", "--porcelain"]), "");
    assert_no_merge_or_landing(&fx, &host, TaskStatus::Review, "chase");
}
/// [`Fixture::complete_ungated_input`] as the dispatcher hands it to the completion step.
fn completion_with_step(fx: &Fixture) -> Value {
    let mut input = fx.complete_ungated_input();
    input["step_id"] = json!(COMPLETE_STEP);
    input
}
fn completion_conflict(outcome: Res<Value>) -> RecoverableVcsConflict {
    match outcome {
        Err(OrbitError::RecoverableVcsConflict(conflict)) => *conflict,
        other => panic!("expected a recoverable completion conflict, got {other:?}"),
    }
}
/// Resolve completion's stopped rebase with `resolution` and certify it the way conflict recovery's host continuation does, returning the recovered HEAD.
fn recover_completion(
    fx: &Fixture,
    host: &DeliveryHost,
    conflict: &RecoverableVcsConflict,
    resolution: &str,
) -> String {
    fs::write(fx.repo.join(FEATURE), resolution).unwrap();
    fx.git(&["add", FEATURE]);
    fx.git(&["-c", "core.editor=true", "rebase", "--continue"]);
    let head = fx.head();
    let attempt = host
        .begin_rebase_recovery_attempt(
            RUN_ID,
            COMPLETE_STEP,
            &RebaseRecoveryAttemptScope {
                workspace_path: path_str(&fx.repo).to_string(),
                head_sha_before: fx.candidate.clone(),
                target_base_sha: conflict.target_base_sha.clone(),
            },
        )
        .unwrap();
    host.checkpoint_rebase_recovery(
        RUN_ID,
        COMPLETE_STEP,
        &json!({
            "run_id": RUN_ID,
            "step_id": COMPLETE_STEP,
            "task_ids": [TASK_ID],
            "workspace_path": fx.repo,
            "head": BRANCH,
            "head_sha_before": fx.candidate,
            "original_base_sha": conflict.original_base_sha,
            "base_ref": format!("refs/heads/{BASE}"),
            "target_base_sha": conflict.target_base_sha,
            "base_sha": conflict.target_base_sha,
            "remote_sha_before": fx.candidate,
            "head_sha": head,
            "companion_paths": [],
            "rewritten": true,
            "recovery_attempt": attempt,
        }),
    )
    .unwrap();
    head
}
/// A file of the checkout's stopped rebase.
fn rebase_state(checkout: &Path, name: &str) -> String {
    let path = git(
        checkout,
        &["rev-parse", "--git-path", &format!("rebase-merge/{name}")],
    );
    fs::read_to_string(checkout.join(path))
        .unwrap()
        .trim()
        .to_string()
}
fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    crate::git_test_support::command()
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}
fn required_validation_without_commands_is_a_no_op(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    fx.write_repo("untracked.txt", "left behind\n");
    let validated = validate_ok(&fx, &host);
    assert_eq!(validated["decision"], "skipped_no_required_commands");
    assert_eq!(validated["validation"], json!([]));
    assert!(locked(&host.artifacts).is_empty());
}
fn claimed_validation_without_commands_is_a_recorded_no_op(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let input = fx.claim_input();
    let validated = action_ok(&host, "claim_validate", &input);
    assert_eq!(validated["decision"], "skipped_no_required_commands");
    assert!(validated["note"].is_string(), "{validated}");
    assert_eq!(validated["commands"], json!([]));
    assert_eq!(validated["validation"], json!([]));
    assert_eq!(validated["tested_head"], fx.candidate.as_str());
    assert!(locked(&host.claim_logs).is_empty());
    let mut handoff_input = input.clone();
    handoff_input["candidate"] = validated["candidate"].clone();
    handoff_input["validation"] = validated["validation"].clone();
    let handed = action_ok(&host, "claim_handoff", &handoff_input);
    assert_eq!(handed["handed_off"], true);
    assert_eq!(handed["merged"], false);
    let handoffs = locked(&host.handoffs);
    assert_eq!(handoffs.len(), 1);
    assert!(handoffs[0].validation.is_empty());
    assert_eq!(handoffs[0].candidate.candidate.commit, fx.candidate);
    assert_eq!(
        handoffs[0].candidate.delivery,
        HandoffDelivery::LocalCandidate
    );
    assert!(
        !handoffs[0].execution_summary.contains("validation passed"),
        "a handoff that ran no check must not report one passing: {}",
        handoffs[0].execution_summary
    );
}
/// A leaf that ran the before-PR gate hands its review off as typed evidence for the exact candidate, and a leaf without it reports `not_required` rather than inventing a review [ORB-13895].
fn claimed_handoff_carries_before_pr_evidence_only_for_its_candidate(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let mut input = fx.claim_input();
    let validated = action_ok(&host, "claim_validate", &input);
    input["candidate"] = validated["candidate"].clone();
    input["validation"] = validated["validation"].clone();
    let evidence = |head: &str| {
        json!({
            "attempt_id": "attempt-1",
            "verdict": "accept_with_fixes",
            "reviewed_head_sha": head,
            "reviewed_base_sha": fx.base_sha,
            "reviewer_commit": head,
            "reviewer_crew": "reviewer",
            "reviewer_run_id": "leaf-run",
            "certificate": {"path": "review-gate.json", "sha256": "0".repeat(64)},
        })
    };
    let mut moved = input.clone();
    moved["review_evidence"] = evidence(&fx.base_sha);
    let refused = action_err(&host, "claim_handoff", &moved);
    assert!(matches!(refused, OrbitError::PolicyDenied(_)), "{refused}");
    assert!(host.handoffs().is_empty());
    let mut reviewed = input.clone();
    reviewed["review_evidence"] = evidence(&fx.candidate);
    action_ok(&host, "claim_handoff", &reviewed);
    action_ok(&host, "claim_handoff", &input);
    let handoffs = locked(&host.handoffs);
    assert_eq!(handoffs[0].review.policy, ReviewTiming::BeforePr);
    let carried = handoffs[0].review.evidence().unwrap();
    assert_eq!(carried.reviewed_head_sha, fx.candidate);
    assert_eq!(carried.verdict, ReviewVerdict::AcceptWithFixes);
    assert_eq!(handoffs[1].review.policy, ReviewTiming::None);
    assert_eq!(
        handoffs[1].review.disposition,
        HandoffReviewDisposition::NotRequired
    );
}
fn claimed_handoff_without_logs_is_refused_when_commands_are_required(sandbox: &Path) {
    let (fx, host) = fx_host_requiring(sandbox, &["true"]);
    let mut input = fx.claim_input();
    let validated = action_ok(&host, "claim_validate", &input);
    assert_eq!(validated["decision"], "passed");
    assert_eq!(locked(&host.claim_logs).len(), 1);
    input["candidate"] = validated["candidate"].clone();
    input["validation"] = json!([]);
    let refused = action_err(&host, "claim_handoff", &input);
    assert!(matches!(refused, OrbitError::PolicyDenied(_)), "{refused}");
    assert!(host.handoffs().is_empty());
}
/// A recorded admitted review gate's output for attempt `attempt_id` in lineage `lineage`, reviewed by `codex / review-model`.
fn admitted_review(attempt_id: &str, lineage: &str) -> Value {
    serde_json::to_value(ReviewGateAdmissionOutput::Admitted(Box::new(
        ReviewGateAdmitted {
            decision: ReviewGateDecision::Admitted,
            held_attempt_id: None,
            first_task_id: TASK_ID.into(),
            attempt_id: attempt_id.into(),
            attempt_index: 1,
            lineage_key: lineage.into(),
            timing: ReviewTiming::BeforePr,
            timing_source: "workspace".into(),
            reviewer: ReviewGateReviewer {
                crew: "reviewers".into(),
                crew_source: "workspace".into(),
                provider: "codex".into(),
                model: "review-model".into(),
                reasoning_effort: None,
                eligible_pool: Vec::new(),
            },
            owed_evidence: Vec::new(),
            owner_owed_evidence: Vec::new(),
            host_validation: Vec::new(),
            base_sha: "0".repeat(40),
            base_tree: "0".repeat(40),
            head_sha: "0".repeat(40),
            head_tree: "0".repeat(40),
            implementation_commit_count: 1,
            task_meaning_digest: "digest".into(),
            task_selectors: BTreeMap::new(),
            evidence_carry: Value::Null,
            manifest_artifact: REVIEW_MANIFEST_ARTIFACT.into(),
            report_artifact: REVIEW_REPORT_ARTIFACT.into(),
            budget: ReviewBudget::default(),
            remaining: ReviewConsumption { seconds: 1800 },
            started_at: Utc::now(),
        },
    )))
    .unwrap()
}
fn a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
    host.publish_pr(TASK_ID);
    let admission =
        |attempt: &str| admitted_review(attempt, "ws/T-LANDING/agent-main/jrun-landing");
    let handoff = action_ok(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "re_review", "error_code": "pipeline_step_failed", "error_message": "reviewer exceeded its wall clock", "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID]}, "pipeline": { "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo}, "review_gate_admit": admission("rvw-first-1"), "re_review_gate_admit": admission("rvw-first-2"), }, }),
    );
    let released = host
        .releases()
        .into_iter()
        .map(|request| (request.run_id, request.attempt_id))
        .collect::<Vec<_>>();
    assert_eq!(
        released,
        vec![
            (RUN_ID.to_string(), "rvw-first-1".to_string()),
            (RUN_ID.to_string(), "rvw-first-2".to_string()),
        ]
    );
    assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{handoff}");
    assert_eq!(fx.forge_state("pr-head"), None);
    action_err(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "review_gate", "error_code": "pipeline_step_failed", "error_message": "reviewer exceeded its wall clock", "run_id": "jrun-bundle", "job_input": {"task_ids": [TASK_ID, "T-BUNDLED"]}, "pipeline": {"review_gate_admit": admission("rvw-bundle-1")}, }),
    );
    assert_eq!(
        host.releases()
            .last()
            .map(|request| request.attempt_id.as_str()),
        Some("rvw-bundle-1"),
        "the bundle's failed reviewer step still closed its attempt"
    );
}
const WORKTREE_FAILURE: &str = "could not resolve host: github.com while fetching the base";
/// The shipped `task_pr_pipeline` failure hook, run after its `worktree` step fails: the engine dispatches the hook with the run's checkpoints so far, which hold no `worktree` entry.
fn run_shipped_worktree_failure(host: &DeliveryHost) -> Result<(), String> {
    let shipped = shipped_job("task_pr_pipeline.yaml");
    let mut job = job_asset(json!([{
        "id": "worktree",
        "spec": { "type": "deterministic", "action": "test_stub_worktree", "config": {} },
    }]));
    job.failure_activity = shipped.failure_activity.clone();
    let catalog = activity_catalog(&["pr_failure_handoff"]);
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    run_job(host, &job)
}
/// [ORB-17710] A `worktree` step that fails before its checkpoint leaves the run without a checkout.
fn a_worktree_step_failure_ends_the_failure_hook_as_no_candidate(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let remote_before = fx.remote_tip(BRANCH);
    host.fail("test_stub_worktree", WORKTREE_FAILURE);
    let error = run_shipped_worktree_failure(&host).unwrap_err();
    assert!(error.contains(WORKTREE_FAILURE));
    assert!(!error.contains("missing pipeline.worktree checkpoint"));
    let checkpoints = host.failure_checkpoints();
    let [(activity, failed_step, output)] = checkpoints.as_slice() else {
        panic!("the hook completes once and is checkpointed: {checkpoints:?}");
    };
    assert_eq!(activity, "pr_failure_handoff");
    assert_eq!(failed_step, "worktree");
    assert_eq!(output["decision"], "no_candidate", "{output}");
    assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
    assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
}
/// [ORB-17710] The no-op closes the run's admitted review attempts first and touches nothing else.
fn a_failure_before_the_worktree_checkpoint_releases_reviews_and_a_later_gap_is_refused(
    sandbox: &Path,
) {
    let (fx, host) = fx_host(sandbox);
    let admission = admitted_review("rvw-worktree-1", "ws/T-LANDING/agent-main/jrun-landing");
    let handoff = action_ok(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "worktree", "error_code": "pipeline_step_failed", "error_message": WORKTREE_FAILURE, "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID]}, "pipeline": {"review_gate_admit": admission}, }),
    );
    assert_eq!(handoff["decision"], "no_candidate", "{handoff}");
    let released = host
        .releases()
        .into_iter()
        .map(|request| request.attempt_id)
        .collect::<Vec<_>>();
    assert_eq!(released, vec!["rvw-worktree-1".to_string()]);
    assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
    let error = action_err(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "implement_bundle", "error_code": "pipeline_step_failed", "error_message": "implementer exited", "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID]}, "pipeline": {}, }),
    );
    assert!(
        error
            .to_string()
            .contains("missing pipeline.worktree checkpoint")
    );
}
/// [ORB-13990] Owner revalidation of a reviewer commit accepts a path no delivered task owns, widening the task's selectors with review provenance instead of refusing; a path the selectors already own widens nothing.
#[test]
fn review_revalidation_widens_selectors_for_a_reviewer_change_outside_task_ownership() {
    isolated(
        "review_revalidation_widens_selectors_for_a_reviewer_change_outside_task_ownership",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            host.set_context_files(TASK_ID, &["file:src/feature.txt"]);
            let fixed_head = fx.commit("docs/fix.md", "reviewer fix\n");
            let mut input = fx.validate_input();
            input["ownership_base_sha"] = json!(fx.candidate);
            let validated = action_ok(&host, "candidate_validate", &input);
            assert_eq!(validated["owned_paths"], json!(["docs/fix.md"]));
            assert_eq!(fx.head(), fixed_head, "validation changes no commit");
            assert_eq!(
                host.widenings(),
                vec![(
                    TASK_ID.to_string(),
                    ContextWideningStep::Review,
                    "candidate_validate".to_string(),
                    vec!["docs/fix.md".to_string()],
                )]
            );
            action_ok(&host, "candidate_validate", &input);
            assert_eq!(host.widenings().len(), 1, "owned paths widen nothing");
        },
    );
}
#[test]
fn review_revalidation_preserves_quoted_reviewer_filenames() {
    isolated(
        "review_revalidation_preserves_quoted_reviewer_filenames",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            host.set_context_files(TASK_ID, &["file:src/feature.txt", "file:docs/café.md"]);
            fx.commit("docs/café.md", "already covered\n");
            fx.commit("docs/new\nline.md", "newline name\n");
            fx.commit("docs/quote\".md", "quote name\n");
            let fixed_head = fx.commit("docs/trailing.md ", "trailing space\n");
            let mut input = fx.validate_input();
            input["ownership_base_sha"] = json!(fx.candidate);
            let validated = action_ok(&host, "candidate_validate", &input);
            assert_eq!(
                validated["owned_paths"],
                json!([
                    "docs/café.md",
                    "docs/new\nline.md",
                    "docs/quote\".md",
                    "docs/trailing.md "
                ])
            );
            assert_eq!(fx.head(), fixed_head, "validation changes no commit");
            assert_eq!(
                host.widenings(),
                vec![(
                    TASK_ID.to_string(),
                    ContextWideningStep::Review,
                    "candidate_validate".to_string(),
                    vec![
                        "docs/new\nline.md".to_string(),
                        "docs/quote\".md".to_string(),
                        "docs/trailing.md ".to_string(),
                    ],
                )],
                "only uncovered literal paths widen, with no quoted phantom for the covered path"
            );
        },
    );
}
/// [ORB-13989] A reviewer commit that fails owner revalidation rejects the candidate.
fn a_failed_review_revalidation_rejects_and_preserves_both_commits(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let fixed_head = fx.commit("src/feature.txt", "reviewer fix\n");
    let handoff = action_ok(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "review_validate", "error_code": "pipeline_step_failed", "error_message": "required validation 'make test' did not pass", "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"}, "pipeline": { "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo}, "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}, "review_gate_admit": admitted_review( "rvw-fixes-1", "ws/T-LANDING/agent-main/jrun-landing", ), "review_gate_settle": { "gate": "passed", "verdict": "accept_with_fixes", "reviewed_head_sha": fixed_head, "implementation_head_sha": fx.candidate, "reviewer_fixed": true, }, }, }),
    );
    assert_eq!(handoff["decision"], "blocked_review_gate", "{handoff}");
    assert_eq!(handoff["partial_repair_commit"], Value::Null, "{handoff}");
    assert_eq!(fx.head(), fixed_head, "nothing is amended or added");
    assert_eq!(fx.git(&["rev-parse", "HEAD^"]), fx.candidate);
    assert_eq!(fx.remote_tip(BRANCH), fixed_head, "both commits are pushed");
    assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    assert_eq!(
        host.releases()
            .into_iter()
            .map(|request| request.attempt_id)
            .collect::<Vec<_>>(),
        vec!["rvw-fixes-1".to_string()],
        "the admitted attempt is closed"
    );
}
fn a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let handoff = action_ok(
        &host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "review", "error_code": ReviewGateRefusal::TimeoutIncomplete.error_code(), "error_message": "review_timeout_incomplete: reviewer exceeded its wall clock", "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"}, "pipeline": { "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo}, "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}, "review_gate_admit": admitted_review("rvw-partial", "lineage"), }, }),
    );
    assert_eq!(handoff["decision"], "incomplete_review_timeout");
    assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
    assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    assert_eq!(fx.forge_state("pr-head"), None);
}
fn an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks(sandbox: &Path) {
    let (fx, host) = fx_host(sandbox);
    let hold = json!({
        "schema_version": 1, "attempt_id": "rvw-held", "lineage_key": "lineage", "run_id": RUN_ID,
        "candidate": {"commit": fx.candidate, "tree": fx.git(&["rev-parse", "HEAD^{tree}"])},
        "task_meaning_digest": "meaning",
        "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
    });
    locked(&host.artifacts).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        serde_json::to_vec(&hold).unwrap(),
    );
    locked(&host.artifact_creators).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        "system".into(),
    );
    // Settlement never reaches the handoff with a hold: it ends the run held. The hold reaches the handoff only as an admission refusal.
    let mut input = json!({
        "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(),
        "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID,
        "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
        "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
            "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}},
    });
    let held = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(held["decision"], "awaiting_review_evidence");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
    assert_eq!(fx.forge_state("pr-head"), None);
    input["failed_step_id"] = json!("review_gate_settle");
    input["error_code"] = json!(ReviewGateRefusal::Blocked.error_code());
    input["error_message"] = json!("review_gate_blocked: changes_required; wrong approach");
    let rejected = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(rejected["decision"], "blocked_review_gate");
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    let forged_fx = Fixture::new(sandbox);
    let forged_host = DeliveryHost::new(&forged_fx.repo, TaskStatus::InProgress);
    let forged_hold = json!({
        "schema_version": 1, "attempt_id": "rvw-forged", "lineage_key": "lineage", "run_id": RUN_ID,
        "candidate": {"commit": forged_fx.candidate, "tree": forged_fx.git(&["rev-parse", "HEAD^{tree}"])},
        "task_meaning_digest": "meaning",
        "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
    });
    locked(&forged_host.artifacts).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        serde_json::to_vec(&forged_hold).unwrap(),
    );
    let forged = action_ok(
        &forged_host,
        "pr_failure_handoff",
        &json!({ "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(), "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"}, "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": forged_fx.repo}, "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}}, }),
    );
    assert_eq!(forged["decision"], "blocked_review_gate");
    assert_eq!(forged_host.status(TASK_ID), TaskStatus::Blocked);
}
/// ORB-14651: a requeued evidence-held task is admitted again by a later run.
fn an_admission_refusal_for_held_evidence_stays_held_not_escalated(sandbox: &Path) {
    const RERUN_ID: &str = "jrun-landing-rerun";
    let fx = Fixture::new(sandbox);
    // The operator requeue leaves the task in backlog; the rerun's admission moves it back to in-progress and claims it for the rerun before the handoff.
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    locked(&host.tasks).get_mut(TASK_ID).unwrap().job_run_id = Some(RERUN_ID.to_string());
    let hold = json!({
        "schema_version": 1, "attempt_id": "rvw-held", "lineage_key": "lineage", "run_id": RUN_ID,
        "candidate": {"commit": fx.candidate, "tree": fx.git(&["rev-parse", "HEAD^{tree}"])},
        "task_meaning_digest": "meaning",
        "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
    });
    locked(&host.artifacts).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        serde_json::to_vec(&hold).unwrap(),
    );
    locked(&host.artifact_creators).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        "system".into(),
    );
    // Admission refused before reserving an attempt, so no `review_gate_admit` output reaches the handoff.
    let mut input = json!({
        "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(),
        "error_message": "review_awaiting_evidence: named external checks have not arrived for the held candidate",
        "run_id": RERUN_ID,
        "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
        "pipeline": {"worktree": {"job_run_id": RERUN_ID, "workspace_path": fx.repo},
            "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}},
    });
    let held = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(held["decision"], "awaiting_review_evidence");
    assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
    assert_eq!(fx.forge_state("pr-head"), None);
    let mut other_candidate = hold.clone();
    other_candidate["candidate"]["commit"] = json!("0000000000000000000000000000000000000000");
    locked(&host.artifacts).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        serde_json::to_vec(&other_candidate).unwrap(),
    );
    let stale = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(stale["decision"], "blocked_review_gate");
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    locked(&host.artifacts).insert(
        (TASK_ID.into(), "review-evidence-hold.json".into()),
        serde_json::to_vec(&hold).unwrap(),
    );
    host.set_status(TASK_ID, TaskStatus::InProgress);
    input["error_code"] = json!("pipeline_step_failed");
    let untyped = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(untyped["decision"], "blocked_review_gate", "ORB-15714");
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
    host.set_status(TASK_ID, TaskStatus::InProgress);
    input["error_message"] = json!("review_gate_stale: completion rebased a reviewed head");
    let substantive = action_ok(&host, "pr_failure_handoff", &input);
    assert_eq!(substantive["decision"], "blocked_review_gate");
    assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
}
// Fixtures
fn fixture_push_refuses_non_file_transports_without_network(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("https://{}/remote.git", listener.local_addr().unwrap());
    fx.git(&["remote", "set-url", "origin", &url]);
    let refused = std::panic::catch_unwind(|| fx.git(&["push", "origin", BRANCH]));
    let refusal = refused.unwrap_err().downcast::<String>().unwrap();
    assert!(refusal.starts_with("refusing fixture push to non-local origin"));
    for clear_policy_variable in [false, true] {
        let mut command = crate::git_test_support::command();
        command
            .args(["push", "origin", BRANCH])
            .current_dir(&fx.repo);
        if clear_policy_variable {
            command.env_remove("GIT_ALLOW_PROTOCOL");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("transport 'https' not allowed"),
            "Git must refuse before resolving or connecting: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let error = execute_deterministic_action(
        &host,
        "git_push",
        &json!({}),
        &fx.push_input(),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("transport 'https' not allowed"));
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
fn fixture_push_guard_checks_all_effective_push_urls(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let remote = fx.forge.join("remote.git");
    for case in ["second_pushurl", "rewrite"] {
        if case == "second_pushurl" {
            fx.git(&[
                "config",
                "--add",
                "remote.origin.pushurl",
                path_str(&remote),
            ]);
            fx.git(&[
                "config",
                "--add",
                "remote.origin.pushurl",
                "https://fixture.invalid/remote.git",
            ]);
        } else {
            fx.git(&["config", "--unset-all", "remote.origin.pushurl"]);
            fx.git(&[
                "config",
                "url.https://fixture.invalid/.pushInsteadOf",
                path_str(&remote),
            ]);
        }
        let refused = std::panic::catch_unwind(|| fx.git(&["push", "origin", BRANCH]));
        let refusal = refused.unwrap_err().downcast::<String>().unwrap();
        assert!(
            refusal.starts_with("refusing fixture push to non-local origin"),
            "{case} must refuse before starting git push: {refusal}"
        );
        assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
    }
}
/// The tip of `branch` in the bare repository at `bare`, when it has one.
fn bare_tip(bare: &Path, branch: &str) -> Option<String> {
    let output = crate::git_test_support::command()
        .args([
            "--git-dir",
            path_str(bare),
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
/// ORB-17671: the fetch remote already holding the candidate must not make `git_push` report `reused_current` when the effective push destination is a different, empty repository.
fn git_push_publishes_to_effective_push_destination_not_fetch_remote(sandbox: &Path) {
    for case in ["pushurl", "pushInsteadOf"] {
        let fx = Fixture::new(sandbox);
        let push_remote = fx.bare_repo("push.git");
        assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{case}");
        assert_eq!(bare_tip(&push_remote, BRANCH), None, "{case}");
        let fetch_remote = fx.forge.join("remote.git");
        if case == "pushurl" {
            fx.git(&["config", "remote.origin.pushurl", path_str(&push_remote)]);
        } else {
            fx.git(&[
                "config",
                &format!("url.{}.pushInsteadOf", path_str(&push_remote)),
                path_str(&fetch_remote),
            ]);
        }
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        // A lease on the published head is a lease at the push destination: the fetch remote holding it does not satisfy it.
        let leased = action_err(
            &host,
            "git_push",
            &json!({ "workspace_path": fx.repo, "branch": BRANCH, "lease_remote_sha": fx.candidate, }),
        );
        assert!(leased.to_string().contains("push_lease_lost"), "{case}");
        assert_eq!(bare_tip(&push_remote, BRANCH), None, "{case}");
        let pushed = action_ok(&host, "git_push", &fx.push_input());
        assert_eq!(pushed["decision"], "performed_create", "{case}");
        assert_eq!(
            bare_tip(&push_remote, BRANCH).as_deref(),
            Some(fx.candidate.as_str()),
            "{case}"
        );
        // Only now, with the destination holding the exact candidate, is reuse a truthful report.
        let reused = action_ok(&host, "git_push", &fx.push_input());
        assert_eq!(reused["decision"], "reused_current", "{case}: {reused}");
    }
    // Several distinct push destinations are refused before any of them is written or reported as published.
    let fx = Fixture::new(sandbox);
    let first = fx.forge.join("push-one.git");
    let second = fx.forge.join("push-two.git");
    fx.bare_repo_at(&first);
    fx.bare_repo_at(&second);
    fx.git(&["config", "--add", "remote.origin.pushurl", path_str(&first)]);
    fx.git(&[
        "config",
        "--add",
        "remote.origin.pushurl",
        path_str(&second),
    ]);
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    let refused = action_err(&host, "git_push", &fx.push_input());
    assert!(refused.to_string().contains("distinct effective push URLs"));
    assert_eq!(bare_tip(&first, BRANCH), None);
    assert_eq!(bare_tip(&second, BRANCH), None);
}
/// ORB-17686: a `pushInsteadOf` destination that also matches an `insteadOf` rule must be probed and fetched as itself, not as the repository the `insteadOf` rule rewrites it to.
fn git_push_probes_composed_rewrite_destination_not_its_rewrite(sandbox: &Path) {
    let fx = Fixture::new(sandbox);
    let fetch_remote = fx.forge.join("remote.git");
    let push_remote = fx.forge.join("push.git");
    let decoy = fx.forge.join("decoy.git");
    fx.bare_repo_at(&push_remote);
    fx.bare_repo_at(&decoy);
    // The decoy and the fetch remote both hold the candidate; the publication destination holds only the base.
    fx.git(&["push", path_str(&decoy), BRANCH]);
    assert_eq!(
        bare_tip(&decoy, BRANCH).as_deref(),
        Some(fx.candidate.as_str())
    );
    fx.git(&[
        "push",
        path_str(&push_remote),
        &format!("{BASE}:refs/heads/{BRANCH}"),
    ]);
    // origin fetches from fetch.git, pushes to push.git, and push.git as a URL argument would rewrite to decoy.git.
    fx.git(&[
        "config",
        &format!("url.{}.pushInsteadOf", path_str(&push_remote)),
        path_str(&fetch_remote),
    ]);
    fx.git(&[
        "config",
        &format!("url.{}.insteadOf", path_str(&decoy)),
        path_str(&push_remote),
    ]);
    assert_eq!(
        fx.git(&["remote", "get-url", "--push", "--all", "origin"]),
        path_str(&push_remote)
    );
    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
    // A lease on the candidate is not satisfied by the decoy (or the fetch remote) holding it: the destination is at the base, so the lease is lost and nothing is written.
    let leased = action_err(
        &host,
        "git_push",
        &json!({ "workspace_path": fx.repo, "branch": BRANCH, "lease_remote_sha": fx.candidate, }),
    );
    assert!(leased.to_string().contains("push_lease_lost"), "{leased}");
    assert_eq!(
        bare_tip(&push_remote, BRANCH).as_deref(),
        Some(fx.base_sha.as_str())
    );
    // The probe sees the base at the destination, fetches that head from the destination, and fast-forwards it.
    let pushed = action_ok(&host, "git_push", &fx.push_input());
    assert_eq!(pushed["decision"], "performed_fast_forward");
    assert_eq!(
        bare_tip(&push_remote, BRANCH).as_deref(),
        Some(fx.candidate.as_str())
    );
    // Only now, with the destination itself holding the exact candidate, is reuse a truthful report.
    let reused = action_ok(&host, "git_push", &fx.push_input());
    assert_eq!(reused["decision"], "reused_current", "{reused}");
}
/// ORB-17671: a self-pointing effective push URL is refused even when the fetch URL names a real remote, including from a linked worktree that shares the primary checkout's Git common directory.
fn git_push_refuses_self_pointing_effective_push_url(sandbox: &Path) {
    for case in ["direct", "linked_worktree"] {
        let fx = Fixture::new(sandbox);
        fx.git(&["config", "remote.origin.pushurl", path_str(&fx.repo)]);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        let workspace = if case == "linked_worktree" {
            let linked = fx.forge.join("linked-worktree");
            fx.git(&["worktree", "add", "--detach", path_str(&linked)]);
            linked.canonicalize().unwrap()
        } else {
            fx.repo.clone()
        };
        let refused = action_err(
            &host,
            "git_push",
            &json!({"workspace_path": workspace, "branch": BRANCH}),
        );
        assert!(
            refused.to_string().contains("is this same repository"),
            "{case}: {refused}"
        );
        assert!(
            refused.to_string().contains("push URL"),
            "{case}: {refused}"
        );
    }
}
/// ORB-14717: a silent origin must not turn reviewed work into a candidate failure. Only a cached ref sharing ancestry can complete the handoff.
fn claimed_handoff_fetch_timeout_uses_only_a_related_cached_base(sandbox: &Path) {
    for cache in ["related", "missing", "unrelated"] {
        let fx = Fixture::new(sandbox);
        // A local upload-pack stalls without permitting any network transport from this test process.
        let silent = fx._root.path().join("silent-upload-pack");
        fs::write(&silent, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&silent, fs::Permissions::from_mode(0o755)).unwrap();
        fx.git(&["config", "remote.origin.uploadpack", path_str(&silent)]);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        let mut input = fx.claim_input();
        let validated = action_ok(&host, "claim_validate", &input);
        input["candidate"] = validated["candidate"].clone();
        input["validation"] = validated["validation"].clone();
        input["base_sync"] = json!("remote");
        input["git_timeouts"] = json!({"fetch": 1});
        let cached = format!("refs/remotes/origin/{BASE}");
        match cache {
            "missing" => {
                fx.git(&["update-ref", "-d", &cached]);
            }
            "unrelated" => {
                let tree = fx.git(&["rev-parse", "HEAD^{tree}"]);
                let orphan = fx.git(&["commit-tree", &tree, "-m", "unrelated"]);
                fx.git(&["update-ref", &cached, &orphan]);
            }
            _ => {}
        }
        let handoff = action(&host, "claim_handoff", &input);
        if cache == "related" {
            let handed = handoff.unwrap();
            assert_eq!(handed["handed_off"], true);
            assert_eq!(handed["base"], fx.base_sha);
            assert_eq!(handed["candidate"], fx.candidate);
            assert_eq!(locked(&host.handoffs).len(), 1);
        } else {
            let error = handoff.unwrap_err().to_string();
            assert!(error.contains("timed out after 1ms"), "{cache}: {error}");
            assert_eq!(
                ClaimFailureClass::of_step_failure(None, Some(&error)),
                Some(ClaimFailureClass::Transient),
                "{cache}: {error}"
            );
            assert!(host.handoffs().is_empty());
        }
        assert_eq!(fx.head(), fx.candidate, "the candidate stays intact");
    }
}
/// ORB-14717: a deleted remote base must refuse even with a usable cached ref. It says something different from an unavailable network.
#[test]
fn claimed_handoff_missing_remote_ref_never_uses_the_cached_base() {
    isolated(
        "claimed_handoff_missing_remote_ref_never_uses_the_cached_base",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            let mut input = json!({"workspace_path": fx.repo, "base_sync": "local"});
            let validated = action_ok(&host, "claim_validate", &input);
            input["candidate"] = validated["candidate"].clone();
            input["validation"] = validated["validation"].clone();
            input["base_sync"] = json!("remote");
            fx.remote_git(&["update-ref", "-d", &format!("refs/heads/{BASE}")]);
            let error = action_err(&host, "claim_handoff", &input).to_string();
            assert!(error.contains("couldn't find remote ref"), "{error}");
            assert_eq!(ClaimFailureClass::of_step_failure(None, Some(&error)), None);
            assert!(host.handoffs().is_empty());
        },
    );
}
fn remote_base_transport_retries_preserve_access_refusals(sandbox: &Path) {
    for (diagnostic, recovery, attempts, transient) in rows_by_name! {
        "Connection reset by peer" => (true, 2, false);
        "Could not resolve hostname fixture.invalid" => (false, 3, true);
        "Connection reset by peer" => (false, 3, true);
        "Permission denied (publickey). Connection closed" => (false, 1, false);
        "Authentication failed. Connection reset by peer" => (false, 1, false)
    } {
        let fx = Fixture::new(sandbox);
        let count = fx._root.path().join("fetch-attempts");
        let upload_pack = fx._root.path().join("upload-pack");
        fs::write(&count, "0").unwrap();
        fs::write(&upload_pack, format!(
            "#!/bin/sh\nn=$(cat '{}')\nn=$((n + 1))\necho $n > '{}'\nif [ '{}' = true ] && [ $n -gt 1 ]; then exec git-upload-pack '{}'; fi\necho '{}' >&2\nexit 1\n",
            count.display(), count.display(), recovery, fx.forge.join("remote.git").display(), diagnostic,
        )).unwrap();
        fs::set_permissions(&upload_pack, fs::Permissions::from_mode(0o755)).unwrap();
        fx.git(&["config", "remote.origin.uploadpack", path_str(&upload_pack)]);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        if !transient && !recovery {
            let mut input = json!({"workspace_path": fx.repo, "base_sync": "local"});
            let validated = action_ok(&host, "claim_validate", &input);
            input["candidate"] = validated["candidate"].clone();
            input["validation"] = validated["validation"].clone();
            input["base_sync"] = json!("remote");
            let error = action_err(&host, "claim_handoff", &input).to_string();
            assert!(error.contains(diagnostic), "{error}");
            assert_eq!(ClaimFailureClass::of_step_failure(None, Some(&error)), None);
            assert!(host.handoffs().is_empty());
        } else {
            let fetched = orbit_engine::fetch_remote_base(&fx.repo, BASE);
            if recovery {
                fetched.unwrap();
            } else {
                let error = fetched.unwrap_err().to_string();
                assert!(error.contains(diagnostic), "{error}");
                assert_eq!(
                    ClaimFailureClass::of_step_failure(None, Some(&error)),
                    Some(ClaimFailureClass::Transient),
                    "{error}"
                );
            }
        }
        assert_eq!(
            fs::read_to_string(&count)
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap(),
            attempts,
            "{diagnostic}"
        );
    }
}
/// A checkout on [`BRANCH`] one commit ahead of [`BASE`], both pushed to a bare remote that the substitute forge reads and merges into.
struct Fixture {
    _root: TempDir,
    repo: PathBuf,
    forge: PathBuf,
    base_sha: String,
    candidate: String,
}
impl Fixture {
    /// A fresh repository and forge, made the active forge for `gh`.
    fn new(sandbox: &Path) -> Self {
        thread_local! {
            static TEMPLATE: std::cell::RefCell<Option<Fixture>> = const { std::cell::RefCell::new(None) };
        }
        TEMPLATE.with(|slot| {
            let mut template = slot.borrow_mut();
            let seed = template.get_or_insert_with(|| Self::fresh(sandbox));
            seed.copy_to(sandbox)
        })
    }
    fn git(&self, args: &[&str]) -> String {
        git(&self.repo, args)
    }
    /// Run git against the forge's bare remote.
    fn remote_git(&self, args: &[&str]) -> String {
        let git_dir = path_str(&self.forge.join("remote.git")).to_string();
        let mut full = vec!["--git-dir", git_dir.as_str()];
        full.extend_from_slice(args);
        self.git(&full)
    }
    fn remote_parse(&self, rev: &str) -> String {
        self.remote_git(&["rev-parse", rev])
    }
    fn write_forge(&self, name: &str, contents: impl AsRef<[u8]>) {
        fs::write(self.forge.join(name), contents).unwrap();
    }
    fn write_repo(&self, rel: &str, contents: impl AsRef<[u8]>) {
        fs::write(self.repo.join(rel), contents).unwrap();
    }
    fn script_lines(&self, name: &str, lines: &[&str]) {
        fs::write(self.forge.join(name), format!("{}\n", lines.join("\n"))).unwrap();
    }
    /// Copy only pristine preparation, before any action or scripted response mutates it.
    fn copy_to(&self, sandbox: &Path) -> Self {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        git_fixture::copy_template(self._root.path(), root.path());
        let repo = root.path().join("repo").canonicalize().unwrap();
        let forge = root.path().join("forge");
        fs::write(sandbox.join("active-forge"), path_str(&forge)).unwrap();
        Self {
            _root: root,
            repo,
            forge,
            base_sha: self.base_sha.clone(),
            candidate: self.candidate.clone(),
        }
    }
    fn fresh(sandbox: &Path) -> Self {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let forge = root.path().join("forge");
        let remote = forge.join("remote.git");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(root.path(), &["init", "--bare", path_str(&remote)]);
        git_fixture::init(&repo);
        let g = |args: &[&str]| git(&repo, args);
        g(&["remote", "add", "origin", path_str(&remote)]);
        g(&["checkout", "-b", BASE]);
        g(&["config", "user.name", "Orbit Test"]);
        g(&["config", "user.email", "orbit-test@example.invalid"]);
        fs::write(repo.join("README.md"), "base\n").unwrap();
        g(&["add", "README.md"]);
        g(&["commit", "-m", "base"]);
        g(&["push", "-u", "origin", BASE]);
        g(&["checkout", "-b", BRANCH]);
        let repo = repo.canonicalize().unwrap();
        let fixture = Self {
            _root: root,
            base_sha: g(&["rev-parse", BASE]),
            candidate: String::new(),
            repo,
            forge,
        };
        let candidate = fixture.commit("src/feature.txt", "reviewed change\n");
        fs::write(sandbox.join("active-forge"), path_str(&fixture.forge)).unwrap();
        fixture.script_checks(&["success"]);
        Self {
            candidate,
            ..fixture
        }
    }
    fn rebase_candidate(&mut self) {
        self.git(&["rebase", BASE]);
        self.git(&["push", "--force", "origin", BRANCH]);
        self.base_sha = self.git(&["rev-parse", BASE]);
        self.candidate = self.head();
    }
    /// [`Fixture::new`] whose base carries a `Makefile` with a red `ci-lint` target, and whose candidate is rebased onto it.
    fn with_red_lint(sandbox: &Path, runs: &Path) -> Self {
        let mut fixture = Self::new(sandbox);
        fixture.commit_on_base(
            "Makefile",
            &format!(
                "ci-lint:\n\t@echo ran >> '{}'\n\t@printf 'error: lint is red\\n  --> README.md:1:1\\n' >&2; exit 2\n",
                runs.display()
            ),
        );
        fixture.rebase_candidate();
        fixture
    }
    /// [`Fixture::new`] whose base fails [`RED_TESTS`] with nextest's `FAIL` line for each of `base_tests` (and libtest's echo of the first), with the candidate rebased onto it.
    fn with_red_tests(
        sandbox: &Path,
        base_tests: &[&str],
        candidate_tests: Option<&[&str]>,
    ) -> Self {
        let mut fixture = Self::new(sandbox);
        let mut base_output = base_tests
            .first()
            .map(|test| format!("test {test} ... FAILED\n"))
            .unwrap_or_default();
        base_output.push_str(&nextest_failures(base_tests));
        fixture.commit_files_on_base(&[
            ("failing.txt", &base_output),
            ("Makefile", "ci-test:\n\t@cat failing.txt\n\t@exit 100\n"),
        ]);
        fixture.rebase_candidate();
        if let Some(tests) = candidate_tests {
            fixture.candidate = fixture.commit("failing.txt", &nextest_failures(tests));
        }
        fixture
    }
    /// Commit `file` on the base and push it, leaving the candidate checked out.
    fn commit_on_base(&self, file: &str, contents: &str) {
        self.commit_files_on_base(&[(file, contents)]);
    }
    /// Seed one final base tree when the fixture observes no intermediate write. Separate base advances still use `commit_on_base` at each observation.
    fn commit_files_on_base(&self, files: &[(&str, &str)]) {
        self.git(&["checkout", BASE]);
        for (file, contents) in files {
            let path = self.repo.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let mut add = vec!["add", "--"];
        add.extend(files.iter().map(|(file, _)| *file));
        self.git(&add);
        let names = files
            .iter()
            .map(|(file, _)| *file)
            .collect::<Vec<_>>()
            .join(", ");
        self.git(&["commit", "-m", &format!("base writes {names}")]);
        self.git(&["push", "origin", BASE]);
        self.git(&["checkout", BRANCH]);
    }
    /// Check states the forge reports, one per status read; the last repeats.
    fn script_checks(&self, states: &[&str]) {
        self.script_lines("checks", states);
    }
    /// Answers to successive merge requests (see `merge-outcomes` in [`FAKE_GH`]); requests past the script merge normally.
    fn script_merges(&self, outcomes: &[&str]) {
        self.script_lines("merge-outcomes", outcomes);
    }
    /// Answers to successive creates; the final outcome repeats.
    fn script_creates(&self, outcomes: &[&str]) {
        self.script_lines("create-outcomes", outcomes);
    }
    /// Answers to successive PR views; the final outcome repeats.
    fn script_pr_views(&self, outcomes: &[&str]) {
        fs::write(
            self.forge.join("pr-view-outcomes"),
            format!("{}\n", outcomes.join("\n")),
        )
        .unwrap();
        fs::write(self.forge.join("pr-view-reads"), "0\n").unwrap();
    }
    fn script_pr_close_failure(&self, diagnostic: &str) {
        fs::write(self.forge.join("pr-close-failure"), diagnostic).unwrap();
    }
    fn pr_view_reads(&self) -> usize {
        self.forge_state("pr-view-reads")
            .map(|value| value.parse().unwrap())
            .unwrap_or(0)
    }
    /// Script PR metadata independently of the real remote PR head ref.
    fn script_heads(&self, heads: &[&str], remote_head: Option<&str>) {
        self.script_lines("heads", heads);
        self.write_forge(
            "heads-start",
            self.forge_state("reads").unwrap_or_else(|| "0".into()),
        );
        match remote_head {
            Some(head) => self.remote_git(&["update-ref", "refs/pull/42/head", head]),
            None => self.remote_git(&["update-ref", "-d", "refs/pull/42/head"]),
        };
    }
    /// Replace the opened head through the production lease-push boundary and hand its checkpoints to completion as the shipped pipeline does.
    fn republished_completion_input(&self, host: &DeliveryHost) -> Value {
        self.git(&["commit", "--amend", "-m", "republished candidate"]);
        let pushed = action_ok(
            host,
            "git_push",
            &json!({ "workspace_path": self.repo, "job_run_id": RUN_ID, "completed_task_ids": [TASK_ID], "branch": BRANCH, "rewrite_performed": true, "rewrite_head_before": self.candidate, "expected_remote_sha": self.candidate, }),
        );
        assert_eq!(pushed["decision"], "performed_force_with_lease");
        assert_eq!(pushed["remote_sha_before"], self.candidate);
        let mut input = self.complete_input();
        input["published_head_sha"] = pushed["local_sha"].clone();
        input["reviewed_head_sha"] = pushed["local_sha"].clone();
        input["previous_published_head_sha"] = pushed["remote_sha_before"].clone();
        input["max_wait_seconds"] = json!(60);
        input
    }
    /// Commit `file` on the checkout and push it, returning the new head.
    fn commit(&self, file: &str, contents: &str) -> String {
        let path = self.repo.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-m", &format!("write {file}")]);
        self.git(&["push", "-u", "origin", BRANCH]);
        self.head()
    }
    /// Land unrelated work on the base, leaving the candidate behind it.
    fn advance_base(&self) {
        self.git(&["checkout", BASE]);
        fs::write(self.repo.join("base.txt"), "advanced\n").unwrap();
        self.git(&["add", "base.txt"]);
        self.git(&["commit", "-m", "advance base"]);
        self.git(&["push", "origin", BASE]);
        self.git(&["checkout", BRANCH]);
    }
    /// Land unrelated work on the base, then rebase and republish the candidate onto it.
    fn advance_base_and_rebase(&self) {
        self.advance_base();
        self.git(&["rebase", BASE]);
        self.git(&["push", "--force", "origin", BRANCH]);
    }
    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }
    fn remote_parent(&self, commit: &str) -> String {
        self.remote_parse(&format!("{commit}^"))
    }
    fn local_tip(&self, branch: &str) -> String {
        self.git(&["rev-parse", branch])
    }
    /// [ORB-14905] The commit a held candidate's durable ref names on the remote, after checking the handoff says it carried it there.
    fn durable_candidate(&self, handoff: &Value) -> String {
        assert_eq!(handoff["carry"], "durable", "{handoff}");
        let reference = handoff["durable_ref"].as_str().unwrap();
        assert_eq!(
            reference,
            format!("refs/orbit/candidates/{TASK_ID}/{RUN_ID}")
        );
        self.remote_parse(reference)
    }
    fn remote_tip(&self, branch: &str) -> String {
        self.remote_parse(&format!("refs/heads/{branch}"))
    }
    fn push_input(&self) -> Value {
        json!({"workspace_path": self.repo, "branch": BRANCH})
    }
    fn bare_repo_at(&self, bare: &Path) {
        self.git(&["init", "--bare", path_str(bare)]);
    }
    fn bare_repo(&self, name: &str) -> PathBuf {
        let bare = self.forge.join(name);
        self.bare_repo_at(&bare);
        bare
    }
    /// The validation step as the owner pipeline hands it the synchronized candidate.
    fn validate_input(&self) -> Value {
        json!({"workspace_path": self.repo, "job_run_id": RUN_ID, "completed_task_ids": [TASK_ID], "base_sha": self.base_sha})
    }
    fn claim_input(&self) -> Value {
        json!({"workspace_path": self.repo, "base_sync": "local", "base_sha": self.base_sha})
    }
    fn open_input(&self, reviewed_head: &str, reviewed_base: &str) -> Value {
        json!({
            "workspace_path": self.repo, "job_run_id": RUN_ID, "completed_task_ids": [TASK_ID], "head": BRANCH, "base": BASE,
            "base_ref": BASE, "base_sha": self.base_sha, "base_sync": "local", "reviewed_head_sha": reviewed_head, "reviewed_base_sha": reviewed_base,
        })
    }
    /// The completion step as a `--complete` pipeline hands it the run's published and reviewed candidate.
    fn complete_input(&self) -> Value {
        json!({
            "workspace_path": self.repo, "job_run_id": RUN_ID, "completed_task_ids": [TASK_ID], "completion": "done", "pr_number": PR_NUMBER,
            "head": BRANCH, "base": BASE, "base_sync": "local", "published_head_sha": self.candidate, "reviewed_head_sha": self.candidate,
            "max_wait_seconds": 30, "poll_interval_seconds": 5,
        })
    }
    /// The same completion for an ungated run: the published head is still pinned, but no before-PR review settled it.
    fn complete_ungated_input(&self) -> Value {
        let mut input = self.complete_input();
        input.as_object_mut().unwrap().remove("reviewed_head_sha");
        input
    }
    fn forge_state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.forge.join(name))
            .ok()
            .map(|value| value.trim().to_string())
    }
    fn forge_lines(&self, name: &str) -> Vec<String> {
        self.forge_state(name)
            .map(|value| value.lines().map(ToOwned::to_owned).collect())
            .unwrap_or_default()
    }
    fn forge_calls(&self) -> Vec<String> {
        self.forge_lines("calls")
    }
    fn merge_requests(&self) -> Vec<String> {
        self.forge_lines("merge-requests")
    }
    fn status_reads(&self) -> usize {
        self.forge_state("reads")
            .map(|value| value.parse().unwrap())
            .unwrap_or(0)
            + usize::from(self.forge_state("merged-reads").is_some())
    }
}
fn action(host: &DeliveryHost, name: &str, input: &Value) -> Res<Value> {
    if matches!(name, "git_push" | "pr_complete" | "pr_failure_handoff") {
        git_fixture::assert_local_push_remote(&host.repo);
    }
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}
/// [`action`] that must succeed; panics with the action's error.
fn action_ok(host: &DeliveryHost, name: &str, input: &Value) -> Value {
    action(host, name, input).unwrap()
}
/// [`action`] that must fail; returns its error.
fn action_err(host: &DeliveryHost, name: &str, input: &Value) -> OrbitError {
    action(host, name, input).expect_err(name)
}
fn task(status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        requires_labels: Vec::new(),
        job_run_machine: None,
        id: TASK_ID.to_string(),
        title: "Land the reviewed candidate".to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: "Outcome: success\n\nChanges:\n- Reviewed change.".to_string(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some(RUN_ID.to_string()),
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        origin: None,
        created_at: now,
        updated_at: now,
    }
}
/// The task store and review-landing record the delivery actions write through, kept in memory. The provider boundary is left to the engine.
#[derive(Default)]
struct DeliveryHost {
    repo: PathBuf,
    runs: Mutex<BTreeMap<String, JobRun>>,
    tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    completion_notes: Mutex<Vec<String>>,
    landings: Mutex<Vec<ReviewLandingRequest>>,
    required_commands: Mutex<Vec<String>>,
    baseline_commands: Mutex<Vec<String>>,
    scripted: Mutex<BTreeMap<String, Value>>,
    failing: Mutex<BTreeMap<String, String>>,
    failure_checkpoints: Mutex<Vec<(String, String, Value)>>,
    fail_evidence_writes: Mutex<bool>,
    artifacts: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    artifact_creators: Mutex<BTreeMap<(String, String), String>>,
    releases: Mutex<Vec<ReviewReleaseRequest>>,
    validation_env: Mutex<Option<ValidationEnvironment>>,
    widenings: Mutex<Vec<Widening>>,
    claim_logs: Mutex<Vec<String>>,
    claim_log_contents: Mutex<BTreeMap<String, Vec<u8>>>,
    handoffs: Mutex<Vec<TaskHandoff>>,
    ship_mode: Mutex<String>,
    status_events: Mutex<Vec<StatusEvent>>,
    history: Mutex<BTreeMap<String, Vec<TaskHistoryEntry>>>,
    recovery_attempts: Mutex<Vec<(String, String, RebaseRecoveryAttemptScope)>>,
    recovery_certificates: Mutex<Vec<(String, String, Value)>>,
    run_states: Mutex<BTreeMap<String, PipelineState>>,
    scripted_inputs: Mutex<BTreeMap<String, Value>>,
    deferral_answers: Mutex<BTreeMap<String, NamespaceProbe>>,
    probes: Mutex<Vec<(String, Option<String>, PathBuf)>>,
    /// The OS this host runs implementers on [ORB-15587].
    os: Mutex<Option<EvidenceHostOs>>,
    /// The workspace's PR settings and delivery conventions.
    pr_config: Mutex<orbit_engine::PrConfig>,
}
type Widening = (String, ContextWideningStep, String, Vec<String>);
type StatusEvent = (String, Option<String>, Option<String>);
impl DeliveryHost {
    fn new(repo: &Path, status: TaskStatus) -> Self {
        Self {
            repo: repo.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(TASK_ID.to_string(), task(status))])),
            ship_mode: Mutex::new("local".to_string()),
            os: Mutex::new(EvidenceHostOs::current()),
            ..Self::default()
        }
    }
    fn ship_pr(&self) {
        *locked(&self.ship_mode) = "pr".to_string();
    }
    fn set_conventions(&self, conventions: orbit_types::workflow::DeliveryConventions) {
        locked(&self.pr_config).conventions = conventions;
    }
    /// `pr.delivery_authors` and whether `pr_open` closes earlier delivery PRs.
    fn set_delivery_identity(&self, authors: &[&str], close_superseded: bool) {
        let mut config = locked(&self.pr_config);
        config.delivery_authors = authors.iter().map(|author| (*author).to_string()).collect();
        config.close_superseded = close_superseded;
    }
    fn put_artifact(&self, task_id: &str, path: &str, content: &str) {
        locked(&self.artifacts).insert((task_id.into(), path.into()), content.into());
    }
    fn put_artifact_bytes(&self, task_id: &str, path: &str, content: &[u8]) {
        locked(&self.artifacts).insert((task_id.into(), path.into()), content.to_vec());
    }
    fn set_os(&self, os: EvidenceHostOs) {
        *locked(&self.os) = Some(os);
    }
    fn resolve_validation_env(&self, environment: ValidationEnvironment) {
        *locked(&self.validation_env) = Some(environment);
    }
    fn require_commands(&self, commands: &[&str]) {
        *locked(&self.required_commands) = commands.iter().map(ToString::to_string).collect();
    }
    fn baseline_commands(&self, commands: &[&str]) {
        *locked(&self.baseline_commands) = commands.iter().map(ToString::to_string).collect();
    }
    /// Require `true` and name `gate` as the deferred baseline gate.
    fn gate_commands(&self, gate: &Gate) {
        self.require_commands(&["true"]);
        self.baseline_commands(&[&gate.command()]);
    }
    fn script(&self, action: &str, output: Value) {
        locked(&self.scripted).insert(action.to_string(), output);
    }
    fn script_all(&self, entries: &[(&str, Value)]) {
        for (action, output) in entries {
            self.script(action, output.clone());
        }
    }
    fn fail(&self, action: &str, message: &str) {
        locked(&self.failing).insert(action.to_string(), message.to_string());
    }
    fn failure_checkpoints(&self) -> Vec<(String, String, Value)> {
        locked(&self.failure_checkpoints).clone()
    }
    fn scripted_input(&self, action: &str) -> Value {
        locked(&self.scripted_inputs)
            .get(action)
            .cloned()
            .unwrap_or_else(|| panic!("`{action}` ran"))
    }
    fn has_artifact(&self, task_id: &str, path: &str) -> bool {
        locked(&self.artifacts).contains_key(&(task_id.to_string(), path.to_string()))
    }
    fn validation_log(&self, task_id: &str, path: &str) -> Value {
        let artifacts = locked(&self.artifacts);
        let content = artifacts
            .get(&(task_id.to_string(), path.to_string()))
            .unwrap_or_else(|| panic!("{task_id} has no artifact {path}"));
        serde_json::from_slice(content).unwrap()
    }
    fn publish_pr(&self, id: &str) {
        locked(&self.tasks)
            .get_mut(id)
            .unwrap()
            .external_refs
            .push(ExternalRef {
                system: GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(),
                id: PR_NUMBER.to_string(),
                url: None,
            });
    }
    fn releases(&self) -> Vec<ReviewReleaseRequest> {
        locked(&self.releases).clone()
    }
    fn set_context_files(&self, id: &str, selectors: &[&str]) {
        locked(&self.tasks).get_mut(id).unwrap().context_files =
            selectors.iter().map(ToString::to_string).collect();
    }
    fn set_status(&self, id: &str, status: TaskStatus) {
        locked(&self.tasks).get_mut(id).unwrap().status = status;
    }
    fn task(&self, id: &str) -> Task {
        locked(&self.tasks)[id].clone()
    }
    fn status(&self, id: &str) -> TaskStatus {
        locked(&self.tasks)[id].status
    }
    fn comments(&self, id: &str) -> Vec<TaskComment> {
        locked(&self.comments).get(id).cloned().unwrap_or_default()
    }
    fn history(&self, id: &str) -> Vec<TaskHistoryEntry> {
        locked(&self.history).get(id).cloned().unwrap_or_default()
    }
    fn completion_notes(&self) -> Vec<String> {
        locked(&self.completion_notes).clone()
    }
    fn landings(&self) -> Vec<ReviewLandingRequest> {
        locked(&self.landings).clone()
    }
    fn handoffs(&self) -> Vec<TaskHandoff> {
        locked(&self.handoffs).clone()
    }
    fn probes(&self) -> Vec<(String, Option<String>, PathBuf)> {
        locked(&self.probes).clone()
    }
    fn run_log(&self, name: &str) -> Value {
        self.validation_log(TASK_ID, &format!("validation/{RUN_ID}/{name}"))
    }
    fn claim_log(&self, path: &str) -> Value {
        serde_json::from_slice(&locked(&self.claim_log_contents)[path]).unwrap()
    }
    fn widenings(&self) -> Vec<Widening> {
        locked(&self.widenings).clone()
    }
    fn artifact(&self, task_id: &str, path: &str, content: &[u8]) -> TaskArtifact {
        TaskArtifact {
            path: path.into(),
            content: content.to_vec(),
            media_type: "application/json".into(),
            created_by: locked(&self.artifact_creators)
                .get(&(task_id.into(), path.into()))
                .cloned(),
        }
    }
    fn seed_reserved(&self, event: &str, note: &impl serde::Serialize, file: &str) {
        auto_fix::seed_auto_fix_history(self, event, note);
        fs::write(self.repo.parent().unwrap().join(file), "reserved").unwrap();
    }
    fn recovery_certificates(&self) -> Vec<(String, String, Value)> {
        locked(&self.recovery_certificates).clone()
    }
}
impl orbit_engine::test_support::NullRuntimeHost for DeliveryHost {
    fn pr_config(&self) -> orbit_engine::PrConfig {
        locked(&self.pr_config).clone()
    }
    fn get_job_run(&self, run_id: &str) -> Res<Option<JobRun>> {
        Ok(locked(&self.runs).get(run_id).cloned())
    }
    fn admit_pr_auto_fix(&self, request: &PrAutoFixRequest) -> Res<PrAutoFixAdmission> {
        let attempts = pr_auto_fix_attempts(&self.history(TASK_ID), &request.pr_number);
        let decision = decide_pr_auto_fix(
            &attempts,
            &PrAutoFixClaim {
                pr_number: &request.pr_number,
                head_sha: &request.head_sha,
                blocker: request.blocker,
                run_id: &request.run_id,
            },
            PR_AUTO_FIX_BUDGET,
            "fixture",
            "fixture",
        );
        if let PrAutoFixDecision::Admit(attempt) = &decision {
            auto_fix::seed_auto_fix_history(self, PR_AUTO_FIX_ADMITTED_EVENT, attempt);
        }
        Ok(PrAutoFixAdmission::Decided(decision))
    }
    fn admit_pr_base_update(
        &self,
        _tasks: &[String],
        request: &PrBaseUpdateReservation,
    ) -> Res<PrBaseUpdateAdmission> {
        let decision = decide_pr_base_update(&self.history(TASK_ID), request);
        if matches!(decision, PrBaseUpdateAdmission::Reserved) {
            self.seed_reserved(
                PR_BASE_UPDATE_RESERVED_EVENT,
                request,
                "forge/base-reserved",
            );
        }
        Ok(decision)
    }
    fn admit_pr_infrastructure(
        &self,
        execution: &str,
        _task_ids: &[String],
        evidence: &PrInfrastructureEvidence,
    ) -> Res<PrInfrastructureAdmission> {
        let decision = decide_pr_infrastructure(&self.history(TASK_ID), execution, evidence);
        if let PrInfrastructureAdmission::Reserved(reservation) = &decision {
            self.seed_reserved(
                PR_INFRASTRUCTURE_RESERVED_EVENT,
                reservation,
                "forge/infra-reserved",
            );
        }
        Ok(decision)
    }
    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        let mut attempts = locked(&self.recovery_attempts);
        attempts.push((run_id.to_string(), step_id.to_string(), scope.clone()));
        Ok(attempts
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .count() as u64)
    }
    /// Like the runtime's recovery authority, certify only the newest reservation of the step, for exactly the HEAD and base it reserved, then copy the evidence into the run store `git_rebase` reads.
    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        let attempts = locked(&self.recovery_attempts);
        let reserved = attempts
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .collect::<Vec<_>>();
        let Some((_, _, scope)) = reserved.last() else {
            return Err(DispatchError::JobExecution("no reserved attempt".into()));
        };
        if output["recovery_attempt"] != json!(reserved.len())
            || output["head_sha_before"] != json!(scope.head_sha_before)
            || output["target_base_sha"] != json!(scope.target_base_sha)
        {
            return Err(DispatchError::JobExecution(
                "evidence does not describe the newest reserved attempt".into(),
            ));
        }
        locked(&self.recovery_certificates).push((
            run_id.to_string(),
            step_id.to_string(),
            output.clone(),
        ));
        locked(&self.run_states)
            .entry(run_id.to_string())
            .or_insert_with(|| {
                PipelineState::new(
                    run_id.to_string(),
                    "task_pr_pipeline".to_string(),
                    json!({}),
                )
            })
            .rebase_recovery_checkpoints
            .insert(step_id.to_string(), output.clone());
        Ok(())
    }
    fn verify_rebase_recovery(&self, run_id: &str, step_id: &str, checkpoint: &Value) -> Res<bool> {
        Ok(self
            .recovery_certificates()
            .iter()
            .rev()
            .find(|(run, step, _)| run == run_id && step == step_id)
            .is_some_and(|(_, _, certified)| certified == checkpoint))
    }
    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Res<Vec<RebaseRecoveryAttemptScope>> {
        Ok(locked(&self.recovery_attempts)
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .map(|(_, _, scope)| scope.clone())
            .collect())
    }
    fn read_run_state(&self, run_id: &str) -> Res<Option<PipelineState>> {
        Ok(locked(&self.run_states).get(run_id).cloned())
    }
    fn widen_task_context_files(
        &self,
        task_id: &str,
        _run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Res<Vec<String>> {
        locked(&self.widenings).push((
            task_id.to_string(),
            step,
            activity.to_string(),
            paths.to_vec(),
        ));
        let selectors = paths
            .iter()
            .map(|path| format!("file:{path}"))
            .collect::<Vec<_>>();
        if let Some(task) = locked(&self.tasks).get_mut(task_id) {
            task.context_files.extend(selectors.iter().cloned());
        }
        Ok(selectors)
    }
    fn get_task_artifact(&self, task_id: &str, path: &str) -> Res<Option<TaskArtifact>> {
        let artifact = locked(&self.artifacts)
            .get(&(task_id.into(), path.into()))
            .map(|c| self.artifact(task_id, path, c));
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(task_id)?;
        }
        Ok(artifact)
    }
    fn get_task_artifacts(&self, task_id: &str) -> Res<Vec<TaskArtifact>> {
        Ok(locked(&self.artifacts)
            .iter()
            .filter(|((id, _), _)| id == task_id)
            .map(|((_, path), c)| self.artifact(task_id, path, c))
            .collect())
    }
    fn get_task(&self, task_id: &str) -> Res<Task> {
        locked(&self.tasks)
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }
    fn list_tasks_filtered(
        &self,
        _status: Option<TaskStatus>,
        _priority: Option<TaskPriority>,
        _parent_id: Option<&str>,
        job_run_id: Option<&str>,
        _external_ref: Option<&ExternalRef>,
        _has_external_ref_system: Option<&str>,
    ) -> Res<Vec<Task>> {
        Ok(locked(&self.tasks)
            .values()
            .filter(|task| job_run_id.is_none_or(|run| task.job_run_id.as_deref() == Some(run)))
            .cloned()
            .collect())
    }
    fn get_task_comments(&self, task_id: &str) -> Res<Vec<TaskComment>> {
        Ok(self.comments(task_id))
    }
    fn get_task_history(&self, task_id: &str) -> Res<Vec<TaskHistoryEntry>> {
        Ok(self.history(task_id))
    }
    /// The guarded transition: like the task store, refuse a write whose expected status is stale.
    fn update_task_from_activity(&self, task_id: &str, update: TaskActivityUpdate) -> Res<Task> {
        let mut tasks = locked(&self.tasks);
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if task.status != update.expected_status {
            return Err(OrbitError::Execution(format!(
                "task {task_id} is {} but the write expected {}",
                task.status, update.expected_status
            )));
        }
        task.status = update.status;
        if update.status == TaskStatus::Done {
            locked(&self.completion_notes).push(update.note.unwrap_or_default());
        }
        Ok(task.clone())
    }
    fn apply_task_automation_update(&self, task_id: &str, update: TaskAutomationUpdate) -> Res<()> {
        let mut tasks = locked(&self.tasks);
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(status) = update.status {
            task.status = status;
        }
        for external_ref in update.external_refs {
            push_external_ref_if_missing(&mut task.external_refs, external_ref);
        }
        locked(&self.status_events).push((
            task_id.to_string(),
            update.status_event,
            update.status_note,
        ));
        locked(&self.comments)
            .entry(task_id.to_string())
            .or_default()
            .extend(update.append_comments);
        locked(&self.history)
            .entry(task_id.to_string())
            .or_default()
            .extend(update.append_history);
        Ok(())
    }
    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Res<()> {
        locked(&self.landings).push(request.clone());
        Ok(())
    }
    fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Res<()> {
        locked(&self.releases).push(request.clone());
        Ok(())
    }
    fn admit_final_recovery(
        &self,
        _: &str,
        _: &FinalRecoveryAdmissionRequest,
    ) -> Res<FinalRecoveryAdmission> {
        Ok(
            if locked(&self.scripted).contains_key("test_final_decision") {
                FinalRecoveryAdmission::Admitted
            } else {
                FinalRecoveryAdmission::Skipped {
                    reason: "fixture has no recovery".into(),
                }
            },
        )
    }
    fn apply_final_recovery(
        &self,
        _: &str,
        _: &FinalRecoveryApplication,
    ) -> Res<FinalRecoveryApplied> {
        Ok(FinalRecoveryApplied::Escalated {
            outcome: "fixture".into(),
        })
    }
    fn final_recovery_log_tail(&self, _run_id: &str) -> Res<Option<String>> {
        Ok(None)
    }
    fn repo_root(&self) -> Res<String> {
        Ok(self.repo.to_string_lossy().into_owned())
    }
    fn required_validation_commands(&self) -> Vec<String> {
        locked(&self.required_commands).clone()
    }
    fn review_baseline_commands(&self) -> Vec<String> {
        locked(&self.baseline_commands).clone()
    }
    fn host_os(&self) -> Option<EvidenceHostOs> {
        *locked(&self.os)
    }
    /// An implementer lane that refuses the probed sandbox, unless a test scripted another answer [ORB-15516].
    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        provider: Option<&str>,
        _fs_profile: Option<&str>,
        cwd: &Path,
    ) -> NamespaceProbe {
        locked(&self.probes).push((
            probe.command.to_string(),
            provider.map(str::to_string),
            cwd.to_path_buf(),
        ));
        if let Some(answer) = locked(&self.deferral_answers).get(probe.command) {
            return answer.clone();
        }
        let (exit_code, output) = if probe.command == SEATBELT_APPLY_PROBE {
            (71, "sandbox-exec: sandbox_apply: Operation not permitted")
        } else if probe.command == PS_EXEC_PROBE {
            (
                71,
                "sandbox-exec: execvp() of '/bin/ps' failed: Operation not permitted",
            )
        } else {
            (1, "bwrap: No permissions to create new namespace")
        };
        NamespaceProbe {
            command: probe.command.to_string(),
            exit_code,
            output: output.to_string(),
        }
    }
    /// A job graph's scripted steps; every other action is the engine's own.
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        if action == "test_ci_repair" {
            return auto_fix::perform_ci_repair(self, input);
        }
        if let Some(message) = locked(&self.failing).get(action).cloned() {
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message,
            });
        }
        if let Some(output) = locked(&self.scripted).get(action).cloned() {
            locked(&self.scripted_inputs).insert(action.to_string(), input.clone());
            return Ok(output);
        }
        execute_deterministic_action(self, action, config, input, false, &HashMap::new(), None)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            })
    }
    /// The engine records a failure hook's output only when the hook succeeds.
    fn checkpoint_failure_activity(
        &self,
        _run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        locked(&self.failure_checkpoints).push((
            activity_name.to_string(),
            failed_step_id.to_string(),
            output.clone(),
        ));
        Ok(())
    }
    fn run_private_vcs_operation(&self, operation: &str, input: Value) -> Res<Value> {
        if operation == "ci.commit_jobs"
            && let Some(answer) = locked(&self.scripted).get(operation).cloned()
        {
            let commit = input.get("commit").and_then(Value::as_str).unwrap_or("");
            // A head-only script does not describe the base tip. An inherited red base is planted through the substitute `gh`.
            if commit == git(&self.repo, &["rev-parse", BASE]) {
                return Ok(json!({"commit": commit, "jobs": []}));
            }
            return Ok(answer);
        }
        if matches!(operation, "ci.job_log" | "ci.prerequisite_evidence")
            && let Some(answer) = locked(&self.scripted).get(operation).cloned()
        {
            return Ok(answer);
        }
        if locked(&self.scripted).contains_key("test_ci_repair")
            && let Some(answer) = auto_fix::ci_answer(self, operation, &input)
        {
            return Ok(answer);
        }
        orbit_engine::run_private_vcs_operation(operation, &input)
    }
    /// One owner-local claim on the fixture's candidate branch, requiring whatever this host requires.
    fn claim_execution_context(&self) -> Res<ClaimExecutionContext> {
        Ok(ClaimExecutionContext {
            footprint: vec!["file:src/feature.txt".to_string()],
            workspace_id: "ws_owner".to_string(),
            task_id: TASK_ID.to_string(),
            claim_id: "claim-landing".to_string(),
            machine_id: "hm_follower".to_string(),
            run_id: RUN_ID.to_string(),
            ship_mode: locked(&self.ship_mode).clone(),
            base_branch: BASE.to_string(),
            landing_branch: BASE.to_string(),
            required_commands: RuntimeHost::required_validation_commands(self),
        })
    }
    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Res<()> {
        locked(&self.claim_logs).push(path.to_string());
        locked(&self.claim_log_contents).insert(path.to_string(), content);
        if *locked(&self.fail_evidence_writes) && path.contains("/evidence/") {
            return Err(OrbitError::Execution(
                "attachment response unknown after write".into(),
            ));
        }
        Ok(())
    }
    fn record_flaky_validation(&self, task_id: &str, record: &FlakyValidation) -> Res<()> {
        RuntimeHost::apply_task_automation_update(
            self,
            task_id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: FLAKY_VALIDATION_EVENT.into(),
                    note: Some(record.text()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::FlakyValidation(record.clone())),
                }],
                ..Default::default()
            },
        )
    }
    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Res<()> {
        locked(&self.handoffs).push(handoff.clone());
        Ok(())
    }
    fn validation_subprocess_environment(&self) -> ValidationEnvironment {
        locked(&self.validation_env).clone().unwrap_or_else(|| {
            ValidationEnvironment::launcher(RuntimeHost::agent_subprocess_environment(self, &[]))
        })
    }
    /// Like the task store, accept evidence only from the run owning the task.
    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Res<()> {
        if RuntimeHost::get_task(self, task_id)?.job_run_id.as_deref() != Some(run_id) {
            return Err(OrbitError::PolicyDenied(format!(
                "run {run_id} does not own {task_id}"
            )));
        }
        locked(&self.artifacts).insert((task_id.to_string(), path.to_string()), content);
        if *locked(&self.fail_evidence_writes) && path.contains("/evidence/") {
            return Err(OrbitError::Execution(
                "attachment response unknown after write".into(),
            ));
        }
        Ok(())
    }
}
fn git(current_dir: &Path, args: &[&str]) -> String {
    git_fixture::run(current_dir, args)
}
fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}
// Substitute forge
/// The `gh` surface the engine's provider adapter calls, over one pull request (#42).
const FAKE_GH: &str = r#"#!/bin/sh
set -eu
forge=$(cat '__SANDBOX__/active-forge')
remote="$forge/remote.git"
url="https://github.com/orbit/test/pull/42"
number_json=42
if [ -f "$forge/pr-number-json" ]; then number_json=$(cat "$forge/pr-number-json"); fi
export GIT_AUTHOR_NAME=Forge GIT_AUTHOR_EMAIL=forge@example.invalid
export GIT_COMMITTER_NAME=Forge GIT_COMMITTER_EMAIL=forge@example.invalid
printf '%s %s\n' "$1" "${2:-}" >> "$forge/calls"
head_sha() {
    # A deleted branch reads as empty so a test can model its absence.
    git --git-dir="$remote" rev-parse --verify --quiet "refs/heads/$(cat "$forge/pr-head")" || true
}
check() {
    case "$1" in
        mixed_pending|mixed_success|mixed_failure)
            test_state=IN_PROGRESS; conclusion=''
            if [ "$1" = mixed_success ]; then test_state=COMPLETED; conclusion=SUCCESS; fi
            if [ "$1" = mixed_failure ]; then test_state=COMPLETED; conclusion=FAILURE; fi
            printf '{"__typename":"CheckRun","name":"test","status":"%s","conclusion":"%s","isRequired":true},{"__typename":"CheckRun","name":"coverage","status":"COMPLETED","conclusion":"FAILURE","isRequired":false}' "$test_state" "$conclusion" ;;
        pending) printf '{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""}' ;;
        success) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}' ;;
        failure) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}' ;;
        # Two required failures. `test` then `lint`, or the reverse. Both
        # carry isRequired so the policy read does not tag only the last node.
        paired_pending|paired_failure|paired_failure_reversed)
            test_check='{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE","isRequired":true}'
            lint_check='{"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"FAILURE","isRequired":true}'
            if [ "$1" = paired_pending ]; then
                lint_check='{"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":"","isRequired":true}'
            fi
            if [ "$1" = paired_failure_reversed ]; then
                printf '%s,%s' "$lint_check" "$test_check"
            else
                printf '%s,%s' "$test_check" "$lint_check"
            fi ;;
    esac
}
status() {
    # 1 when this `pr view` asked for isDraft. A draft fixture refuses to
    # answer otherwise, so dropping the field fails the regression.
    draft_field=${1:-0}
    pr_head=$(cat "$forge/pr-head")
    pr_base=$(cat "$forge/pr-base")
    if [ -f "$forge/merged" ]; then
        touch "$forge/merged-reads"
        # GitHub keeps headRefOid after the head branch is deleted. `land`
        # records it; a forge that merged before this substitute did falls
        # back to the live branch.
        merged_head=$(head_sha)
        if [ -f "$forge/merged-head" ]; then
            merged_head=$(cat "$forge/merged-head")
        fi
        printf '{"number":42,"state":"MERGED","isDraft":false,"mergedAt":"2026-10-03T00:00:00Z","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","reviewDecision":"","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":{"oid":"%s"},"url":"%s"}\n' \
            "$(check success)" "$pr_head" "$merged_head" "$pr_base" "$(cat "$forge/merged")" "$url"
        return
    fi
    reads=$(( $(cat "$forge/reads" 2>/dev/null || echo 0) + 1 ))
    printf '%s\n' "$reads" > "$forge/reads"
    observed=$(sed -n "${reads}p" "$forge/checks")
    [ -n "$observed" ] || observed=$(tail -n 1 "$forge/checks")
    if [ "$observed" = source_contract ]; then
        if [ "$(git --git-dir="$remote" show "$(head_sha):src/contract" )" = fixed ]; then
            observed=success
        else
            observed=failure
        fi
    fi
    # A provider app's required aggregate fails beside its passing analysis
    # job; the mixed form also fails the workflow check `test`.
    case "$observed" in source_aggregate|source_aggregate_mixed)
        if [ "$(git --git-dir="$remote" show "$(head_sha):src/contract" )" = fixed ]; then
            observed=success
        else
            observed="${observed#source_}_failure"
        fi ;;
    esac
    state=OPEN
    reported_head=$(head_sha)
    if [ -f "$forge/heads" ]; then
        head_read=$(( reads - $(cat "$forge/heads-start") ))
        reported_head=$(sed -n "${head_read}p" "$forge/heads")
        [ -n "$reported_head" ] || reported_head=$(tail -n 1 "$forge/heads")
        [ "$reported_head" != current ] || reported_head=$(head_sha)
        if [ -f "$forge/remote-heads" ]; then
            remote_head=$(sed -n "${head_read}p" "$forge/remote-heads")
            [ -n "$remote_head" ] || remote_head=$(tail -n 1 "$forge/remote-heads")
            [ "$remote_head" != current ] || remote_head=$(head_sha)
            git --git-dir="$remote" update-ref refs/pull/42/head "$remote_head"
        fi
    fi
    merged_at=null
    review=""
    case "$observed" in
        mixed_pending|mixed_failure) merge_state=BLOCKED; rollup=$(check "$observed") ;;
        mixed_success) merge_state=CLEAN; rollup=$(check "$observed") ;;
        pending) merge_state=BLOCKED; rollup=$(check pending) ;;
        success) merge_state=CLEAN; rollup=$(check success) ;;
        failure) merge_state=BLOCKED; rollup=$(check failure) ;;
        paired_pending|paired_failure|paired_failure_reversed) merge_state=BLOCKED; rollup=$(check "$observed") ;;
        aggregate_failure|aggregate_mixed_failure)
            merge_state=BLOCKED
            rollup='{"__typename":"CheckRun","name":"CodeQL","workflowName":"","status":"COMPLETED","conclusion":"FAILURE"},{"__typename":"CheckRun","name":"Analyze (rust)","workflowName":"CodeQL","status":"COMPLETED","conclusion":"SUCCESS"}'
            [ "$observed" = aggregate_failure ] || rollup="$rollup,$(check failure)" ;;
        prerequisite)
            merge_state=BLOCKED
            rollup='{"__typename":"CheckRun","name":"Read state","workflowName":"Guarded CI","status":"COMPLETED","conclusion":"FAILURE"}'
            if [ -f "$forge/rerun-applied" ]; then
                case "$(cat "$forge/infrastructure")" in
                    positive|merge|lost|moved|deleted|legacy_success) merge_state=CLEAN; rollup='{"__typename":"CheckRun","name":"Protected (rust)","workflowName":"Guarded CI","status":"COMPLETED","conclusion":"SUCCESS"}' ;;
                    clean_missing|legacy_substitute) merge_state=CLEAN; rollup=$(check success) ;;
                esac
            fi ;;
        review_required) merge_state=BLOCKED; review=REVIEW_REQUIRED; rollup=$(check success) ;;
        # A freshly pushed head before the provider has queued any check.
        unreported) merge_state=BLOCKED; rollup='' ;;
        dirty) merge_state=DIRTY; rollup=$(check success) ;;
        closed) state=CLOSED; merge_state=CLEAN; rollup=$(check success) ;;
        contradictory) merged_at='"2026-10-05T12:47:15Z"'; merge_state=CLEAN; rollup=$(check success) ;;
        *) echo "fake gh: unknown check state '$observed'" >&2; exit 2 ;;
    esac
    if [ -f "$forge/review-decision" ]; then review=$(cat "$forge/review-decision"); fi
    # [ORB-16342] An explicit draft whose merge state is not DRAFT. The file
    # names that merge state. Ready pull requests omit it and report false.
    is_draft=false
    if [ -f "$forge/draft-merge-state" ]; then
        if [ "$draft_field" != 1 ]; then
            echo "fake gh: isDraft was not requested" >&2
            exit 2
        fi
        is_draft=true
        merge_state=$(cat "$forge/draft-merge-state")
        case "$merge_state" in
            BLOCKED|UNKNOWN) ;;
            *) echo "fake gh: unknown draft merge state '$merge_state'" >&2; exit 2 ;;
        esac
    fi
    printf '{"number":42,"state":"%s","isDraft":%s,"mergedAt":%s,"mergeable":"MERGEABLE","mergeStateStatus":"%s","reviewDecision":"%s","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":null,"url":"%s"}\n' \
        "$state" "$is_draft" "$merged_at" "$merge_state" "$review" "$rollup" "$pr_head" "$reported_head" "$pr_base" "$url"
}
create() {
    attempt=$(awk '$0 == "pr create" { n++ } END { print n }' "$forge/calls")
    outcome=success
    if [ -f "$forge/create-outcomes" ]; then
        outcome=$(sed -n "${attempt}p" "$forge/create-outcomes")
        [ -n "$outcome" ] || outcome=$(tail -n 1 "$forge/create-outcomes")
    fi
    case "$outcome" in
        gateway) echo 'HTTP 502: 502 Bad Gateway' >&2; exit 1 ;;
        auth) echo 'Resource not accessible by integration (HTTP 403)' >&2; exit 1 ;;
        unknown_head) echo "Head sha can't be blank" >&2; exit 1 ;;
        no_commits) echo 'No commits between agent-main and orbit/landing-candidate' >&2; exit 1 ;;
        body_too_long) echo 'Body is too long (maximum is 65536 characters)' >&2; exit 1 ;;
        rate_limit) echo 'API rate limit exceeded (HTTP 403)' >&2; exit 1 ;;
        success|applied_gateway) ;;
        *) echo "fake gh: unknown create outcome '$outcome'" >&2; exit 2 ;;
    esac
    if [ -f "$forge/pr-head" ]; then
        echo 'a pull request for this branch already exists' >&2
        exit 1
    fi
    shift 2
    while [ $# -gt 0 ]; do
        case "$1" in
            --base) printf '%s\n' "$2" > "$forge/pr-base"; shift 2 ;;
            --head) printf '%s\n' "$2" > "$forge/pr-head"; shift 2 ;;
            --body) printf '%s' "$2" > "$forge/pr-body"; shift 2 ;;
            --draft) touch "$forge/pr-draft"; shift ;;
            --label) printf '%s\n' "$2" >> "$forge/pr-labels"; shift 2 ;;
            --reviewer) printf '%s\n' "$2" >> "$forge/pr-reviewers"; shift 2 ;;
            *) shift ;;
        esac
    done
    echo created >> "$forge/created-prs"
    if [ "$outcome" = applied_gateway ]; then
        echo 'HTTP 502: 502 Bad Gateway' >&2
        exit 1
    fi
    printf '%s\n' "$url"
}
# A provider error as `gh api` reports it: the response body on stdout and
# `gh: <message> (HTTP <status>)` on stderr.
refuse() {
    printf '{"message":"%s","documentation_url":"https://docs.github.com/rest/pulls/pulls#merge-a-pull-request","status":"%s"}\n' "$2" "$1"
    printf 'gh: %s (HTTP %s)\n' "$2" "$1" >&2
    exit 1
}
# Jobs and logs planted per commit under `$forge/commit-ci/<sha>/`.
commit_ci_api() {
    if [ ! -d "$forge/commit-ci" ]; then
        echo "fake gh: unsupported call: $*" >&2
        exit 2
    fi
    path="${4:-}"
    case "$path" in
        repos/*/actions/runs)
            sha=""
            for arg in "$@"; do
                case "$arg" in
                    head_sha=*) sha=${arg#head_sha=} ;;
                esac
            done
            if [ -n "$sha" ] && [ -f "$forge/commit-ci/$sha/runs.json" ]; then
                cat "$forge/commit-ci/$sha/runs.json"
            else
                echo '{"workflow_runs":[]}'
            fi
            ;;
        repos/*/actions/runs/*/jobs)
            run=$(basename "$(dirname "$path")")
            found=""
            for dir in "$forge/commit-ci"/*; do
                [ -f "$dir/run_id" ] || continue
                if [ "$(cat "$dir/run_id")" = "$run" ]; then
                    found=$dir
                    break
                fi
            done
            if [ -z "$found" ]; then
                echo "fake gh: no jobs for run $run" >&2
                exit 2
            fi
            # `jobs-from-<n>.json` replaces the answer from status read <n>
            # on; one holding `unreadable` refuses the listing.
            answer="$found/jobs.json"
            reads=$(cat "$forge/reads" 2>/dev/null || echo 0)
            poll=1
            while [ "$poll" -le "$reads" ]; do
                [ -f "$found/jobs-from-$poll.json" ] && answer="$found/jobs-from-$poll.json"
                poll=$((poll + 1))
            done
            if [ "$(cat "$answer")" = unreadable ]; then
                echo "fake gh: jobs for run $run unavailable" >&2
                exit 2
            fi
            cat "$answer"
            ;;
        *)
            echo "fake gh: unsupported call: $*" >&2
            exit 2
            ;;
    esac
}
commit_ci_log() {
    if [ ! -d "$forge/commit-ci" ]; then
        echo "fake gh: unsupported call: $*" >&2
        exit 2
    fi
    run="${3:-}"
    for dir in "$forge/commit-ci"/*; do
        [ -f "$dir/run_id" ] || continue
        if [ "$(cat "$dir/run_id")" = "$run" ]; then
            job=""
            previous=""
            for arg in "$@"; do
                if [ "$previous" = --job ]; then job="$arg"; fi
                previous="$arg"
            done
            if [ -f "$dir/log-$job" ]; then cat "$dir/log-$job"; else cat "$dir/log"; fi
            return
        fi
    done
    echo "fake gh: no log for run $run" >&2
    exit 2
}
# Advance a remote branch by one commit, as another writer would.
advance() {
    ref="refs/heads/$1"
    parent=$(git --git-dir="$remote" rev-parse "$ref")
    next=$(git --git-dir="$remote" commit-tree "$parent^{tree}" -p "$parent" -m "$2")
    git --git-dir="$remote" update-ref "$ref" "$next" "$parent"
}
# Squash `$1` onto the PR base and mark the PR merged.
land() {
    # Remember the head oid before any later deletion. GitHub still reports
    # it on a merged pull request after the branch is gone.
    if [ ! -f "$forge/merged-head" ]; then
        printf '%s\n' "$(head_sha)" > "$forge/merged-head"
    fi
    base_ref="refs/heads/$(cat "$forge/pr-base")"
    parent=$(git --git-dir="$remote" rev-parse "$base_ref")
    tree=$(git --git-dir="$remote" rev-parse "$1^{tree}")
    landed=$(git --git-dir="$remote" commit-tree "$tree" -p "$parent" -m "Squash pull request #42")
    git --git-dir="$remote" update-ref "$base_ref" "$landed" "$parent"
    printf '%s\n' "$landed" > "$forge/merged"
}
merge() {
    sha=""
    method=""
    for arg in "$@"; do
        case "$arg" in
            sha=*) sha=${arg#sha=} ;;
            merge_method=*) method=${arg#merge_method=} ;;
        esac
    done
    printf 'sha=%s merge_method=%s\n' "$sha" "$method" >> "$forge/merge-requests"
    # `merge-outcomes` scripts the answer to each merge request in turn; an
    # unscripted request is an ordinary conditional merge.
    attempt=$(( $(wc -l < "$forge/merge-requests") ))
    outcome=$(sed -n "${attempt}p" "$forge/merge-outcomes" 2>/dev/null || true)
    base_modified="Base branch was modified. Review and try the merge again."
    case "$outcome" in
        ""|merge) ;;
        base_modified_422) refuse 422 "$base_modified" ;;
        http_499) refuse 499 "499 (https://api.github.com/graphql)" ;;
        base_modified*)
            # Another PR landed on the base between the provider's
            # mergeability check and this mutation.
            advance "$(cat "$forge/pr-base")" "Concurrent landing"
            case "$outcome" in
                *+head_moved) advance "$(cat "$forge/pr-head")" "Unreviewed push" ;;
                *+retargeted) printf 'other-base\n' > "$forge/pr-base" ;;
                *+merged) land "$sha" ;;
                *+merged_other_head)
                    advance "$(cat "$forge/pr-head")" "Unreviewed push"
                    land "$(head_sha)" ;;
                *+policy_disallowed) touch "$forge/merge-methods-disallowed" ;;
            esac
            refuse 405 "$base_modified" ;;
        policy) refuse 405 "Merge commits are not allowed on this repository." ;;
        queue) refuse 405 "Changes must be made through the merge queue" ;;
        protection) refuse 405 "At least 1 approving review is required by reviewers with write access." ;;
        auth) refuse 403 "Resource not accessible by integration" ;;
        head_modified) refuse 409 "Head branch was modified. Review and try the merge again." ;;
        server_error) refuse 502 "Server Error" ;;
        mismatched_body)
            printf '{"message":"Pull Request is not mergeable","status":"405"}\n'
            printf 'gh: %s (HTTP 405)\n' "$base_modified" >&2
            exit 1 ;;
        malformed_body)
            printf '<html>upstream error</html>\n'
            printf 'gh: %s (HTTP 405)\n' "$base_modified" >&2
            exit 1 ;;
        trailing_stderr)
            printf '{"message":"%s","status":"405"}\n' "$base_modified"
            printf 'gh: %s (HTTP 405)\nwarning: retried after a proxy reset\n' "$base_modified" >&2
            exit 1 ;;
        transport_timeout)
            echo 'Put "https://api.github.com/repos/orbit/test/pulls/42/merge": net/http: request canceled (Client.Timeout exceeded while awaiting headers)' >&2
            exit 1 ;;
        *) echo "fake gh: unknown merge outcome '$outcome'" >&2; exit 2 ;;
    esac
    if [ "$sha" != "$(head_sha)" ]; then
        echo "gh: Head branch was modified. Review and try the merge again. (HTTP 409)" >&2
        exit 1
    fi
    land "$sha"
    printf '{"sha":"%s","merged":true,"message":"Pull Request successfully merged"}\n' "$(cat "$forge/merged")"
}
if [ -f "$forge/failed-command-log" ]; then
    case "$1 ${2:-}" in
        "run view") cat "$forge/failed-command-log"; exit ;;
        "api --method")
            case "${4:-}" in
                'repos/{owner}/{repo}/actions/runs')
                    sha=""
                    for arg in "$@"; do
                        case "$arg" in
                            head_sha=*) sha=${arg#head_sha=} ;;
                        esac
                    done
                    if [ "$sha" = "$(head_sha)" ]; then
                        echo '{"workflow_runs":[{"id":10,"name":"CI"}]}'
                    else
                        echo '{"workflow_runs":[]}'
                    fi
                    exit ;;
                'repos/{owner}/{repo}/actions/runs/10/jobs') cat "$forge/failed-command-jobs"; exit ;;
            esac ;;
    esac
fi
if [ -f "$forge/infrastructure" ]; then
    phase=before
    [ ! -f "$forge/rerun-applied" ] || phase=after
    if [ -f "$forge/infra-auth403" ] && [ "$1" = api ]; then
        echo 'gh: Resource not accessible by integration (HTTP 403)' >&2; exit 1
    fi
    case "$1 ${2:-}" in
        "api graphql")
            case "$*" in
                *requiredStatusChecks*) cat "$forge/infra-policy"; exit ;;
                *statusCheckRollup*) cat "$forge/infra-rollup-policy"; exit ;;
            esac ;;
        "api repos/{owner}/{repo}") cat "$forge/infra-repo"; exit ;;
        "api repos/{owner}/{repo}/pulls/42") cat "$forge/infra-pr"; exit ;;
        "api repos/{owner}/{repo}/rules/branches/agent-main?per_page=100") cat "$forge/infra-rules"; exit ;;
        "api repos/{owner}/{repo}/actions/runs/700") cat "$forge/infra-run-$phase"; exit ;;
        "api repos/{owner}/{repo}/actions/runs/700/attempts/1/jobs?per_page=100") cat "$forge/infra-jobs"; exit ;;
        "api repos/{owner}/{repo}/actions/workflows/600") cat "$forge/infra-workflow"; exit ;;
        "api repos/{owner}/{repo}/check-suites/800") cat "$forge/infra-suite"; exit ;;
        "run view") cat "$forge/infra-log"; exit ;;
        "run rerun")
            [ -f "$forge/infra-reserved" ] || { echo 'rerun without durable reservation' >&2; exit 2; }
            mode=$(cat "$forge/infrastructure")
            if [ "$mode" = auth ]; then echo 'gh: Resource not accessible by integration (HTTP 403)' >&2; exit 1; fi
            if [ "$mode" != no_apply ]; then touch "$forge/rerun-applied"; fi
            if [ "$mode" = moved ]; then advance "$(cat "$forge/pr-head")" foreign; fi
            if [ "$mode" = deleted ]; then git --git-dir="$remote" update-ref -d "refs/heads/$(cat "$forge/pr-head")"; fi
            if [ "$mode" = lost ] || [ "$mode" = no_apply ]; then echo 'HTTP 503: lost mutation reply' >&2; exit 1; fi
            exit ;;
        "api "*)
            case "$2" in
                *'/commits/'*'/status?'*) cat "$forge/infra-statuses-$phase"; exit ;;
                *'/commits/'*'/check-runs?'*) cat "$forge/infra-checks-$phase"; exit ;;
                *'/actions/runs?head_sha='*) cat "$forge/infra-runs-$phase"; exit ;;
                *'/contents/'*) cat "$forge/infra-contents"; exit ;;
            esac ;;
    esac
fi
# Current branch policy requires `test` [ORB-16339]. The head reports it
# unless the latest status read scripted `unreported`.
if [ -f "$forge/gates" ]; then
    observed=$(sed -n "$(cat "$forge/reads")p" "$forge/checks")
    [ -n "$observed" ] || observed=$(tail -n 1 "$forge/checks")
    head=$(head_sha)
    pr_base=$(cat "$forge/pr-base")
    repo='{"id":500,"full_name":"orbit/test","url":"https://api.github.com/repos/orbit/test"}'
    case "$1 ${2:-}" in
        "api repos/{owner}/{repo}") printf '%s\n' "$repo"; exit ;;
        "api repos/{owner}/{repo}/pulls/42")
            printf '{"number":42,"state":"open","merged":false,"head":{"sha":"%s","ref":"%s","repo":%s},"base":{"sha":"%s","ref":"%s","repo":%s}}\n' \
                "$head" "$(cat "$forge/pr-head")" "$repo" "$(git --git-dir="$remote" rev-parse "refs/heads/$pr_base")" "$pr_base" "$repo"
            exit ;;
        "api graphql")
            case "$*" in
                *requiredStatusChecks*)
                    printf '{"data":{"repository":{"pullRequest":{"headRefOid":"%s","baseRefName":"%s","reviewDecision":null,"baseRef":{"branchProtectionRule":{"id":"RULE-1","requiredStatusChecks":[{"context":"test","app":null}]}}}}}}\n' "$head" "$pr_base"
                    exit ;;
            esac ;;
        "api "*)
            case "$2" in
                *'/rules/branches/'*) echo '[]'; exit ;;
                *'/check-runs?'*)
                    if [ "$observed" = unreported ]; then echo '{"total_count":0,"check_runs":[]}'; exit; fi
                    printf '{"total_count":1,"check_runs":[{"id":1,"name":"test","head_sha":"%s","app":{"id":900},"status":"completed","conclusion":"success"}]}\n' "$head"
                    exit ;;
                *'/status?'*) printf '{"sha":"%s","repository":%s,"total_count":0,"statuses":[]}\n' "$head" "$repo"; exit ;;
                *'/actions/runs?'*) echo '{"total_count":0,"workflow_runs":[]}'; exit ;;
            esac ;;
    esac
fi
case "$1 ${2:-}" in
    "pr list")
        # An unfiltered listing (the one that asks for `body`) can be planted
        # so a delivery's earlier pull requests are visible. Head lookups stay
        # on the single pull request this substitute creates.
        case "$*" in
            *body*)
                if [ -f "$forge/open-prs.json" ]; then
                    cat "$forge/open-prs.json"
                    exit
                fi
                ;;
        esac
        if [ -f "$forge/pr-head" ]; then
            printf '[{"number":%s,"title":"Landing","headRefName":"%s","author":{"login":"orbit"}}]\n' "$number_json" "$(cat "$forge/pr-head")"
        else
            echo '[]'
        fi ;;
    "pr create") create "$@" ;;
    "pr close")
        if [ -f "$forge/pr-close-failure" ]; then
            cat "$forge/pr-close-failure" >&2
            exit 1
        fi
        printf 'Closed\n' ;;
    "pr view")
        if [ -f "$forge/pr-view-outcomes" ]; then
            view_attempt=$(( $(cat "$forge/pr-view-reads") + 1 ))
            printf '%s\n' "$view_attempt" > "$forge/pr-view-reads"
            view_outcome=$(sed -n "${view_attempt}p" "$forge/pr-view-outcomes")
            [ -n "$view_outcome" ] || view_outcome=$(tail -n 1 "$forge/pr-view-outcomes")
            if [ "$view_outcome" = invalid_json ]; then printf '{'; exit 0; fi
            if [ "$view_outcome" != success ]; then
                printf '%s\n' "$view_outcome" >&2
                exit 1
            fi
        fi
        case "$*" in
            *mergeStateStatus*)
                draft_field=0
                case "$*" in *isDraft*) draft_field=1 ;; esac
                status "$draft_field" ;;
            *) printf '{"number":%s,"title":"Landing","body":"","headRefName":"%s","files":[],"commits":[],"url":"%s"}\n' "$number_json" "$(cat "$forge/pr-head")" "$url" ;;
        esac ;;
    "repo view") echo '{"nameWithOwner":"orbit/test","url":"https://github.com/orbit/test"}' ;;
    "api graphql")
        case "$*" in
            *statusCheckRollup*)
                observed=$(sed -n "$(cat "$forge/reads")p" "$forge/checks")
                [ -n "$observed" ] || observed=$(tail -n 1 "$forge/checks")
                if [ -f "$forge/required-auth" ]; then echo 'HTTP 401: Bad credentials' >&2; exit 1; fi
                if [ -f "$forge/required-policy" ]; then cat "$forge/required-policy"; exit; fi
                rollup=$(check "$observed")
                case "$observed" in
                    mixed_*|paired_failure|paired_failure_reversed) ;;
                    *) rollup=$(printf '%s' "$rollup" | sed 's/}$/,"isRequired":true}/') ;;
                esac
                printf '{"data":{"repository":{"pullRequest":{"headRefOid":"%s","commits":{"nodes":[{"commit":{"oid":"%s","statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[%s]}}}}]}}}}}\n' "$(head_sha)" "$(head_sha)" "$rollup"
                exit ;;
        esac
        # An unreviewed push that lands after the freshness check, while the
        # merge strategy is being resolved and before the merge request.
        if [ -f "$forge/push-before-merge" ] && [ ! -f "$forge/pushed" ]; then
            advance "$(cat "$forge/pr-head")" "Unreviewed push"
            touch "$forge/pushed"
        fi
        allowed=true
        [ ! -f "$forge/merge-methods-disallowed" ] || allowed=false
        printf '{"data":{"repository":{"autoMergeAllowed":true,"mergeCommitAllowed":false,"rebaseMergeAllowed":%s,"squashMergeAllowed":%s,"pullRequest":{"baseRefName":"%s","baseRef":{"branchProtectionRule":{"requiresLinearHistory":true}}}}}}\n' "$allowed" "$allowed" "$(cat "$forge/pr-base")" ;;
    "api repos/{owner}/{repo}/pulls/42/merge") merge "$@" ;;
    "api --method") commit_ci_api "$@" ;;
    "run view") commit_ci_log "$@" ;;
    *) echo "fake gh: unsupported call: $*" >&2; exit 2 ;;
esac
"#;
// Isolated child process
/// Run `body` in a copy of this test binary whose `PATH` starts with the substitute `gh`, and fail if it fails or outlives [`CHILD_DEADLINE`].
fn isolated(test: &str, body: impl FnOnce(&Path)) {
    if std::env::var_os(CHILD_ENV).is_some() {
        let sandbox = PathBuf::from(std::env::var_os(SANDBOX_ENV).unwrap());
        if std::env::var(CHILD_ENV).as_deref() == Ok(test) {
            orbit_engine::test_support::with_vcs_retry_wait(|_| {}, || body(&sandbox));
        } else {
            in_fresh_row(&sandbox, body);
        }
        return;
    }
    let sandbox = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let home = sandbox.path().join("home");
    let tmp = sandbox.path().join("tmp");
    let bin = sandbox.path().join("bin");
    for dir in [&home, &tmp, &bin] {
        fs::create_dir_all(dir).unwrap();
    }
    let gh = bin.join("gh");
    fs::write(
        &gh,
        FAKE_GH.replace("__SANDBOX__", path_str(sandbox.path())),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    // libtest names a test by its module path below the crate root.
    let qualified = format!("{}::{test}", module_path!().split_once("::").unwrap().1);
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") || name.starts_with("GH_") {
            command.env_remove(name.as_ref());
        }
    }
    git_fixture::configure_child(&mut command, &home, sandbox.path());
    command
        .args([&qualified, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env(CHILD_ENV, test)
        .env(SANDBOX_ENV, sandbox.path())
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TMPDIR", &tmp)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = run_bounded_capped(&mut command, CHILD_DEADLINE, 256 * 1024)
        .unwrap_or_else(|error| panic!("{test} did not finish in its isolated child: {error}"));
    test_env::assert_child_test_passed(&qualified, output.status, &output.stdout, &output.stderr);
}
/// The bounded child runs exactly one libtest thread. Change its environment only between completed rows, and restore it and cwd on return or unwind.
fn in_fresh_row(parent: &Path, body: impl FnOnce(&Path)) {
    struct RestoreCwd(PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).unwrap();
        }
    }
    let row = tempfile::tempdir_in(parent).unwrap();
    let home = row.path().join("home");
    let bin = row.path().join("bin");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::write(home.join(".gitconfig"), git_fixture::ISOLATED_GIT_CONFIG).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, FAKE_GH.replace("__SANDBOX__", path_str(row.path()))).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(path).unwrap();
    let xdg = home.join(".config");
    let _env = test_env::scoped([
        (SANDBOX_ENV, Some(path_str(row.path()))),
        ("HOME", Some(path_str(&home))),
        ("XDG_CONFIG_HOME", Some(path_str(&xdg))),
        ("PATH", Some(path.to_str().unwrap())),
        // The capture-stop row deliberately lowers this process-global ceiling. Restore it before a truncated-but-bounded output row runs.
        ("ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES", None),
    ]);
    let _cwd = RestoreCwd(std::env::current_dir().unwrap());
    std::env::set_current_dir(row.path()).unwrap();
    // TMPDIR stays at the parent-owned child root: a cached immutable Git seed must outlive the row. Each scenario still owns separate TempDirs.
    orbit_engine::test_support::with_vcs_retry_wait(|_| {}, || body(row.path()));
}
type Scenario = fn(&Path);
/// Run each scenario in its own fresh row under `parent` (the isolated child's sandbox), recording each pass by name as `run_scenarios` does.
#[allow(clippy::print_stderr)]
fn run_rows(parent: &Path, rows: &[(&str, Scenario)]) {
    for (name, scenario) in rows {
        eprintln!("scenario: {name}");
        in_fresh_row(parent, scenario);
        test_env::record_scenario_pass(name);
    }
}
// Wrapper tables: each `#[test]` runs its scenarios in one isolated child (see `isolated`); scenario names double as their row names.
macro_rules! container_groups {
    ($($name:ident => [$($s:ident),+]),+ $(,)?) => { $(
        fn $name(sandbox: &Path) {
            run_rows(sandbox, &[$((stringify!($s), $s as fn(&Path))),+]);
        }
    )+ };
}
macro_rules! test_groups {
    ($($name:ident => [$($s:ident),+]),+ $(,)?) => { $(
        #[test]
        fn $name() {
            isolated(stringify!($name), |sandbox| {
                run_rows(sandbox, &[$((stringify!($s), $s as fn(&Path))),+]);
            });
        }
    )+ };
}
macro_rules! row_groups {
    ($runner:ident; $($name:ident => [$($row:literal),+]),+ $(,)?) => { $(
        fn $name(_sandbox: &Path) { $runner(stringify!($name), &[$($row),+]); }
    )+ };
    (test $runner:ident; $($name:ident => [$($row:literal),+]),+ $(,)?) => { $(
        #[test]
        fn $name() { $runner(stringify!($name), &[$($row),+]); }
    )+ };
}
container_groups! {
    validation_environment_contracts => [missing_required_validation_tool_reports_its_search_path, validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain, a_missing_validation_tool_is_an_environment_failure],
    empty_validation_and_handoff_contracts => [required_validation_without_commands_is_a_no_op, claimed_validation_without_commands_is_a_recorded_no_op, claimed_handoff_carries_before_pr_evidence_only_for_its_candidate, claimed_handoff_without_logs_is_refused_when_commands_are_required],
    failure_handoff_contracts => [a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle, a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking, an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks, an_admission_refusal_for_held_evidence_stays_held_not_escalated, a_worktree_step_failure_ends_the_failure_hook_as_no_candidate, a_failure_before_the_worktree_checkpoint_releases_reviews_and_a_later_gap_is_refused],
    implementation_handoff_contracts => [an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr, a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write, an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task, a_child_its_parent_stopped_waiting_for_holds_its_candidate],
}
test_groups! {
    pr_body_and_promotion_contracts => [pr_promote_records_the_pull_request_page_with_its_number, pr_open_appends_the_review_fixes_section_to_the_published_body],
    metadata_lag_pins_and_repair => [previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins, previous_published_head_metadata_lag_after_in_run_conflict_repair],
    pr_open_branch_identity => [pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip, pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch, pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate],
    pr_create_retry_contracts => [pr_open_permanent_create_failures_never_retry_or_look_up_again, pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr, pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase],
    pr_lookup_retry_contracts => [pr_open_retries_http_499_pr_views_and_stops_at_the_existing_attempt_ceiling, pr_lookup_retry_classification_preserves_refusals_and_nonmatching_tokens],
    pr_number_and_push_transports => [pr_open_preserves_provider_number_representations, fixture_push_guard_checks_all_effective_push_urls, fixture_push_refuses_non_file_transports_without_network],
    git_push_effective_destination_contracts => [git_push_publishes_to_effective_push_destination_not_fetch_remote, git_push_probes_composed_rewrite_destination_not_its_rewrite, git_push_refuses_self_pointing_effective_push_url],
    base_race_fresh_gates => [a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence, a_base_race_refresh_refuses_unmet_gates_without_another_merge],
    base_race_candidate_identity => [a_base_race_refresh_refuses_a_changed_candidate_without_another_merge, an_already_merged_refresh_reconciles_without_another_merge_request],
    merge_refusal_bounds => [other_merge_refusals_fail_on_the_first_request, repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget],
    recovered_base_advances => [conflicting_base_advances_after_recovery_are_chased_until_the_bound, a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands],
    review_checks_and_delivery => [a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review, passing_checks_merge_the_reviewed_candidate_and_record_its_delivery, required_checks_the_head_has_not_reported_are_awaited_before_completion],
    review_fix_revalidation => [a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges, a_failed_review_revalidation_rejects_and_preserves_both_commits, a_red_base_during_pr_revalidation_holds_the_task],
    review_repair_leases => [a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease, a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes],
    claimed_validation_failures => [a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs, a_claimed_candidate_that_cannot_reach_the_network_fails_transient, a_required_command_red_on_the_base_holds_the_task_without_a_pr],
    claimed_publication_and_merge_pins => [a_pr_claim_validates_before_publication_and_pins_after_it, a_pr_merge_batch_lands_the_checked_head_at_its_pin, a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check],
    supervisor_and_truncated_failures => [supervisor_stops_are_environment_failures_and_never_poison_the_base_cache, a_failure_only_the_truncated_middle_names_still_requires_candidate_repair],
    network_retry_and_cached_base => [a_network_flake_is_rerun_before_the_candidate_is_judged, remote_base_transport_retries_preserve_access_refusals, claimed_handoff_fetch_timeout_uses_only_a_related_cached_base],
    linux_deferral_contracts => [a_claimed_leaf_replays_its_implementers_deferral_before_publication, a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step, a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery],
    macos_deferral_contracts => [a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication, a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication, a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs, a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure],
}
row_groups! { pull_ref_refusal_rows;
    pull_ref_identity_refusals => ["pull_ref_third", "pull_ref_third_after_old", "branch_missing", "branch_at_previous", "previous_unknown", "previous_empty"],
}
row_groups! { claimed_known_red_rows;
    claimed_known_red_equal_and_subset => ["equal", "subset"],
}
row_groups! { test known_red_coverage_rows;
    known_red_sandbox_notices => ["bubblewrap", "seatbelt", "deferred", "skip", "skipping-colon"],
    known_red_dropped_notices => ["skipping-space", "raw-refusal", "dropped-deferral", "dropped-skip"],
    known_red_replay_coverage => ["replay-with-notice", "replay-zero-tests", "replay-missing-count", "replay-missing-tool", "replay-executed"],
}
row_groups! { test pull_ref_refusal_rows;
    pull_ref_wait_and_review_refusals => ["previous_is_candidate", "persistent", "persistent_after_metadata", "time_cap", "review_mismatch", "ls_remote_fails"],
}
row_groups! { test claimed_known_red_rows;
    claimed_known_red_superset_and_disjoint => ["shared-plus-new", "disjoint-six-vs-one"],
}
#[test]
fn validation_repairs_and_logs() {
    isolated("validation_repairs_and_logs", |sandbox| {
        run_rows(
            sandbox,
            &[
                (
                    "required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs",
                    required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs
                        as fn(&Path),
                ),
                (
                    "required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes",
                    required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes
                        as fn(&Path),
                ),
            ],
        );
        in_fresh_row(
            sandbox,
            required_validation_retains_reported_evidence_after_scratch_lifetime,
        );
        run_rows(
            sandbox,
            &[(
                "printed_supervisor_notes_remain_candidate_failures_on_both_streams",
                printed_supervisor_notes_remain_candidate_failures_on_both_streams as fn(&Path),
            )],
        );
    });
}
// Group wrappers whose rows manage their own fresh rows (containers and *_rows runners) are called directly: nesting in_fresh_row would deadlock on its process-wide scoped-env lock.
#[test]
fn validation_and_empty_handoffs() {
    isolated("validation_and_empty_handoffs", |sandbox| {
        validation_environment_contracts(sandbox);
        empty_validation_and_handoff_contracts(sandbox);
    });
}
#[test]
fn failure_and_implementation_handoffs() {
    isolated("failure_and_implementation_handoffs", |sandbox| {
        failure_handoff_contracts(sandbox);
        implementation_handoff_contracts(sandbox);
    });
}
#[test]
fn pull_ref_pins_and_identity() {
    isolated("pull_ref_pins_and_identity", |sandbox| {
        run_rows(
            sandbox,
            &[(
                "previous_published_head_pull_ref_lag_settles_with_exact_merge_pin",
                previous_published_head_pull_ref_lag_settles_with_exact_merge_pin as fn(&Path),
            )],
        );
        pull_ref_identity_refusals(sandbox);
    });
}
#[test]
fn known_red_validation_and_subset() {
    isolated("known_red_validation_and_subset", |sandbox| {
        run_rows(
            sandbox,
            &[(
                "a_candidate_failing_only_known_red_tests_proceeds_and_records_them",
                a_candidate_failing_only_known_red_tests_proceeds_and_records_them as fn(&Path),
            )],
        );
        claimed_known_red_equal_and_subset(sandbox);
    });
}
