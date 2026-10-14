// Drives a `#!/bin/sh` substitute `gh` from every test.
#![cfg(unix)]
#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! PR landing and merge gating through the engine's deterministic actions,
//! and the required validation that gates a candidate before it is published.
//!
//! Each test drives the shipped `candidate_validate`, `pr_open` and
//! `pr_complete` actions through
//! [`execute_deterministic_action`]. The host keeps tasks in memory but does
//! not replace the provider boundary: the engine's own adapter runs `gh` and
//! `git` as it does in production. A substitute `gh` first on `PATH` stands in
//! for the forge. It serves one pull request whose head and base branches live
//! in a real bare remote. Each status read takes the next scripted check state.
//! A conditional merge request lands a real squash commit on the remote base.
//! The substitute enforces only the head-SHA condition, never branch
//! protection, so the engine is the only gate between a red check and a merge.
//! The deferred-sandbox fixtures [ORB-15287] run the shipped pipeline's
//! implementation and validation steps as one job graph instead, so the
//! implement step boundary and the replay are exercised together.
//!
//! `PATH`, `$HOME` and Git configuration are process-global, so each test body
//! re-runs in an isolated copy of this binary. That child has disposable
//! `$HOME`/`$TMPDIR`, no inherited Orbit, Git or GitHub authority, and a bounded
//! wait. The shared supervisor kills and reaps it on timeout.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{
    NotFoundKind, OrbitError, RecoverableVcsConflict, process::run_bounded_capped, test_env,
};
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    BaselineHoldStatus, ClaimExecutionContext, ClaimedKnownRed, DispatchError, KnownRedUnconfirmed,
    RebaseRecoveryAttemptScope, ReviewLandingRequest, ReviewReleaseRequest, RuntimeHost,
    TaskActivityUpdate, TaskAutomationUpdate, baseline_hold_status, execute_deterministic_action,
    execute_job_with_resume, resolve_job_catalog_refs_for_execution, review_gate,
    verify_claimed_known_red,
};
use orbit_exec::{LoginShell, ValidationEnvPolicy, ValidationEnvironment};
use orbit_types::task::{
    ContextWideningStep, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskArtifact,
    TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
    push_external_ref_if_missing,
};
use orbit_types::workflow::activity_job::{ActivityV2, ActivityV2Spec, DeterministicSpec};
use orbit_types::workflow::handoff::{
    HandoffDelivery, HandoffReviewDisposition, HandoffValidationLog, TaskHandoff,
};
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BUBBLEWRAP_NAMESPACE_PROBE, BaselineRedHold, ClaimFailureClass,
    DEFERRED_SANDBOX_ARTIFACT, DeferralProbe, EvidenceHostOs, JobRun, JobV2StepBody,
    KNOWN_RED_VALIDATION_EVENT, KnownRedValidation, NamespaceProbe, PR_AUTO_FIX_ADMITTED_EVENT,
    PR_AUTO_FIX_BUDGET, PR_AUTO_FIX_PUSHED_EVENT, PS_EXEC_PROBE, PipelineState, PrAutoFixAttempt,
    PrAutoFixBlocker, PrAutoFixPush, ReviewGateRefusal, ReviewTiming, ReviewVerdict,
    SEATBELT_APPLY_PROBE, is_baseline_red_failure, pr_auto_fix_attempts,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod auto_fix;
mod flaky;
#[cfg(target_os = "linux")]
mod stable_build_mount;

use super::git_fixture;
use super::v2_runtime::{build_writer_and_sinks, job_asset, workspace_root};

/// Names the test whose body this process runs as the isolated child.
const CHILD_ENV: &str = "ORBIT_PR_LANDING_CHILD";
/// The parent-owned directory holding the substitute `gh` and its pointer to
/// the forge state of the case being run.
const SANDBOX_ENV: &str = "ORBIT_PR_LANDING_SANDBOX";
/// Upper bound on one isolated test body, including the engine's poll sleeps.
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;

const BASE: &str = "agent-main";
const BRANCH: &str = "orbit/landing-candidate";
const RUN_ID: &str = "jrun-landing";
const TASK_ID: &str = "T-LANDING";
const PR_NUMBER: &str = "42";

// ---------------------------------------------------------------------------
// Merge gating
// ---------------------------------------------------------------------------

/// `old` for `old_reads` reads, then `new`.
fn scripted_heads(old: &str, new: &str, old_reads: usize) -> Vec<String> {
    let mut heads = vec![old.to_string(); old_reads];
    heads.push(new.to_string());
    heads
}

/// Regression for ORB-14315: provider metadata may lag a verified lease-push,
/// but both the merge mutation and review landing must keep the new exact SHA.
fn previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins() {
    isolated(
        "previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins",
        |sandbox| {
            // Review presence and bounded reread count are independent: retain
            // both routes and both counts without their Cartesian product.
            for (reviewed, stale_reads) in [(true, 2), (false, 1)] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                host.set_status(TASK_ID, TaskStatus::Review);
                let mut input = fx.republished_completion_input(&host);
                let candidate = input["published_head_sha"].as_str().unwrap().to_string();
                if !reviewed {
                    input.as_object_mut().unwrap().remove("reviewed_head_sha");
                }
                let mut heads = vec![fx.candidate.as_str(); stale_reads];
                heads.push(&candidate);
                fx.script_heads(&heads, Some(&candidate));
                let before = fx.status_reads();

                let completed = action(&host, "pr_complete", &input)
                    .expect("known previous-head metadata catches up within the bound");

                assert_eq!(completed["merge"]["stale_head_observations"], stale_reads);
                assert_eq!(completed["merge"]["waited_seconds"], stale_reads * 5);
                assert_eq!(fx.status_reads() - before, stale_reads + 2);
                assert_eq!(
                    fx.merge_requests(),
                    vec![format!("sha={candidate} merge_method=squash")]
                );
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
        },
    );
}

/// Only the recorded previous head can wait, and only with the exact remote
/// PR ref. Exhaustion and foreign identity never merge or complete a task.
#[test]
fn previous_published_head_metadata_lag_refuses_unverified_or_persistent_heads() {
    isolated(
        "previous_published_head_metadata_lag_refuses_unverified_or_persistent_heads",
        |sandbox| {
            for case in [
                "persistent",
                "review_only",
                "time_cap",
                "third_head",
                "third_after_old",
                "remote_missing",
                "remote_moved_after_old",
                "review_mismatch",
                "review_only_third",
                "no_checkpoint",
            ] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                host.set_status(TASK_ID, TaskStatus::Review);
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

                let error = action(&host, "pr_complete", &input)
                    .expect_err("unverified metadata cannot authorize delivery");

                assert!(error.to_string().contains(error_code), "{case}: {error}");
                assert_eq!(
                    fx.status_reads() - before,
                    expected_reads,
                    "{case}: bounded reads"
                );
                assert!(fx.merge_requests().is_empty(), "{case}: no merge request");
                assert!(host.landings().is_empty(), "{case}: no review landing");
                assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
                assert_eq!(fx.remote_tip(BASE), fx.base_sha, "{case}: base unchanged");
            }
        },
    );
}

/// Regression for ORB-14663: after a verified lease-push, `refs/pull/<N>/head`
/// may briefly name the previous head too. Completion waits inside the same
/// bound while the task branch on origin already names the candidate, and
/// still merges only that exact SHA.
fn previous_published_head_pull_ref_lag_settles_with_exact_merge_pin() {
    isolated(
        "previous_published_head_pull_ref_lag_settles_with_exact_merge_pin",
        |sandbox| {
            // Reads that still report the previous head: (metadata, pull ref).
            for (metadata_old_reads, pull_ref_old_reads) in [(2, 2), (1, 2)] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                host.set_status(TASK_ID, TaskStatus::Review);
                let input = fx.republished_completion_input(&host);
                let candidate = input["published_head_sha"].as_str().unwrap().to_string();
                assert_eq!(fx.remote_tip(BRANCH), candidate, "branch ref is replaced");
                let metadata = scripted_heads(&fx.candidate, &candidate, metadata_old_reads);
                let pull_ref = scripted_heads(&fx.candidate, &candidate, pull_ref_old_reads);
                fx.script_heads(
                    &metadata.iter().map(String::as_str).collect::<Vec<_>>(),
                    Some(&fx.candidate),
                );
                fs::write(fx.forge.join("remote-heads"), pull_ref.join("\n") + "\n").unwrap();
                let before = fx.status_reads();

                let completed = action(&host, "pr_complete", &input)
                    .expect("the pull ref catches up within the bound");

                assert_eq!(
                    completed["merge"]["stale_head_observations"],
                    pull_ref_old_reads
                );
                assert_eq!(completed["merge"]["waited_seconds"], pull_ref_old_reads * 5);
                assert_eq!(fx.status_reads() - before, pull_ref_old_reads + 2);
                assert_eq!(
                    fx.merge_requests(),
                    vec![format!("sha={candidate} merge_method=squash")]
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Done);
            }
        },
    );
}

/// The pull-ref wait is fail-closed: anything but the known previous head with
/// the task branch at the candidate, inside the bound, refuses and leaves the
/// task in review without a merge request.
fn pull_ref_refusal_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        for case in [
            "pull_ref_third",
            "pull_ref_third_after_old",
            "branch_missing",
            "branch_at_previous",
            "previous_unknown",
            "previous_empty",
            "previous_is_candidate",
            "persistent",
            "persistent_after_metadata",
            "time_cap",
            "review_mismatch",
            "ls_remote_fails",
        ]
        .into_iter()
        .filter(|case| selected.contains(case))
        {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .unwrap();
            host.set_status(TASK_ID, TaskStatus::Review);
            let mut input = fx.republished_completion_input(&host);
            let candidate = input["published_head_sha"].as_str().unwrap().to_string();
            let remote_git_dir = fx.forge.join("remote.git");
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
                    git(
                        &fx.repo,
                        &[
                            "--git-dir",
                            path_str(&remote_git_dir),
                            "update-ref",
                            "-d",
                            &format!("refs/heads/{BRANCH}"),
                        ],
                    );
                    (1, "delivery_evidence_stale")
                }
                "branch_at_previous" => {
                    git(
                        &fx.repo,
                        &[
                            "--git-dir",
                            path_str(&remote_git_dir),
                            "update-ref",
                            &format!("refs/heads/{BRANCH}"),
                            &fx.candidate,
                        ],
                    );
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
                    git(
                        &fx.repo,
                        &[
                            "remote",
                            "set-url",
                            "origin",
                            "/nonexistent/orbit-remote.git",
                        ],
                    );
                    (1, "delivery_evidence_stale")
                }
                _ => unreachable!(),
            };
            fx.script_heads(
                &metadata.iter().map(String::as_str).collect::<Vec<_>>(),
                Some(&fx.candidate),
            );
            fs::write(fx.forge.join("remote-heads"), pull_ref.join("\n") + "\n").unwrap();
            let before = fx.status_reads();

            let error = action(&host, "pr_complete", &input)
                .expect_err("an unverified pull ref cannot authorize delivery");

            assert!(error.to_string().contains(error_code), "{case}: {error}");
            assert_eq!(
                fx.status_reads() - before,
                expected_reads,
                "{case}: bounded reads"
            );
            assert!(fx.merge_requests().is_empty(), "{case}: no merge request");
            assert!(host.landings().is_empty(), "{case}: no review landing");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
            assert_eq!(fx.remote_tip(BASE), fx.base_sha, "{case}: base unchanged");
        }
    });
}

/// An in-run conflict repair retains the head it replaces even without an
/// upstream previous-head checkpoint, so its verified push can settle too.
fn previous_published_head_metadata_lag_after_in_run_conflict_repair() {
    isolated(
        "previous_published_head_metadata_lag_after_in_run_conflict_repair",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .unwrap();
            host.set_status(TASK_ID, TaskStatus::Review);
            fx.advance_base();
            fx.script_checks(&["dirty", "success"]);
            fx.script_heads(
                &[&fx.candidate, &fx.candidate, "current"],
                Some(&fx.candidate),
            );
            fs::write(fx.forge.join("remote-heads"), "current\n").unwrap();
            let mut input = fx.complete_ungated_input();
            input["max_wait_seconds"] = json!(60);

            let completed = action(&host, "pr_complete", &input)
                .expect("the repaired push's metadata catches up");

            let repaired = fx.head();
            assert_ne!(repaired, fx.candidate);
            assert_eq!(completed["merge"]["stale_head_observations"], 1);
            assert_eq!(
                completed["merge"]["delivery_evidence"]["head_sha"],
                repaired
            );
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={repaired} merge_method=squash")]
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);
        },
    );
}

fn pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr() {
    isolated(
        "pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr",
        |sandbox| {
            for (outcome, expected_calls, reused) in [
                (
                    "gateway",
                    vec!["pr list", "pr create", "pr list", "pr create", "pr view"],
                    false,
                ),
                (
                    "applied_gateway",
                    vec!["pr list", "pr create", "pr list", "pr view"],
                    true,
                ),
            ] {
                let fx = Fixture::new(sandbox);
                fx.script_creates(&[outcome, "success"]);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let opened = action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .expect("a transient create must reconcile and recover");

                assert_eq!(opened["pr_created"], !reused, "{outcome}");
                assert_eq!(opened["pr_reused"], reused, "{outcome}");
                assert_eq!(opened["pr_number"], PR_NUMBER);
                assert_eq!(fx.forge_calls(), expected_calls, "{outcome}");
                assert_eq!(
                    fx.forge_lines("created-prs").len(),
                    1,
                    "an ambiguous create must never create a second PR: {outcome}"
                );
                assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
                assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
                assert!(host.comments(TASK_ID).is_empty());
            }
        },
    );
}

fn pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase() {
    isolated(
        "pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.script_creates(&["gateway"]);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let error = action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect_err("persistent gateway failures must stop retrying");
            assert_eq!(
                fx.forge_calls(),
                ["pr list", "pr create"].repeat(3),
                "every create attempt must be preceded by reconciliation"
            );
            assert!(fx.forge_lines("created-prs").is_empty());
            assert_create_failure(&host, &error, "HTTP 502: 502 Bad Gateway");
        },
    );
}

fn pr_open_permanent_create_failures_never_retry_or_look_up_again() {
    isolated(
        "pr_open_permanent_create_failures_never_retry_or_look_up_again",
        |sandbox| {
            for (outcome, diagnostic) in [
                ("auth", "Resource not accessible by integration (HTTP 403)"),
                ("unknown_head", "Head sha can't be blank"),
                (
                    "no_commits",
                    "No commits between agent-main and orbit/landing-candidate",
                ),
                (
                    "body_too_long",
                    "Body is too long (maximum is 65536 characters)",
                ),
                ("rate_limit", "API rate limit exceeded (HTTP 403)"),
            ] {
                let fx = Fixture::new(sandbox);
                fx.script_creates(&[outcome, "success"]);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let error = action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .expect_err("a permanent create failure must fail immediately");

                assert_eq!(fx.forge_calls(), ["pr list", "pr create"], "{outcome}");
                assert!(fx.forge_lines("created-prs").is_empty(), "{outcome}");
                assert_create_failure(&host, &error, diagnostic);
            }
        },
    );
}

fn assert_create_failure(host: &DeliveryHost, error: &OrbitError, diagnostic: &str) {
    let expected = format!("private automation VCS PR create failed: {diagnostic}");
    assert!(
        matches!(error, OrbitError::Execution(message) if message == &expected),
        "create must preserve the underlying diagnostic: {error}"
    );
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

/// Pending checks are waited out, never merged early. Once they pass, the
/// reviewed candidate is merged with a request conditional on its head SHA.
/// The merge is read back from the forge, the landing is recorded for review
/// coverage, and only then does the task reach `done`.
fn passing_checks_merge_the_reviewed_candidate_and_record_its_delivery() {
    isolated(
        "passing_checks_merge_the_reviewed_candidate_and_record_its_delivery",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            fx.script_checks(&["pending", "success"]);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);

            let opened = action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the reviewed candidate");
            assert_eq!(opened["pr_created"], true);
            assert_eq!(opened["pr_number"], PR_NUMBER);
            assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));
            assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
            // `pr_promote` hands the bundle to review before completion runs.
            host.set_status(TASK_ID, TaskStatus::Review);

            let completed = action(&host, "pr_complete", &fx.complete_input())
                .expect("complete once checks pass");

            let merge = &completed["merge"];
            assert_eq!(merge["merged"], true);
            assert!(
                merge["waited_seconds"].as_u64().unwrap() > 0,
                "the pending read must be waited out before merging: {merge}"
            );
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={} merge_method=squash", fx.candidate)],
                "exactly one merge, conditional on the reviewed head"
            );
            assert!(
                fx.status_reads() >= 3,
                "pending, passing and merged states are each read from the forge"
            );

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

            // The recorded landing is what Core checks coverage against: the
            // reviewed tree, squashed onto the reviewed base.
            review_gate::fetch_landed_commit(&fx.repo, &landed).expect("fetch landing");
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
        },
    );
}

/// [ORB-14849] A before-landing review runs on the open PR. Its reviewer's
/// fix reaches the PR branch only under a lease on the published head the
/// review settled on, and completion merges exactly the head that review
/// settled, never a head pushed after it.
fn a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges() {
    isolated(
        "a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            // The PR opens before any review settles.
            action(&host, "pr_open", &fx.open_input("", "")).expect("open unreviewed");
            host.set_status(TASK_ID, TaskStatus::Review);

            fs::write(fx.repo.join("src/feature.txt"), "reviewed and fixed\n").unwrap();
            git(&fx.repo, &["commit", "-am", "review: fix the change"]);
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
            let lost = push(&fix).expect_err("a lease on another head pushes nothing");
            assert!(lost.to_string().contains("push_lease_lost"), "{lost}");
            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
            let pushed = push(&fx.candidate).expect("the fix goes onto the published head");
            assert_eq!(pushed["remote_sha_before"], fx.candidate.as_str());
            assert_eq!(fx.remote_tip(BRANCH), fix);

            // The shipped pipeline's pins: no before-PR review, the
            // before-landing one, and `push`'s published head.
            let mut input = fx.complete_input();
            input["reviewed_head_sha"] = json!("");
            input["landing_reviewed_head_sha"] = json!(fix);
            let completed = action(&host, "pr_complete", &input).expect("merge the settled head");
            assert_eq!(completed["merge"]["merged"], true);
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={fix} merge_method=squash")],
                "the merge is conditional on the head the review settled"
            );
            assert_eq!(host.landings()[0].reviewed_head_sha, fix);
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);

            // A head pushed after the review settled is never merged.
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            action(&host, "pr_open", &fx.open_input("", "")).expect("open unreviewed");
            host.set_status(TASK_ID, TaskStatus::Review);
            fx.commit("src/later.txt", "pushed after the review\n");
            let mut input = fx.complete_input();
            input["reviewed_head_sha"] = json!("");
            input["landing_reviewed_head_sha"] = json!(fx.candidate);
            let refused =
                action(&host, "pr_complete", &input).expect_err("an unreviewed head is refused");
            assert!(
                refused.to_string().contains(&fx.candidate),
                "the refusal names the settled head: {refused}"
            );
            assert!(fx.merge_requests().is_empty());
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
        },
    );
}

/// A pull request merged at the leased head, including a squash, does not
/// lose the before-landing repair. Both shipped pipelines preserve it and
/// block with the operator recipe. A branch that moved somewhere else, or
/// that moved and was then merged, still loses the lease and preserves nothing.
fn a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease() {
    isolated(
        "a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease",
        |sandbox| {
            for pipeline in ["task_pr_pipeline.yaml", "task_claimed_pr_pipeline.yaml"] {
                for method in ["squash", "merge"] {
                    let fx = Fixture::new(sandbox);
                    let lease = fx.candidate.clone();
                    let first = commit_local(&fx, "src/repair-a.txt", "repair a\n");
                    let second = commit_local(&fx, "src/repair-b.txt", "repair b\n");
                    merge_remote_head(&fx, &lease, method);
                    delete_remote_branch(&fx, BRANCH);

                    let host = landing_host(&fx, pipeline);
                    let error = run_shipped_landing_push(&fx, &host, pipeline)
                        .expect_err("a merged lease preserves the repair and fails the step");

                    assert!(
                        error.contains("review_repair_after_merge:"),
                        "{pipeline} {method}: {error}"
                    );
                    assert!(
                        !error.contains("push_lease_lost"),
                        "{pipeline} {method}: {error}"
                    );
                    assert!(
                        error.contains(&format!("pull request #{PR_NUMBER}")),
                        "{pipeline} {method}: {error}"
                    );
                    assert!(error.contains(&first), "{pipeline} {method}: {error}");
                    assert!(error.contains(&second), "{pipeline} {method}: {error}");
                    let evidence = format!("refs/heads/orbit-evidence/{BRANCH}");
                    assert!(error.contains(&evidence), "{pipeline} {method}: {error}");
                    assert_eq!(
                        remote_ref(&fx, &evidence).as_deref(),
                        Some(second.as_str()),
                        "{pipeline} {method}: the preserved ref is the repair tip"
                    );
                    assert!(
                        remote_ref(&fx, &format!("refs/heads/{BRANCH}")).is_none(),
                        "{pipeline} {method}: the merged branch stays deleted"
                    );
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
                let error = run_shipped_landing_push(&fx, &host, pipeline)
                    .expect_err("an unmerged move loses the lease");
                assert!(
                    error.contains("push_lease_lost"),
                    "{pipeline} unmerged: {error}"
                );
                assert!(
                    !error.contains("review_repair_after_merge"),
                    "{pipeline} unmerged: {error}"
                );
                assert!(remote_ref(&fx, &format!("refs/heads/orbit-evidence/{BRANCH}")).is_none());
                assert_eq!(host.status(TASK_ID), started);

                let fx = Fixture::new(sandbox);
                let lease = fx.candidate.clone();
                let _repair = commit_local(&fx, "src/repair-a.txt", "repair a\n");
                let moved = advance_remote_branch(&fx, BRANCH);
                assert_ne!(moved, lease);
                merge_remote_head(&fx, &moved, "squash");
                delete_remote_branch(&fx, BRANCH);
                let host = landing_host(&fx, pipeline);
                let started = host.status(TASK_ID);
                let error = run_shipped_landing_push(&fx, &host, pipeline)
                    .expect_err("a branch moved and then merged loses the lease");
                assert!(
                    error.contains("push_lease_lost"),
                    "{pipeline} moved-then-merged: {error}"
                );
                assert!(
                    !error.contains("review_repair_after_merge"),
                    "{pipeline} moved-then-merged: {error}"
                );
                assert!(remote_ref(&fx, &format!("refs/heads/orbit-evidence/{BRANCH}")).is_none());
                assert_eq!(host.status(TASK_ID), started);
            }
        },
    );
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
    git(&fx.repo, &["add", file]);
    git(
        &fx.repo,
        &["commit", "-m", &format!("review repair {file}")],
    );
    fx.head()
}

fn open_pr_files(fx: &Fixture) {
    fs::write(fx.forge.join("pr-head"), format!("{BRANCH}\n")).unwrap();
    fs::write(fx.forge.join("pr-base"), format!("{BASE}\n")).unwrap();
}

fn advance_remote_branch(fx: &Fixture, branch: &str) -> String {
    let git_dir = path_str(&fx.forge.join("remote.git")).to_string();
    let parent = git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "rev-parse",
            &format!("refs/heads/{branch}"),
        ],
    );
    let tree = git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "rev-parse",
            &format!("{parent}^{{tree}}"),
        ],
    );
    let next = commit_tree(fx, &tree, &[&parent], "unreviewed move");
    git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "update-ref",
            &format!("refs/heads/{branch}"),
            &next,
            &parent,
        ],
    );
    next
}

/// Land `head` on the remote base the way the forge does, and remember `head`
/// as the merged pull request's head oid.
fn merge_remote_head(fx: &Fixture, head: &str, method: &str) {
    let git_dir = path_str(&fx.forge.join("remote.git")).to_string();
    let base_ref = format!("refs/heads/{BASE}");
    let parent = git(&fx.repo, &["--git-dir", &git_dir, "rev-parse", &base_ref]);
    let tree = git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "rev-parse",
            &format!("{head}^{{tree}}"),
        ],
    );
    let parents: Vec<&str> = if method == "merge" {
        vec![&parent, head]
    } else {
        vec![&parent]
    };
    let landed = commit_tree(fx, &tree, &parents, "Land pull request #42");
    git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "update-ref",
            &base_ref,
            &landed,
            &parent,
        ],
    );
    open_pr_files(fx);
    fs::write(fx.forge.join("merged"), format!("{landed}\n")).unwrap();
    fs::write(fx.forge.join("merged-head"), format!("{head}\n")).unwrap();
}

fn delete_remote_branch(fx: &Fixture, branch: &str) {
    let git_dir = path_str(&fx.forge.join("remote.git")).to_string();
    git(
        &fx.repo,
        &[
            "--git-dir",
            &git_dir,
            "update-ref",
            "-d",
            &format!("refs/heads/{branch}"),
        ],
    );
}

fn commit_tree(fx: &Fixture, tree: &str, parents: &[&str], message: &str) -> String {
    let git_dir = path_str(&fx.forge.join("remote.git")).to_string();
    let mut args = vec![
        "-c".to_string(),
        "user.name=Forge".to_string(),
        "-c".to_string(),
        "user.email=forge@example.invalid".to_string(),
        "--git-dir".to_string(),
        git_dir,
        "commit-tree".to_string(),
        tree.to_string(),
    ];
    for parent in parents {
        args.push("-p".to_string());
        args.push((*parent).to_string());
    }
    args.push("-m".to_string());
    args.push(message.to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    git(&fx.repo, &refs)
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

/// The shipped pipeline's real `landing_push` step, with the stubs its
/// templates read, and that pipeline's real failure activity.
fn run_shipped_landing_push(
    fx: &Fixture,
    host: &DeliveryHost,
    pipeline: &str,
) -> Result<(), String> {
    host.script(
        "test_stub_worktree",
        json!({ "job_run_id": RUN_ID, "workspace_path": fx.repo, "base_sha": fx.base_sha }),
    );
    host.script(
        "test_stub_sync_base",
        json!({ "head": BRANCH, "base": BASE, "base_ref": BASE, "base_sha": fx.base_sha }),
    );
    host.script(
        "test_stub_push",
        json!({ "branch": BRANCH, "local_sha": fx.candidate, "phase": "push", "decision": "reused_current" }),
    );
    host.script("test_stub_pr_open", json!({ "pr_number": PR_NUMBER }));
    host.script(
        "test_stub_landing_review_gate_settle",
        json!({ "reviewer_fixed": true }),
    );
    host.script(
        "test_stub_implement_one",
        json!({ "summary": "implemented" }),
    );

    let shipped = fs::read_to_string(
        workspace_root()
            .join("crates/orbit-core/assets/jobs")
            .join(pipeline),
    )
    .unwrap();
    let shipped = load_job_asset(&shipped).unwrap().spec;
    let mut landing = shipped
        .steps
        .iter()
        .find(|step| step.id == "landing_push")
        .cloned()
        .unwrap_or_else(|| panic!("{pipeline} has no landing_push"));
    landing.recovery_activity = None;
    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": format!("test_stub_{id}"), "config": {} },
        })
    };
    let mut job = job_asset(json!([
        stub("worktree"),
        stub("sync_base"),
        stub("push"),
        stub("pr_open"),
        stub("landing_review_gate_settle"),
        stub("implement_one"),
    ]));
    job.steps.push(landing);
    job.failure_activity = shipped.failure_activity.clone();
    let mut catalog = V2ActivityCatalog::new();
    for name in ["git_push", "pr_failure_handoff", "claim_candidate_carry"] {
        let yaml = fs::read_to_string(
            workspace_root()
                .join("crates/orbit-core/assets/activities")
                .join(format!("{name}.yaml")),
        )
        .unwrap();
        let asset = load_activity_asset(&yaml).unwrap();
        catalog.insert(asset.name, asset.spec);
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();

    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), RUN_ID);
    match execute_job_with_resume(
        &job,
        json!({ "task_ids": [TASK_ID], "base_sync": "local" }),
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

/// Both creation's view and reuse's list/view retain the provider number
/// representations the private PR reader already accepted.
fn pr_open_preserves_provider_number_representations() {
    isolated(
        "pr_open_preserves_provider_number_representations",
        |sandbox| {
            for reuse in [false, true] {
                for (number, expected) in [
                    (json!(42), "42"),
                    (json!(-1), "-1"),
                    (json!("0042"), "0042"),
                    (json!(u64::MAX), "18446744073709551615"),
                ] {
                    let fx = Fixture::new(sandbox);
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                    let input = fx.open_input(&fx.candidate, &fx.base_sha);
                    if reuse {
                        action(&host, "pr_open", &input).unwrap();
                    }
                    fs::write(fx.forge.join("pr-number-json"), number.to_string()).unwrap();
                    let opened = action(&host, "pr_open", &input);
                    if reuse && expected == "-1" {
                        let error = opened.expect_err(
                            "negative selectors remain invalid at the provider input boundary",
                        );
                        assert!(
                            matches!(error, OrbitError::InvalidInput(ref message) if message.contains("invalid private automation VCS PR selector '-1'"))
                        );
                        continue;
                    }
                    let opened =
                        opened.expect("the existing number representations remain readable");
                    assert_eq!(opened["pr_number"], expected);
                    assert_eq!(opened["pr_reused"], reuse);
                }
            }
        },
    );
}

/// A required check that fails, before or after a wait, or an outstanding
/// required review, refuses completion before any merge request. The PR stays
/// open, the remote base does not move, and the task stays in `review`.
fn a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review() {
    isolated(
        "a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review",
        |sandbox| {
            for (case, checks, refusal) in [
                (
                    "failed required check",
                    &["failure"][..],
                    "status check 'test' failed",
                ),
                (
                    "pending checks that then fail",
                    &["pending", "failure"][..],
                    "status check 'test' failed",
                ),
                (
                    "required review outstanding",
                    &["review_required"][..],
                    "REVIEW_REQUIRED",
                ),
            ] {
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

                let error = action(&host, "pr_complete", &fx.complete_input())
                    .expect_err("a red or blocked PR must not complete");

                assert!(error.to_string().contains(refusal), "{case}: {error}");
                assert_eq!(
                    fx.status_reads(),
                    checks.len(),
                    "{case}: refused at the first red read"
                );
                assert!(
                    fx.merge_requests().is_empty(),
                    "{case}: no merge is requested"
                );
                assert_eq!(fx.forge_state("merged"), None, "{case}: PR stays open");
                assert_eq!(
                    fx.remote_tip(BASE),
                    fx.base_sha,
                    "{case}: the remote base is untouched"
                );
                assert!(host.landings().is_empty(), "{case}: no landing recorded");
                assert!(
                    host.completion_notes().is_empty(),
                    "{case}: no done transition is attempted"
                );
                assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Batch merge of the checked head [ORB-15701]
// ---------------------------------------------------------------------------

fn batch_merge_input(fx: &Fixture) -> Value {
    json!({
        "job_run_id": RUN_ID,
        "strategy": "pr_merge",
        "base": BASE,
        "workspace_path": fx.repo,
    })
}

/// Open the candidate's pull request, promote it into review, and approve it,
/// as the pipeline leaves a batch before `git_merge` runs.
fn approve_batch(fx: &Fixture, host: &DeliveryHost) {
    let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
    let opened = action(host, "pr_open", &input).expect("open the reviewed candidate");
    input["pr_number"] = opened["pr_number"].clone();
    input["pr_url"] = opened["pr_url"].clone();
    action(host, "pr_promote", &input).expect("the published PR enters review");
    host.tasks
        .lock()
        .unwrap()
        .get_mut(TASK_ID)
        .unwrap()
        .pr_status = Some("APPROVED".into());
}

fn a_pr_merge_batch_lands_the_checked_head_at_its_pin() {
    isolated(
        "a_pr_merge_batch_lands_the_checked_head_at_its_pin",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            approve_batch(&fx, &host);

            let merged = action(&host, "git_merge", &batch_merge_input(&fx))
                .expect("merge the approved head");

            assert_eq!(merged, json!({"merged": true, "strategy": "squash"}));
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={} merge_method=squash", fx.candidate)],
                "one merge, conditional on the full commit that was checked"
            );
            assert_ne!(fx.remote_tip(BASE), fx.base_sha, "the remote base advanced");
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);
        },
    );
}

/// A PR head that moves after the freshness check must not merge: the batch
/// pins the commit it checked, and the provider refuses the moved head.
fn a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check() {
    isolated(
        "a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            approve_batch(&fx, &host);
            // Lands after the freshness check, while the merge strategy resolves.
            fs::write(fx.forge.join("push-before-merge"), "").unwrap();

            action(&host, "git_merge", &batch_merge_input(&fx))
                .expect_err("a head that moved must not merge");

            assert_ne!(
                fx.remote_tip(BRANCH),
                fx.candidate,
                "the PR head moved after the freshness check"
            );
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={} merge_method=squash", fx.candidate)],
                "the merge names the checked head, and the provider refuses it"
            );
            assert_eq!(
                fx.remote_tip(BASE),
                fx.base_sha,
                "the remote base is untouched"
            );
            assert!(fx.forge_state("merged").is_none(), "the PR stays open");
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::Review,
                "the task stays in review"
            );
        },
    );
}

// ---------------------------------------------------------------------------
// Synchronous merge refused by a concurrent base landing [ORB-14205]
// ---------------------------------------------------------------------------

const MERGE_CALL: &str = "api repos/{owner}/{repo}/pulls/42/merge";
const BASE_MODIFIED: &str =
    "gh: Base branch was modified. Review and try the merge again. (HTTP 405)";

/// Opens the fixture's candidate and hands it to review, returning how many
/// `gh` calls publication made so completion's own calls can be isolated.
fn open_for_completion(fx: &Fixture, host: &DeliveryHost) -> usize {
    action(host, "pr_open", &fx.open_input(&fx.candidate, &fx.base_sha))
        .expect("open the reviewed candidate");
    host.set_status(TASK_ID, TaskStatus::Review);
    fx.forge_calls().len()
}

/// The refusal F2026-10-071 recorded: GitHub refuses the SHA-conditioned
/// merge because another PR landed on the base first. Completion waits one
/// poll, re-reads the PR, re-resolves the merge policy and asks again with the
/// same authorized SHA, so the unchanged candidate lands without a provider
/// recovery activity. Pending checks on that fresh read are waited out as
/// before. A reviewed and an ungated-but-published candidate behave alike.
fn a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence() {
    isolated(
        "a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence",
        |sandbox| {
            for (case, reviewed, checks) in [
                ("reviewed", true, &["success"][..]),
                ("ungated but published", false, &["success"][..]),
                (
                    "pending on refresh",
                    true,
                    &["success", "pending", "success"][..],
                ),
            ] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let published = open_for_completion(&fx, &host);
                fx.script_checks(checks);
                fx.script_merges(&["base_modified"]);
                let input = if reviewed {
                    fx.complete_input()
                } else {
                    fx.complete_ungated_input()
                };

                let completed = action(&host, "pr_complete", &input)
                    .unwrap_or_else(|error| panic!("{case}: {error}"));

                let condition = format!("sha={} merge_method=squash", fx.candidate);
                assert_eq!(
                    fx.merge_requests(),
                    vec![condition.clone(), condition],
                    "{case}: the retry is conditioned on the same authorized head"
                );
                // Every read after the refusal, up to the one that permits
                // the retry.
                let fresh_reads = checks.len().max(2) - 1;
                let mut expected = vec!["pr view", "repo view", "api graphql", MERGE_CALL];
                expected.extend(std::iter::repeat_n("pr view", fresh_reads));
                expected.extend(["repo view", "api graphql", MERGE_CALL, "pr view"]);
                assert_eq!(
                    fx.forge_calls()[published..],
                    expected,
                    "{case}: a fresh status read and merge policy precede the retry, and \
                     merged state is read back afterwards"
                );

                let merge = &completed["merge"];
                let landed = fx.remote_tip(BASE);
                let concurrent = fx.remote_parent(&landed);
                assert_ne!(concurrent, fx.base_sha, "{case}: the base raced ahead");
                assert_eq!(fx.remote_parent(&concurrent), fx.base_sha, "{case}");
                assert_eq!(merge["merged"], true, "{case}");
                assert_eq!(merge["landed_commit"], landed.as_str(), "{case}");
                assert_eq!(merge["managed_merge"], true, "{case}");
                assert_eq!(merge["auto_merge_requested"], false, "{case}");
                assert_eq!(
                    merge["waited_seconds"],
                    5 * fresh_reads as u64,
                    "{case}: one poll after the refusal, plus any pending wait"
                );
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
        },
    );
}

/// Only a failed synchronous merge whose output is exactly the provider's
/// base-modification refusal (HTTP 405) is retried. Policy, queue and
/// protection 405s, an auth refusal, a moved head (409), the same message on
/// another status, a body that disagrees or is not JSON, extra diagnostics, a
/// transport timeout and a server error each fail on the one request, with the
/// provider's message, and the task stays in review.
fn other_merge_refusals_fail_on_the_first_request() {
    isolated(
        "other_merge_refusals_fail_on_the_first_request",
        |sandbox| {
            for (outcome, diagnostic) in [
                (
                    "policy",
                    "Merge commits are not allowed on this repository. (HTTP 405)",
                ),
                (
                    "queue",
                    "Changes must be made through the merge queue (HTTP 405)",
                ),
                ("protection", "At least 1 approving review is required"),
                ("auth", "Resource not accessible by integration (HTTP 403)"),
                (
                    "head_modified",
                    "Head branch was modified. Review and try the merge again. (HTTP 409)",
                ),
                (
                    "base_modified_422",
                    "Base branch was modified. Review and try the merge again. (HTTP 422)",
                ),
                ("mismatched_body", BASE_MODIFIED),
                ("malformed_body", BASE_MODIFIED),
                ("trailing_stderr", "retried after a proxy reset"),
                ("transport_timeout", "Client.Timeout exceeded"),
                ("server_error", "Server Error (HTTP 502)"),
            ] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                open_for_completion(&fx, &host);
                fx.script_merges(&[outcome]);

                let error = action(&host, "pr_complete", &fx.complete_input())
                    .expect_err("an unclassified merge refusal must not complete");

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
        },
    );
}

/// Shared by the two refresh-refusal tests: after one base-modification
/// refusal, the fresh read in each case must refuse without a second merge
/// request, leaving the checkout, landings and task untouched.
fn assert_refresh_refuses(sandbox: &Path, cases: &[(&str, bool, &str, &str, &str)]) {
    for &(case, reviewed, outcome, checks, refusal) in cases {
        let fx = Fixture::new(sandbox);
        let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
        open_for_completion(&fx, &host);
        fx.script_checks(&["success", checks]);
        fx.script_merges(&[outcome]);
        let input = if reviewed {
            fx.complete_input()
        } else {
            fx.complete_ungated_input()
        };

        let error = action(&host, "pr_complete", &input)
            .expect_err("changed evidence after a base race must not complete");

        assert!(error.to_string().contains(refusal), "{case}: {error}");
        assert_eq!(fx.merge_requests().len(), 1, "{case}: no second mutation");
        assert_eq!(fx.status_reads(), 2, "{case}: one fresh read");
        assert_eq!(fx.head(), fx.candidate, "{case}: no local rewrite");
        assert!(host.landings().is_empty(), "{case}");
        assert!(host.completion_notes().is_empty(), "{case}");
        assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{case}");
    }
}

/// The read after a base-modification refusal still pins the candidate: a
/// moved head or repointed base, a closed or contradictory PR, or a merge of
/// some other head refuses without a second merge request.
fn a_base_race_refresh_refuses_a_changed_candidate_without_another_merge() {
    isolated(
        "a_base_race_refresh_refuses_a_changed_candidate_without_another_merge",
        |sandbox| {
            assert_refresh_refuses(
                sandbox,
                &[
                    (
                        "moved head",
                        true,
                        "base_modified+head_moved",
                        "success",
                        "headRefOid",
                    ),
                    (
                        "moved head, ungated",
                        false,
                        "base_modified+head_moved",
                        "success",
                        "headRefOid",
                    ),
                    (
                        "repointed base",
                        true,
                        "base_modified+retargeted",
                        "success",
                        "baseRefName",
                    ),
                    (
                        "repointed base, ungated",
                        false,
                        "base_modified+retargeted",
                        "success",
                        "baseRefName",
                    ),
                    (
                        "other head merged",
                        true,
                        "base_modified+merged_other_head",
                        "success",
                        "headRefOid",
                    ),
                    (
                        "closed",
                        true,
                        "base_modified",
                        "closed",
                        "closed without being merged",
                    ),
                    (
                        "contradictory",
                        true,
                        "base_modified",
                        "contradictory",
                        "contradictory merge state",
                    ),
                ],
            );
        },
    );
}

/// The read after a base-modification refusal still gates the merge: failed
/// checks, an outstanding review, a merge policy that no longer permits a
/// method, or a real conflict on a reviewed head refuses without a second
/// merge request.
fn a_base_race_refresh_refuses_unmet_gates_without_another_merge() {
    isolated(
        "a_base_race_refresh_refuses_unmet_gates_without_another_merge",
        |sandbox| {
            assert_refresh_refuses(
                sandbox,
                &[
                    (
                        "failed checks",
                        true,
                        "base_modified",
                        "failure",
                        "status check 'test' failed",
                    ),
                    (
                        "review required",
                        false,
                        "base_modified",
                        "review_required",
                        "REVIEW_REQUIRED",
                    ),
                    (
                        "policy disallowed",
                        true,
                        "base_modified+policy_disallowed",
                        "success",
                        "no permitted merge method",
                    ),
                    (
                        "conflict on a reviewed head",
                        true,
                        "base_modified",
                        "dirty",
                        "review_gate_stale",
                    ),
                ],
            );
        },
    );
}

/// A fresh read that finds the same candidate already merged reconciles
/// through the pinned delivery check without another merge request. The
/// refused request produced no merge commit, so the landing is not
/// attributed to this run as a managed merge.
fn an_already_merged_refresh_reconciles_without_another_merge_request() {
    isolated(
        "an_already_merged_refresh_reconciles_without_another_merge_request",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            open_for_completion(&fx, &host);
            fx.script_merges(&["base_modified+merged"]);

            let completed = action(&host, "pr_complete", &fx.complete_input())
                .expect("the merged candidate reconciles");

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
        },
    );
}

/// Repeated base races stop at the fixed attempt ceiling, and a budget that
/// is zero or runs out first stops sooner; the refusal never resets the wait.
/// Each exit names the provider refusal and the attempts made, and leaves the
/// task in review.
fn repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget() {
    isolated(
        "repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget",
        |sandbox| {
            for (case, max_wait_seconds, requests, refusal) in [
                ("attempt ceiling", 30, 3, "attempt 3 of 3"),
                ("zero budget", 0, 1, "timed out after 0s"),
                ("budget spent by the wait", 5, 1, "timed out after 5s"),
            ] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                open_for_completion(&fx, &host);
                fx.script_merges(&["base_modified"; 4]);
                let mut input = fx.complete_input();
                input["max_wait_seconds"] = json!(max_wait_seconds);

                let error = action(&host, "pr_complete", &input)
                    .expect_err("repeated base races must stop");

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
        },
    );
}

// ---------------------------------------------------------------------------
// Reviewed-candidate binding at publication
// ---------------------------------------------------------------------------

/// A freshly created stacked base shares the landing tip but has not landed
/// any work yet. Its first candidate can open a PR and enter review.
fn pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip() {
    isolated(
        "pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            git(&fx.repo, &["branch", "main", &fx.base_sha]);
            git(&fx.repo, &["push", "origin", "main"]);
            assert_eq!(fx.remote_tip(BASE), fx.remote_tip("main"));

            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["landing_branch"] = json!("main");
            let opened = action(&host, "pr_open", &input)
                .expect("the first delivery into a fresh stacked base can publish");

            assert_eq!(opened["pr_created"], true);
            assert_eq!(fx.forge_state("pr-base").as_deref(), Some(BASE));
            assert_eq!(fx.forge_state("pr-head").as_deref(), Some(BRANCH));

            input["pr_number"] = json!(PR_NUMBER);
            action(&host, "pr_promote", &input).expect("the live base can enter review");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(host.comments(TASK_ID).is_empty());
        },
    );
}

/// Promotion records the published PR's page with its number, so the task
/// links the pull request a reviewer must read. A provider URL that is not an
/// https page is dropped rather than failing the promotion.
fn pr_promote_records_the_pull_request_page_with_its_number() {
    isolated(
        "pr_promote_records_the_pull_request_page_with_its_number",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            let opened = action(&host, "pr_open", &input).expect("the candidate publishes");
            let pr_url = opened["pr_url"].as_str().expect("pr_open reports the page");
            assert!(pr_url.starts_with("https://"), "{opened}");

            input["pr_number"] = opened["pr_number"].clone();
            input["pr_url"] = json!(pr_url);
            action(&host, "pr_promote", &input).expect("the published PR enters review");
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
            action(&unlinked, "pr_promote", &input)
                .expect("an unusable page does not fail the promotion");
            let refs = unlinked.task(TASK_ID).external_refs;
            assert_eq!(refs.len(), 1, "{refs:?}");
            assert_eq!(refs[0].id, PR_NUMBER);
            assert_eq!(refs[0].url, None, "only an https page is recorded");
        },
    );
}

/// A base with its own work merged into the landing branch remains obsolete
/// at publication and on a resumed promotion, before any forge call or status
/// transition.
fn pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch() {
    isolated(
        "pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch",
        |sandbox| {
            let mut fx = Fixture::new(sandbox);
            git(&fx.repo, &["branch", "main", &fx.base_sha]);
            fx.advance_base_and_rebase();
            fx.base_sha = fx.local_tip(BASE);
            fx.candidate = fx.head();
            git(&fx.repo, &["checkout", "main"]);
            git(
                &fx.repo,
                &["merge", "--no-ff", BASE, "-m", "Merge stacked base"],
            );
            git(&fx.repo, &["push", "origin", "main"]);
            git(&fx.repo, &["checkout", BRANCH]);
            assert_ne!(fx.base_sha, fx.remote_tip("main"));

            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
            input["landing_branch"] = json!("main");
            input["pr_number"] = json!(PR_NUMBER);
            for phase in ["pr_open", "pr_promote"] {
                let error = action(&host, phase, &input)
                    .expect_err("a genuinely merged base must refuse delivery");
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
        },
    );
}

/// `pr_open` publishes only the candidate the review gate settled. A
/// checkout that gained an unreviewed commit or a candidate rebased onto a
/// base other than the reviewed one is refused before the forge is asked
/// anything. The refusal is recorded on the task. Opening the same fixture with
/// the current head and base as the reviewed pair then succeeds, so each
/// refusal is caused by the mismatch alone.
fn pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate() {
    isolated(
        "pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate",
        |sandbox| {
            for case in ["unreviewed commit on the head", "rebased onto another base"] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
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
                let error = action(&host, "pr_open", &input)
                    .expect_err("a candidate other than the reviewed one must not publish");

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
        },
    );
}

/// [ORB-13989] The settled review's "Review fixes" section is published with
/// the PR it explains: appended to a body the run supplied, and to the body
/// Orbit generates when none was supplied.
fn pr_open_appends_the_review_fixes_section_to_the_published_body() {
    isolated(
        "pr_open_appends_the_review_fixes_section_to_the_published_body",
        |sandbox| {
            let section = "## Review fixes\n\n- `F1` [high] Missing guard — added the guard";
            for supplied in [Some("Operator-written body."), None] {
                let fx = Fixture::new(sandbox);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let mut input = fx.open_input(&fx.candidate, &fx.base_sha);
                input["review_fixes"] = json!(section);
                if let Some(body) = supplied {
                    input["body"] = json!(body);
                }

                let opened = action(&host, "pr_open", &input).expect("open the reviewed head");

                assert_eq!(opened["pr_created"], true);
                let body = fx
                    .forge_state("pr-body")
                    .expect("the forge received a body");
                assert!(
                    body.trim_end().ends_with(section),
                    "{supplied:?}: the section closes the body: {body}"
                );
                if let Some(supplied) = supplied {
                    assert!(body.starts_with(supplied), "{body}");
                } else {
                    assert!(
                        body.len() > section.len() + 2,
                        "the generated body is kept before the section: {body}"
                    );
                }
            }
        },
    );
}

// ---------------------------------------------------------------------------
// Required validation before publication
// ---------------------------------------------------------------------------

/// The owner path's required validation runs every command on the exact
/// synchronized candidate. A command that fails refuses the candidate before
/// anything is pushed, with a summary in the error and its log — beside the
/// passing log of the command before it — attached to the task. A repair left
/// uncommitted is refused, because the result would not describe the committed
/// candidate. Once the repair is committed, as step recovery does, the retry
/// passes on the new head and replaces the logs.
fn required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes() {
    isolated(
        "required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            // The base has no src/feature.txt, so it passes: the failure is
            // the candidate's to repair.
            let format_check = "test ! -e src/feature.txt || grep -qx formatted src/feature.txt || \
                 { echo 'src/feature.txt: not formatted' >&2; exit 3; }";
            host.require_commands(&["echo suite-ok", format_check]);
            let remote_before = fx.remote_tip(BRANCH);

            let error = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("an unformatted candidate must be refused");

            let message = error.to_string();
            assert!(
                message.contains("required validation")
                    && message.contains(&fx.candidate)
                    && message.contains("exit 3")
                    && message.contains(&format!("validation/{RUN_ID}/1.json")),
                "the refusal names the candidate, exit code and log: {message}"
            );
            assert!(
                !is_baseline_red_failure(None, Some(&message))
                    && message.contains("passes this command, so the candidate introduced"),
                "a failure the base does not share stays the candidate's: {message}"
            );
            let passed = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(passed["command"], "echo suite-ok");
            assert_eq!(passed["exit_code"], 0);
            assert_eq!(passed["output"], "suite-ok");
            let failed = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/1.json"));
            assert_eq!(failed["exit_code"], 3);
            assert_eq!(failed["tested_head"], fx.candidate.as_str());
            assert_eq!(failed["base_sha"], fx.base_sha.as_str());
            assert_eq!(failed["baseline"]["decision"], "passed");
            let base_log =
                host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/1.baseline.json"));
            assert_eq!(base_log["exit_code"], 0, "the base ran the same command");
            assert_eq!(
                failed["output"],
                format!(
                    "src/feature.txt: not formatted\nRequired validation PATH={}",
                    std::env::var("PATH").unwrap()
                )
            );
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");

            fs::write(fx.repo.join("src/feature.txt"), "formatted\n").unwrap();
            let dirty = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("an uncommitted repair is not the candidate");
            assert!(
                dirty
                    .to_string()
                    .contains("staged, tracked, or untracked changes"),
                "{dirty}"
            );

            git(&fx.repo, &["commit", "-am", "format feature"]);
            let repaired = fx.head();
            let validated = action(&host, "candidate_validate", &fx.validate_input())
                .expect("the committed repair passes");

            assert_eq!(validated["decision"], "passed");
            assert_eq!(validated["tested_head"], repaired.as_str());
            assert_eq!(
                validated["commands"],
                json!(["echo suite-ok", format_check])
            );
            assert_eq!(validated["validation"].as_array().unwrap().len(), 2);
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/1.json"));
            assert_eq!(log["exit_code"], 0, "the retry replaces the failing log");
            assert_eq!(log["tested_head"], repaired.as_str());
        },
    );
}

/// Large captured output belongs only to artifacts, on both delivery routes.
fn required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs() {
    isolated(
        "required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
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
                "passes this command",
                &candidate_path,
                &base_path,
            ] {
                assert!(
                    message.contains(field),
                    "missing evidence {field}: {message}"
                );
            }
            assert!(
                !message.contains("foo_bar:"),
                "passing output must stay in artifacts"
            );
            let log = host.validation_log(TASK_ID, &candidate_path);
            assert!(log["output"].as_str().unwrap().starts_with(&output));
            assert_eq!(log["baseline"]["log"], base_path);
            assert_eq!(host.validation_log(TASK_ID, &base_path)["exit_code"], 0);

            let input =
                json!({"workspace_path": fx.repo, "base_sync": "local", "base_sha": fx.base_sha});
            let message = action(&host, "claim_validate", &input)
                .unwrap_err()
                .to_string();
            assert!(message.len() <= 16 * 1024);
            for field in [
                "actual_failure",
                "exit 3",
                "validation/claim-landing/0.failed.json",
                "validation/claim-landing/0.baseline.json",
            ] {
                assert!(
                    message.contains(field),
                    "missing claim evidence {field}: {message}"
                );
            }
            let logs = host.claim_log_contents.lock().unwrap();
            let log: Value =
                serde_json::from_slice(&logs["validation/claim-landing/0.failed.json"]).unwrap();
            assert!(log["output"].as_str().unwrap().starts_with(&output));
            drop(logs);

            // Shared exit status without failure identities proves no shared
            // failure, even when the output looks the same on both sides.
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
                assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
                assert!(
                    message.contains("failure identities are missing"),
                    "{message}"
                );
                assert!(!message.contains("failed identically"), "{message}");
                assert!(!message.contains(&output));
            }
        },
    );
}

/// A missing validation tool is refused with the full searched PATH,
/// both in the action error and the durable command log [ORB-13963], even
/// when the resolved PATH differs from the launcher or exceeds 1024 bytes.
fn missing_required_validation_tool_reports_its_search_path() {
    isolated(
        "missing_required_validation_tool_reports_its_search_path",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let launcher_path = std::env::var("PATH").unwrap();
            // Deterministically exercise long, multibyte paths on short-PATH
            // hosts too, without changing the fixture's own tool lookup.
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
                let error = action(&host, "candidate_validate", &fx.validate_input())
                    .expect_err("a missing required tool must refuse publication");
                let searched_path = resolved_path.unwrap_or(&launcher_path);
                let diagnostic = format!("Required validation PATH={searched_path}");
                assert!(error.to_string().contains(&diagnostic), "{error}");
                let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                assert_eq!(log["exit_code"], 127);
                assert_eq!(log["validation_env"]["path"], searched_path);
                assert!(log["output"].as_str().unwrap().contains(&diagnostic));
            }
        },
    );
}

/// What `env -i PATH=/usr/bin:/bin orbit run auto …` hands the owner.
const MINIMAL_PATH: &str = "/usr/bin:/bin";
/// A repository tool only the owner's login-shell profile puts on PATH.
const PROFILE_TOOL: &str = "orbit-profile-lint-13987";

/// A drain launched from a minimal PATH still validates with the owner's
/// login-shell toolchain, and the log records that the login shell decided
/// PATH [ORB-13987].
fn validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain() {
    isolated(
        "validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let (launcher, shell, tools) = login_shell_toolchain(sandbox);
            host.resolve_validation_env(ValidationEnvironment::resolve(
                launcher,
                &ValidationEnvPolicy::default(),
                &LoginShell::new(shell, Duration::from_secs(10)),
            ));
            host.require_commands(&[PROFILE_TOOL]);

            let validated = action(&host, "candidate_validate", &fx.validate_input())
                .expect("the login-shell toolchain runs the required tool");

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
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["output"], "lint-ok");
            assert_eq!(log["validation_env"]["source"], "login_shell");
            let path = log["validation_env"]["path"].as_str().unwrap();
            assert!(
                path.starts_with(&format!("{}:", tools.display())) && path.ends_with(MINIMAL_PATH),
                "the profile's PATH is used over the launcher's: {path}"
            );
            assert_eq!(log["failure_kind"], Value::Null);
        },
    );
}

/// Without the login shell's PATH the same tool is missing. That failure is
/// the environment's, not the candidate's: the typed marker carries the PATH
/// and the tool, the log classifies it, and nothing is published.
fn a_missing_validation_tool_is_an_environment_failure() {
    isolated(
        "a_missing_validation_tool_is_an_environment_failure",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let (launcher, _shell, _tools) = login_shell_toolchain(sandbox);
            host.resolve_validation_env(ValidationEnvironment::launcher(launcher));
            host.require_commands(&[PROFILE_TOOL]);
            let remote_before = fx.remote_tip(BRANCH);

            let error = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a missing tool cannot pass validation");

            let message = error.to_string();
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&message)),
                "a missing tool is typed as an environment failure: {message}"
            );
            assert!(
                message.contains(&format!("`{PROFILE_TOOL}`"))
                    && message.contains(&format!("PATH={MINIMAL_PATH}"))
                    && message.contains("source: launcher_fallback"),
                "the failure reports the tool, PATH and its source: {message}"
            );
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["exit_code"], 127);
            assert_eq!(log["failure_kind"], "environment");
            assert_eq!(log["missing_tool"], PROFILE_TOOL);
            assert_eq!(log["validation_env"]["source"], "launcher_fallback");
            assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);

            // The run's failure handoff keeps the unjudged candidate for
            // `orbit job resume`: no `[BLOCKED]` PR, no push, no repair.
            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "validate",
                    "error_code": "validation_environment",
                    "error_message": message,
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the environment failure");
            assert_eq!(handoff["decision"], "blocked_validation_environment");
            assert_eq!(handoff["head_sha"], fx.candidate.as_str());
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(
                fx.durable_candidate(&handoff),
                fx.candidate,
                "a claim on another host can fetch the candidate"
            );
            assert_eq!(
                fx.head(),
                fx.candidate,
                "the candidate is kept as validated"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
            let comments = host.comments(TASK_ID);
            assert!(
                comments
                    .iter()
                    .any(|comment| comment.message.contains(&format!("PATH={MINIMAL_PATH}"))),
                "the block reports the PATH the tool was missing from: {comments:?}"
            );
        },
    );
}

/// The owner validates outside any agent sandbox, so a required command that
/// exits zero after printing a `DEFERRED:` notice never ran its sandbox-confined
/// path before delivery. Both validation steps refuse that pass as the
/// environment's failure, which no recovery repairs, and keep the notice in the
/// log. The refusal is narrow: a real failure stays the candidate's, and a
/// `DEFERRED:` that does not lead a line is ordinary output [ORB-15287].
fn a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure() {
    isolated(
        "a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let notice = orbit_exec::bwrap_deferral_notice(
                "sandboxed_path",
                "bwrap: No permissions to create new namespace",
            );
            let remote_before = fx.remote_tip(BRANCH);

            host.require_commands(&[&format!("echo '{notice}' >&2")]);
            let error = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a deferred sandbox path is not a validation pass");
            let message = error.to_string();
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&message))
                    && message.contains(&notice)
                    && message.contains(&fx.candidate),
                "typed as the environment's failure, naming the notice: {message}"
            );
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["exit_code"], 0, "the process itself succeeded");
            assert_eq!(log["failure_kind"], "environment");
            assert_eq!(log["deferred_notice"], notice.as_str());
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");

            // The claimed path validates before publication the same way.
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            let input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });
            let claim = action(&host, "claim_validate", &input)
                .expect_err("a claim must not publish a deferred pass");
            assert!(
                orbit_types::workflow::is_validation_environment_failure(
                    None,
                    Some(&claim.to_string())
                ) && claim.to_string().contains(&notice),
                "{claim}"
            );

            // A real failure alongside the notice stays the candidate's: the
            // base lacks src/feature.txt, so it passes the same command.
            host.require_commands(&[&format!("echo '{notice}' >&2; test ! -e src/feature.txt")]);
            let failed = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a nonzero exit fails whatever else it printed");
            let failed = failed.to_string();
            assert!(
                !orbit_types::workflow::is_validation_environment_failure(None, Some(&failed))
                    && failed.contains("passes this command, so the candidate introduced"),
                "a failing gate with a notice is the candidate's failure: {failed}"
            );
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["failure_kind"], "candidate");

            // A notice that does not lead its line is not a deferral.
            host.require_commands(&[&format!("echo 'saw {notice}'")]);
            let validated = action(&host, "candidate_validate", &fx.validate_input())
                .expect("only a line that starts with the prefix is a deferral");
            assert_eq!(validated["decision"], "passed");
        },
    );
}

// ---------------------------------------------------------------------------
// Deferred sandbox handoff [ORB-15287]
// ---------------------------------------------------------------------------

/// An implementer whose affected-test gate passed with only Bubblewrap
/// deferrals hands the gate off instead of failing [ORB-15287]. The implement
/// step accepts the record before commit and keeps it on the task, and owner
/// validation replays the gate outside any agent sandbox before delivery: on
/// this run, and on a resumed candidate whose run skips the implementer. A
/// replay that defers again is the validation environment's failure, a gate
/// that reports no executed tests (a lint or golden gate) replays nothing and
/// is refused, and a replay red on the base too is held as `baseline_red`. A
/// later implementation that deferred nothing clears the obligation.
fn a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery() {
    isolated(
        "a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let gate = Gate::new(sandbox);
            host.require_commands(&["true"]);
            host.baseline_commands(&[&gate.command()]);
            let record = deferral(&fx, &gate.command());
            let handoff = json!({
                "summary": "done",
                "deferred_sandbox_validation": handoff_for(&fx, &record),
            });

            run_implementation(&fx, &host, Some(handoff))
                .expect("the handoff delivers once the gate passes natively");
            let kept = host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT);
            assert_eq!(kept["run_id"], RUN_ID);
            assert_eq!(
                kept["deferred"], record,
                "Orbit derived the record from the gate summary and its own probe"
            );
            assert_eq!(
                host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
                record,
                "the derived record replaces the handoff in the step's output"
            );
            assert_eq!(
                *host.probes.lock().unwrap(),
                vec![(
                    BUBBLEWRAP_NAMESPACE_PROBE.to_string(),
                    None,
                    fx.repo.clone()
                )],
                "Orbit ran the notices' probe once, in the implementer's checkout and lane"
            );
            assert_eq!(gate.runs(), 1, "the owner ran the deferred gate itself");
            let replay = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/1.json"));
            assert_eq!(replay["command"], gate.command().as_str());
            assert_eq!(replay["deferred_sandbox_replay"], true);
            assert_eq!(replay["exit_code"], 0);
            assert_eq!(replay["summary"]["tests_run"], 3);

            run_implementation(&fx, &host, None)
                .expect("a resumed candidate replays the kept gate without its implementer");
            assert_eq!(gate.runs(), 2);

            gate.defers();
            let held = run_implementation(&fx, &host, None)
                .expect_err("a replay that defers again is not a pass");
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&held))
                    && held.contains("deferred a sandbox-confined path"),
                "the host that cannot run it holds the candidate: {held}"
            );

            gate.executes_nothing();
            let refused = run_implementation(&fx, &host, None)
                .expect_err("a gate that reports no executed tests replays nothing");
            assert!(refused.contains("replayed nothing"), "{refused}");

            gate.fails();
            let red = run_implementation(&fx, &host, None)
                .expect_err("a replay red on the base too is not a pass");
            assert!(!is_baseline_red_failure(None, Some(&red)), "{red}");
            assert!(red.contains("failure identities are missing"), "{red}");

            gate.executes();
            let runs = gate.runs();
            run_implementation(&fx, &host, Some(json!({ "summary": "done" })))
                .expect("an implementation that deferred nothing delivers");
            assert_eq!(gate.runs(), runs, "nothing is left to replay");
            assert_eq!(
                host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT)["deferred"],
                Value::Null
            );
        },
    );
}

/// Every handoff whose derivation is not the sanctioned case, only notices
/// of sandboxes whose probe the lane refuses (Bubblewrap [ORB-15287],
/// Seatbelt [ORB-15409], or both [ORB-15692]), fails the implement step
/// before commit: nothing is validated, replayed or kept, and the failure
/// names what is wrong. The gate summary, Orbit's own probes and any
/// hand-written field each decide [ORB-15516].
fn a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step() {
    isolated(
        "a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step",
        |sandbox| {
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
            // (case, the run the gate summary reports, the lane's answer to
            // the record's `probe` when not the sandbox's refusal,
            // hand-written fields beside `summary`, reason)
            let cases = [
                (
                    "a SKIP line",
                    with(
                        "notices",
                        alongside("SKIP: shell_child: sandbox unavailable"),
                    ),
                    none.clone(),
                    none.clone(),
                    "is not a Bubblewrap, Seatbelt or ps-denied deferral",
                ),
                (
                    "a notice of neither sandbox",
                    with(
                        "notices",
                        json!(["DEFERRED: landlock unavailable: confined_child: unsupported"]),
                    ),
                    none.clone(),
                    none.clone(),
                    "is not a Bubblewrap, Seatbelt or ps-denied deferral",
                ),
                (
                    "a Seatbelt denial that is not the guard's notice",
                    seatbelt_with(
                        "notices",
                        json!(["SKIP: seatbelt_child: sandbox_apply: Operation not permitted"]),
                    ),
                    none.clone(),
                    none.clone(),
                    "is not a Bubblewrap, Seatbelt or ps-denied deferral",
                ),
                (
                    "a Seatbelt notice beside a Bubblewrap one, in a lane that applies Seatbelt",
                    seatbelt_with("notices", alongside(&seatbelt_notice())),
                    seatbelt_probe(0, ""),
                    none.clone(),
                    "no refused Seatbelt apply probe",
                ),
                (
                    "a Bubblewrap notice beside a Seatbelt one, in a lane that creates namespaces",
                    with("notices", json!([seatbelt_notice(), bwrap_notice()])),
                    probe(0, ""),
                    none.clone(),
                    "no failing namespace probe",
                ),
                (
                    "a non-Seatbelt skip beside a Seatbelt notice",
                    seatbelt_with(
                        "notices",
                        json!([
                            seatbelt_notice(),
                            "SKIP: the sandbox denied running ps: Operation not permitted (os error 1)",
                        ]),
                    ),
                    none.clone(),
                    none.clone(),
                    "is not a Bubblewrap, Seatbelt or ps-denied deferral",
                ),
                (
                    "a ps-denied notice in an owner run",
                    with("notices", json!([ps_notice()])),
                    none.clone(),
                    none.clone(),
                    "only a claimed leaf on macOS may defer",
                ),
                (
                    "a lane that answers the Seatbelt probe with the namespace one",
                    seatbelt.clone(),
                    valid["probe"].clone(),
                    none.clone(),
                    "names the probe",
                ),
                (
                    "a lane that applies Seatbelt",
                    seatbelt.clone(),
                    seatbelt_probe(0, ""),
                    none.clone(),
                    "no refused Seatbelt apply probe",
                ),
                (
                    "a Seatbelt probe that failed otherwise",
                    seatbelt.clone(),
                    seatbelt_probe(1, "sandbox-exec: sandbox_apply: Operation not permitted"),
                    none.clone(),
                    "no refused Seatbelt apply probe",
                ),
                (
                    "a Seatbelt probe carrying the namespace refusal",
                    seatbelt.clone(),
                    seatbelt_probe(71, "bwrap: No permissions to create new namespace"),
                    none.clone(),
                    "no refused Seatbelt apply probe",
                ),
                (
                    "a Seatbelt gate that failed",
                    seatbelt_with("exit_code", json!(1)),
                    none.clone(),
                    none.clone(),
                    "exited 1",
                ),
                (
                    "a Seatbelt handoff that claims a pass",
                    seatbelt.clone(),
                    none.clone(),
                    json!({ "passed": true }),
                    "unknown field `passed`",
                ),
                (
                    "no notice",
                    with("notices", json!([])),
                    none.clone(),
                    none.clone(),
                    "lists no deferral notice",
                ),
                (
                    "zero tests",
                    with("tests_run", json!(0)),
                    none.clone(),
                    none.clone(),
                    "executed no tests",
                ),
                (
                    "the wrong base",
                    with("base", json!(fx.candidate)),
                    none.clone(),
                    none.clone(),
                    "not the pinned delivery base",
                ),
                (
                    "a nonzero exit",
                    with("exit_code", json!(101)),
                    none.clone(),
                    none.clone(),
                    "exited 101",
                ),
                (
                    "a lane that creates namespaces",
                    valid.clone(),
                    probe(0, ""),
                    none.clone(),
                    "no failing namespace probe",
                ),
                (
                    "a probe that never reached the kernel",
                    valid.clone(),
                    probe(-1, "the probe could not run: No such file or directory"),
                    none.clone(),
                    "no failing namespace probe",
                ),
                (
                    "a hand-written probe that is not Orbit's",
                    valid.clone(),
                    none.clone(),
                    json!({ "probe": probe(1, "bwrap: setting up uid map: Permission denied") }),
                    "hands `probe` as",
                ),
                (
                    "a hand-written count that is not the summary's",
                    valid.clone(),
                    none.clone(),
                    json!({ "tests_run": 1 }),
                    "hands `tests_run` as 1",
                ),
                (
                    "a command no host replays",
                    with("command", json!("scripts/codeql-rust-local.sh")),
                    none.clone(),
                    none.clone(),
                    "not one of the owner's validation gates",
                ),
                (
                    "a host-required criterion",
                    with("command", json!("cargo test -p orbit-cli --test mcp")),
                    none.clone(),
                    none.clone(),
                    "not one of the owner's validation gates",
                ),
                (
                    "a gate that records no command",
                    with("command", Value::Null),
                    none.clone(),
                    none.clone(),
                    "names no gate `command`",
                ),
                (
                    "a handoff that claims a coverage",
                    valid.clone(),
                    none.clone(),
                    json!({ "coverage": "not_run" }),
                    "unknown field `coverage`",
                ),
            ];
            let refuse =
                |case: &str, deferral: Value, answered: &Value, probe: Value, reason: &str| {
                    let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                    host.require_commands(&["true"]);
                    host.baseline_commands(&[&gate.command()]);
                    if !probe.is_null() {
                        host.deferral_answers.lock().unwrap().insert(
                            answered.as_str().unwrap().to_string(),
                            serde_json::from_value(probe).unwrap(),
                        );
                    }
                    let handoff =
                        json!({ "summary": "done", "deferred_sandbox_validation": deferral });
                    let refusal = run_implementation(&fx, &host, Some(handoff)).expect_err(case);
                    assert!(
                        refusal.contains("`deferred_sandbox_validation`")
                            && refusal.contains(reason),
                        "{case}: {refusal}"
                    );
                    assert!(
                        !host.has_artifact(TASK_ID, &format!("validation/{RUN_ID}/0.json")),
                        "{case}: no validation ran"
                    );
                    assert!(
                        !host.has_artifact(TASK_ID, DEFERRED_SANDBOX_ARTIFACT),
                        "{case}: nothing is kept"
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
        },
    );
}

/// A claimed leaf always runs its implementer, so its validation replays the
/// gate from this attempt's output before publication, and the pin expects
/// the replay among the results [ORB-15287]. A replay that defers again is
/// the validation environment's failure, and a record whose command this
/// host does not name as a gate is refused.
fn a_claimed_leaf_replays_its_implementers_deferral_before_publication() {
    isolated(
        "a_claimed_leaf_replays_its_implementers_deferral_before_publication",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            let gate = Gate::new(sandbox);
            host.require_commands(&["true"]);
            host.baseline_commands(&[&gate.command()]);
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
                "implementation": {
                    "deferred_sandbox_validation": deferral(&fx, &gate.command()),
                },
            });

            let pending =
                action(&host, "claim_validate", &input).expect("replay before publication");
            assert_eq!(pending["commands"], json!(["true", gate.command()]));
            assert_eq!(pending["replayed"], json!([gate.command()]));
            assert_eq!(gate.runs(), 1);

            input["pull_request"] = json!(PR_NUMBER);
            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input)
                .expect("the pin accepts the replay beside the required commands");
            assert_eq!(pinned["validation"].as_array().unwrap().len(), 2);
            assert_eq!(gate.runs(), 1, "the pin runs nothing");

            let fields = input.as_object_mut().unwrap();
            fields.remove("pull_request");
            fields.remove("prevalidated");
            gate.defers();
            let held = action(&host, "claim_validate", &input)
                .expect_err("a replay that defers again is not a pass")
                .to_string();
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&held)),
                "{held}"
            );

            input["implementation"]["deferred_sandbox_validation"]["command"] =
                json!("make goldens");
            let refused = action(&host, "claim_validate", &input)
                .expect_err("only this host's gates replay")
                .to_string();
            assert!(
                refused.contains("not one of this host's validation gates")
                    && refused.contains("from this host's configuration"),
                "[ORB-15622] the refusal names the gates it checked and their source: {refused}"
            );
        },
    );
}

/// A macOS implementer whose gate passed with only the Seatbelt guard's skips
/// hands it off like a Bubblewrap deferral [ORB-15409]. The implement step
/// accepts the record before commit, and validation outside the agent
/// sandbox (a claim's on its claim host, before publication) replays the
/// gate and records the native pass as the gate's evidence. A replay that
/// still skips is the validation environment's failure, and a replay that
/// fails is not a pass.
fn a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication() {
    isolated(
        "a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let gate = Gate::new(sandbox);
            host.require_commands(&["true"]);
            host.baseline_commands(&[&gate.command()]);
            let record = seatbelt_deferral(&fx, &gate.command());

            run_implementation(
                &fx,
                &host,
                Some(json!({
                    "summary": "done",
                    "deferred_sandbox_validation": handoff_for(&fx, &record),
                })),
            )
            .expect("the implement step accepts the Seatbelt handoff and the replay passes");
            let kept = host.validation_log(TASK_ID, DEFERRED_SANDBOX_ARTIFACT);
            assert_eq!(
                kept["deferred"], record,
                "the record Orbit derived, with its own Seatbelt probe, is kept"
            );
            let replay = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/1.json"));
            assert_eq!(replay["deferred_sandbox_replay"], true);
            assert_eq!(replay["exit_code"], 0);
            assert_eq!(gate.runs(), 1);

            *host.ship_mode.lock().unwrap() = "pr".to_string();
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
                "implementation": { "deferred_sandbox_validation": record },
            });
            let pending =
                action(&host, "claim_validate", &input).expect("replay before publication");
            assert_eq!(pending["replayed"], json!([gate.command()]));
            assert_eq!(
                gate.runs(),
                2,
                "the claim host ran the deferred gate itself"
            );
            input["pull_request"] = json!(PR_NUMBER);
            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input)
                .expect("the native replay is pinned as the gate's evidence");
            assert_eq!(pinned["validation"].as_array().unwrap().len(), 2);

            let fields = input.as_object_mut().unwrap();
            fields.remove("pull_request");
            fields.remove("prevalidated");
            gate.skips_seatbelt();
            let held = action(&host, "claim_validate", &input)
                .expect_err("a replay that still skips its Seatbelt tests is not a pass")
                .to_string();
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&held))
                    && held.contains(&seatbelt_notice()),
                "the claim host that cannot apply Seatbelt holds the candidate: {held}"
            );

            gate.fails();
            let failed = action(&host, "claim_validate", &input)
                .expect_err("a replay that fails is not a pass")
                .to_string();
            assert!(
                failed.contains(&gate.command()),
                "the failing replay names the gate: {failed}"
            );
        },
    );
}

/// A macOS claimed leaf whose agent sandbox refuses to exec `ps` hands off
/// the tests that compare against it like a nested-Seatbelt skip [ORB-15587].
/// The implement step derives the record with Orbit's own `ps` probe, and the
/// claim host's `claim_validate` replays the gate outside the agent sandbox
/// before publication: a native pass is the gate's evidence, a native failure
/// fails the step, and a replay that still cannot run `ps` holds the
/// candidate. The same notices from a claimed leaf on Linux are refused
/// before commit.
fn a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication() {
    isolated(
        "a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            *host.os.lock().unwrap() = Some(EvidenceHostOs::Macos);
            let gate = Gate::new(sandbox);
            host.require_commands(&["true"]);
            host.baseline_commands(&[&gate.command()]);
            let record = ps_deferral(&fx, &gate.command());
            let handoff = |fx: &Fixture| {
                json!({
                    "summary": "done",
                    "deferred_sandbox_validation": handoff_for(fx, &record),
                })
            };

            run_claimed_implementation(&fx, &host, handoff(&fx))
                .expect("the claim host's native replay passes");
            assert_eq!(
                host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
                record,
                "Orbit derived the record from the gate summary and its own `ps` probe"
            );
            assert_eq!(
                host.probes
                    .lock()
                    .unwrap()
                    .first()
                    .map(|probe| probe.0.as_str()),
                Some(PS_EXEC_PROBE),
                "Orbit ran the `ps` probe in the implementer's lane"
            );
            assert!(
                !host.has_artifact(TASK_ID, DEFERRED_SANDBOX_ARTIFACT),
                "a claimed leaf keeps nothing on the task; its validation reads this output"
            );
            assert_eq!(
                gate.runs(),
                1,
                "the claim host ran the deferred gate itself"
            );

            gate.fails();
            let failed = run_claimed_implementation(&fx, &host, handoff(&fx))
                .expect_err("a native replay that fails is not a pass");
            assert!(
                failed.contains(&gate.command())
                    && !orbit_types::workflow::is_validation_environment_failure(
                        None,
                        Some(&failed)
                    ),
                "the failing replay is the candidate's failure, naming the gate: {failed}"
            );

            gate.denies_ps();
            let held = run_claimed_implementation(&fx, &host, handoff(&fx))
                .expect_err("a replay that still cannot run `ps` is not a pass");
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&held))
                    && held.contains(&ps_notice()),
                "the claim host that cannot run `ps` holds the candidate: {held}"
            );

            gate.executes();
            *host.os.lock().unwrap() = Some(EvidenceHostOs::Linux);
            let runs = gate.runs();
            let refused = run_claimed_implementation(&fx, &host, handoff(&fx))
                .expect_err("only a macOS claimed leaf defers ps-denied tests");
            assert!(
                refused.contains("`deferred_sandbox_validation`")
                    && refused.contains("only a claimed leaf on macOS may defer"),
                "{refused}"
            );
            assert_eq!(gate.runs(), runs, "a refused handoff reaches no replay");
        },
    );
}

/// A macOS claimed leaf's broad gate prints Seatbelt skips and ps-denied
/// deferrals together, and one record defers both once Orbit's probe of each
/// refuses in the lane [ORB-15692]. The claim host's replay, and an owner's
/// replay of a kept record, judge it as they judge a single sandbox's: a
/// native pass with no deferral line is pinned as the gate's evidence, and a
/// replay that still skips Seatbelt or still cannot run `ps` holds the
/// candidate.
fn a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs() {
    isolated(
        "a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            *host.os.lock().unwrap() = Some(EvidenceHostOs::Macos);
            let gate = Gate::new(sandbox);
            host.require_commands(&["true"]);
            host.baseline_commands(&[&gate.command()]);
            let record = mixed_deferral(&fx, &gate.command());
            let handoff = json!({
                "summary": "done",
                "deferred_sandbox_validation": handoff_for(&fx, &record),
            });

            run_claimed_implementation(&fx, &host, handoff)
                .expect("the claim host's native replay passes");
            assert_eq!(
                host.scripted_input("test_stub_commit")["implementation"]["deferred_sandbox_validation"],
                record,
                "Orbit derived one record naming both sandboxes' probes"
            );
            assert_eq!(
                host.probes
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|probe| probe.0.as_str())
                    .collect::<Vec<_>>(),
                vec![SEATBELT_APPLY_PROBE, PS_EXEC_PROBE],
                "Orbit ran each sandbox's probe in the implementer's lane"
            );
            assert_eq!(gate.runs(), 1, "the claim host replayed the gate once");

            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
                "implementation": { "deferred_sandbox_validation": record },
            });
            let pending =
                action(&host, "claim_validate", &input).expect("replay before publication");
            assert_eq!(pending["replayed"], json!([gate.command()]));
            input["pull_request"] = json!(PR_NUMBER);
            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input)
                .expect("the native replay is pinned as the gate's evidence");
            assert_eq!(pinned["validation"].as_array().unwrap().len(), 2);
            let fields = input.as_object_mut().unwrap();
            fields.remove("pull_request");
            fields.remove("prevalidated");

            for (still, notice) in [
                (Gate::skips_seatbelt as fn(&Gate), seatbelt_notice()),
                (Gate::denies_ps, ps_notice()),
            ] {
                still(&gate);
                let held = action(&host, "claim_validate", &input)
                    .expect_err("a replay that still defers is not a pass")
                    .to_string();
                assert!(
                    orbit_types::workflow::is_validation_environment_failure(None, Some(&held))
                        && held.contains(&notice),
                    "the claim host that still defers holds the candidate: {held}"
                );
            }

            let owner = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            owner.require_commands(&["true"]);
            owner.baseline_commands(&[&gate.command()]);
            RuntimeHost::attach_task_validation_log(
                &owner,
                TASK_ID,
                RUN_ID,
                DEFERRED_SANDBOX_ARTIFACT,
                serde_json::to_vec(&json!({
                    "schema_version": 1,
                    "run_id": RUN_ID,
                    "deferred": mixed_deferral(&fx, &gate.command()),
                }))
                .unwrap(),
            )
            .unwrap();
            gate.executes();
            let runs = gate.runs();
            run_implementation(&fx, &owner, None)
                .expect("an owner replays a kept record of both sandboxes and delivers");
            assert_eq!(
                gate.runs(),
                runs + 1,
                "the owner ran the deferred gate itself"
            );
            gate.skips_seatbelt();
            let held = run_implementation(&fx, &owner, None)
                .expect_err("an owner replay that still skips is not a pass");
            assert!(
                orbit_types::workflow::is_validation_environment_failure(None, Some(&held)),
                "{held}"
            );
        },
    );
}

/// The handed-off gate, run by path so a test can change what it does between
/// runs while its command, and so the kept record, stays the same. Each run
/// appends a line to `runs`.
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

    /// Defers its Bubblewrap tests again: this host cannot create the
    /// namespaces either.
    fn defers(&self) {
        self.set(&format!(
            "echo '{}' >&2\n{}",
            bwrap_notice(),
            reported_tests(3)
        ));
    }

    /// Skips its Seatbelt tests again: `sandbox-exec` cannot apply a profile
    /// on this host either.
    fn skips_seatbelt(&self) {
        self.set(&format!(
            "echo '{}' >&2\n{}",
            seatbelt_notice(),
            reported_tests(3)
        ));
    }

    /// Defers its ps-denied tests again: this host refuses `ps` too.
    fn denies_ps(&self) {
        self.set(&format!(
            "echo '{}' >&2\n{}",
            ps_notice(),
            reported_tests(3)
        ));
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

/// Shell that writes the validation summary of a run that executed `tests`.
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

/// Where a handoff's gate summary lives in the checkout.
const GATE_SUMMARY: &str = ".orbit/tmp/gate-summary.json";

/// Write the summary of the gate run `record` describes, as
/// `make ci-test-affected` writes it, and return the handoff that names it
/// [ORB-15516]. Orbit derives `record` from it when its probe refuses.
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

/// The accepted macOS record, with the gate and base substituted for this
/// run's values [ORB-15409].
fn seatbelt_deferral(fx: &Fixture, command: &str) -> Value {
    json!({
        "command": command,
        "exit_code": 0,
        "tests_run": 1650,
        "base": fx.base_sha,
        "notices": [seatbelt_notice()],
        "probe": {
            "command": SEATBELT_APPLY_PROBE,
            "exit_code": 71,
            "output": "sandbox-exec: sandbox_apply: Operation not permitted",
        },
    })
}

/// The accepted macOS claimed-leaf record for ps-denied tests, with the gate
/// and base substituted for this run's values [ORB-15587].
fn ps_deferral(fx: &Fixture, command: &str) -> Value {
    json!({
        "command": command,
        "exit_code": 0,
        "tests_run": 1650,
        "base": fx.base_sha,
        "notices": [ps_notice()],
        "probe": {
            "command": PS_EXEC_PROBE,
            "exit_code": 71,
            "output": "sandbox-exec: execvp() of '/bin/ps' failed: Operation not permitted",
        },
    })
}

/// The accepted macOS claimed-leaf record of a broad gate that printed
/// Seatbelt skips and ps-denied deferrals, with the gate and base substituted
/// for this run's values [ORB-15692].
fn mixed_deferral(fx: &Fixture, command: &str) -> Value {
    let seatbelt = seatbelt_deferral(fx, command);
    let ps = ps_deferral(fx, command);
    json!({
        "command": command,
        "exit_code": 0,
        "tests_run": 1650,
        "base": fx.base_sha,
        "notices": [seatbelt_notice(), ps_notice(), seatbelt_notice()],
        "probe": seatbelt["probe"],
        "other_probes": [ps["probe"]],
    })
}

/// The accepted Linux record, with the gate and base substituted for this
/// run's values.
fn deferral(fx: &Fixture, command: &str) -> Value {
    json!({
        "command": command,
        "exit_code": 0,
        "tests_run": 2164,
        "base": fx.base_sha,
        "notices": [bwrap_notice()],
        "probe": {
            "command": BUBBLEWRAP_NAMESPACE_PROBE,
            "exit_code": 1,
            "output": "bwrap: No permissions to create new namespace",
        },
    })
}

/// Run the shipped PR pipeline's `implement_bundle` and `validate` steps once,
/// between scripted worktree, resume, commit and base-sync steps.
/// `implementation` is the implementer's output, or `None` for a resumed
/// candidate, whose run skips the implementer. Validation is the engine's own
/// `candidate_validate`. Step recovery is left out, so a refused step ends the
/// run with its message.
fn run_implementation(
    fx: &Fixture,
    host: &DeliveryHost,
    implementation: Option<Value>,
) -> Result<(), String> {
    run_shipped_implementation(fx, host, implementation, false)
}

/// [`run_implementation`] through the claimed PR pipeline, whose implementer
/// runs in claimed mode and whose `validate` is the engine's own
/// `claim_validate`, on this claim host before publication [ORB-15587].
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
    host.script(
        "test_stub_worktree",
        json!({ "job_run_id": RUN_ID, "workspace_path": fx.repo, "base_sha": fx.base_sha }),
    );
    host.script(
        "test_stub_resume_candidate",
        json!({ "implement": implementation.is_some(), "repair": null }),
    );
    host.script("test_stub_implement", implementation.unwrap_or(Value::Null));
    host.script(
        "test_stub_commit",
        json!({ "skipped_no_diff_expected": false }),
    );
    host.script(
        "test_stub_sync_base",
        json!({ "base_sha": fx.base_sha, "base_ref": BASE }),
    );

    let shipped = fs::read_to_string(
        workspace_root()
            .join("crates/orbit-core/assets/jobs")
            .join(pipeline),
    )
    .unwrap();
    let shipped = load_job_asset(&shipped).unwrap().spec;
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
    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": format!("test_stub_{id}"), "config": {} },
        })
    };
    let mut job = job_asset(json!([stub("worktree"), stub("resume_candidate")]));
    job.steps.push(step("implement_bundle"));
    let mut commit = stub("commit");
    if implemented {
        // What the commit step sees of the implementer's output.
        commit["default_input"] = json!({ "implementation": "{{ steps.implement_one.output }}" });
    }
    job.steps
        .extend(job_asset(json!([commit, stub("sync_base")])).steps);
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

    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), RUN_ID);
    match execute_job_with_resume(
        &job,
        json!({ "task_ids": [TASK_ID], "base_sync": "local" }),
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

/// An implementer blocker arrives before commit, so the worktree may be dirty.
/// The handoff blocks the task with the kind, leaves the uncommitted file and
/// the head where they are, and opens no `[BLOCKED]` PR.
fn an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr() {
    isolated(
        "an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr",
        |sandbox| {
            let _ = sandbox;
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let head_before = fx.head();
            let remote_before = fx.remote_tip(BRANCH);
            fs::write(fx.repo.join("src/wip.txt"), "still drafting\n").unwrap();
            let message = orbit_types::workflow::task_blocked_by_agent_message(
                &orbit_types::workflow::AgentBlocker {
                    kind: "environment".to_string(),
                    evidence: "the toolchain the task needs is not installed".to_string(),
                },
            );

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "implement_bundle",
                    "error_code": "task_blocked_by_agent",
                    "error_message": message,
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the implementer blocker");

            assert_eq!(handoff["decision"], "blocked_by_agent");
            assert_eq!(handoff["blocker_kind"], "environment");
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(handoff["candidate_preserved"], true);
            assert_eq!(handoff["head_sha"], head_before);
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(fx.head(), head_before, "the dirty tree is not committed");
            assert_eq!(
                fs::read_to_string(fx.repo.join("src/wip.txt")).unwrap(),
                "still drafting\n"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
            let (_, event, note) = host.status_events.lock().unwrap().last().cloned().unwrap();
            assert_eq!(event.as_deref(), Some("task_blocked_by_agent"));
            let note = note.expect("the block records a note");
            assert!(
                note.contains("kind=environment")
                    && orbit_types::workflow::is_task_blocked_by_agent(None, Some(&note)),
                "the history note records the kind: {note}"
            );
            assert!(
                host.comments(TASK_ID)
                    .iter()
                    .any(|comment| comment.message.contains("kind=environment")),
                "the task comment records the kind"
            );
        },
    );
}

/// [ORB-14266] A run its provider failed did not judge the candidate. The
/// handoff commits what the agent left, so the next run can resume it, but
/// pushes nothing, opens no `[BLOCKED]` PR and leaves the status alone: run
/// finalization holds the task in the backlog for another crew.
fn a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write() {
    isolated(
        "a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let head_before = fx.head();
            let remote_before = fx.remote_tip(BRANCH);
            fs::write(fx.repo.join("src/wip.txt"), "half written\n").unwrap();
            let message = format!(
                "step `implement_one`: {}",
                orbit_types::workflow::provider_failure_text(
                    orbit_types::workflow::ProviderFailureClass::Refusal,
                    "codex",
                    "cli subprocess exited with code 1: codex provider refused the request: \
                     This content was flagged for possible cybersecurity risk.",
                )
            );

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "implement_one",
                    "error_code": "provider_refusal",
                    "error_message": message,
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the provider failure");

            assert_eq!(handoff["decision"], "held_provider_failure");
            assert_eq!(handoff["provider_failure"], "provider_refusal");
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(handoff["candidate_preserved"], true);
            assert_ne!(fx.head(), head_before, "the partial candidate is committed");
            assert_eq!(handoff["head_sha"], fx.head());
            assert_eq!(
                git(&fx.repo, &["show", "HEAD:src/wip.txt"]),
                "half written",
                "the commit holds what the agent left"
            );
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(
                fx.durable_candidate(&handoff),
                fx.head(),
                "a claim on another host can fetch the partial candidate"
            );
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::InProgress,
                "run finalization owns the hold"
            );
        },
    );
}

/// An Orbit upgrade refused a reviewer's own `orbit` commands mid-step, and
/// step recovery declared it. The host is mid-upgrade; the candidate was not
/// judged. The handoff keeps it as a provider failure does: what the agent
/// left is committed and carried to its durable ref, no PR is opened, the
/// task is not blocked, and `candidate_resume` resumes the decision. Run
/// finalization returns the task to the backlog.
fn an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task() {
    isolated(
        "an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let head_before = fx.head();
            let remote_before = fx.remote_tip(BRANCH);
            fs::write(fx.repo.join("src/wip.txt"), "reviewer fix in progress\n").unwrap();
            let blocker = orbit_types::workflow::AgentBlocker {
                kind: "orbit.generation_switch_pending".to_string(),
                evidence: "upgrade admission refused: cannot record the pending generation \
                           switch: Operation not permitted (os error 1)"
                    .to_string(),
            };
            let message = format!(
                "{}\noriginal error before recovery: the review report was never recorded",
                blocker.step_failure_message()
            );

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "review",
                    "error_code": "upgrade_pending",
                    "error_message": message,
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the upgrade refusal");

            assert_eq!(handoff["decision"], "held_upgrade_pending");
            assert_eq!(handoff["blocker_kind"], "orbit.generation_switch_pending");
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(handoff["candidate_preserved"], true);
            assert_ne!(fx.head(), head_before, "the reviewer's edit is committed");
            assert_eq!(handoff["head_sha"], fx.head());
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(
                fx.durable_candidate(&handoff),
                fx.head(),
                "the next run, on any host, can fetch the candidate"
            );
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::InProgress,
                "the task is not blocked; run finalization requeues it"
            );
        },
    );
}

/// A delivery its gate stopped waiting for [ORB-15643] is cancelled
/// mid-validation. Nothing judged its candidate: the handoff commits what it
/// left, carries it to its durable ref and opens no PR. The cancellation, not
/// the handoff, returns the task to the backlog.
fn a_child_its_parent_stopped_waiting_for_holds_its_candidate() {
    isolated(
        "a_child_its_parent_stopped_waiting_for_holds_its_candidate",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let head_before = fx.head();
            let remote_before = fx.remote_tip(BRANCH);
            fs::write(
                fx.repo.join("src/wip.txt"),
                "implementation under validation\n",
            )
            .unwrap();

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "validate",
                    "error_code": "child_timeout",
                    "error_message": "child_timeout: parent run jrun-parent stopped waiting for \
                                      child run jrun-child after 14400 s",
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the timed-out child");

            assert_eq!(handoff["decision"], "held_child_timeout");
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(handoff["candidate_preserved"], true);
            assert_ne!(fx.head(), head_before, "the unvalidated edit is committed");
            assert_eq!(handoff["head_sha"], fx.head());
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(
                fx.durable_candidate(&handoff),
                fx.head(),
                "the next run, on any host, can fetch the candidate"
            );
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::InProgress,
                "the cancellation, not the handoff, settles the task"
            );
        },
    );
}

// ---------------------------------------------------------------------------
// A red base and network flakes [ORB-14258]
// ---------------------------------------------------------------------------

/// The command a red-base fixture requires; its base's `Makefile` fails it.
const RED_LINT: &str = "make ci-lint";

/// Normal child output cannot impersonate a supervisor stop [ORB-15507].
/// The synchronized base prints the same note but passes the command.
fn printed_supervisor_notes_remain_candidate_failures_on_both_streams() {
    isolated(
        "printed_supervisor_notes_remain_candidate_failures_on_both_streams",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
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
                    let message = action(&host, "candidate_validate", &fx.validate_input())
                        .expect_err("a normal exit-1 command fails the candidate")
                        .to_string();
                    assert!(
                        !message.contains("[validation_environment]")
                            && !is_baseline_red_failure(None, Some(&message)),
                        "[ORB-15507] child output cannot supply stop metadata: {message}"
                    );
                    let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                    assert_eq!(log["exit_code"], 1, "{command}: {log}");
                    assert_eq!(log["timed_out"], false, "{command}: {log}");
                    assert_eq!(log["failure_kind"], "candidate", "{command}: {log}");
                    assert_eq!(log["baseline"]["decision"], "passed", "{command}: {log}");
                    let base = host
                        .validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.baseline.json"));
                    assert_eq!(base["exit_code"], 0, "the healthy base ran: {base}");
                    assert_eq!(base["base_sha"], fx.base_sha, "{base}");

                    let claim_input = json!({
                        "workspace_path": fx.repo,
                        "base_sync": "local",
                        "base_sha": fx.base_sha,
                    });
                    let message = action(&host, "claim_validate", &claim_input)
                        .expect_err("the claim must also reject the candidate's normal exit")
                        .to_string();
                    assert!(
                        !message.contains("[validation_environment]")
                            && !is_baseline_red_failure(None, Some(&message)),
                        "[ORB-15507] claimed validation uses the same stop metadata: {message}"
                    );
                    let logs = host.claim_log_contents.lock().unwrap();
                    let claim: Value = serde_json::from_slice(
                        logs.get("validation/claim-landing/0.failed.json").unwrap(),
                    )
                    .unwrap();
                    assert_eq!(claim["exit_code"], 1, "{command}: {claim}");
                    assert_eq!(claim["timed_out"], false, "{command}: {claim}");
                    assert_eq!(claim["failure_kind"], "candidate", "{command}: {claim}");
                    assert_eq!(
                        claim["baseline"]["decision"], "passed",
                        "{command}: {claim}"
                    );
                }
            }
            assert_eq!(
                fx.head(),
                fx.candidate,
                "validation preserves the candidate"
            );
        },
    );
}

fn supervisor_stops_are_environment_failures_and_never_poison_the_base_cache() {
    isolated(
        "supervisor_stops_are_environment_failures_and_never_poison_the_base_cache",
        |sandbox| {
            // The supervisor restores and re-raises an incoming SIGTERM after
            // cleanup. A harmless prior handler keeps this isolated fixture
            // alive to inspect the returned classification; exec resets the
            // caught disposition in the actual shell child.
            extern "C" fn retain_fixture_process(_signal: libc::c_int) {}
            let previous = unsafe {
                libc::signal(
                    libc::SIGTERM,
                    retain_fixture_process as *const () as libc::sighandler_t,
                )
            };
            assert_ne!(
                previous,
                libc::SIG_ERR,
                "install the fixture SIGTERM handler"
            );
            // Pin the supervisor, not just the command's environment: the drain
            // may have raised this ceiling as an operational mitigation.
            unsafe { std::env::set_var("ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES", "4096") };
            // A capture stop still names the first failure before it, so a
            // flood of one spawn failure says what failed [ORB-15870].
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
                    let fx = Fixture::new(&root);
                    fx.commit_on_base(
                        "stop.sh",
                        &format!("#!/bin/sh\necho ran >> '{}'\n{stop}\n", runs.display()),
                    );
                    git(&fx.repo, &["rebase", BASE]);
                    git(&fx.repo, &["push", "--force", "origin", BRANCH]);
                    let fx = Fixture {
                        base_sha: git(&fx.repo, &["rev-parse", BASE]),
                        candidate: fx.head(),
                        ..fx
                    };
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
                        let message = action(&host, "candidate_validate", &fx.validate_input())
                            .expect_err("a supervisor stop is inconclusive")
                            .to_string();
                        assert!(
                            message.contains("[validation_environment]") && message.contains(cause),
                            "the infrastructure cause must be named: {message}"
                        );
                        assert_eq!(
                            message.contains(first_failure),
                            index == 0 && !base_only,
                            "only the candidate's capture stop printed a failure first: {message}"
                        );
                        assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
                        let log =
                            host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                        assert_eq!(log["failure_kind"], "environment", "{log}");
                        if base_only {
                            assert_eq!(log["baseline"]["decision"], "inconclusive", "{log}");
                            assert_eq!(log["baseline"]["cached"], false, "{log}");
                        } else {
                            assert!(
                                log["baseline"].is_null(),
                                "a stopped candidate needs no base replay: {log}"
                            );
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
        },
    );
}

/// A required command that fails on the synchronized base exactly as on the
/// candidate is the base's failure. The step fails typed `baseline_red` with
/// the candidate's and the base's logs. Two candidates over that base share
/// one base run. The failure handoff holds the task in the backlog: nothing is
/// pushed, no PR is opened, and the candidate is kept. The hold stands while
/// the base ref points at the red commit and lifts once it moves to a commit
/// that passes.
fn a_required_command_red_on_the_base_holds_the_task_without_a_pr() {
    isolated(
        "a_required_command_red_on_the_base_holds_the_task_without_a_pr",
        |sandbox| {
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
                .map(|result| result.expect_err("a red base fails validation").to_string())
                .collect::<Vec<_>>();
            for message in &messages {
                assert!(
                    is_baseline_red_failure(None, Some(message)),
                    "the failure is typed as the base's: {message}"
                );
            }
            assert_eq!(
                fs::read_to_string(&runs).unwrap().lines().count(),
                3,
                "two candidate runs share one base run"
            );
            let hold =
                BaselineRedHold::from_text(&messages[0]).expect("the failure names its hold");
            assert_eq!(
                hold.failures.identities,
                ["README.md:1:1: error: lint is red"],
                "a hold requires an identified inherited error"
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
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["exit_code"], 2);
            assert_eq!(log["failure_kind"], "baseline_red");
            assert_eq!(log["baseline"]["decision"], "failed");
            assert_eq!(log["baseline"]["exit_code"], 2);
            let base_log =
                host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.baseline.json"));
            assert_eq!(base_log["base_sha"], fx.base_sha.as_str());
            assert_eq!(base_log["passed"], false);
            assert!(
                base_log["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("error: lint is red")),
                "{base_log}"
            );

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "validate",
                    "error_code": "baseline_red",
                    "error_message": messages[0],
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("hand off the red base");
            assert_eq!(handoff["decision"], "held_baseline_red");
            assert_eq!(handoff["candidate_preserved"], true);
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(fx.forge_state("pr-head"), None, "no PR is opened");
            assert_eq!(fx.remote_tip(BRANCH), remote_before, "nothing was pushed");
            assert_eq!(fx.head(), fx.candidate, "the candidate is kept");
            assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
            let (_, event, note) = host.status_events.lock().unwrap().last().cloned().unwrap();
            assert_eq!(event.as_deref(), Some(BASELINE_RED_HOLD_EVENT));
            assert_eq!(
                note.as_deref().and_then(BaselineRedHold::from_text),
                Some(hold.clone()),
                "the task history carries the hold admission reads"
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
        },
    );
}

/// The command a red-test fixture requires ([`Fixture::with_red_tests`]).
const RED_TESTS: &str = "make ci-test";

/// A known-red case: its name, the base's tests, the candidate's tests (`None`
/// fails exactly as the base), and the tests it proceeds with as known red
/// (`None` requires candidate repair).
type KnownRedCase<'a> = (
    &'a str,
    &'a [&'a str],
    Option<&'a [&'a str]>,
    Option<&'a [&'a str]>,
);

// Reuse the existing runner and suite identities for the observed six-vs-one
// shape and mixed failures. Both delivery routes must make the same decision.
const KNOWN_RED_CASES: &[KnownRedCase<'_>] = &[
    (
        "equal",
        &["suite::broken", "suite::flaky"],
        None,
        Some(&["suite::broken", "suite::flaky"]),
    ),
    (
        "subset",
        &["suite::broken", "suite::flaky"],
        Some(&["suite::flaky"]),
        Some(&["suite::flaky"]),
    ),
    (
        "shared-plus-new",
        &["suite::broken", "suite::flaky"],
        Some(&["suite::broken", "suite::flaky", "suite::added"]),
        None,
    ),
    (
        "disjoint-six-vs-one",
        &["suite::broken"],
        Some(&[
            "orbit-core suite::broken",
            "orbit-core suite::flaky",
            "orbit-core suite::added",
            "orbit-engine suite::broken",
            "orbit-engine suite::flaky",
            "orbit-engine suite::added",
        ]),
        None,
    ),
    ("candidate-unknown", &["suite::broken"], Some(&[]), None),
    ("base-unknown", &[], Some(&["suite::added"]), None),
    ("both-unknown", &[], None, None),
];

/// nextest's `FAIL` summary line for each of `tests`.
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

/// [ORB-15375] A candidate whose failing tests all fail on a red base — the
/// same tests, or some of them — introduced none of them: validation passes
/// as `passed_with_known_red`, and the run's output, the log, the task's
/// history and the pull request name the known-red tests. A candidate that
/// fails a test the base does not, beside the base's or instead of them,
/// requires repair, retaining both complete failure sets in durable logs.
fn a_candidate_failing_only_known_red_tests_proceeds_and_records_them() {
    isolated(
        "a_candidate_failing_only_known_red_tests_proceeds_and_records_them",
        |sandbox| {
            let named = |tests: &[&str]| {
                tests
                    .iter()
                    .map(|test| format!("test orbit-core::runtime {test}"))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            };
            for &(case, base, candidate_tests, known_red) in KNOWN_RED_CASES {
                let fx = Fixture::with_red_tests(sandbox, base, candidate_tests);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                host.require_commands(&[RED_TESTS]);
                let mut input = fx.validate_input();
                input["base_ref"] = json!(BASE);

                let outcome = action(&host, "candidate_validate", &input);
                let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
                let recorded = host
                    .history(TASK_ID)
                    .into_iter()
                    .filter(|entry| entry.event == KNOWN_RED_VALIDATION_EVENT)
                    .map(|entry| {
                        KnownRedValidation::from_text(entry.note.as_deref().unwrap_or_default())
                            .expect("the note carries its record")
                    })
                    .collect::<Vec<_>>();

                let Some(known_red) = known_red else {
                    let message = outcome
                        .expect_err(&format!("{case}: candidate failures need repair"))
                        .to_string();
                    assert!(
                        !is_baseline_red_failure(None, Some(&message))
                            && !message.contains("failed identically"),
                        "different or unknown failures cannot hold for a red base: {case}: {message}"
                    );
                    assert_eq!(log["failure_kind"], "candidate", "{case}");
                    assert_eq!(log["baseline_reproduces"], false, "{case}");
                    assert_eq!(
                        log["failures"],
                        json!(named(candidate_tests.unwrap_or(base))),
                        "{case}"
                    );
                    let baseline = host
                        .validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.baseline.json"));
                    assert_eq!(
                        baseline["failures"],
                        json!(
                            named(base)
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
                assert_eq!(
                    output["commands"],
                    json!([]),
                    "{case}: nothing passed outright"
                );
                let record = KnownRedValidation {
                    command: RED_TESTS.to_string(),
                    base_sha: fx.base_sha.clone(),
                    tested_head: fx.candidate.clone(),
                    run_id: RUN_ID.to_string(),
                    failures: recorded[0].failures.clone(),
                };
                assert_eq!(record.failures.identities, named(known_red), "{case}");
                assert_eq!(
                    output["known_red"],
                    json!([record]),
                    "{case}: the run records it"
                );
                assert_eq!(log["failure_kind"], "known_red", "{case}");
                assert_eq!(
                    log["known_red"]["identities"],
                    json!(named(known_red)),
                    "{case}: the log records it"
                );
                assert_eq!(
                    recorded,
                    vec![record],
                    "{case}: the task's history records it"
                );

                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap_or_else(|error| panic!("{case}: open the PR: {error}"));
                let body = fx
                    .forge_state("pr-body")
                    .expect("the forge received a body");
                assert!(body.contains("## Known-red validation"), "{case}: {body}");
                assert!(
                    body.contains(&format!("`{RED_TESTS}` fails on base `{}`", fx.base_sha)),
                    "{case}: {body}"
                );
                for test in known_red {
                    assert!(
                        body.contains(&format!("`orbit-core::runtime {test}`")),
                        "{case}: the PR names {test}: {body}"
                    );
                }
            }
        },
    );
}

/// [ORB-15548] Shared failed tests do not waive unexecuted host coverage.
/// Both delivery paths retain complete-output notices even when the bounded
/// log drops them, and check declared replay counts before known-red admission.
fn known_red_coverage_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        let base = ["suite::broken", "suite::flaky"];
        let cases = [
            (
                "bubblewrap",
                Some(bwrap_notice()),
                false,
                false,
                Some(3),
                Some("sandbox_unavailable"),
            ),
            (
                "seatbelt",
                Some(seatbelt_notice()),
                false,
                false,
                Some(3),
                Some("sandbox_unavailable"),
            ),
            (
                "deferred",
                Some("DEFERRED: sandbox fixture did not run".to_string()),
                false,
                false,
                Some(3),
                Some("self_skipped"),
            ),
            (
                "skip",
                Some("SKIP: sandbox fixture did not run".to_string()),
                false,
                false,
                Some(3),
                Some("self_skipped"),
            ),
            (
                "skipping-colon",
                Some("skipping: sandbox fixture".to_string()),
                false,
                false,
                Some(3),
                Some("self_skipped"),
            ),
            (
                "skipping-space",
                Some("skipping sandbox fixture".to_string()),
                false,
                false,
                Some(3),
                Some("self_skipped"),
            ),
            (
                "raw-refusal",
                Some("bwrap: No permissions to create new namespace".to_string()),
                false,
                false,
                Some(3),
                Some("sandbox_unavailable"),
            ),
            (
                "dropped-deferral",
                Some(bwrap_notice()),
                true,
                false,
                Some(3),
                Some("sandbox_unavailable"),
            ),
            (
                "dropped-skip",
                Some("SKIP: sandbox fixture did not run".to_string()),
                true,
                false,
                Some(3),
                Some("self_skipped"),
            ),
            (
                "replay-with-notice",
                Some(bwrap_notice()),
                false,
                true,
                Some(3),
                Some("sandbox_unavailable"),
            ),
            ("replay-zero-tests", None, false, true, Some(0), None),
            ("replay-missing-count", None, false, true, None, None),
            ("replay-missing-tool", None, false, true, None, None),
            ("replay-executed", None, false, true, Some(3), None),
        ];
        for (case, notice, dropped, replay, tests_run, reason) in
            cases.into_iter().filter(|row| selected.contains(&row.0))
        {
            let fx = Fixture::with_red_tests(sandbox, &base, Some(&base));
            let padding = if dropped {
                "padding line that names no failure\n".repeat(8 * 1024)
            } else {
                String::new()
            };
            // Alternate complete stdout and stderr, including notices in
            // the dropped middle, without depending on sandbox availability.
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
                host.attach_task_validation_log(
                    TASK_ID,
                    RUN_ID,
                    DEFERRED_SANDBOX_ARTIFACT,
                    serde_json::to_vec(&json!({
                        "schema_version": 1,
                        "run_id": RUN_ID,
                        "deferred": record,
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
            let outcome = action(&host, "candidate_validate", &fx.validate_input());
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
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
            let mut claim_input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });
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
                assert!(
                    is_baseline_red_failure(None, Some(&failure)),
                    "{case}: {failure}"
                );
                assert!(
                    is_baseline_red_failure(None, Some(&claim_failure)),
                    "{case}: {claim_failure}"
                );
                assert_eq!(log["host_output_refusal"]["reason"], reason, "{case}");
                assert_eq!(
                    log["host_output_refusal"]["detail"],
                    notice.as_deref().unwrap(),
                    "{case}"
                );
                let claim_log: Value = serde_json::from_slice(
                        &host.claim_log_contents.lock().unwrap()
                            ["validation/claim-landing/0.failed.json"],
                    )
                    .unwrap();
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
                assert!(
                    orbit_types::workflow::is_validation_environment_failure(None, Some(&failure)),
                    "{case}: {failure}"
                );
                assert!(
                    orbit_types::workflow::is_validation_environment_failure(
                        None,
                        Some(&claim_failure)
                    ),
                    "{case}: {claim_failure}"
                );
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
            assert!(
                host.handoffs.lock().unwrap().is_empty(),
                "{case}: no publication"
            );
        }
    });
}

/// [ORB-15427] A claimed leaf applies the owner's known-red rule. A
/// candidate failing the base's tests, or some of them, passes validation as
/// `passed_with_known_red`, before publication and when pinned: its handoff
/// log keeps the exit status and reports the base and the failures, the
/// output names them, and the leaf's pull request lists them. The owner
/// trusts none of that: its own check of the base
/// (`verify_claimed_known_red`), here with the base result cache cleared so
/// it reruns the base, confirms the report. A candidate failing a test the
/// base does not requires candidate repair, and the owner's check refuses
/// the same report if a leaf made it anyway.
fn claimed_known_red_rows(test: &str, selected: &[&str]) {
    isolated(test, |sandbox| {
        let named = |tests: &[&str]| {
            tests
                .iter()
                .map(|test| format!("test orbit-core::runtime {test}"))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        };
        for &(case, base, candidate_tests, known_red) in KNOWN_RED_CASES
            .iter()
            .filter(|row| selected.contains(&row.0))
        {
            let fx = Fixture::with_red_tests(sandbox, base, candidate_tests);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&[RED_TESTS]);
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });
            let outcome = action(&host, "claim_validate", &input);
            // The owner's own base run, not the one the leaf cached.
            let cache = PathBuf::from(git(&fx.repo, &["rev-parse", "--git-common-dir"]))
                .join("orbit-baseline");
            let owner_check = |base_sha: &str, failures: &BTreeSet<String>| {
                if cache.exists() {
                    fs::remove_dir_all(&cache).unwrap();
                }
                verify_claimed_known_red(
                    &host,
                    &fx.repo,
                    &ClaimedKnownRed {
                        base_branch: BASE,
                        base_sync: orbit_engine::BaseSyncMode::Local,
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
                    !is_baseline_red_failure(None, Some(&message))
                        && !message.contains("failed identically"),
                    "different or unknown failures cannot hold for a red base: {case}: {message}"
                );
                assert!(host.handoffs.lock().unwrap().is_empty(), "{case}");
                let logs = host.claim_log_contents.lock().unwrap();
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
                    json!(named(candidate_tests.unwrap_or(base))),
                    "{case}"
                );
                assert_eq!(
                    baseline["failures"],
                    json!(
                        named(base)
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
                let claimed = named(candidate_tests.unwrap()).into_iter().collect();
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
            assert_eq!(records[0].failures.identities, named(known_red), "{case}");
            let path = "validation/claim-landing/0.json";
            let log: HandoffValidationLog =
                serde_json::from_slice(&host.claim_log_contents.lock().unwrap()[path]).unwrap();
            assert_eq!(log.exit_code, 2, "{case}: the log keeps the exit status");
            let report = log.known_red.expect("the log reports known red");
            assert_eq!(report.base_sha, fx.base_sha, "{case}");
            assert!(
                named(known_red)
                    .iter()
                    .all(|test| report.failures.contains(test)),
                "{case}: {report:?}"
            );
            assert_eq!(
                owner_check(&fx.base_sha, &report.failures),
                Ok(report.failures.clone()),
                "{case}: the owner's base run fails every reported test"
            );
            // A base off the owner's own base branch is no base: here the
            // candidate itself, which fails the same tests. Nothing runs.
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

            // Before publication the results carry the report, and the
            // pin attaches the same log for the published candidate.
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            host.claim_log_contents.lock().unwrap().clear();
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
            let body = fx
                .forge_state("pr-body")
                .expect("the forge received a body");
            assert!(body.contains("## Known-red validation"), "{case}: {body}");
            for test in known_red {
                assert!(
                    body.contains(&format!("`orbit-core::runtime {test}`")),
                    "{case}: the PR names {test}: {body}"
                );
            }
            input["pull_request"] = json!(PR_NUMBER);
            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input)
                .unwrap_or_else(|error| panic!("{case}: pin: {error}"));
            assert_eq!(pinned["decision"], "passed_with_known_red", "{case}");
            let pinned_log: HandoffValidationLog =
                serde_json::from_slice(&host.claim_log_contents.lock().unwrap()[path]).unwrap();
            assert_eq!(pinned_log.exit_code, 2, "{case}");
            assert_eq!(pinned_log.known_red, Some(report), "{case}");
        }
    });
}

/// [ORB-15375] Captured output keeps only a head and a tail, so a failure in
/// the middle of a long run never reaches the log. The failing-test set is
/// read from the complete output: a candidate whose extra failure only the
/// dropped middle names requires repair, retaining that identity in its log.
fn a_failure_only_the_truncated_middle_names_still_requires_candidate_repair() {
    isolated(
        "a_failure_only_the_truncated_middle_names_still_requires_candidate_repair",
        |sandbox| {
            let base = ["suite::broken", "suite::flaky"];
            let fx = Fixture::with_red_tests(sandbox, &base, Some(&base));
            // About 590 KB: past the 256 KiB recorded budget, under the 1 MiB
            // supervisor capture limit that stops a run as an environment failure.
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

            let message = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a failure the base lacks requires candidate repair")
                .to_string();

            assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["tested_head"], candidate.as_str());
            let captured = log["output"].as_str().unwrap_or_default();
            assert!(
                captured.contains("truncated") && captured.len() <= 256 * 1024 + 1024,
                "bounded evidence must disclose omitted output"
            );
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
            let message = action(&host, "claim_validate", &claim_input)
                .expect_err("the claimed middle failure also requires repair")
                .to_string();
            assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
            let logs = host.claim_log_contents.lock().unwrap();
            let claim: Value =
                serde_json::from_slice(&logs["validation/claim-landing/0.failed.json"]).unwrap();
            assert_eq!(claim["tested_head"], candidate.as_str());
            assert_eq!(claim["base_sha"], fx.base_sha);
            assert_eq!(claim["failure_kind"], log["failure_kind"]);
            assert_eq!(claim["failures"], log["failures"]);
            assert_eq!(claim["output"], log["output"]);
            drop(logs);
            assert!(
                log["baseline"]["log"].as_str().is_some(),
                "the candidate comparison retains its base evidence"
            );
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
                    .all(|entry| entry.event != KNOWN_RED_VALIDATION_EVENT),
                "nothing is recorded as known red"
            );
        },
    );
}

/// A red base discovered while revalidating a rebased, already-published PR
/// still holds the task. The earlier completion-failure path used to preserve
/// every PR failure in `review`, bypassing the baseline hold.
fn a_red_base_during_pr_revalidation_holds_the_task() {
    isolated(
        "a_red_base_during_pr_revalidation_holds_the_task",
        |sandbox| {
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

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "re_review_validate",
                    "error_code": "baseline_red",
                    "error_message": diagnostic,
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    },
                }),
            )
            .expect("a red base is held before completion failures preserve review status");

            assert_eq!(handoff["decision"], "held_baseline_red");
            assert_eq!(handoff["pr_created"], false);
            assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
            assert_eq!(
                fx.remote_tip(BRANCH),
                remote_before,
                "the failing head is not pushed"
            );
            let (_, event, note) = host.status_events.lock().unwrap().last().cloned().unwrap();
            assert_eq!(event.as_deref(), Some(BASELINE_RED_HOLD_EVENT));
            assert_eq!(
                note.as_deref().and_then(BaselineRedHold::from_text),
                Some(hold.clone()),
                "the hold retains the base ref needed for automatic lift"
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
        },
    );
}

/// A failure that looks network-inconclusive is rerun after a backoff. One
/// that clears passes, with the rerun recorded. One that persists stops after
/// two reruns and is judged as any failure is.
fn a_network_flake_is_rerun_before_the_candidate_is_judged() {
    isolated(
        "a_network_flake_is_rerun_before_the_candidate_is_judged",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let attempts = sandbox.join("attempts");
            let flaky = format!(
                "echo try >> '{0}'; [ $(wc -l < '{0}') -gt 1 ] || \
                 {{ echo 'curl: (6) Could not resolve host: forge.invalid' >&2; exit 6; }}",
                attempts.display()
            );
            host.require_commands(&[&flaky]);

            let validated = action(&host, "candidate_validate", &fx.validate_input())
                .expect("the rerun passes");
            assert_eq!(validated["decision"], "passed");
            assert_eq!(fs::read_to_string(&attempts).unwrap().lines().count(), 2);
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
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
            let error = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a persistent failure is still a failure");
            let message = error.to_string();
            assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
            assert_eq!(
                fs::read_to_string(&offline).unwrap().lines().count(),
                3,
                "one run and two reruns"
            );
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["network_retries"], 2);
            assert_eq!(log["baseline"]["decision"], "passed");
        },
    );
}

/// A claimed leaf's required command that fails the same way on its base is
/// typed `baseline_red`. The hold names the claim's base ref, the owner is
/// sent the candidate's and the base's logs, and nothing is handed off.
fn a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs() {
    isolated(
        "a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs",
        |sandbox| {
            let fx = Fixture::with_red_lint(sandbox, &sandbox.join("lint-runs"));
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&[RED_LINT]);
            let input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });

            let error = action(&host, "claim_validate", &input).expect_err("a red base fails");

            let message = error.to_string();
            let hold = BaselineRedHold::from_text(&message).expect("typed baseline_red");
            assert_eq!(hold.base_ref, BASE);
            assert_eq!(hold.base_sha, fx.base_sha);
            assert_eq!(hold.command, RED_LINT);
            assert_eq!(
                *host.claim_logs.lock().unwrap(),
                [
                    "validation/claim-landing/0.failed.json",
                    "validation/claim-landing/0.baseline.json"
                ]
            );
            assert!(host.handoffs.lock().unwrap().is_empty());
        },
    );
}

/// [ORB-14257] A claimed leaf's required command that still cannot reach the
/// network after its reruns, on a base that passes, says nothing about the
/// candidate: it fails typed `transient`, so the leaf releases its claim
/// instead of blocking the task, and the owner is still sent both logs.
fn a_claimed_candidate_that_cannot_reach_the_network_fails_transient() {
    isolated(
        "a_claimed_candidate_that_cannot_reach_the_network_fails_transient",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            // Only the candidate has src/feature.txt, so the base passes.
            host.require_commands(&["test ! -e src/feature.txt || \
                 { echo 'curl: (6) Could not resolve host: forge.invalid' >&2; exit 6; }"]);
            let input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });

            let error = action(&host, "claim_validate", &input)
                .expect_err("an unreachable network fails the step");

            let message = error.to_string();
            assert_eq!(
                ClaimFailureClass::of_step_failure(None, Some(&message)),
                Some(ClaimFailureClass::Transient),
                "{message}"
            );
            assert!(!is_baseline_red_failure(None, Some(&message)), "{message}");
            assert_eq!(
                *host.claim_logs.lock().unwrap(),
                [
                    "validation/claim-landing/0.failed.json",
                    "validation/claim-landing/0.baseline.json"
                ]
            );
            assert!(host.handoffs.lock().unwrap().is_empty());
        },
    );
}

/// A PR-mode claimed leaf validates before it publishes. That run attaches
/// no log and pins no candidate. After `pr_open`, the pin step attaches the
/// logs for the published candidate without running a command again, and the
/// owner's handoff accepts them. A pin for a different commit is refused.
fn a_pr_claim_validates_before_publication_and_pins_after_it() {
    isolated(
        "a_pr_claim_validates_before_publication_and_pins_after_it",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            *host.ship_mode.lock().unwrap() = "pr".to_string();
            let runs = sandbox.join("runs");
            let command = format!("echo ran >> '{}'; echo suite-ok", runs.display());
            host.require_commands(&[&command]);
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });

            let pending = action(&host, "claim_validate", &input).expect("validate before push");
            assert_eq!(pending["decision"], "passed");
            assert_eq!(pending["publication"], "pending");
            assert_eq!(pending["candidate"], Value::Null);
            assert_eq!(pending["tested_head"], fx.candidate.as_str());
            assert!(
                host.claim_logs.lock().unwrap().is_empty(),
                "nothing attached"
            );

            input["pull_request"] = json!(PR_NUMBER);
            let mut moved = pending.clone();
            moved["tested_head"] = json!(fx.base_sha);
            input["prevalidated"] = moved;
            let refused = action(&host, "claim_validate", &input)
                .expect_err("a pin for another commit is refused");
            assert!(
                refused.to_string().contains("rerun validation"),
                "{refused}"
            );

            input["prevalidated"] = pending;
            let pinned = action(&host, "claim_validate", &input).expect("pin");
            assert_eq!(pinned["decision"], "passed");
            assert_eq!(pinned["validation"].as_array().unwrap().len(), 1);
            assert_eq!(host.claim_logs.lock().unwrap().len(), 1);
            assert_eq!(
                fs::read_to_string(&runs).unwrap().lines().count(),
                1,
                "the pin runs no command"
            );

            let handoff = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
                "pull_request": PR_NUMBER,
                "candidate": pinned["candidate"],
                "validation": pinned["validation"],
            });
            action(&host, "claim_handoff", &handoff).expect("the owner's handoff accepts the pin");
            assert_eq!(host.handoffs.lock().unwrap().len(), 1);
        },
    );
}

/// A launcher environment with only [`MINIMAL_PATH`], plus a substitute login
/// shell whose profile prints a banner and prepends a toolchain directory
/// holding [`PROFILE_TOOL`].
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

// ---------------------------------------------------------------------------
// Conflicting reviewed head at completion [ORB-13890]
// ---------------------------------------------------------------------------

/// A reviewed PR that GitHub reports conflicting is never merged as rebased,
/// unreviewed content. Without `re_review_on_conflict` completion refuses as
/// `review_gate_stale` and changes nothing. With it, completion rebases the
/// branch locally, publishes nothing, completes no task, and hands back the
/// rebase checkpoint. Once that head is (re)reviewed and republished under
/// the rebase lease, completing it with the new reviewed head merges exactly
/// that head and reaches `done`.
fn a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes() {
    isolated(
        "a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the reviewed candidate");
            host.set_status(TASK_ID, TaskStatus::Review);
            fx.advance_base();
            fx.script_checks(&["dirty"]);

            let error = action(&host, "pr_complete", &fx.complete_input())
                .expect_err("a caller without re-review cannot repair a reviewed head");
            assert!(error.to_string().contains("review_gate_stale"), "{error}");
            assert_eq!(fx.head(), fx.candidate, "the checkout is untouched");

            let mut input = fx.complete_input();
            input["re_review_on_conflict"] = json!(true);
            let rebased = action(&host, "pr_complete", &input)
                .expect("a conflicting reviewed head is rebased for re-review");
            assert_eq!(rebased["re_review_required"], true);
            assert_eq!(rebased["completed_task_ids"], json!([]));
            assert_eq!(rebased["merge"]["merged"], false);
            let checkpoint = &rebased["rebased"];
            let rebased_head = checkpoint["head_sha"].as_str().unwrap().to_string();
            assert_eq!(rebased_head, fx.head());
            assert_ne!(rebased_head, fx.candidate);
            assert_eq!(checkpoint["rewritten"], true);
            assert_eq!(
                fx.remote_tip(BRANCH),
                fx.candidate,
                "nothing unreviewed is published"
            );
            assert!(fx.merge_requests().is_empty(), "no merge is requested");
            assert!(host.landings().is_empty());
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);

            // The pipeline reviews the rebased head, then republishes it
            // under the lease the rebase checkpoint carries.
            let pushed = action(
                &host,
                "git_push",
                &json!({
                    "workspace_path": fx.repo,
                    "job_run_id": RUN_ID,
                    "completed_task_ids": [TASK_ID],
                    "branch": checkpoint["head"],
                    "rewrite_performed": checkpoint["rewritten"],
                    "rewrite_head_before": checkpoint["head_sha_before"],
                    "expected_remote_sha": checkpoint["remote_sha_before"],
                }),
            )
            .expect("republish the re-reviewed head");
            assert_eq!(pushed["local_sha"], rebased_head.as_str());
            assert_eq!(fx.remote_tip(BRANCH), rebased_head);

            fx.script_checks(&["success"]);
            let mut reviewed = fx.complete_input();
            reviewed["published_head_sha"] = json!(rebased_head);
            reviewed["reviewed_head_sha"] = json!(rebased_head);
            let completed =
                action(&host, "pr_complete", &reviewed).expect("the re-reviewed head completes");
            assert_eq!(completed["re_review_required"], false);
            assert_eq!(completed["merge"]["merged"], true);
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={rebased_head} merge_method=squash")],
                "exactly one merge, conditional on the re-reviewed head"
            );
            assert_eq!(host.landings().len(), 1);
            assert_eq!(host.landings()[0].reviewed_head_sha, rebased_head);
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);
        },
    );
}

// ---------------------------------------------------------------------------
// A base that advances after completion's conflict recovery [ORB-14393]
// ---------------------------------------------------------------------------

/// The completion step's id, which the dispatcher names in its input and
/// conflict recovery certifies under.
const COMPLETE_STEP: &str = "complete_pr";
/// The candidate's file, which a conflicting base advance also writes.
const FEATURE: &str = "src/feature.txt";

/// ORB-14344's shape: completion's rebase conflicts on base B, recovery
/// certifies the candidate on B, and the base advances to B' before the retry,
/// which re-pins to B'. A clean advance carries the recovered HEAD onto B',
/// certifies that result as the step's newest recovery, and lands it, where
/// the retry used to refuse the provenance and block. The provenance guard
/// still refuses a recovered HEAD whose checkpoint is not the one this
/// attempt prepared.
fn a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands() {
    isolated(
        "a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
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
            let error = action(&host, "git_rebase", &forged)
                .expect_err("a checkpoint for another published head is not this attempt's");
            assert!(
                error.to_string().contains("provenance does not match"),
                "{error}"
            );
            assert_eq!(fx.head(), recovered, "the refusal touches nothing");

            let completed = action(&host, "pr_complete", &input)
                .expect("the recovered head follows the advanced base and lands");
            let landed = fx.head();
            assert_ne!(landed, recovered);
            assert!(is_ancestor(&fx.repo, &advanced, &landed));
            assert_eq!(
                fs::read_to_string(fx.repo.join(FEATURE)).unwrap(),
                "resolved\n",
                "the certified resolution is carried, not redone"
            );
            assert_eq!(completed["merge"]["merged"], true);
            assert_eq!(
                fx.merge_requests(),
                vec![format!("sha={landed} merge_method=squash")]
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Done);

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
        },
    );
}

/// The same race where every advance also conflicts with the certified
/// resolution. Each one is redone from the published head onto the new base,
/// a stopped rebase the next bounded conflict recovery can be admitted on,
/// until the step has followed its base as often as allowed. The next advance
/// blocks as `base_chase_exhausted`, keeping the last certified recovery and
/// requesting no merge.
fn conflicting_base_advances_after_recovery_are_chased_until_the_bound() {
    isolated(
        "conflicting_base_advances_after_recovery_are_chased_until_the_bound",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
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
                assert_eq!(
                    rebase_state(&fx.repo, "orig-head"),
                    fx.candidate,
                    "the redo starts from the published head recovery is admitted on"
                );
                assert!(conflict.diagnostic.contains(&recovered), "{conflict:?}");
                chased += 1;
                recovered =
                    recover_completion(&fx, &host, &conflict, &format!("resolved {chased}\n"));
            };
            assert!(
                exhausted.to_string().contains("base_chase_exhausted:"),
                "{exhausted}"
            );
            assert!(chased >= 1, "at least one more bounded recovery ran");
            assert_eq!(fx.head(), recovered, "the last certified recovery is kept");
            assert_eq!(git(&fx.repo, &["status", "--porcelain"]), "");
            assert!(fx.merge_requests().is_empty());
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
        },
    );
}

/// [`Fixture::complete_ungated_input`] as the dispatcher hands it to the
/// completion step.
fn completion_with_step(fx: &Fixture) -> Value {
    let mut input = fx.complete_ungated_input();
    input["step_id"] = json!(COMPLETE_STEP);
    input
}

fn completion_conflict(outcome: Result<Value, OrbitError>) -> RecoverableVcsConflict {
    match outcome {
        Err(OrbitError::RecoverableVcsConflict(conflict)) => *conflict,
        other => panic!("expected a recoverable completion conflict, got {other:?}"),
    }
}

/// Resolve completion's stopped rebase with `resolution` and certify it the
/// way conflict recovery's host continuation does, returning the recovered
/// HEAD.
fn recover_completion(
    fx: &Fixture,
    host: &DeliveryHost,
    conflict: &RecoverableVcsConflict,
    resolution: &str,
) -> String {
    fs::write(fx.repo.join(FEATURE), resolution).unwrap();
    git(&fx.repo, &["add", FEATURE]);
    git(
        &fx.repo,
        &["-c", "core.editor=true", "rebase", "--continue"],
    );
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

/// Without `workflow.required_validation_commands` the step changes nothing:
/// no command runs, no evidence is attached, and even a checkout that would be
/// refused is passed through as today.
fn required_validation_without_commands_is_a_no_op() {
    isolated(
        "required_validation_without_commands_is_a_no_op",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            fs::write(fx.repo.join("untracked.txt"), "left behind\n").unwrap();

            let validated = action(&host, "candidate_validate", &fx.validate_input())
                .expect("no requirement is no gate");

            assert_eq!(validated["decision"], "skipped_no_required_commands");
            assert_eq!(validated["validation"], json!([]));
            assert!(host.artifacts.lock().unwrap().is_empty());
        },
    );
}

/// A claimed leaf on an owner that requires no command runs none and still
/// hands off: `claim_validate` pins the candidate and records the no-op, and
/// `claim_handoff` records a handoff with no validation logs whose summary
/// does not claim a check passed.
fn claimed_validation_without_commands_is_a_recorded_no_op() {
    isolated(
        "claimed_validation_without_commands_is_a_recorded_no_op",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });

            let validated =
                action(&host, "claim_validate", &input).expect("no requirement is no gate");

            assert_eq!(validated["decision"], "skipped_no_required_commands");
            assert!(validated["note"].is_string(), "{validated}");
            assert_eq!(validated["commands"], json!([]));
            assert_eq!(validated["validation"], json!([]));
            assert_eq!(validated["tested_head"], fx.candidate.as_str());
            assert!(host.claim_logs.lock().unwrap().is_empty());

            let mut handoff_input = input.clone();
            handoff_input["candidate"] = validated["candidate"].clone();
            handoff_input["validation"] = validated["validation"].clone();
            let handed = action(&host, "claim_handoff", &handoff_input)
                .expect("a handoff needs no logs the owner does not require");

            assert_eq!(handed["handed_off"], true);
            assert_eq!(handed["merged"], false);
            let handoffs = host.handoffs.lock().unwrap();
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
        },
    );
}

/// A leaf that ran the before-PR gate hands its review off as typed evidence
/// for the exact candidate, and a leaf without it reports `not_required`
/// rather than inventing a review. Evidence for another head is refused
/// before anything is recorded [ORB-13895].
fn claimed_handoff_carries_before_pr_evidence_only_for_its_candidate() {
    isolated(
        "claimed_handoff_carries_before_pr_evidence_only_for_its_candidate",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });
            let validated = action(&host, "claim_validate", &input).expect("validate");
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
            let refused = action(&host, "claim_handoff", &moved)
                .expect_err("review evidence for another head is refused");
            assert!(matches!(refused, OrbitError::PolicyDenied(_)), "{refused}");
            assert!(host.handoffs.lock().unwrap().is_empty());

            let mut reviewed = input.clone();
            reviewed["review_evidence"] = evidence(&fx.candidate);
            action(&host, "claim_handoff", &reviewed).expect("reviewed handoff");
            action(&host, "claim_handoff", &input).expect("unreviewed handoff");

            let handoffs = host.handoffs.lock().unwrap();
            assert_eq!(handoffs[0].review.policy, ReviewTiming::BeforePr);
            let carried = handoffs[0].review.evidence().expect("before-PR evidence");
            assert_eq!(carried.reviewed_head_sha, fx.candidate);
            assert_eq!(carried.verdict, ReviewVerdict::AcceptWithFixes);
            assert_eq!(handoffs[1].review.policy, ReviewTiming::None);
            assert_eq!(
                handoffs[1].review.disposition,
                HandoffReviewDisposition::NotRequired
            );
        },
    );
}

/// The same leaf on an owner that does require a command still refuses a
/// handoff that carries none of its logs.
fn claimed_handoff_without_logs_is_refused_when_commands_are_required() {
    isolated(
        "claimed_handoff_without_logs_is_refused_when_commands_are_required",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&["true"]);
            let mut input = json!({
                "workspace_path": fx.repo,
                "base_sync": "local",
                "base_sha": fx.base_sha,
            });
            let validated = action(&host, "claim_validate", &input).expect("validate");
            assert_eq!(validated["decision"], "passed");
            assert_eq!(host.claim_logs.lock().unwrap().len(), 1);

            input["candidate"] = validated["candidate"].clone();
            input["validation"] = json!([]);
            let refused = action(&host, "claim_handoff", &input)
                .expect_err("required validation must travel with the handoff");
            assert!(matches!(refused, OrbitError::PolicyDenied(_)), "{refused}");
            assert!(host.handoffs.lock().unwrap().is_empty());
        },
    );
}

/// A completion-stage failure — here the re-review of a rebased head — closes
/// every review attempt the run admitted, so none stays open, and keeps the
/// published PR and the task in review. A bundle's handoff, which refuses to
/// reconcile more than one task, still closes its attempts first.
fn a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle() {
    isolated(
        "a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
            host.publish_pr(TASK_ID);
            let admission = |attempt: &str| {
                json!({
                    "applies": true,
                    "lineage_key": "ws/T-LANDING/agent-main/jrun-landing",
                    "attempt_id": attempt,
                })
            };

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "re_review",
                    "error_code": "pipeline_step_failed",
                    "error_message": "reviewer exceeded its wall clock",
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID]},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                        "review_gate_admit": admission("rvw-first-1"),
                        "re_review_gate_admit": admission("rvw-first-2"),
                    },
                }),
            )
            .expect("hand off the completion failure");

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
            assert_eq!(
                fx.forge_state("pr-head"),
                None,
                "no PR is opened or changed"
            );

            action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "review_gate",
                    "error_code": "pipeline_step_failed",
                    "error_message": "reviewer exceeded its wall clock",
                    "run_id": "jrun-bundle",
                    "job_input": {"task_ids": [TASK_ID, "T-BUNDLED"]},
                    "pipeline": {"review_gate_admit": admission("rvw-bundle-1")},
                }),
            )
            .expect_err("the handoff reconciles one task only");
            assert_eq!(
                host.releases()
                    .last()
                    .map(|request| request.attempt_id.as_str()),
                Some("rvw-bundle-1"),
                "the bundle's failed reviewer step still closed its attempt"
            );
        },
    );
}

/// [ORB-13990] Owner revalidation of a reviewer commit accepts a path no
/// delivered task owns, widening the task's selectors with review provenance
/// instead of refusing; a path the selectors already own widens nothing.
#[test]
fn review_revalidation_widens_selectors_for_a_reviewer_change_outside_task_ownership() {
    isolated(
        "review_revalidation_widens_selectors_for_a_reviewer_change_outside_task_ownership",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.set_context_files(TASK_ID, &["file:src/feature.txt"]);
            let fixed_head = fx.commit("docs/fix.md", "reviewer fix\n");
            let mut input = fx.validate_input();
            input["ownership_base_sha"] = json!(fx.candidate);

            let validated = action(&host, "candidate_validate", &input)
                .expect("a reviewer path outside the selectors validates");
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

            action(&host, "candidate_validate", &input).expect("owned paths validate");
            assert_eq!(host.widenings().len(), 1, "owned paths widen nothing");
        },
    );
}

/// [ORB-13989] A reviewer commit that fails owner revalidation rejects the
/// candidate. The handoff keeps the implementation commit and the reviewer
/// commit exactly as they are, pushes them so they are recoverable, blocks the
/// task, and opens no PR.
fn a_failed_review_revalidation_rejects_and_preserves_both_commits() {
    isolated(
        "a_failed_review_revalidation_rejects_and_preserves_both_commits",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let fixed_head = fx.commit("src/feature.txt", "reviewer fix\n");

            let handoff = action(
                &host,
                "pr_failure_handoff",
                &json!({
                    "failed_step_id": "review_validate",
                    "error_code": "pipeline_step_failed",
                    "error_message": "required validation 'make test' did not pass",
                    "run_id": RUN_ID,
                    "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                    "pipeline": {
                        "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                        "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE},
                        "review_gate_admit": {
                            "applies": true,
                            "lineage_key": "ws/T-LANDING/agent-main/jrun-landing",
                            "attempt_id": "rvw-fixes-1",
                            "reviewer": {"provider": "codex", "model": "review-model"},
                        },
                        "review_gate_settle": {
                            "gate": "passed",
                            "verdict": "accept_with_fixes",
                            "reviewed_head_sha": fixed_head,
                            "implementation_head_sha": fx.candidate,
                            "reviewer_fixed": true,
                        },
                    },
                }),
            )
            .expect("hand off the rejected candidate");

            assert_eq!(handoff["decision"], "blocked_review_gate", "{handoff}");
            assert_eq!(handoff["partial_repair_commit"], Value::Null, "{handoff}");
            assert_eq!(fx.head(), fixed_head, "nothing is amended or added");
            assert_eq!(git(&fx.repo, &["rev-parse", "HEAD^"]), fx.candidate);
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
        },
    );
}

fn a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking() {
    isolated(
        "a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let handoff = action(&host, "pr_failure_handoff", &json!({
            "failed_step_id": "review", "error_code": ReviewGateRefusal::TimeoutIncomplete.error_code(),
            "error_message": "review_timeout_incomplete: reviewer exceeded its wall clock",
            "run_id": RUN_ID, "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
            "pipeline": {
                "worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE},
                "review_gate_admit": {"applies": true, "attempt_id": "rvw-partial", "lineage_key": "lineage",
                    "reviewer": {"provider": "codex", "model": "review-model"}},
            },
        })).unwrap();
            assert_eq!(handoff["decision"], "incomplete_review_timeout");
            assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
            assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
            assert_eq!(fx.forge_state("pr-head"), None);
        },
    );
}

fn an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks() {
    isolated(
        "an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let hold = json!({
                "schema_version": 1, "attempt_id": "rvw-held", "lineage_key": "lineage", "run_id": RUN_ID,
                "candidate": {"commit": fx.candidate, "tree": git(&fx.repo, &["rev-parse", "HEAD^{tree}"])},
                "task_meaning_digest": "meaning",
                "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
            });
            host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&hold).unwrap(),
            );
            host.artifact_creators.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                "system".into(),
            );
            // Settlement never reaches the handoff with a hold: it ends the run
            // held. The hold reaches the handoff only as an admission refusal.
            let mut input = json!({
                "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(),
                "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID,
                "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}},
            });
            let held = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(held["decision"], "awaiting_review_evidence");
            assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
            assert_eq!(fx.forge_state("pr-head"), None);
            input["failed_step_id"] = json!("review_gate_settle");
            input["error_code"] = json!(ReviewGateRefusal::Blocked.error_code());
            input["error_message"] = json!("review_gate_blocked: changes_required; wrong approach");
            let rejected = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(
                rejected["decision"], "blocked_review_gate",
                "a substantive settlement rejection blocks even while a hold exists"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);

            let forged_fx = Fixture::new(sandbox);
            let forged_host = DeliveryHost::new(&forged_fx.repo, TaskStatus::InProgress);
            let forged_hold = json!({
                "schema_version": 1, "attempt_id": "rvw-forged", "lineage_key": "lineage", "run_id": RUN_ID,
                "candidate": {"commit": forged_fx.candidate, "tree": git(&forged_fx.repo, &["rev-parse", "HEAD^{tree}"])},
                "task_meaning_digest": "meaning",
                "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
            });
            forged_host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&forged_hold).unwrap(),
            );
            let forged = action(&forged_host, "pr_failure_handoff", &json!({
                "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(),
                "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID,
                "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": forged_fx.repo},
                    "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}},
            })).unwrap();
            assert_eq!(
                forged["decision"], "blocked_review_gate",
                "a hold the system did not write is not evidence"
            );
            assert_eq!(forged_host.status(TASK_ID), TaskStatus::Blocked);
        },
    );
}

/// ORB-14651: a requeued evidence-held task is admitted again by a later run.
/// Admission refuses for the still-missing evidence before it reserves an
/// attempt, so the handoff must keep the hold rather than escalate.
fn an_admission_refusal_for_held_evidence_stays_held_not_escalated() {
    isolated(
        "an_admission_refusal_for_held_evidence_stays_held_not_escalated",
        |sandbox| {
            const RERUN_ID: &str = "jrun-landing-rerun";
            let fx = Fixture::new(sandbox);
            // The operator requeue leaves the task in backlog; the rerun's
            // admission moves it back to in-progress and claims it for the
            // rerun before the handoff.
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.tasks
                .lock()
                .unwrap()
                .get_mut(TASK_ID)
                .unwrap()
                .job_run_id = Some(RERUN_ID.to_string());
            let hold = json!({
                "schema_version": 1, "attempt_id": "rvw-held", "lineage_key": "lineage", "run_id": RUN_ID,
                "candidate": {"commit": fx.candidate, "tree": git(&fx.repo, &["rev-parse", "HEAD^{tree}"])},
                "task_meaning_digest": "meaning",
                "requirements": [{"kind": "native_os", "name": "macOS native", "command": "native run", "artifact": "macos.json"}],
            });
            host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&hold).unwrap(),
            );
            host.artifact_creators.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                "system".into(),
            );
            // Admission refused before reserving an attempt, so no
            // `review_gate_admit` output reaches the handoff.
            let mut input = json!({
                "failed_step_id": "review_gate_admit", "error_code": ReviewGateRefusal::AwaitingEvidence.error_code(),
                "error_message": "review_awaiting_evidence: named external checks have not arrived for the held candidate",
                "run_id": RERUN_ID,
                "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                "pipeline": {"worktree": {"job_run_id": RERUN_ID, "workspace_path": fx.repo},
                    "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE}},
            });
            let held = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(held["decision"], "awaiting_review_evidence");
            assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
            assert_eq!(fx.forge_state("pr-head"), None);

            let mut other_candidate = hold.clone();
            other_candidate["candidate"]["commit"] =
                json!("0000000000000000000000000000000000000000");
            host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&other_candidate).unwrap(),
            );
            let stale = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(
                stale["decision"], "blocked_review_gate",
                "a hold for another candidate does not cover this admission refusal"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);

            host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&hold).unwrap(),
            );
            host.set_status(TASK_ID, TaskStatus::InProgress);
            input["error_code"] = json!("pipeline_step_failed");
            let untyped = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(
                untyped["decision"], "blocked_review_gate",
                "ORB-15714: the handoff decides on the typed code, not on message text"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);

            host.set_status(TASK_ID, TaskStatus::InProgress);
            input["error_message"] = json!("review_gate_stale: completion rebased a reviewed head");
            let substantive = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(substantive["decision"], "blocked_review_gate");
            assert_eq!(host.status(TASK_ID), TaskStatus::Blocked);
        },
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Both the fixture guard and Git itself refuse HTTPS before opening a socket,
/// including the engine's VCS child that clears GIT_ALLOW_PROTOCOL.
fn fixture_push_refuses_non_file_transports_without_network() {
    isolated(
        "fixture_push_refuses_non_file_transports_without_network",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("https://{}/remote.git", listener.local_addr().unwrap());
            git(&fx.repo, &["remote", "set-url", "origin", &url]);
            let refused = std::panic::catch_unwind(|| git(&fx.repo, &["push", "origin", BRANCH]));
            let refusal = refused
                .expect_err("fixture helper must refuse a non-local push")
                .downcast::<String>()
                .expect("fixture guard reports the refused origin URL");
            assert!(
                refusal.starts_with("refusing fixture push to non-local origin"),
                "the fixture guard must refuse before invoking Git: {refusal}"
            );

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
                    String::from_utf8_lossy(&output.stderr)
                        .contains("transport 'https' not allowed"),
                    "Git must refuse before resolving or connecting: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let error = execute_deterministic_action(
                &host,
                "git_push",
                &json!({}),
                &json!({"workspace_path": fx.repo, "branch": BRANCH}),
                false,
                &HashMap::new(),
                None,
            )
            .expect_err("the production adapter must inherit the HOME transport policy");
            assert!(
                error.to_string().contains("transport 'https' not allowed"),
                "{error}"
            );
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "refused HTTPS pushes must never attempt a network connection"
            );
        },
    );
}

/// Check effective push URLs, including a second pushurl and rewrite rules,
/// rather than trusting a local fetch URL.
fn fixture_push_guard_checks_all_effective_push_urls() {
    isolated(
        "fixture_push_guard_checks_all_effective_push_urls",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let remote = fx.forge.join("remote.git");
            for case in ["second_pushurl", "rewrite"] {
                if case == "second_pushurl" {
                    git(
                        &fx.repo,
                        &[
                            "config",
                            "--add",
                            "remote.origin.pushurl",
                            path_str(&remote),
                        ],
                    );
                    git(
                        &fx.repo,
                        &[
                            "config",
                            "--add",
                            "remote.origin.pushurl",
                            "https://fixture.invalid/remote.git",
                        ],
                    );
                } else {
                    git(
                        &fx.repo,
                        &["config", "--unset-all", "remote.origin.pushurl"],
                    );
                    git(
                        &fx.repo,
                        &[
                            "config",
                            "url.https://fixture.invalid/.pushInsteadOf",
                            path_str(&remote),
                        ],
                    );
                }
                let refused =
                    std::panic::catch_unwind(|| git(&fx.repo, &["push", "origin", BRANCH]));
                let refusal = refused
                    .expect_err("effective non-local push URL must be refused")
                    .downcast::<String>()
                    .expect("fixture guard reports the refused origin URL");
                assert!(
                    refusal.starts_with("refusing fixture push to non-local origin"),
                    "{case} must refuse before starting git push: {refusal}"
                );
                assert_eq!(fx.remote_tip(BRANCH), fx.candidate);
            }
        },
    );
}

/// ORB-14717: a silent origin must not turn reviewed work into a candidate
/// failure. Only a cached ref sharing ancestry can complete the handoff.
fn claimed_handoff_fetch_timeout_uses_only_a_related_cached_base() {
    isolated(
        "claimed_handoff_fetch_timeout_uses_only_a_related_cached_base",
        |sandbox| {
            for cache in ["related", "missing", "unrelated"] {
                let fx = Fixture::new(sandbox);
                // A local upload-pack stalls without permitting any network
                // transport from this test process.
                let silent = fx._root.path().join("silent-upload-pack");
                fs::write(&silent, "#!/bin/sh\nexec sleep 30\n").unwrap();
                fs::set_permissions(&silent, fs::Permissions::from_mode(0o755)).unwrap();
                git(
                    &fx.repo,
                    &["config", "remote.origin.uploadpack", path_str(&silent)],
                );
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                let mut input = json!({
                    "workspace_path": fx.repo,
                    "base_sync": "local",
                    "base_sha": fx.base_sha,
                });
                let validated = action(&host, "claim_validate", &input).unwrap();
                input["candidate"] = validated["candidate"].clone();
                input["validation"] = validated["validation"].clone();
                input["base_sync"] = json!("remote");
                input["git_timeouts"] = json!({"fetch": 1});
                let cached = format!("refs/remotes/origin/{BASE}");
                match cache {
                    "missing" => {
                        git(&fx.repo, &["update-ref", "-d", &cached]);
                    }
                    "unrelated" => {
                        let tree = git(&fx.repo, &["rev-parse", "HEAD^{tree}"]);
                        let orphan = git(&fx.repo, &["commit-tree", &tree, "-m", "unrelated"]);
                        git(&fx.repo, &["update-ref", &cached, &orphan]);
                    }
                    _ => {}
                }
                let handoff = action(&host, "claim_handoff", &input);
                if cache == "related" {
                    let handed = handoff.expect("cached ancestry preserves the validated handoff");
                    assert_eq!(handed["handed_off"], true);
                    assert_eq!(handed["base"], fx.base_sha);
                    assert_eq!(handed["candidate"], fx.candidate);
                    assert_eq!(host.handoffs.lock().unwrap().len(), 1);
                } else {
                    let error = handoff
                        .expect_err("an unusable cache preserves the fetch failure")
                        .to_string();
                    assert!(error.contains("timed out after 1ms"), "{cache}: {error}");
                    assert_eq!(
                        ClaimFailureClass::of_step_failure(None, Some(&error)),
                        Some(ClaimFailureClass::Transient),
                        "{cache}: {error}"
                    );
                    assert!(host.handoffs.lock().unwrap().is_empty());
                }
                assert_eq!(fx.head(), fx.candidate, "the candidate stays intact");
            }
        },
    );
}

/// ORB-14717: a deleted remote base must refuse even with a usable cached
/// ref. It says something different from an unavailable network.
#[test]
fn claimed_handoff_missing_remote_ref_never_uses_the_cached_base() {
    isolated(
        "claimed_handoff_missing_remote_ref_never_uses_the_cached_base",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let mut input = json!({"workspace_path": fx.repo, "base_sync": "local"});
            let validated = action(&host, "claim_validate", &input).unwrap();
            input["candidate"] = validated["candidate"].clone();
            input["validation"] = validated["validation"].clone();
            input["base_sync"] = json!("remote");
            git(
                &fx.forge.join("remote.git"),
                &["update-ref", "-d", &format!("refs/heads/{BASE}")],
            );
            let error = action(&host, "claim_handoff", &input)
                .expect_err("a missing remote base is a refusal")
                .to_string();
            assert!(error.contains("couldn't find remote ref"), "{error}");
            assert_eq!(
                ClaimFailureClass::of_step_failure(None, Some(&error)),
                None,
                "{error}"
            );
            assert!(host.handoffs.lock().unwrap().is_empty());
        },
    );
}

/// Drive real Git's local upload-pack: temporary connection failures retry and
/// can recover; access refusals stay untyped and never retry or use the cache.
fn remote_base_transport_retries_preserve_access_refusals() {
    isolated(
        "remote_base_transport_retries_preserve_access_refusals",
        |sandbox| {
            for (diagnostic, recovery, attempts, transient) in [
                ("Connection reset by peer", true, 2, false),
                ("Could not resolve hostname fixture.invalid", false, 3, true),
                ("Connection reset by peer", false, 3, true),
                (
                    "Permission denied (publickey). Connection closed",
                    false,
                    1,
                    false,
                ),
                (
                    "Authentication failed. Connection reset by peer",
                    false,
                    1,
                    false,
                ),
            ] {
                let fx = Fixture::new(sandbox);
                let count = fx._root.path().join("fetch-attempts");
                let upload_pack = fx._root.path().join("upload-pack");
                fs::write(&count, "0").unwrap();
                fs::write(&upload_pack, format!(
                    "#!/bin/sh\nn=$(cat '{}')\nn=$((n + 1))\necho $n > '{}'\nif [ '{}' = true ] && [ $n -gt 1 ]; then exec git-upload-pack '{}'; fi\necho '{}' >&2\nexit 1\n",
                    count.display(), count.display(), recovery, fx.forge.join("remote.git").display(), diagnostic,
                )).unwrap();
                fs::set_permissions(&upload_pack, fs::Permissions::from_mode(0o755)).unwrap();
                git(
                    &fx.repo,
                    &["config", "remote.origin.uploadpack", path_str(&upload_pack)],
                );
                let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
                if !transient && !recovery {
                    let mut input = json!({"workspace_path": fx.repo, "base_sync": "local"});
                    let validated = action(&host, "claim_validate", &input).unwrap();
                    input["candidate"] = validated["candidate"].clone();
                    input["validation"] = validated["validation"].clone();
                    input["base_sync"] = json!("remote");
                    let error = action(&host, "claim_handoff", &input)
                        .expect_err("access refusal")
                        .to_string();
                    assert!(error.contains(diagnostic), "{error}");
                    assert_eq!(
                        ClaimFailureClass::of_step_failure(None, Some(&error)),
                        None,
                        "{error}"
                    );
                    assert!(host.handoffs.lock().unwrap().is_empty());
                } else {
                    let fetched = orbit_engine::fetch_remote_base(&fx.repo, BASE);
                    if recovery {
                        fetched.expect("the second transport attempt recovers");
                    } else {
                        let error = fetched
                            .expect_err("transport stays unavailable")
                            .to_string();
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
        },
    );
}

/// A checkout on [`BRANCH`] one commit ahead of [`BASE`], both pushed to a
/// bare remote that the substitute forge reads and merges into.
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
            let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
            git_fixture::copy_template(seed._root.path(), root.path());
            let repo = root.path().join("repo").canonicalize().unwrap();
            let forge = root.path().join("forge");
            fs::write(sandbox.join("active-forge"), path_str(&forge)).unwrap();
            Self {
                _root: root,
                repo,
                forge,
                base_sha: seed.base_sha.clone(),
                candidate: seed.candidate.clone(),
            }
        })
    }

    fn fresh(sandbox: &Path) -> Self {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let forge = root.path().join("forge");
        let remote = forge.join("remote.git");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(root.path(), &["init", "--bare", path_str(&remote)]);
        git_fixture::init(&repo);
        git(&repo, &["remote", "add", "origin", path_str(&remote)]);
        git(&repo, &["checkout", "-b", BASE]);
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(
            &repo,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["push", "-u", "origin", BASE]);
        git(&repo, &["checkout", "-b", BRANCH]);
        let repo = repo.canonicalize().unwrap();
        let fixture = Self {
            _root: root,
            base_sha: git(&repo, &["rev-parse", BASE]),
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

    /// [`Fixture::new`] whose base carries a `Makefile` with a red `ci-lint`
    /// target, and whose candidate is rebased onto it. Every run of the target
    /// appends a line to `runs`.
    fn with_red_lint(sandbox: &Path, runs: &Path) -> Self {
        let fixture = Self::new(sandbox);
        fixture.commit_on_base(
            "Makefile",
            &format!(
                "ci-lint:\n\t@echo ran >> '{}'\n\t@printf 'error: lint is red\\n  --> README.md:1:1\\n' >&2; exit 2\n",
                runs.display()
            ),
        );
        git(&fixture.repo, &["rebase", BASE]);
        git(&fixture.repo, &["push", "--force", "origin", BRANCH]);
        Self {
            base_sha: git(&fixture.repo, &["rev-parse", BASE]),
            candidate: fixture.head(),
            ..fixture
        }
    }

    /// [`Fixture::new`] whose base fails [`RED_TESTS`] with nextest's `FAIL`
    /// line for each of `base_tests` (and libtest's echo of the first), with
    /// the candidate rebased onto it. A candidate given `candidate_tests`
    /// commits its own list on top; otherwise it fails exactly as the base.
    fn with_red_tests(
        sandbox: &Path,
        base_tests: &[&str],
        candidate_tests: Option<&[&str]>,
    ) -> Self {
        let fixture = Self::new(sandbox);
        let mut base_output = base_tests
            .first()
            .map(|test| format!("test {test} ... FAILED\n"))
            .unwrap_or_default();
        base_output.push_str(&nextest_failures(base_tests));
        fixture.commit_files_on_base(&[
            ("failing.txt", &base_output),
            ("Makefile", "ci-test:\n\t@cat failing.txt\n\t@exit 100\n"),
        ]);
        git(&fixture.repo, &["rebase", BASE]);
        git(&fixture.repo, &["push", "--force", "origin", BRANCH]);
        let candidate = match candidate_tests {
            Some(tests) => fixture.commit("failing.txt", &nextest_failures(tests)),
            None => fixture.head(),
        };
        Self {
            base_sha: git(&fixture.repo, &["rev-parse", BASE]),
            candidate,
            ..fixture
        }
    }

    /// Commit `file` on the base and push it, leaving the candidate checked out.
    fn commit_on_base(&self, file: &str, contents: &str) {
        self.commit_files_on_base(&[(file, contents)]);
    }

    /// Seed one final base tree when the fixture observes no intermediate write.
    /// Separate base advances still use `commit_on_base` at each observation.
    fn commit_files_on_base(&self, files: &[(&str, &str)]) {
        git(&self.repo, &["checkout", BASE]);
        for (file, contents) in files {
            let path = self.repo.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let mut add = vec!["add", "--"];
        add.extend(files.iter().map(|(file, _)| *file));
        git(&self.repo, &add);
        let names = files
            .iter()
            .map(|(file, _)| *file)
            .collect::<Vec<_>>()
            .join(", ");
        git(
            &self.repo,
            &["commit", "-m", &format!("base writes {names}")],
        );
        git(&self.repo, &["push", "origin", BASE]);
        git(&self.repo, &["checkout", BRANCH]);
    }

    /// Check states the forge reports, one per status read; the last repeats.
    fn script_checks(&self, states: &[&str]) {
        fs::write(
            self.forge.join("checks"),
            format!("{}\n", states.join("\n")),
        )
        .unwrap();
    }

    /// Answers to successive merge requests (see `merge-outcomes` in
    /// [`FAKE_GH`]); requests past the script merge normally.
    fn script_merges(&self, outcomes: &[&str]) {
        fs::write(
            self.forge.join("merge-outcomes"),
            format!("{}\n", outcomes.join("\n")),
        )
        .unwrap();
    }

    /// Answers to successive creates; the final outcome repeats.
    fn script_creates(&self, outcomes: &[&str]) {
        fs::write(
            self.forge.join("create-outcomes"),
            format!("{}\n", outcomes.join("\n")),
        )
        .unwrap();
    }

    /// Script PR metadata independently of the real remote PR head ref.
    fn script_heads(&self, heads: &[&str], remote_head: Option<&str>) {
        fs::write(self.forge.join("heads"), format!("{}\n", heads.join("\n"))).unwrap();
        fs::write(
            self.forge.join("heads-start"),
            self.forge_state("reads").unwrap_or_else(|| "0".into()),
        )
        .unwrap();
        let remote = self.forge.join("remote.git");
        match remote_head {
            Some(head) => {
                git(
                    &self.repo,
                    &[
                        "--git-dir",
                        path_str(&remote),
                        "update-ref",
                        "refs/pull/42/head",
                        head,
                    ],
                );
            }
            None => {
                git(
                    &self.repo,
                    &[
                        "--git-dir",
                        path_str(&remote),
                        "update-ref",
                        "-d",
                        "refs/pull/42/head",
                    ],
                );
            }
        }
    }

    /// Replace the opened head through the production lease-push boundary and
    /// hand its checkpoints to completion as the shipped pipeline does.
    fn republished_completion_input(&self, host: &DeliveryHost) -> Value {
        git(
            &self.repo,
            &["commit", "--amend", "-m", "republished candidate"],
        );
        let pushed = action(
            host,
            "git_push",
            &json!({
                "workspace_path": self.repo,
                "job_run_id": RUN_ID,
                "completed_task_ids": [TASK_ID],
                "branch": BRANCH,
                "rewrite_performed": true,
                "rewrite_head_before": self.candidate,
                "expected_remote_sha": self.candidate,
            }),
        )
        .unwrap();
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
        git(&self.repo, &["add", file]);
        git(&self.repo, &["commit", "-m", &format!("write {file}")]);
        git(&self.repo, &["push", "-u", "origin", BRANCH]);
        self.head()
    }

    /// Land unrelated work on the base, leaving the candidate behind it.
    fn advance_base(&self) {
        git(&self.repo, &["checkout", BASE]);
        fs::write(self.repo.join("base.txt"), "advanced\n").unwrap();
        git(&self.repo, &["add", "base.txt"]);
        git(&self.repo, &["commit", "-m", "advance base"]);
        git(&self.repo, &["push", "origin", BASE]);
        git(&self.repo, &["checkout", BRANCH]);
    }

    /// Land unrelated work on the base, then rebase and republish the
    /// candidate onto it.
    fn advance_base_and_rebase(&self) {
        git(&self.repo, &["checkout", BASE]);
        fs::write(self.repo.join("base.txt"), "advanced\n").unwrap();
        git(&self.repo, &["add", "base.txt"]);
        git(&self.repo, &["commit", "-m", "advance base"]);
        git(&self.repo, &["push", "origin", BASE]);
        git(&self.repo, &["checkout", BRANCH]);
        git(&self.repo, &["rebase", BASE]);
        git(&self.repo, &["push", "--force", "origin", BRANCH]);
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn remote_parent(&self, commit: &str) -> String {
        git(
            &self.forge,
            &[
                "--git-dir",
                "remote.git",
                "rev-parse",
                &format!("{commit}^"),
            ],
        )
    }

    fn local_tip(&self, branch: &str) -> String {
        git(&self.repo, &["rev-parse", branch])
    }

    /// [ORB-14905] The commit a held candidate's durable ref names on the
    /// remote, after checking the handoff says it carried it there.
    fn durable_candidate(&self, handoff: &Value) -> String {
        assert_eq!(handoff["carry"], "durable", "{handoff}");
        let reference = handoff["durable_ref"].as_str().expect("durable ref");
        assert_eq!(
            reference,
            format!("refs/orbit/candidates/{TASK_ID}/{RUN_ID}"),
            "{handoff}"
        );
        git(
            &self.forge,
            &["--git-dir", "remote.git", "rev-parse", reference],
        )
    }

    fn remote_tip(&self, branch: &str) -> String {
        git(
            &self.forge,
            &[
                "--git-dir",
                "remote.git",
                "rev-parse",
                &format!("refs/heads/{branch}"),
            ],
        )
    }

    /// The validation step as the owner pipeline hands it the synchronized
    /// candidate.
    fn validate_input(&self) -> Value {
        json!({
            "workspace_path": self.repo,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "base_sha": self.base_sha,
        })
    }

    fn open_input(&self, reviewed_head: &str, reviewed_base: &str) -> Value {
        json!({
            "workspace_path": self.repo,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "head": BRANCH,
            "base": BASE,
            "base_ref": BASE,
            "base_sha": self.base_sha,
            "base_sync": "local",
            "reviewed_head_sha": reviewed_head,
            "reviewed_base_sha": reviewed_base,
        })
    }

    /// The completion step as a `--complete` pipeline hands it the run's
    /// published and reviewed candidate.
    fn complete_input(&self) -> Value {
        json!({
            "workspace_path": self.repo,
            "job_run_id": RUN_ID,
            "completed_task_ids": [TASK_ID],
            "completion": "done",
            "pr_number": PR_NUMBER,
            "head": BRANCH,
            "base": BASE,
            "base_sync": "local",
            "published_head_sha": self.candidate,
            "reviewed_head_sha": self.candidate,
            "max_wait_seconds": 30,
            "poll_interval_seconds": 5,
        })
    }

    /// The same completion for an ungated run: the published head is still
    /// pinned, but no before-PR review settled it.
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

    /// Every `gh` call, as its first two arguments.
    fn forge_calls(&self) -> Vec<String> {
        self.forge_lines("calls")
    }

    /// Every merge mutation the forge received, as its `sha=` and
    /// `merge_method=` fields.
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

fn action(host: &DeliveryHost, name: &str, input: &Value) -> Result<Value, OrbitError> {
    if matches!(name, "git_push" | "pr_complete" | "pr_failure_handoff") {
        git_fixture::assert_local_push_remote(&host.repo);
    }
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}

fn task(status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
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
        created_at: now,
        updated_at: now,
    }
}

/// The task store and review-landing record the delivery actions write
/// through, kept in memory. The provider boundary is left to the engine.
struct DeliveryHost {
    repo: PathBuf,
    runs: Mutex<BTreeMap<String, JobRun>>,
    tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    completion_notes: Mutex<Vec<String>>,
    landings: Mutex<Vec<ReviewLandingRequest>>,
    required_commands: Mutex<Vec<String>>,
    /// `review.baseline_commands`, as the owner's configuration names them.
    baseline_commands: Mutex<Vec<String>>,
    /// Outputs of the actions a job graph scripts, by action; every other
    /// action is the engine's own.
    scripted: Mutex<BTreeMap<String, Value>>,
    /// Attached task artifacts, by task id and path.
    artifacts: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    /// Trusted system artifacts, by task id and path.
    artifact_creators: Mutex<BTreeMap<(String, String), String>>,
    releases: Mutex<Vec<ReviewReleaseRequest>>,
    /// The resolved validation environment, standing in for the owner's
    /// resolver; `None` keeps the trait default.
    validation_env: Mutex<Option<ValidationEnvironment>>,
    /// Selector widenings requested, as (task, step, activity, paths).
    widenings: Mutex<Vec<Widening>>,
    /// Claimed-leaf validation logs attached to the owner, by path.
    claim_logs: Mutex<Vec<String>>,
    /// Their contents, by path.
    claim_log_contents: Mutex<BTreeMap<String, Vec<u8>>>,
    /// Claimed-leaf handoffs recorded as pending settlements.
    handoffs: Mutex<Vec<TaskHandoff>>,
    /// The claim's owner-resolved ship mode.
    ship_mode: Mutex<String>,
    /// Status history written by automation updates, as (task, event, note).
    status_events: Mutex<Vec<StatusEvent>>,
    /// History entries automation updates append, by task.
    history: Mutex<BTreeMap<String, Vec<TaskHistoryEntry>>>,
    /// Rebase recovery attempts reserved, as (run, step, scope), oldest first.
    recovery_attempts: Mutex<Vec<(String, String, RebaseRecoveryAttemptScope)>>,
    /// Certified rebase recoveries, as (run, step, checkpoint), oldest first.
    recovery_certificates: Mutex<Vec<(String, String, Value)>>,
    /// Run-store copies of the certified recoveries, by run.
    run_states: Mutex<BTreeMap<String, PipelineState>>,
    /// Inputs the scripted actions last received, by action.
    scripted_inputs: Mutex<BTreeMap<String, Value>>,
    /// What the implementer's lane answers a deferral probe with, by the
    /// probe's command; a probe with no scripted answer gets its sandbox's
    /// refusal.
    deferral_answers: Mutex<BTreeMap<String, NamespaceProbe>>,
    /// Deferral probes run, as (probe, provider, checkout).
    probes: Mutex<Vec<(String, Option<String>, PathBuf)>>,
    /// The OS this host runs implementers on [ORB-15587].
    os: Mutex<Option<EvidenceHostOs>>,
}

type Widening = (String, ContextWideningStep, String, Vec<String>);
/// A status history write, as (task, event, note).
type StatusEvent = (String, Option<String>, Option<String>);

impl DeliveryHost {
    fn new(repo: &Path, status: TaskStatus) -> Self {
        Self {
            repo: repo.to_path_buf(),
            runs: Mutex::default(),
            tasks: Mutex::new(BTreeMap::from([(TASK_ID.to_string(), task(status))])),
            comments: Mutex::default(),
            completion_notes: Mutex::default(),
            landings: Mutex::default(),
            required_commands: Mutex::default(),
            baseline_commands: Mutex::default(),
            scripted: Mutex::default(),
            artifacts: Mutex::default(),
            artifact_creators: Mutex::default(),
            releases: Mutex::default(),
            validation_env: Mutex::default(),
            widenings: Mutex::default(),
            claim_logs: Mutex::default(),
            claim_log_contents: Mutex::default(),
            handoffs: Mutex::default(),
            ship_mode: Mutex::new("local".to_string()),
            status_events: Mutex::default(),
            history: Mutex::default(),
            recovery_attempts: Mutex::default(),
            recovery_certificates: Mutex::default(),
            run_states: Mutex::default(),
            scripted_inputs: Mutex::default(),
            deferral_answers: Mutex::default(),
            probes: Mutex::default(),
            os: Mutex::new(EvidenceHostOs::current()),
        }
    }

    fn resolve_validation_env(&self, environment: ValidationEnvironment) {
        *self.validation_env.lock().unwrap() = Some(environment);
    }

    fn require_commands(&self, commands: &[&str]) {
        *self.required_commands.lock().unwrap() =
            commands.iter().map(ToString::to_string).collect();
    }

    fn baseline_commands(&self, commands: &[&str]) {
        *self.baseline_commands.lock().unwrap() =
            commands.iter().map(ToString::to_string).collect();
    }

    fn script(&self, action: &str, output: Value) {
        self.scripted
            .lock()
            .unwrap()
            .insert(action.to_string(), output);
    }

    fn scripted_input(&self, action: &str) -> Value {
        self.scripted_inputs
            .lock()
            .unwrap()
            .get(action)
            .cloned()
            .unwrap_or_else(|| panic!("`{action}` ran"))
    }

    fn has_artifact(&self, task_id: &str, path: &str) -> bool {
        self.artifacts
            .lock()
            .unwrap()
            .contains_key(&(task_id.to_string(), path.to_string()))
    }

    fn validation_log(&self, task_id: &str, path: &str) -> Value {
        let artifacts = self.artifacts.lock().unwrap();
        let content = artifacts
            .get(&(task_id.to_string(), path.to_string()))
            .unwrap_or_else(|| panic!("{task_id} has no artifact {path}"));
        serde_json::from_slice(content).unwrap()
    }

    /// Record the task's published PR, as `pr_promote` does.
    fn publish_pr(&self, id: &str) {
        self.tasks
            .lock()
            .unwrap()
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
        self.releases.lock().unwrap().clone()
    }

    fn set_context_files(&self, id: &str, selectors: &[&str]) {
        self.tasks
            .lock()
            .unwrap()
            .get_mut(id)
            .unwrap()
            .context_files = selectors.iter().map(ToString::to_string).collect();
    }

    fn set_status(&self, id: &str, status: TaskStatus) {
        self.tasks.lock().unwrap().get_mut(id).unwrap().status = status;
    }

    fn task(&self, id: &str) -> Task {
        self.tasks.lock().unwrap()[id].clone()
    }

    fn status(&self, id: &str) -> TaskStatus {
        self.tasks.lock().unwrap()[id].status
    }

    fn comments(&self, id: &str) -> Vec<TaskComment> {
        self.comments
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    fn history(&self, id: &str) -> Vec<TaskHistoryEntry> {
        self.history
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    fn completion_notes(&self) -> Vec<String> {
        self.completion_notes.lock().unwrap().clone()
    }

    fn landings(&self) -> Vec<ReviewLandingRequest> {
        self.landings.lock().unwrap().clone()
    }

    fn widenings(&self) -> Vec<Widening> {
        self.widenings.lock().unwrap().clone()
    }

    fn recovery_certificates(&self) -> Vec<(String, String, Value)> {
        self.recovery_certificates.lock().unwrap().clone()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for DeliveryHost {
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Ok(self.runs.lock().unwrap().get(run_id).cloned())
    }

    fn admit_pr_auto_fix(
        &self,
        request: &orbit_engine::PrAutoFixRequest,
    ) -> Result<orbit_engine::PrAutoFixAdmission, OrbitError> {
        let attempts = pr_auto_fix_attempts(&self.history(TASK_ID), &request.pr_number);
        let decision = orbit_types::workflow::decide_pr_auto_fix(
            &attempts,
            &orbit_types::workflow::PrAutoFixClaim {
                pr_number: &request.pr_number,
                head_sha: &request.head_sha,
                blocker: request.blocker,
                run_id: &request.run_id,
            },
            PR_AUTO_FIX_BUDGET,
            "fixture",
            "fixture",
        );
        if let orbit_types::workflow::PrAutoFixDecision::Admit(attempt) = &decision {
            auto_fix::seed_auto_fix_history(self, PR_AUTO_FIX_ADMITTED_EVENT, attempt);
        }
        Ok(orbit_engine::PrAutoFixAdmission::Decided(decision))
    }

    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        let mut attempts = self.recovery_attempts.lock().unwrap();
        attempts.push((run_id.to_string(), step_id.to_string(), scope.clone()));
        Ok(attempts
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .count() as u64)
    }

    /// Like the runtime's recovery authority, certify only the newest
    /// reservation of the step, for exactly the HEAD and base it reserved,
    /// then copy the evidence into the run store `git_rebase` reads.
    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        let attempts = self.recovery_attempts.lock().unwrap();
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
        self.recovery_certificates.lock().unwrap().push((
            run_id.to_string(),
            step_id.to_string(),
            output.clone(),
        ));
        self.run_states
            .lock()
            .unwrap()
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

    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
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
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        Ok(self
            .recovery_attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .map(|(_, _, scope)| scope.clone())
            .collect())
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        Ok(self.run_states.lock().unwrap().get(run_id).cloned())
    }

    fn widen_task_context_files(
        &self,
        task_id: &str,
        _run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        self.widenings.lock().unwrap().push((
            task_id.to_string(),
            step,
            activity.to_string(),
            paths.to_vec(),
        ));
        let selectors = paths
            .iter()
            .map(|path| format!("file:{path}"))
            .collect::<Vec<_>>();
        if let Some(task) = self.tasks.lock().unwrap().get_mut(task_id) {
            task.context_files.extend(selectors.iter().cloned());
        }
        Ok(selectors)
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        let artifact = self
            .artifacts
            .lock()
            .unwrap()
            .get(&(task_id.to_string(), path.to_string()))
            .map(|content| TaskArtifact {
                path: path.into(),
                content: content.clone(),
                media_type: "application/json".into(),
                created_by: self
                    .artifact_creators
                    .lock()
                    .unwrap()
                    .get(&(task_id.to_string(), path.to_string()))
                    .cloned(),
            });
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(task_id)?;
        }
        Ok(artifact)
    }

    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .iter()
            .filter(|((id, _), _)| id == task_id)
            .map(|((_, path), content)| TaskArtifact {
                path: path.clone(),
                content: content.clone(),
                media_type: "application/json".into(),
                created_by: self
                    .artifact_creators
                    .lock()
                    .unwrap()
                    .get(&(task_id.to_string(), path.clone()))
                    .cloned(),
            })
            .collect())
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .unwrap()
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
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self
            .tasks
            .lock()
            .unwrap()
            .values()
            .filter(|task| job_run_id.is_none_or(|run| task.job_run_id.as_deref() == Some(run)))
            .cloned()
            .collect())
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(self.comments(task_id))
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(self.history(task_id))
    }

    /// The guarded transition: like the task store, refuse a write whose
    /// expected status is stale.
    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
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
            self.completion_notes
                .lock()
                .unwrap()
                .push(update.note.unwrap_or_default());
        }
        Ok(task.clone())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(status) = update.status {
            task.status = status;
        }
        for external_ref in update.external_refs {
            push_external_ref_if_missing(&mut task.external_refs, external_ref);
        }
        self.status_events.lock().unwrap().push((
            task_id.to_string(),
            update.status_event,
            update.status_note,
        ));
        self.comments
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .extend(update.append_comments);
        self.history
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .extend(update.append_history);
        Ok(())
    }

    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        self.landings.lock().unwrap().push(request.clone());
        Ok(())
    }

    fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Result<(), OrbitError> {
        self.releases.lock().unwrap().push(request.clone());
        Ok(())
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.required_commands.lock().unwrap().clone()
    }

    fn review_baseline_commands(&self) -> Vec<String> {
        self.baseline_commands.lock().unwrap().clone()
    }

    fn host_os(&self) -> Option<EvidenceHostOs> {
        *self.os.lock().unwrap()
    }

    /// An implementer lane that refuses the probed sandbox, unless a test
    /// scripted another answer [ORB-15516].
    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        provider: Option<&str>,
        _fs_profile: Option<&str>,
        cwd: &Path,
    ) -> NamespaceProbe {
        self.probes.lock().unwrap().push((
            probe.command.to_string(),
            provider.map(str::to_string),
            cwd.to_path_buf(),
        ));
        if let Some(answer) = self.deferral_answers.lock().unwrap().get(probe.command) {
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
        if let Some(output) = self.scripted.lock().unwrap().get(action).cloned() {
            self.scripted_inputs
                .lock()
                .unwrap()
                .insert(action.to_string(), input.clone());
            return Ok(output);
        }
        execute_deterministic_action(self, action, config, input, false, &HashMap::new(), None)
            .map_err(|error| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: error.to_string(),
            })
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        if self.scripted.lock().unwrap().contains_key("test_ci_repair")
            && let Some(answer) = auto_fix::ci_answer(self, operation, &input)
        {
            return Ok(answer);
        }
        orbit_engine::run_private_vcs_operation(operation, &input)
    }

    /// One owner-local claim on the fixture's candidate branch, requiring
    /// whatever this host requires.
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(ClaimExecutionContext {
            footprint: vec!["file:src/feature.txt".to_string()],
            workspace_id: "ws_owner".to_string(),
            task_id: TASK_ID.to_string(),
            claim_id: "claim-landing".to_string(),
            machine_id: "hm_follower".to_string(),
            run_id: RUN_ID.to_string(),
            ship_mode: self.ship_mode.lock().unwrap().clone(),
            base_branch: BASE.to_string(),
            landing_branch: BASE.to_string(),
            required_commands: RuntimeHost::required_validation_commands(self),
        })
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        self.claim_logs.lock().unwrap().push(path.to_string());
        self.claim_log_contents
            .lock()
            .unwrap()
            .insert(path.to_string(), content);
        Ok(())
    }

    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        RuntimeHost::apply_task_automation_update(
            self,
            task_id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: orbit_types::workflow::FLAKY_VALIDATION_EVENT.into(),
                    note: Some(record.text()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
    }

    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        self.handoffs.lock().unwrap().push(handoff.clone());
        Ok(())
    }

    fn validation_subprocess_environment(&self) -> ValidationEnvironment {
        self.validation_env
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                ValidationEnvironment::launcher(RuntimeHost::agent_subprocess_environment(
                    self,
                    &[],
                ))
            })
    }

    /// Like the task store, accept evidence only from the run owning the task.
    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        if RuntimeHost::get_task(self, task_id)?.job_run_id.as_deref() != Some(run_id) {
            return Err(OrbitError::PolicyDenied(format!(
                "run {run_id} does not own {task_id}"
            )));
        }
        self.artifacts
            .lock()
            .unwrap()
            .insert((task_id.to_string(), path.to_string()), content);
        Ok(())
    }
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    git_fixture::run(current_dir, args)
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf8 fixture path")
}

// ---------------------------------------------------------------------------
// Substitute forge
// ---------------------------------------------------------------------------

/// The `gh` surface the engine's provider adapter calls, over one pull request
/// (#42). `__SANDBOX__/active-forge` names the forge directory of the running
/// case; its `remote.git` holds the real branches.
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
        pending) printf '{"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""}' ;;
        success) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}' ;;
        failure) printf '{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}' ;;
    esac
}

status() {
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
        printf '{"number":42,"state":"MERGED","mergedAt":"2026-10-03T00:00:00Z","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","reviewDecision":"","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":{"oid":"%s"},"url":"%s"}\n' \
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
        pending) merge_state=BLOCKED; rollup=$(check pending) ;;
        success) merge_state=CLEAN; rollup=$(check success) ;;
        failure) merge_state=BLOCKED; rollup=$(check failure) ;;
        review_required) merge_state=BLOCKED; review=REVIEW_REQUIRED; rollup=$(check success) ;;
        dirty) merge_state=DIRTY; rollup=$(check success) ;;
        closed) state=CLOSED; merge_state=CLEAN; rollup=$(check success) ;;
        contradictory) merged_at='"2026-10-05T12:47:15Z"'; merge_state=CLEAN; rollup=$(check success) ;;
        *) echo "fake gh: unknown check state '$observed'" >&2; exit 2 ;;
    esac
    printf '{"number":42,"state":"%s","mergedAt":%s,"mergeable":"MERGEABLE","mergeStateStatus":"%s","reviewDecision":"%s","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":null,"url":"%s"}\n' \
        "$state" "$merged_at" "$merge_state" "$review" "$rollup" "$pr_head" "$reported_head" "$pr_base" "$url"
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

case "$1 ${2:-}" in
    "pr list")
        if [ -f "$forge/pr-head" ]; then
            printf '[{"number":%s,"title":"Landing","headRefName":"%s","author":{"login":"orbit"}}]\n' "$number_json" "$(cat "$forge/pr-head")"
        else
            echo '[]'
        fi ;;
    "pr create") create "$@" ;;
    "pr view")
        case "$*" in
            *mergeStateStatus*) status ;;
            *) printf '{"number":%s,"title":"Landing","body":"","headRefName":"%s","files":[],"commits":[],"url":"%s"}\n' "$number_json" "$(cat "$forge/pr-head")" "$url" ;;
        esac ;;
    "repo view") echo '{"nameWithOwner":"orbit/test"}' ;;
    "api graphql")
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
    *) echo "fake gh: unsupported call: $*" >&2; exit 2 ;;
esac
"#;

// ---------------------------------------------------------------------------
// Isolated child process
// ---------------------------------------------------------------------------

/// Run `body` in a copy of this test binary whose `PATH` starts with the
/// substitute `gh`, and fail if it fails or outlives [`CHILD_DEADLINE`].
fn isolated(test: &str, body: impl FnOnce(&Path)) {
    if std::env::var_os(CHILD_ENV).is_some() {
        let sandbox = PathBuf::from(std::env::var_os(SANDBOX_ENV).expect("sandbox path"));
        // Container-only wrappers do no fixture work themselves. Give each
        // leaf row its own tool, home and sandbox-root counter files below.
        let container = matches!(
            test,
            "validation_environment_contracts"
                | "empty_validation_and_handoff_contracts"
                | "failure_handoff_contracts"
                | "implementation_handoff_contracts"
                | "pr_body_and_promotion_contracts"
        );
        if std::env::var(CHILD_ENV).as_deref() == Ok(test) || container {
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
    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{test} failed in its isolated child ({}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The bounded child runs exactly one libtest thread. Change its environment
/// only between completed rows, and restore it and cwd on return or unwind.
fn in_fresh_row(parent: &Path, body: impl FnOnce(&Path)) {
    struct RestoreCwd(PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.0).expect("restore family cwd");
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
        ("PATH", Some(path.to_str().expect("fixture PATH"))),
        // The capture-stop row deliberately lowers this process-global
        // ceiling. Restore it before a truncated-but-bounded output row runs.
        ("ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES", None),
    ]);
    let _cwd = RestoreCwd(std::env::current_dir().unwrap());
    std::env::set_current_dir(row.path()).unwrap();
    // TMPDIR stays at the parent-owned child root: a cached immutable Git
    // seed must outlive the row. Each scenario still owns separate TempDirs.
    orbit_engine::test_support::with_vcs_retry_wait(|_| {}, || body(row.path()));
}

fn validation_environment_contracts() {
    isolated("validation_environment_contracts", |_| {
        git_fixture::run_scenarios(&[
            (
                "missing_required_validation_tool_reports_its_search_path",
                missing_required_validation_tool_reports_its_search_path,
            ),
            (
                "validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain",
                validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain,
            ),
            (
                "a_missing_validation_tool_is_an_environment_failure",
                a_missing_validation_tool_is_an_environment_failure,
            ),
        ]);
    });
}

fn empty_validation_and_handoff_contracts() {
    isolated("empty_validation_and_handoff_contracts", |_| {
        git_fixture::run_scenarios(&[
            (
                "required_validation_without_commands_is_a_no_op",
                required_validation_without_commands_is_a_no_op,
            ),
            (
                "claimed_validation_without_commands_is_a_recorded_no_op",
                claimed_validation_without_commands_is_a_recorded_no_op,
            ),
            (
                "claimed_handoff_carries_before_pr_evidence_only_for_its_candidate",
                claimed_handoff_carries_before_pr_evidence_only_for_its_candidate,
            ),
            (
                "claimed_handoff_without_logs_is_refused_when_commands_are_required",
                claimed_handoff_without_logs_is_refused_when_commands_are_required,
            ),
        ]);
    });
}

fn failure_handoff_contracts() {
    isolated("failure_handoff_contracts", |_| {
        git_fixture::run_scenarios(&[
            (
                "a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle",
                a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle,
            ),
            (
                "a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking",
                a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking,
            ),
            (
                "an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks",
                an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks,
            ),
            (
                "an_admission_refusal_for_held_evidence_stays_held_not_escalated",
                an_admission_refusal_for_held_evidence_stays_held_not_escalated,
            ),
        ]);
    });
}

fn implementation_handoff_contracts() {
    isolated("implementation_handoff_contracts", |_| {
        git_fixture::run_scenarios(&[
            (
                "an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr",
                an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr,
            ),
            (
                "a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write",
                a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write,
            ),
            (
                "an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task",
                an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task,
            ),
            (
                "a_child_its_parent_stopped_waiting_for_holds_its_candidate",
                a_child_its_parent_stopped_waiting_for_holds_its_candidate,
            ),
        ]);
    });
}

#[test]
fn pr_body_and_promotion_contracts() {
    isolated("pr_body_and_promotion_contracts", |_| {
        git_fixture::run_scenarios(&[
            (
                "pr_promote_records_the_pull_request_page_with_its_number",
                pr_promote_records_the_pull_request_page_with_its_number,
            ),
            (
                "pr_open_appends_the_review_fixes_section_to_the_published_body",
                pr_open_appends_the_review_fixes_section_to_the_published_body,
            ),
        ]);
    });
}

#[test]
fn known_red_sandbox_notices() {
    known_red_coverage_rows(
        "known_red_sandbox_notices",
        &[
            "bubblewrap",
            "seatbelt",
            "deferred",
            "skip",
            "skipping-colon",
        ],
    );
}

#[test]
fn known_red_dropped_notices() {
    known_red_coverage_rows(
        "known_red_dropped_notices",
        &[
            "skipping-space",
            "raw-refusal",
            "dropped-deferral",
            "dropped-skip",
        ],
    );
}

#[test]
fn known_red_replay_coverage() {
    known_red_coverage_rows(
        "known_red_replay_coverage",
        &[
            "replay-with-notice",
            "replay-zero-tests",
            "replay-missing-count",
            "replay-missing-tool",
            "replay-executed",
        ],
    );
}

fn pull_ref_identity_refusals() {
    pull_ref_refusal_rows(
        "pull_ref_identity_refusals",
        &[
            "pull_ref_third",
            "pull_ref_third_after_old",
            "branch_missing",
            "branch_at_previous",
            "previous_unknown",
            "previous_empty",
        ],
    );
}

#[test]
fn pull_ref_wait_and_review_refusals() {
    pull_ref_refusal_rows(
        "pull_ref_wait_and_review_refusals",
        &[
            "previous_is_candidate",
            "persistent",
            "persistent_after_metadata",
            "time_cap",
            "review_mismatch",
            "ls_remote_fails",
        ],
    );
}

fn claimed_known_red_equal_and_subset() {
    claimed_known_red_rows("claimed_known_red_equal_and_subset", &["equal", "subset"]);
}

#[test]
fn claimed_known_red_superset_and_disjoint() {
    claimed_known_red_rows(
        "claimed_known_red_superset_and_disjoint",
        &[
            "shared-plus-new",
            "disjoint-six-vs-one",
            "candidate-unknown",
            "base-unknown",
            "both-unknown",
        ],
    );
}

#[test]
fn metadata_lag_pins_and_repair() {
    isolated("metadata_lag_pins_and_repair", |_| {
        previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins();
        previous_published_head_metadata_lag_after_in_run_conflict_repair();
    });
}

#[test]
fn pull_ref_pins_and_identity() {
    isolated("pull_ref_pins_and_identity", |_| {
        previous_published_head_pull_ref_lag_settles_with_exact_merge_pin();
        pull_ref_identity_refusals();
    });
}

#[test]
fn pr_open_branch_identity() {
    isolated("pr_open_branch_identity", |_| {
        pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip();
        pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch();
        pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate();
    });
}

#[test]
fn pr_create_retry_contracts() {
    isolated("pr_create_retry_contracts", |_| {
        pr_open_permanent_create_failures_never_retry_or_look_up_again();
        pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr();
        pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase();
    });
}

#[test]
fn pr_number_and_push_transports() {
    isolated("pr_number_and_push_transports", |_| {
        pr_open_preserves_provider_number_representations();
        fixture_push_guard_checks_all_effective_push_urls();
        fixture_push_refuses_non_file_transports_without_network();
    });
}

#[test]
fn base_race_fresh_gates() {
    isolated("base_race_fresh_gates", |_| {
        a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence();
        a_base_race_refresh_refuses_unmet_gates_without_another_merge();
    });
}

#[test]
fn base_race_candidate_identity() {
    isolated("base_race_candidate_identity", |_| {
        a_base_race_refresh_refuses_a_changed_candidate_without_another_merge();
        an_already_merged_refresh_reconciles_without_another_merge_request();
    });
}

#[test]
fn merge_refusal_bounds() {
    isolated("merge_refusal_bounds", |_| {
        other_merge_refusals_fail_on_the_first_request();
        repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget();
    });
}

#[test]
fn recovered_base_advances() {
    isolated("recovered_base_advances", |_| {
        conflicting_base_advances_after_recovery_are_chased_until_the_bound();
        a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands();
    });
}

#[test]
fn review_checks_and_delivery() {
    isolated("review_checks_and_delivery", |_| {
        a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review();
        passing_checks_merge_the_reviewed_candidate_and_record_its_delivery();
    });
}

#[test]
fn review_fix_revalidation() {
    isolated("review_fix_revalidation", |_| {
        a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges();
        a_failed_review_revalidation_rejects_and_preserves_both_commits();
        a_red_base_during_pr_revalidation_holds_the_task();
    });
}

#[test]
fn review_repair_leases() {
    isolated("review_repair_leases", |_| {
        a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease();
        a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes();
    });
}

#[test]
fn claimed_validation_failures() {
    isolated("claimed_validation_failures", |_| {
        a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs();
        a_claimed_candidate_that_cannot_reach_the_network_fails_transient();
        a_required_command_red_on_the_base_holds_the_task_without_a_pr();
    });
}

#[test]
fn claimed_publication_and_merge_pins() {
    isolated("claimed_publication_and_merge_pins", |_| {
        a_pr_claim_validates_before_publication_and_pins_after_it();
        a_pr_merge_batch_lands_the_checked_head_at_its_pin();
        a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check();
    });
}

#[test]
fn validation_repairs_and_logs() {
    isolated("validation_repairs_and_logs", |_| {
        required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes();
        required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs();
        printed_supervisor_notes_remain_candidate_failures_on_both_streams();
    });
}

#[test]
fn supervisor_and_truncated_failures() {
    isolated("supervisor_and_truncated_failures", |_| {
        supervisor_stops_are_environment_failures_and_never_poison_the_base_cache();
        a_failure_only_the_truncated_middle_names_still_requires_candidate_repair();
    });
}

#[test]
fn network_retry_and_cached_base() {
    isolated("network_retry_and_cached_base", |_| {
        a_network_flake_is_rerun_before_the_candidate_is_judged();
        remote_base_transport_retries_preserve_access_refusals();
        claimed_handoff_fetch_timeout_uses_only_a_related_cached_base();
    });
}

#[test]
fn linux_deferral_contracts() {
    isolated("linux_deferral_contracts", |_| {
        a_claimed_leaf_replays_its_implementers_deferral_before_publication();
        a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step();
        a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery();
    });
}

#[test]
fn macos_deferral_contracts() {
    isolated("macos_deferral_contracts", |_| {
        a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication();
        a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication();
        a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs();
        a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure();
    });
}

#[test]
fn validation_and_empty_handoffs() {
    isolated("validation_and_empty_handoffs", |_| {
        validation_environment_contracts();
        empty_validation_and_handoff_contracts();
    });
}

#[test]
fn failure_and_implementation_handoffs() {
    isolated("failure_and_implementation_handoffs", |_| {
        failure_handoff_contracts();
        implementation_handoff_contracts();
    });
}

#[test]
fn known_red_validation_and_subset() {
    isolated("known_red_validation_and_subset", |_| {
        a_candidate_failing_only_known_red_tests_proceeds_and_records_them();
        claimed_known_red_equal_and_subset();
    });
}
