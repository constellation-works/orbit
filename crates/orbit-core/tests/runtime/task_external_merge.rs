//! A task whose delivery PR an operator merged outside Orbit settles to done
//! from that merge. Tests inject a fake forge (recent listing plus older
//! by-number lookup); claims and handoffs use the owner's own mutations, so
//! the store's guards decide.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::TaskStatus;
use orbit_core::application::task::EXTERNAL_MERGE_SETTLED_EVENT;
use orbit_core::runtime::task_pr_forge::ForgePullRequest;
use orbit_store::contracts::HandoffObservation;
use orbit_store::contracts::{ClaimInvocation, ClaimMutation, ExecutionClaimPhase};
use orbit_types::task::ExternalRef;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffDelivery, LandingAttemptState, LandingStartState};
use serde_json::json;
use std::time::{Duration, Instant};

use crate::external_merge_fixture::*;
pub(crate) use crate::external_merge_fixture::{Fixture, Handoff};

mod pr_supersession;

crate::scenario_table!(
    released_claims,
    released_claim_pull_requests_settle_without_an_accepted_handoff_or_task_ref,
    released_claim_pull_requests_require_a_merge_naming_the_task_into_the_captured_branch,
    a_live_leaf_carrying_a_released_claim_task_prevents_settlement
);
crate::scenario_table!(
    eligible_and_blocked,
    external_merge_settlement_refuses_every_ineligible_status,
    a_blocked_task_settles_from_its_merged_delivery_pr_while_another_task_claims_its_files,
    the_operator_settles_one_task_from_a_merge_older_than_the_listing
);
crate::scenario_table!(
    handoff_claims,
    a_revoked_handoff_and_its_claim_settle_from_its_candidate_pr_merged_outside_orbit,
    landing_a_stopped_authorized_handoff_reports_its_stop_and_retry_path,
    a_handed_off_claim_refuses_settlement_unless_its_revoked_handoffs_own_pr_merged,
    external_merge_recovery_suggestions_include_the_observed_claim_phase,
    the_owner_store_settles_a_revoked_handoff_and_its_task_in_one_write_or_neither,
    a_template_body_without_a_task_placeholder_still_settles_an_external_merge
);
crate::scenario_table!(
    clock_ticks,
    clock_tick_settles_blocked_and_review_tasks_without_a_drain,
    clock_tick_leaves_guarded_tasks_and_follower_hosts_unsettled,
    clock_tick_forge_failure_does_not_fail_the_tick
);
crate::scenario_table!(
    stopped_handoff,
    stopped_handoff_settles_a_deleted_branch_after_two_operator_updates
);

fn released_claim_pull_requests_settle_without_an_accepted_handoff_or_task_ref() {
    for held in [true, false] {
        let fx = Fixture::new();
        let landing = "captured-landing"; // captured branch stays authoritative
        let number = if held { 101 } else { 102 };
        let task = fx.released_pr(number, held, landing);
        fx.merge(number, &task, "follower-bot", landing);
        let settlement = if held {
            fx.settle_one(&task)
        } else {
            let mut settled = fx.settle_all();
            assert_eq!(settled.len(), 1, "held={held}");
            settled.pop().unwrap()
        };
        assert_eq!(
            (settlement.pr_number, &settlement.task_id),
            (number, &task),
            "held={held}"
        );
        fx.assert_done(&task, "held settles to done");
        let note = fx.settled_note(&task);
        assert!(
            note.contains(&format!("#{number}"))
                && note.contains(MERGE_COMMIT)
                && note.contains(landing),
            "{note}"
        );
    }
}

fn released_claim_pull_requests_require_a_merge_naming_the_task_into_the_captured_branch() {
    for held in [true, false] {
        for rejected in [
            "open",
            "closed",
            "wrong-base",
            "wrong-task",
            "other-claim",
            "missing-receipt",
            "mismatched-receipt",
        ] {
            let case = format!("{held}/{rejected}");
            let fx = Fixture::new();
            let task = fx.released_pr(103, held, "captured-landing");
            let before = fx.status(&task);
            let receipt_sql = |sql: &str| {
                rusqlite::Connection::open(fx.runtime.global_root().join("orbit.db"))
                    .unwrap()
                    .execute(sql, [fx.runtime.workspace_id().unwrap()])
                    .unwrap()
            };
            match rejected {
                "open" | "closed" => fx.mark_unmerged(103, rejected),
                "wrong-base" => fx.merge(103, &task, BOT, &fx.base.clone()),
                "wrong-task" => fx.merge(103, "OTHER-1", BOT, "captured-landing"),
                "other-claim" => {
                    let unrelated = fx.add_task("Unrelated", TaskStatus::Blocked, &[]);
                    fx.merge(103, &unrelated, BOT, "captured-landing");
                    assert!(
                        fx.runtime
                            .settle_externally_merged_task(&unrelated)
                            .is_err(),
                        "{case}"
                    );
                    fx.assert_status(&unrelated, TaskStatus::Blocked, &case);
                }
                "missing-receipt" => {
                    fx.merge(103, &task, BOT, "captured-landing");
                    receipt_sql(
                        "DELETE FROM task_coordination_rows WHERE workspace_id = ?1 AND kind = 'distributed-admission-receipt-v1'",
                    );
                }
                "mismatched-receipt" => {
                    fx.merge(103, &task, BOT, "captured-landing");
                    receipt_sql(
                        "UPDATE task_coordination_rows SET payload_json = json_set(payload_json, '$.receipt.claim.task_id', 'OTHER-1') WHERE workspace_id = ?1 AND kind = 'distributed-admission-receipt-v1'",
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                fx.runtime.settle_externally_merged_task(&task).is_err(),
                "{case}"
            );
            assert!(fx.settle_all().is_empty(), "{case}");
            fx.assert_status(&task, before, &case);
            fx.assert_no_settled(&task, &case);
        }
    }
}

fn a_live_leaf_carrying_a_released_claim_task_prevents_settlement() {
    let fx = Fixture::new();
    let task = fx.released_pr(104, true, &fx.base);
    fx.merge(104, &task, BOT, &fx.base);
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    );
    jobs.insert_job_run(
        "task_auto_pipeline",
        1,
        Utc::now(),
        Some(json!({"task_ids": [task]})),
        None,
    )
    .unwrap();
    let error = fx.settle_err(&task, "live leaf refuses");
    assert!(error.contains("live `run auto` leaf carries it"), "{error}");
    assert!(
        fx.settle_all().is_empty(),
        "live leaf: owner pass settles nothing"
    );
    fx.assert_status(&task, TaskStatus::InProgress, "live leaf stays in progress");
}

fn external_merge_settlement_refuses_every_ineligible_status() {
    let fx = Fixture::new();
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::Someday,
        TaskStatus::Done,
        TaskStatus::Archived,
        TaskStatus::Rejected,
    ] {
        let task = fx.add_task(
            "Ineligible status",
            if status == TaskStatus::Archived {
                TaskStatus::Proposed
            } else {
                status
            },
            &[105],
        );
        if status == TaskStatus::Archived {
            fx.runtime.archive_task(&task).unwrap();
        }
        fx.merge(105, &task, BOT, &fx.base.clone());
        let error = fx.settle_err(&task, "ineligible status refuses");
        let condition = if status == TaskStatus::Backlog {
            "backlog task has no preserved failure-handoff requeue"
        } else {
            "only a blocked, in-progress, review or preserved failure-handoff backlog task settles"
        };
        assert!(error.contains(condition), "{status:?}: {error}");
        fx.assert_status(&task, status, "ineligible status unchanged");
    }
    assert!(
        fx.settle_all().is_empty(),
        "no ineligible task settles in owner pass"
    );
}

fn a_blocked_task_settles_from_its_merged_delivery_pr_while_another_task_claims_its_files() {
    let fx = Fixture::new();
    // ORB-16371: the operator merged the blocked task's delivery PR while
    // another task's run held a claim on the same files.
    let merged = fx.add_task("Merged by hand", TaskStatus::Blocked, &[4937]);
    let own_claim = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[50]);
    let other_branch = fx.add_task("Merged elsewhere", TaskStatus::Blocked, &[51]);
    let not_delivery = fx.add_task("Merged by a human", TaskStatus::Review, &[52, 53]);
    let live_run = fx.add_task("Still running", TaskStatus::InProgress, &[54]);
    let holder = fx.add_task("Claim holder", TaskStatus::InProgress, &[]);
    fx.bind_live_run(&live_run);
    fx.claim(&holder, "jrun-20261017-0625-t1");
    fx.claim(&own_claim, "jrun-own-claim");
    fx.merge(4937, &merged, BOT, &fx.base.clone());
    fx.merge(50, &own_claim, BOT, &fx.base.clone());
    fx.merge(51, &other_branch, BOT, "release");
    fx.merge(52, &not_delivery, "alice", &fx.base.clone());
    fx.push_merged(
        53,
        format!("orbit/{not_delivery}-dddd4444"),
        format!("Delivers {not_delivery}0."),
        BOT,
        &fx.base.clone(),
    );
    fx.merge(54, &live_run, BOT, &fx.base.clone());
    // A manual start beside another claim is refused; settlement must not depend on it.
    let refused = fx
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": merged, "status": "in-progress", "model": "codex"}),
        )
        .expect_err("a start beside another task's claim is refused");
    assert!(
        refused
            .to_string()
            .contains("task footprint overlaps an execution claim"),
        "{refused}"
    );
    let settled = fx.settle_all();
    assert_eq!(
        settled
            .iter()
            .map(|s| (s.task_id.as_str(), s.pr_number))
            .collect::<Vec<_>>(),
        vec![(merged.as_str(), 4937)],
        "only the delivery PR merged into the target branch settles its task"
    );
    assert_eq!(
        (settled[0].previous_status, &settled[0].merge_commit),
        (TaskStatus::Blocked, &MERGE_COMMIT.to_string()),
        "blocked settlement identity"
    );
    let task = fx.runtime.get_task(&merged).unwrap();
    assert_eq!(task.status, TaskStatus::Done, "merged task done");
    assert!(
        task.execution_summary.contains("#4937") && task.execution_summary.contains(MERGE_COMMIT),
        "the summary names the landing: {}",
        task.execution_summary
    );
    fx.history_is(&merged, TaskStatus::Blocked, TaskStatus::Done, "history");
    fx.note_has(&merged, &["#4937", MERGE_COMMIT, fx.base.as_str()], "note");
    for (task_id, status, why) in [
        (&own_claim, TaskStatus::Blocked, "its own claim is live"),
        (
            &other_branch,
            TaskStatus::Blocked,
            "it merged into another branch",
        ),
        (
            &not_delivery,
            TaskStatus::Review,
            "no delivery PR of it merged",
        ),
        (&live_run, TaskStatus::InProgress, "its own run is live"),
        (&holder, TaskStatus::InProgress, "it has no PR"),
    ] {
        fx.assert_status(task_id, status, why);
    }
    assert!(
        fx.settle_all().is_empty(),
        "a settled task does not settle twice"
    );
}

fn the_operator_settles_one_task_from_a_merge_older_than_the_listing() {
    let fx = Fixture::new();
    let reviewed = fx.add_task("Approved on the forge", TaskStatus::Review, &[70, 71]);
    let claimed = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[80]);
    fx.claim(&claimed, "jrun-own-claim");
    fx.older_merge(71, &reviewed);
    fx.older_merge(80, &claimed);
    assert!(
        fx.settle_all().is_empty(),
        "the owner pass reads only the recent listing"
    );
    fx.assert_status(&reviewed, TaskStatus::Review, "older merge not in listing");
    let settlement = fx.settle_one(&reviewed);
    assert_eq!(
        (settlement.pr_number, settlement.previous_status),
        (71, TaskStatus::Review),
        "operator settlement"
    );
    fx.assert_done(&reviewed, "operator settles reviewed");
    let refused = fx.settle_err(&claimed, "a task with its own live claim is not settled");
    assert!(
        refused.contains("jrun-own-claim"),
        "the refusal names the task's own claim: {refused}"
    );
    fx.assert_status(&claimed, TaskStatus::Blocked, "claimed stays blocked");
    let refused = fx.settle_err(&reviewed, "a done task does not settle again");
    assert!(refused.contains("'done'"), "{refused}");
}

crate::scenario_table!(
    recovered_rescued_delivery,
    a_recovered_claim_with_an_unrecorded_rescued_pr_can_settle
);

fn a_recovered_claim_with_an_unrecorded_rescued_pr_can_settle() {
    let fx = Fixture::new();
    let (task, claim_id, worker) = fx.bound_claim(4940, LANDING, false);
    let rev = |c: &str, t: &str| SourceRevision {
        commit: c.into(),
        tree: t.into(),
    };
    let handoff = fx.handoff_obj(
        &task,
        &claim_id,
        4940,
        (
            rev("42bfe17000000000000000000000000000000000", &"b".repeat(40)),
            rev(&"c".repeat(40), &"d".repeat(40)),
        ),
        HandoffDelivery::PullRequest { number: 4940 },
        "Follower candidate is complete",
    );
    let observation = HandoffObservation {
        candidate: handoff.candidate.clone(),
        required_commands: vec!["newly-required-check".into()],
        footprint_widening: vec![],
        owner_completion_authority: None,
        review: None,
        known_red: vec![],
        light_delivery: vec![],
    };
    let refusal = fx
        .runtime
        .mutate_execution_claim(
            Some(&worker.with_handoff_observation(observation)),
            "refused-handoff",
            &ClaimMutation::AcceptHandoff(handoff),
        )
        .expect_err("the owner's newly required validation was not in the follower handoff");
    assert!(refusal.to_string().contains("validation"), "{refusal}");
    fx.runtime
        .recover_claim_as_operator(
            &claim_id,
            "running",
            TaskStatus::Blocked,
            "operator",
            "Follower stopped; its refused handoff's PR was rescued by hand",
            ("operator-recover", "recover-rescued"),
        )
        .unwrap();
    fx.merge(4940, &task, BOT, LANDING);
    assert!(
        fx.runtime.get_task(&task).unwrap().external_refs.is_empty(),
        "recovered: no task PR ref"
    );
    assert!(
        fx.boundary()
            .find_accepted_handoff(&claim_id)
            .unwrap()
            .is_none(),
        "recovered: no accepted handoff"
    );
    let merged = fx.forge.0.lock().unwrap().recent[0].clone();
    for case in [
        "other-branch",
        "other-base",
        "other-author",
        "other-task",
        "missing-head",
    ] {
        let mut wrong = merged.clone();
        match case {
            "other-branch" => wrong.pull.head_branch = "unrelated-branch".into(),
            "other-base" => wrong.base_branch = fx.base.clone(),
            "other-author" => wrong.pull.author = "unrelated-author".into(),
            "other-task" => wrong.pull.body = "Delivers some other task".into(),
            _ => wrong.head_sha.clear(),
        }
        fx.forge.0.lock().unwrap().recent[0] = wrong;
        fx.runtime
            .settle_recovered_claim_merge(
                &task,
                &claim_id,
                4940,
                "42bfe17000000000000000000000000000000000",
            )
            .expect_err(case);
        fx.assert_status(&task, TaskStatus::Blocked, case);
    }
    fx.forge.0.lock().unwrap().recent[0] = merged;
    fx.runtime
        .settle_recovered_claim_merge(
            &task,
            &claim_id,
            4940,
            "42bfe17000000000000000000000000000000000",
        )
        .expect("an operator can settle the recovered delivery without force");
    fx.assert_done(&task, "recovered settles");
    fx.assert_phase(
        &claim_id,
        ExecutionClaimPhase::Revoked,
        "recovered claim revoked",
    );
    fx.note_has(
        &task,
        &[
            "operator-rescued",
            &claim_id,
            "42bfe17000000000000000000000000000000000",
            "0dbd8e2000000000000000000000000000000000",
            MERGE_COMMIT,
            LANDING,
        ],
        "recovered note",
    );
}

fn a_revoked_handoff_and_its_claim_settle_from_its_candidate_pr_merged_outside_orbit() {
    for owner_pass in [false, true] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4930, Authority::Revoked);
        let task = handoff.task.as_str();
        fx.assert_phase(
            &handoff.claim_id,
            ExecutionClaimPhase::HandedOff,
            "revoked claim handed off",
        );
        let authority = fx.console_authority(task); // console names the way out
        assert_eq!(
            authority["state"], "revoked",
            "console revoked: {authority}"
        );
        let command = authority["settle_command"]
            .as_str()
            .expect("a revoked pull-request handoff names its settle command");
        assert_eq!(
            command,
            format!("orbit task settle-merged {task}"),
            "console settle command"
        );
        assert!(
            authority["summary"].as_str().unwrap().contains(command),
            "the summary the dashboard shows carries the command: {authority}"
        );

        // A human merged the handoff's own PR into its landing branch.
        fx.merge(4930, task, "alice", LANDING);
        let requests = fx.runtime.landing_start_requests().unwrap();
        let attempts = fx.runtime.landing_attempts().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].state, LandingStartState::Revoked);
        assert_eq!(attempts[0].state, LandingAttemptState::Stopped);

        let settlement = if owner_pass {
            let mut settled = fx.settle_all();
            assert_eq!(settled.len(), 1, "{settled:?}");
            settled.pop().unwrap()
        } else {
            fx.settle_one(task)
        };
        assert_eq!(
            (
                &settlement.task_id,
                settlement.pr_number,
                settlement.previous_status,
                &settlement.base_branch,
                &settlement.merge_commit
            ),
            (
                &task.to_string(),
                4930,
                TaskStatus::Review,
                &LANDING.to_string(),
                &MERGE_COMMIT.to_string()
            ),
            "revoked settlement"
        );
        let settled = fx.runtime.get_task(task).unwrap();
        assert_eq!(settled.status, TaskStatus::Done, "revoked task done");
        assert!(
            settled
                .external_refs
                .contains(&ExternalRef::github_pr("4930".to_string()).unwrap()),
            "the landing PR is recorded on the task: {:?}",
            settled.external_refs
        );
        fx.history_is(
            task,
            TaskStatus::Review,
            TaskStatus::Done,
            "revoked history",
        );
        fx.note_has(task, &["#4930", MERGE_COMMIT, LANDING], "revoked note");

        // The claim settled in the same write and protects nothing.
        let claims = fx.runtime.inspect_execution_claims().unwrap();
        for state in claims.iter().filter(|state| state.claim.task_id == task) {
            assert!(!state.claim.phase.is_unsettled(), "{state:?}");
            assert!(!state.claim.phase.protects_footprint(), "{state:?}");
            assert_eq!(state.unresolved_merge_intent, None);
        }
        // No authorization, landing request or attempt was added or reopened.
        assert_eq!(fx.runtime.landing_start_requests().unwrap(), requests);
        assert_eq!(fx.runtime.landing_attempts().unwrap(), attempts);
        assert!(
            fx.settle_all().is_empty(),
            "a settled task does not settle twice"
        );
    }
}

fn landing_a_stopped_authorized_handoff_reports_its_stop_and_retry_path() {
    for authority in [Authority::Stopped, Authority::Revoked] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4939, authority);
        let before = fx.runtime.landing_attempts().unwrap();
        let error = fx
            .runtime
            .land_task_pull_request(&handoff.task, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("task_landing_refused"),
            "{authority:?}: {error}"
        );
        if authority == Authority::Stopped {
            assert!(
                error.contains("required checks are not green")
                    && error.contains("retry-handoff")
                    && !error.contains("operator revoked"),
                "{error}"
            );
        } else {
            assert!(error.contains("operator revoked"), "{error}");
        }
        assert_eq!(
            fx.runtime.landing_attempts().unwrap(),
            before,
            "{authority:?}"
        );
        fx.assert_phase(
            &handoff.claim_id,
            ExecutionClaimPhase::HandedOff,
            "stopped/revoked claim handed off",
        );
        fx.assert_status(
            &handoff.task,
            TaskStatus::Review,
            "stopped/revoked task in review",
        );
    }
}

fn a_handed_off_claim_refuses_settlement_unless_its_revoked_handoffs_own_pr_merged() {
    for (case, authority, reason) in [
        (
            "unapproved",
            Authority::Unapproved,
            "awaits completion approval",
        ),
        (
            "authorized",
            Authority::Authorized,
            "holds completion authority",
        ),
        (
            "dispatched",
            Authority::RevokedWhileDispatched,
            "an owner landing job still carries",
        ),
        ("other-pr", Authority::Revoked, "recover the claim"),
        ("other-branch", Authority::Revoked, "not its landing branch"),
        ("unmerged", Authority::Revoked, "recover the claim"),
    ] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4931, authority);
        let task = handoff.task.as_str();
        match case {
            // only the handoff's own PR counts
            "other-pr" => fx.merge(4932, task, BOT, LANDING),
            "other-branch" => fx.merge(4931, task, BOT, &fx.base.clone()),
            "unmerged" => fx.mark_unmerged(4931, "open"),
            _ => fx.merge(4931, task, BOT, LANDING),
        }
        let refused = fx.settle_err(task, case);
        assert!(refused.contains(reason), "{case}: {refused}");
        assert!(fx.settle_all().is_empty(), "{case}");
        fx.assert_status(task, TaskStatus::Review, case);
        fx.assert_phase(&handoff.claim_id, ExecutionClaimPhase::HandedOff, case);
        if authority == Authority::Authorized {
            assert!(
                fx.console_authority(task)["settle_command"].is_null(),
                "only a revoked handoff names settle-merged"
            );
        }
    }
}

fn external_merge_recovery_suggestions_include_the_observed_claim_phase() {
    let recover = |claim: &str, phase: &str| {
        format!(
            "orbit task recover-claim {claim} --phase {phase} --status blocked --reason <reason> --request <key>"
        )
    };
    for phase in ["claimed", "running", "repair_pending"] {
        let fx = Fixture::new();
        let task = fx.add_task("Live claim refusal", TaskStatus::InProgress, &[4935]);
        fx.claim_with_phase(&task, &format!("live-{phase}"), phase);
        fx.merge(4935, &task, BOT, &fx.base.clone());
        let refusal = fx.settle_err(&task, "a live claim refuses settlement");
        assert!(
            refusal.contains(&recover(&format!("claim-live-{phase}"), phase)),
            "{phase}: {refusal}"
        );
    }
    for (case, authority) in [
        ("unapproved", Authority::Unapproved),
        ("authorized-pending", Authority::Authorized),
    ] {
        let fx = Fixture::new();
        let handoff = fx.handed_off(4936, authority);
        fx.merge(4936, &handoff.task, BOT, LANDING);
        let refusal = fx.settle_err(&handoff.task, case);
        assert!(
            refusal.contains(&recover(&handoff.claim_id, "handed_off")),
            "{case}: {refusal}"
        );
    }
    let fx = Fixture::new();
    let handoff =
        fx.handed_off_with_delivery(4937, Authority::Revoked, HandoffDelivery::LocalCandidate);
    let refusal = fx.settle_err(
        &handoff.task,
        "a revoked handoff without a pull request refuses settlement",
    );
    assert!(
        refusal.contains(&recover(&handoff.claim_id, "handed_off")),
        "{refusal}"
    );
}

fn the_owner_store_settles_a_revoked_handoff_and_its_task_in_one_write_or_neither() {
    fn settle(
        fx: &Fixture,
        handoff: &Handoff,
        number: u64,
        branch: &str,
        mutation_id: &str,
    ) -> Result<orbit_store::contracts::ClaimMutationResult, OrbitError> {
        let operator = ClaimInvocation::trusted_operator(
            handoff.task.clone(),
            handoff.claim_id.clone(),
            "operator".into(),
        );
        fx.runtime.mutate_execution_claim(
            Some(&operator),
            mutation_id,
            &ClaimMutation::SettleExternalMerge {
                handoff_id: handoff.handoff_id.clone(),
                pull_request: number,
                base_branch: branch.into(),
                merge_commit: MERGE_COMMIT.into(),
                note: format!("settled from pull request #{number}"),
                verified_candidate_head: None,
            },
        )
    }
    let authorized_fx = Fixture::new(); // each claim holds the one file: own owner per fixture
    let authorized = authorized_fx.handed_off(4933, Authority::Authorized);
    let fx = Fixture::new();
    let revoked = fx.handed_off(4934, Authority::Revoked);
    for (owner, handoff, number, branch, why) in [
        (
            &authorized_fx,
            &authorized,
            4933,
            LANDING,
            "its authority is not revoked",
        ),
        (
            &fx,
            &revoked,
            4933,
            LANDING,
            "another handoff's pull request",
        ),
        (&fx, &revoked, 4934, "main", "a merge into another branch"),
    ] {
        assert!(
            settle(
                owner,
                handoff,
                number,
                branch,
                &format!("refused-{number}-{branch}")
            )
            .is_err(),
            "{why}"
        );
        owner.assert_status(&handoff.task, TaskStatus::Review, why);
        owner.assert_phase(&handoff.claim_id, ExecutionClaimPhase::HandedOff, why);
    }
    let result = settle(&fx, &revoked, 4934, LANDING, "settle").expect("the exact settlement");
    assert_eq!(
        (result.phase, result.status),
        (ExecutionClaimPhase::Revoked, TaskStatus::Done),
        "owner store settlement"
    );
    fx.assert_done(&revoked.task, "owner store task done");
    fx.assert_phase(
        &revoked.claim_id,
        ExecutionClaimPhase::Revoked,
        "owner store claim revoked",
    );
}
/// Template body without a placeholder settles via the identity comment;
/// another author/branch, no token, or a longer id does not.
fn a_template_body_without_a_task_placeholder_still_settles_an_external_merge() {
    let fx = Fixture::new();
    let (task, bare, human, other_base, other_head, lookalike) = (
        fx.add_task("Fix the widget", TaskStatus::Review, &[61]),
        fx.add_task("Template text only", TaskStatus::Review, &[62]),
        fx.add_task("Merged by a human", TaskStatus::Review, &[63]),
        fx.add_task("Merged elsewhere", TaskStatus::Review, &[64]),
        fx.add_task("Another branch", TaskStatus::Review, &[65]),
        fx.add_task("Lookalike id", TaskStatus::Review, &[66]),
    );
    let idc = |id: &str| format!("<!-- orbit-delivery-task: {id} -->");
    let body = |id: &str| format!("## Summary\n\nFix the widget\n\n{}", idc(id));
    let base = fx.base.clone();
    for (n, head, b, author, bs) in [
        (
            61,
            format!("orbit/{task}-0123abcd"),
            body(&task),
            BOT,
            base.as_str(),
        ),
        (
            62,
            format!("orbit/{bare}-0123abcd"),
            "## Summary\n\nFix the widget".into(),
            BOT,
            base.as_str(),
        ),
        (
            63,
            format!("orbit/{human}-0123abcd"),
            body(&human),
            "alice",
            base.as_str(),
        ),
        (
            64,
            format!("orbit/{other_base}-0123abcd"),
            body(&other_base),
            BOT,
            "release",
        ),
        (
            65,
            format!("orbit/{other_head}0-0123abcd"),
            body(&other_head),
            BOT,
            base.as_str(),
        ),
        (
            66,
            format!("orbit/{lookalike}-0123abcd"),
            body(&format!("{lookalike}0")),
            BOT,
            base.as_str(),
        ),
    ] {
        fx.push_merged(n, head, b, author, bs);
    }
    // Row 66 body must name only the longer id, as `body(&format!(...))` does.
    let settlement = fx.settle_one(&task);
    assert_eq!(
        (settlement.pr_number, &settlement.task_id),
        (61, &task),
        "template settlement"
    );
    fx.assert_done(&task, "template settles");
    for (id, why) in [
        (&bare, "the template text has no task token"),
        (&human, "a human author is not the delivery identity"),
        (&other_base, "it merged into another branch"),
        (&other_head, "the head branch names a longer id"),
        (&lookalike, "the body names only a longer id"),
    ] {
        assert!(
            fx.runtime.settle_externally_merged_task(id).is_err(),
            "{why}"
        );
        fx.assert_status(id, TaskStatus::Review, why);
    }
}

crate::scenario_table!(
    sibling_delivery_contract,
    external_merge_does_not_settle_a_sibling_delivery_named_in_an_external_ref
);

fn external_merge_does_not_settle_a_sibling_delivery_named_in_an_external_ref() {
    let fx = Fixture::with_config(
        "[branch]\nname_template = 'feature/{{ext.jira}}-{{task_id}}-{{hash}}'\n",
    );
    let sibling = fx.add_task("Mentioned task", TaskStatus::Review, &[61]);
    let owner = fx.add_task("Merged task", TaskStatus::Review, &[61]);
    fx.push_merged(
        61,
        format!("feature/X-{sibling}-{owner}-0123abcd"),
        format!("Delivers {owner}; follows {sibling}."),
        BOT,
        &fx.base.clone(),
    );
    assert!(
        fx.runtime.settle_externally_merged_task(&sibling).is_err(),
        "sibling does not settle"
    );
    fx.assert_status(&sibling, TaskStatus::Review, "sibling stays in review");
    let settlement = fx.settle_one(&owner);
    assert_eq!(
        (settlement.pr_number, &settlement.task_id),
        (61, &owner),
        "sibling owner settlement"
    );
    fx.assert_done(&owner, "sibling owner done");
}

fn clock_tick_settles_blocked_and_review_tasks_without_a_drain() {
    let fx = Fixture::new();
    let blocked = fx.add_task("Tick blocked", TaskStatus::Blocked, &[601]);
    let reviewed = fx.add_task("Tick reviewed", TaskStatus::Review, &[602]);
    fx.merge(601, &blocked, BOT, &fx.base.clone());
    fx.merge(602, &reviewed, BOT, &fx.base.clone());
    let dry = tick(&fx, true, false);
    assert!(dry.external_merge_settlements.is_empty(), "{dry:?}");
    fx.assert_status(&blocked, TaskStatus::Blocked, "dry tick settles nothing");
    let outcome = tick(&fx, false, false);
    fx.assert_done(&blocked, "tick blocked done");
    fx.assert_done(&reviewed, "tick reviewed done");
    let mut settled: Vec<(&str, u64)> = outcome
        .external_merge_settlements
        .iter()
        .map(|r| (r.task_id.as_str(), r.pr_number))
        .collect();
    settled.sort();
    let mut expected = vec![(blocked.as_str(), 601), (reviewed.as_str(), 602)];
    expected.sort();
    assert_eq!(settled, expected, "{outcome:?}");
    assert!(
        outcome
            .external_merge_settlements
            .iter()
            .all(|r| r.source == "tick-workspace"),
        "tick source"
    );
    for task in [&blocked, &reviewed] {
        fx.history_to(task, TaskStatus::Done, "tick history");
        fx.note_has(task, &[MERGE_COMMIT], "tick note");
    }
}

fn clock_tick_leaves_guarded_tasks_and_follower_hosts_unsettled() {
    let fx = Fixture::new();
    let live = fx.add_task("Tick live run", TaskStatus::InProgress, &[611]);
    let claimed = fx.add_task("Tick claimed", TaskStatus::Blocked, &[612]);
    fx.bind_live_run(&live);
    fx.claim(&claimed, "jrun-tick-claim");
    fx.merge(611, &live, BOT, &fx.base.clone());
    fx.merge(612, &claimed, BOT, &fx.base.clone());
    let outcome = tick(&fx, false, false);
    assert!(outcome.external_merge_settlements.is_empty(), "{outcome:?}");
    fx.assert_status(&live, TaskStatus::InProgress, "tick live guarded");
    fx.assert_status(&claimed, TaskStatus::Blocked, "tick claimed guarded");
    let eligible = fx.add_task("Tick follower", TaskStatus::Blocked, &[613]); // replica settles nothing
    fx.merge(613, &eligible, BOT, &fx.base.clone());
    let replica = tick(&fx, false, true);
    assert!(replica.external_merge_settlements.is_empty(), "{replica:?}");
    fx.assert_status(&eligible, TaskStatus::Blocked, "tick follower unsettled");
}

fn clock_tick_forge_failure_does_not_fail_the_tick() {
    let fx = Fixture::new();
    let task = fx.add_task("Tick forge down", TaskStatus::Blocked, &[621]);
    fx.merge(621, &task, BOT, &fx.base.clone());
    fx.set_forge_down(true);
    let outcome = tick(&fx, false, false);
    assert!(outcome.external_merge_settlements.is_empty(), "{outcome:?}");
    fx.assert_status(&task, TaskStatus::Blocked, "forge down stays blocked");
    fx.set_forge_down(false);
    let outcome = tick(&fx, false, false);
    assert_eq!(
        outcome.external_merge_settlements.len(),
        1,
        "forge recovered settles one"
    );
    fx.assert_done(&task, "forge recovered done");
}

fn stopped_handoff_settles_a_deleted_branch_after_two_operator_updates() {
    for foreign in [false, true] {
        let fx = Fixture::with_config(
            "[workflow]\nbase_branch = \"release-landing\"\ndistributed_completion = \"done\"\n",
        );
        let handoff = fx.handed_off(4938, Authority::Stopped);
        let repo = fx.runtime.paths().repo_root.clone();
        let accepted = fx.runtime.accepted_task_handoff(&handoff.claim_id).unwrap();
        let candidate = &accepted.handoff.candidate;
        let remote = fx._root.path().join("remote.git");
        let remote_str = remote.to_str().unwrap().to_string();
        settlement_git(&repo, &["clone", "-q", "--bare", ".", &remote_str]);
        let mut head = candidate.candidate.commit.clone();
        for update in 1..=2 {
            let msg = format!("base update {update}");
            let base = commit_tree(&remote, &tree_of(&remote, LANDING), &[LANDING], &msg);
            update_ref(&remote, &format!("refs/heads/{LANDING}"), &base);
            let tree = settlement_git(&remote, &["merge-tree", "--write-tree", &head, &base]);
            head = commit_tree(&remote, &tree, &[&head, &base], "operator base update");
        }
        if foreign {
            let tree = tree_of(&remote, &head);
            head = commit_tree(&remote, &tree, &[&head], "foreign source commit");
        }
        let merge = commit_tree(
            &remote,
            &tree_of(&remote, &head),
            &[LANDING],
            "external squash merge",
        );
        update_ref(&remote, &format!("refs/heads/{LANDING}"), &merge);
        update_ref(&remote, "refs/pull/4938/head", &head);
        let branch_ref = format!("refs/heads/{}", candidate.source_branch);
        settlement_git(&remote, &["update-ref", "-d", &branch_ref]);
        settlement_git(&repo, &["remote", "add", "origin", &remote_str]);
        let absent = crate::git_test_support::command()
            .args(["cat-file", "-e", &head])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            !absent.status.success(),
            "updated PR head starts outside the owner checkout"
        );
        fx.merge(4938, &handoff.task, "alice", LANDING);
        {
            let mut state = fx.forge.0.lock().unwrap();
            state.recent[0].head_sha = head.clone();
            state.recent[0].merge_commit = merge.clone();
        }
        let result = fx.settle_all();
        if foreign {
            assert!(result.is_empty(), "foreign: settles nothing");
            let error = fx.settle_err(&handoff.task, "foreign refuses");
            assert!(error.contains("reconcile_review"), "{error}");
            fx.assert_status(&handoff.task, TaskStatus::Review, "foreign stays review");
            fx.assert_phase(
                &handoff.claim_id,
                ExecutionClaimPhase::HandedOff,
                "foreign handed off",
            );
        } else {
            assert_eq!(result.len(), 1, "stopped settles one");
            fx.assert_done(&handoff.task, "stopped done");
            fx.assert_phase(
                &handoff.claim_id,
                ExecutionClaimPhase::Landed,
                "stopped landed",
            );
            assert_eq!(
                fx.runtime.landing_attempts().unwrap()[0].state,
                LandingAttemptState::Merged,
                "stopped attempt merged"
            );
            assert_eq!(
                fx.runtime.landing_start_requests().unwrap()[0].state,
                LandingStartState::Completed,
                "stopped request completed"
            );
            assert!(
                fx.runtime
                    .get_task_history(&handoff.task)
                    .unwrap()
                    .iter()
                    .any(|r| r.event == EXTERNAL_MERGE_SETTLED_EVENT
                        && r.note.as_ref().is_some_and(|n| n.contains(&head))),
                "stopped history names head"
            );
        }
    }
}

crate::scenario_table!(
    mergeability_unknown_landing,
    land_waits_out_unknown_mergeability_then_merges,
    land_refuses_unknown_mergeability_after_the_budget,
    land_refuses_a_conflicting_pull_request_immediately,
);

fn land_err(fx: &Fixture, task: &str, case: &str) -> String {
    fx.runtime
        .land_task_pull_request(task, Some("operator"))
        .expect_err(case)
        .to_string()
}

fn land_waits_out_unknown_mergeability_then_merges() {
    let _wait = virtual_mergeability_wait();
    let (fx, forge, task) = mergeability_land(
        &[
            MergeabilityAnswer::Unknown,
            MergeabilityAnswer::Unknown,
            MergeabilityAnswer::Mergeable,
        ],
        MergeabilityAnswer::Mergeable,
    );
    let started = Instant::now();
    let landed = fx
        .runtime
        .land_task_pull_request(&task, Some("operator"))
        .expect("unknown mergeability yields to a later mergeable read");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the budget advanced without sleeping"
    );
    assert_eq!(
        (landed.pull_request, &landed.merge_commit, landed.status),
        (LAND_PR, &MERGE_COMMIT.to_string(), TaskStatus::Done),
        "landed identity"
    );
    assert!(landed.settlement_pending.is_none(), "{landed:?}");
    assert_eq!(forge.reads(), 3, "two unknown reads, then one mergeable");
    assert_eq!(
        forge.merges(),
        vec![(LAND_PR, LAND_HEAD.to_string())],
        "land merges once"
    );
    fx.assert_done(&task, "land waits done");
}

fn land_refuses_unknown_mergeability_after_the_budget() {
    let _wait = virtual_mergeability_wait();
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Unknown);
    let started = Instant::now();
    let message = land_err(
        &fx,
        &task,
        "the budget refuses a pull request that stays unknown",
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the budget advanced without sleeping"
    );
    assert!(
        message.contains("task_landing_refused")
            && message.contains(&format!("pull request #{LAND_PR} mergeability is unknown")),
        "{message}"
    );
    assert_eq!(forge.reads(), 6, "the fixed budget is six reads");
    assert!(
        forge.merges().is_empty(),
        "unknown mergeability never merges"
    );
    fx.assert_status(&task, TaskStatus::Review, "budget refusal stays review");
}

fn land_refuses_a_conflicting_pull_request_immediately() {
    let _wait = virtual_mergeability_wait();
    let (fx, forge, task) = mergeability_land(
        &[MergeabilityAnswer::Conflicting],
        MergeabilityAnswer::Mergeable,
    );
    let started = Instant::now();
    let message = land_err(&fx, &task, "a conflict is a refusal");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a conflict does not wait"
    );
    assert!(
        message.contains("task_landing_refused")
            && message.contains(&format!("pull request #{LAND_PR} conflicts with its base")),
        "{message}"
    );
    assert!(
        !message.contains("mergeability is unknown"),
        "a conflict is not the unknown-mergeability refusal: {message}"
    );
    assert_eq!(forge.reads(), 1, "a conflict is refused on the first read");
    assert!(forge.merges().is_empty(), "conflict never merges");
    fx.assert_status(&task, TaskStatus::Review, "conflict stays review");
}
