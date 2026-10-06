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
//!
//! `PATH`, `$HOME` and Git configuration are process-global, so each test body
//! re-runs in an isolated copy of this binary. That child has disposable
//! `$HOME`/`$TMPDIR`, no inherited Orbit, Git or GitHub authority, and a bounded
//! wait. The shared supervisor kills and reaps it on timeout.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError, process::run_bounded_capped, test_env};
use orbit_engine::{
    BaselineHoldStatus, ClaimExecutionContext, ReviewLandingRequest, ReviewReleaseRequest,
    RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate, baseline_hold_status,
    execute_deterministic_action, review_gate,
};
use orbit_exec::{LoginShell, ValidationEnvPolicy, ValidationEnvironment};
use orbit_types::task::{
    ContextWideningStep, ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskArtifact,
    TaskComment, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::handoff::{HandoffDelivery, HandoffReviewDisposition, TaskHandoff};
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, ClaimFailureClass, ReviewTiming, ReviewVerdict,
    is_baseline_red_failure,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Names the test whose body this process runs as the isolated child.
const CHILD_ENV: &str = "ORBIT_PR_LANDING_CHILD";
/// The parent-owned directory holding the substitute `gh` and its pointer to
/// the forge state of the case being run.
const SANDBOX_ENV: &str = "ORBIT_PR_LANDING_SANDBOX";
/// Upper bound on one isolated test body, including the engine's poll sleeps.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

const BASE: &str = "agent-main";
const BRANCH: &str = "orbit/landing-candidate";
const RUN_ID: &str = "jrun-landing";
const TASK_ID: &str = "T-LANDING";
const PR_NUMBER: &str = "42";

// ---------------------------------------------------------------------------
// Merge gating
// ---------------------------------------------------------------------------

/// Regression for ORB-14315: provider metadata may lag a verified lease-push,
/// but both the merge mutation and review landing must keep the new exact SHA.
#[test]
fn previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins() {
    isolated(
        "previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins",
        |sandbox| {
            for reviewed in [true, false] {
                for stale_reads in [1, 2] {
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
                "remote_old",
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
                    "remote_old" => {
                        remote = Some(&fx.candidate);
                        (1, "delivery_evidence_stale")
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

/// An in-run conflict repair retains the head it replaces even without an
/// upstream previous-head checkpoint, so its verified push can settle too.
#[test]
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

/// Pending checks are waited out, never merged early. Once they pass, the
/// reviewed candidate is merged with a request conditional on its head SHA.
/// The merge is read back from the forge, the landing is recorded for review
/// coverage, and only then does the task reach `done`.
#[test]
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

/// A required check that fails, before or after a wait, or an outstanding
/// required review, refuses completion before any merge request. The PR stays
/// open, the remote base does not move, and the task stays in `review`.
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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

/// A base with its own work merged into the landing branch remains obsolete
/// at publication and on a resumed promotion, before any forge call or status
/// transition.
#[test]
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
#[test]
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
#[test]
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
/// anything is pushed, with its output in the error and its log — beside the
/// passing log of the command before it — attached to the task. A repair left
/// uncommitted is refused, because the result would not describe the committed
/// candidate. Once the repair is committed, as step recovery does, the retry
/// passes on the new head and replaces the logs.
#[test]
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
                    && message.contains("src/feature.txt: not formatted"),
                "the refusal names the candidate and carries the output: {message}"
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

/// A missing validation tool is refused with the actual allowlisted PATH,
/// both in the action error and the durable command log [ORB-13963].
#[test]
fn missing_required_validation_tool_reports_its_search_path() {
    isolated(
        "missing_required_validation_tool_reports_its_search_path",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            host.require_commands(&["orbit_missing_validation_tool_13963"]);
            let error = action(&host, "candidate_validate", &fx.validate_input())
                .expect_err("a missing required tool must refuse publication");
            let diagnostic = format!(
                "Required validation PATH={}",
                std::env::var("PATH").unwrap()
            );
            assert!(error.to_string().contains(&diagnostic), "{error}");
            let log = host.validation_log(TASK_ID, &format!("validation/{RUN_ID}/0.json"));
            assert_eq!(log["exit_code"], 127);
            assert!(log["output"].as_str().unwrap().contains(&diagnostic));
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
#[test]
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
#[test]
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

/// An implementer blocker arrives before commit, so the worktree may be dirty.
/// The handoff blocks the task with the kind, leaves the uncommitted file and
/// the head where they are, and opens no `[BLOCKED]` PR.
#[test]
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
#[test]
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
                host.status(TASK_ID),
                TaskStatus::InProgress,
                "run finalization owns the hold"
            );
        },
    );
}

// ---------------------------------------------------------------------------
// A red base and network flakes [ORB-14258]
// ---------------------------------------------------------------------------

/// The command a red-base fixture requires; its base's `Makefile` fails it.
const RED_LINT: &str = "make ci-lint";

/// A required command that fails on the synchronized base exactly as on the
/// candidate is the base's failure. The step fails typed `baseline_red` with
/// the candidate's and the base's logs. Two candidates over that base share
/// one base run. The failure handoff holds the task in the backlog: nothing is
/// pushed, no PR is opened, and the candidate is kept. The hold stands while
/// the base ref points at the red commit and lifts once it moves to a commit
/// that passes.
#[test]
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
                hold,
                BaselineRedHold {
                    base_ref: BASE.to_string(),
                    base_sha: fx.base_sha.clone(),
                    command: RED_LINT.to_string(),
                    run_id: RUN_ID.to_string(),
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
                    .is_some_and(|output| output.contains("README.md: lint is red")),
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

            assert!(
                matches!(
                    baseline_hold_status(&host, &fx.repo, &hold),
                    BaselineHoldStatus::Holding(_)
                ),
                "the base ref still points at the red commit"
            );
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

/// A red base discovered while revalidating a rebased, already-published PR
/// still holds the task. The earlier completion-failure path used to preserve
/// every PR failure in `review`, bypassing the baseline hold.
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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
#[test]
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

/// Without `workflow.required_validation_commands` the step changes nothing:
/// no command runs, no evidence is attached, and even a checkout that would be
/// refused is passed through as today.
#[test]
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
#[test]
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
#[test]
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
            let carried = handoffs[0].review.before_pr().expect("before-PR evidence");
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
#[test]
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
#[test]
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
#[test]
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

#[test]
fn a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking() {
    isolated(
        "a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking",
        |sandbox| {
            let fx = Fixture::new(sandbox);
            let host = DeliveryHost::new(&fx.repo, TaskStatus::InProgress);
            let handoff = action(&host, "pr_failure_handoff", &json!({
            "failed_step_id": "review", "error_code": "deterministic_action_refused",
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

#[test]
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
            let mut input = json!({
                "failed_step_id": "review_gate_settle", "error_code": "deterministic_action_refused",
                "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID,
                "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": fx.repo},
                    "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE},
                    "review_gate_admit": {"attempt_id": "rvw-held", "lineage_key": "lineage"}},
            });
            let held = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(held["decision"], "awaiting_review_evidence");
            assert_eq!(host.status(TASK_ID), TaskStatus::InProgress);
            assert_eq!(fx.forge_state("pr-head"), None);
            let mut stale_hold = hold.clone();
            stale_hold["attempt_id"] = json!("rvw-stale");
            host.artifacts.lock().unwrap().insert(
                (TASK_ID.into(), "review-evidence-hold.json".into()),
                serde_json::to_vec(&stale_hold).unwrap(),
            );
            let stale = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(
                stale["decision"], "blocked_review_gate",
                "a system hold must match the admitted attempt"
            );
            input["error_message"] = json!("review_gate_blocked: changes_required; wrong approach");
            let rejected = action(&host, "pr_failure_handoff", &input).unwrap();
            assert_eq!(rejected["decision"], "blocked_review_gate");
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
                "failed_step_id": "review_gate_settle", "error_code": "deterministic_action_refused",
                "error_message": "review_awaiting_evidence: native macOS run", "run_id": RUN_ID,
                "job_input": {"task_ids": [TASK_ID], "base_branch": BASE, "base_sync": "local"},
                "pipeline": {"worktree": {"job_run_id": RUN_ID, "workspace_path": forged_fx.repo},
                    "sync_base": {"head": BRANCH, "base": BASE, "base_ref": BASE},
                    "review_gate_admit": {"attempt_id": "rvw-forged", "lineage_key": "lineage"}},
            })).unwrap();
            assert_eq!(
                forged["decision"], "blocked_review_gate",
                "attached holds are not settlement evidence"
            );
            assert_eq!(forged_host.status(TASK_ID), TaskStatus::Blocked);
        },
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

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
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let forge = root.path().join("forge");
        let remote = forge.join("remote.git");
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(root.path(), &["init", "--bare", path_str(&remote)]);
        git(&repo, &["init"]);
        git(&repo, &["checkout", "-b", BASE]);
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(
            &repo,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(repo.join("README.md"), "base\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["remote", "add", "origin", path_str(&remote)]);
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
                "ci-lint:\n\t@echo ran >> '{}'\n\t@echo 'README.md: lint is red' >&2; exit 2\n",
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

    /// Commit `file` on the base and push it, leaving the candidate checked out.
    fn commit_on_base(&self, file: &str, contents: &str) {
        git(&self.repo, &["checkout", BASE]);
        fs::write(self.repo.join(file), contents).unwrap();
        git(&self.repo, &["add", file]);
        git(
            &self.repo,
            &["commit", "-m", &format!("base writes {file}")],
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
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

/// The task store and review-landing record the delivery actions write
/// through, kept in memory. The provider boundary is left to the engine.
struct DeliveryHost {
    repo: PathBuf,
    tasks: Mutex<BTreeMap<String, Task>>,
    comments: Mutex<BTreeMap<String, Vec<TaskComment>>>,
    completion_notes: Mutex<Vec<String>>,
    landings: Mutex<Vec<ReviewLandingRequest>>,
    required_commands: Mutex<Vec<String>>,
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
    /// Claimed-leaf handoffs recorded as pending settlements.
    handoffs: Mutex<Vec<TaskHandoff>>,
    /// The claim's owner-resolved ship mode.
    ship_mode: Mutex<String>,
    /// Status history written by automation updates, as (task, event, note).
    status_events: Mutex<Vec<StatusEvent>>,
}

type Widening = (String, ContextWideningStep, String, Vec<String>);
/// A status history write, as (task, event, note).
type StatusEvent = (String, Option<String>, Option<String>);

impl DeliveryHost {
    fn new(repo: &Path, status: TaskStatus) -> Self {
        Self {
            repo: repo.to_path_buf(),
            tasks: Mutex::new(BTreeMap::from([(TASK_ID.to_string(), task(status))])),
            comments: Mutex::default(),
            completion_notes: Mutex::default(),
            landings: Mutex::default(),
            required_commands: Mutex::default(),
            artifacts: Mutex::default(),
            artifact_creators: Mutex::default(),
            releases: Mutex::default(),
            validation_env: Mutex::default(),
            widenings: Mutex::default(),
            claim_logs: Mutex::default(),
            handoffs: Mutex::default(),
            ship_mode: Mutex::new("local".to_string()),
            status_events: Mutex::default(),
        }
    }

    fn resolve_validation_env(&self, environment: ValidationEnvironment) {
        *self.validation_env.lock().unwrap() = Some(environment);
    }

    fn require_commands(&self, commands: &[&str]) {
        *self.required_commands.lock().unwrap() =
            commands.iter().map(ToString::to_string).collect();
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

    fn completion_notes(&self) -> Vec<String> {
        self.completion_notes.lock().unwrap().clone()
    }

    fn landings(&self) -> Vec<ReviewLandingRequest> {
        self.landings.lock().unwrap().clone()
    }

    fn widenings(&self) -> Vec<Widening> {
        self.widenings.lock().unwrap().clone()
    }
}

impl RuntimeHost for DeliveryHost {
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

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(self.comments(task_id))
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

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.required_commands.lock().unwrap().clone()
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
            required_commands: self.required_validation_commands(),
        })
    }

    fn attach_claim_validation_log(&self, path: &str, _content: Vec<u8>) -> Result<(), OrbitError> {
        self.claim_logs.lock().unwrap().push(path.to_string());
        Ok(())
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
                ValidationEnvironment::launcher(self.agent_subprocess_environment(&[]))
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
        if self.get_task(task_id)?.job_run_id.as_deref() != Some(run_id) {
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
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\n{}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
export GIT_AUTHOR_NAME=Forge GIT_AUTHOR_EMAIL=forge@example.invalid
export GIT_COMMITTER_NAME=Forge GIT_COMMITTER_EMAIL=forge@example.invalid
printf '%s %s\n' "$1" "${2:-}" >> "$forge/calls"

head_sha() {
    git --git-dir="$remote" rev-parse "refs/heads/$(cat "$forge/pr-head")"
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
        printf '{"number":42,"state":"MERGED","mergedAt":"2026-10-03T00:00:00Z","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","reviewDecision":"","statusCheckRollup":[%s],"headRefName":"%s","headRefOid":"%s","baseRefName":"%s","mergeCommit":{"oid":"%s"},"url":"%s"}\n' \
            "$(check success)" "$pr_head" "$(head_sha)" "$pr_base" "$(cat "$forge/merged")" "$url"
        return
    fi
    reads=$(( $(cat "$forge/reads" 2>/dev/null || echo 0) + 1 ))
    printf '%s\n' "$reads" > "$forge/reads"
    observed=$(sed -n "${reads}p" "$forge/checks")
    [ -n "$observed" ] || observed=$(tail -n 1 "$forge/checks")
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
    shift 2
    while [ $# -gt 0 ]; do
        case "$1" in
            --base) printf '%s\n' "$2" > "$forge/pr-base"; shift 2 ;;
            --head) printf '%s\n' "$2" > "$forge/pr-head"; shift 2 ;;
            --body) printf '%s' "$2" > "$forge/pr-body"; shift 2 ;;
            *) shift ;;
        esac
    done
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
            printf '[{"number":42,"title":"Landing","headRefName":"%s","author":{"login":"orbit"}}]\n' "$(cat "$forge/pr-head")"
        else
            echo '[]'
        fi ;;
    "pr create") create "$@" ;;
    "pr view")
        case "$*" in
            *mergeStateStatus*) status ;;
            *) printf '{"number":42,"title":"Landing","body":"","headRefName":"%s","files":[],"commits":[],"url":"%s"}\n' "$(cat "$forge/pr-head")" "$url" ;;
        esac ;;
    "repo view") echo '{"nameWithOwner":"orbit/test"}' ;;
    "api graphql")
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
    if std::env::var(CHILD_ENV).as_deref() == Ok(test) {
        let sandbox = PathBuf::from(std::env::var_os(SANDBOX_ENV).expect("sandbox path"));
        body(&sandbox);
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
