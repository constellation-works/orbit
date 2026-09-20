//! The owner's durable landing attempt [ORB-12499].

use super::handoff::{accept, approve, fixture, observation, operator, starts, worker};
use super::*;
use crate::contracts::*;
use orbit_types::workflow::handoff::*;

fn dispatch(
    f: &Coordinated,
    c: &ExecutionClaim,
    id: &str,
    handoff_id: &str,
    job_run_id: Option<&str>,
) -> Result<ClaimMutationResult, OrbitError> {
    f.boundary().mutate_execution_claim(
        Some(&operator(c)),
        id,
        &ClaimMutation::DispatchLanding {
            handoff_id: handoff_id.into(),
            job_run_id: job_run_id.map(ToOwned::to_owned),
        },
    )
}

fn complete(
    f: &Coordinated,
    c: &ExecutionClaim,
    h: &TaskHandoff,
    id: &str,
    handoff_id: &str,
    evidence: &str,
) -> Result<ClaimMutationResult, OrbitError> {
    f.boundary().mutate_execution_claim(
        Some(&operator(c).with_handoff_observation(observation(h))),
        id,
        &ClaimMutation::CompleteLanding {
            handoff_id: handoff_id.into(),
            evidence: evidence.into(),
        },
    )
}

fn stop(
    f: &Coordinated,
    c: &ExecutionClaim,
    id: &str,
    handoff_id: &str,
    reason: &str,
) -> Result<ClaimMutationResult, OrbitError> {
    f.boundary().mutate_execution_claim(
        Some(&operator(c)),
        id,
        &ClaimMutation::StopLanding {
            handoff_id: handoff_id.into(),
            reason: reason.into(),
        },
    )
}

fn intent(id: &str, resolved: bool) -> ClaimMutation {
    ClaimMutation::MergeIntent {
        intent_id: id.into(),
        resolved,
        evidence: "provider request for the pinned candidate".into(),
    }
}

fn attempt(f: &Coordinated, handoff_id: &str) -> LandingAttempt {
    f.boundary()
        .landing_attempt(handoff_id)
        .expect("read attempts")
        .expect("attempt exists")
}

fn authorized(f: &Coordinated, c: &ExecutionClaim, h: &TaskHandoff) -> String {
    accept(f, c, h).expect("handoff");
    approve(f, c, h, "approval").expect("approve");
    starts(f)[0].handoff_id.clone()
}

#[test]
fn one_open_attempt_per_handoff_survives_restart_and_reopens_only_deliberately() {
    let (tmp, f, c, h) = fixture(super::admission::request("first").ship);
    let handoff_id = authorized(&f, &c, &h);

    // Reserved before any job exists, then attached to the job that carries it.
    dispatch(&f, &c, "open", &handoff_id, None).expect("open attempt");
    assert_eq!(attempt(&f, &handoff_id).job_run_id, None);
    dispatch(&f, &c, "attach", &handoff_id, Some("landing-run-1")).expect("attach");
    let first = attempt(&f, &handoff_id);
    assert_eq!(first.attempt, 1);
    assert_eq!(first.job_run_id.as_deref(), Some("landing-run-1"));
    assert_eq!(first.state, LandingAttemptState::Dispatched);
    assert_eq!(first.task_id, c.task_id);

    // A restart reads the same attempt; the same attach replays rather than
    // creating a second one, and a different job cannot take the open attempt.
    let restarted = Coordinated::open(tmp.path());
    assert_eq!(attempt(&restarted, &handoff_id), first);
    dispatch(&restarted, &c, "attach", &handoff_id, Some("landing-run-1")).expect("replay");
    assert!(
        dispatch(&restarted, &c, "second", &handoff_id, Some("landing-run-2"))
            .expect_err("a second consumer cannot take the open attempt")
            .to_string()
            .contains("already in flight")
    );
    assert_eq!(
        restarted.boundary().landing_attempts().expect("read").len(),
        1
    );

    // Stopping is the only thing that reopens, and it reopens as attempt two.
    stop(&restarted, &c, "stop", &handoff_id, "base moved").expect("stop");
    let stopped = attempt(&restarted, &handoff_id);
    assert_eq!(stopped.state, LandingAttemptState::Stopped);
    assert_eq!(stopped.evidence.as_deref(), Some("base moved"));
    assert_eq!(
        restarted.task(&c.task_id).status,
        TaskStatus::Review,
        "a stopped landing leaves the candidate in review"
    );
    dispatch(&restarted, &c, "retry", &handoff_id, None).expect("deliberate retry");
    let retried = attempt(&restarted, &handoff_id);
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.state, LandingAttemptState::Dispatched);
    assert_eq!(retried.job_run_id, None);
    assert_eq!(retried.evidence, None);
}

#[test]
fn completion_requires_an_open_attempt_resolved_intent_and_current_authority() {
    let (_tmp, f, c, h) = fixture(super::admission::request("first").ship);
    accept(&f, &c, &h).expect("handoff");
    let handoff_id = f
        .boundary()
        .accepted_handoff(&c.claim_id)
        .expect("accepted")
        .handoff_id;

    // Review-only work has no authorization yet: neither dispatch nor
    // completion may invent one.
    assert!(
        dispatch(&f, &c, "unapproved", &handoff_id, None)
            .expect_err("no completion authority")
            .to_string()
            .contains("awaits completion approval")
    );
    approve(&f, &c, &h, "approval").expect("approve");

    // Without an attempt there is nothing to complete.
    assert!(
        complete(&f, &c, &h, "early", &handoff_id, "merged")
            .expect_err("no attempt")
            .to_string()
            .contains("no landing attempt")
    );
    dispatch(&f, &c, "open", &handoff_id, Some("landing-run-1")).expect("open");

    // An unresolved external merge intent blocks completion until it is
    // reconciled against real state.
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c).with_handoff_observation(observation(&h))),
            "intent",
            &intent("sent", false),
        )
        .expect("publish intent");
    assert!(
        complete(&f, &c, &h, "uncertain", &handoff_id, "merged")
            .expect_err("uncertain intent")
            .to_string()
            .contains("unresolved external merge intent")
    );
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c).with_handoff_observation(observation(&h))),
            "reconcile",
            &intent("sent", true),
        )
        .expect("reconcile");

    assert!(
        complete(&f, &c, &h, "unproven", &handoff_id, "   ")
            .expect_err("evidence required")
            .to_string()
            .contains("verified merge evidence required")
    );
    // A worker cannot complete its own landing, whatever it observed.
    assert!(
        f.boundary()
            .mutate_execution_claim(
                Some(&worker(&c).with_handoff_observation(observation(&h))),
                "worker-complete",
                &ClaimMutation::CompleteLanding {
                    handoff_id: handoff_id.clone(),
                    evidence: "merged".into(),
                },
            )
            .is_err()
    );

    let done = complete(
        &f,
        &c,
        &h,
        "complete",
        &handoff_id,
        "pull request #42 merged as abc",
    )
    .expect("complete");
    assert_eq!(done.status, TaskStatus::Done);
    assert_eq!(done.phase, ExecutionClaimPhase::Landed);
    assert_eq!(f.task(&c.task_id).status, TaskStatus::Done);
    assert_eq!(attempt(&f, &handoff_id).state, LandingAttemptState::Merged);
    assert_eq!(starts(&f)[0].state, LandingStartState::Completed);
    assert!(
        f.history(&c.task_id)
            .iter()
            .any(|e| e.event == "landing_completed"
                && e.note.as_deref() == Some("pull request #42 merged as abc")),
        "the evidence that permitted done is recorded in task history"
    );
}

#[test]
fn a_landed_handoff_cannot_be_redispatched_revoked_or_reassigned() {
    let (_tmp, f, c, h) = fixture(super::admission::request("first").ship);
    let handoff_id = authorized(&f, &c, &h);
    dispatch(&f, &c, "open", &handoff_id, Some("landing-run-1")).expect("open");
    complete(&f, &c, &h, "complete", &handoff_id, "merged as abc").expect("complete");

    assert!(
        dispatch(&f, &c, "again", &handoff_id, None)
            .expect_err("landed handoffs do not land twice")
            .to_string()
            .contains("stale_claim"),
        "a settled claim refuses every further mutation"
    );
    for mutation in [
        ClaimMutation::RevokeHandoff {
            handoff_id: handoff_id.clone(),
            reason: "withdraw".into(),
        },
        ClaimMutation::Recover {
            status: TaskStatus::Backlog,
            reason: "reassign".into(),
        },
        intent("second-send", false),
    ] {
        assert!(
            f.boundary()
                .mutate_execution_claim(
                    Some(&operator(&c).with_handoff_observation(observation(&h))),
                    "after-landing",
                    &mutation,
                )
                .is_err()
        );
    }
    assert_eq!(f.task(&c.task_id).status, TaskStatus::Done);
}

#[test]
fn revoked_authority_stops_dispatch_and_completion_for_the_same_candidate() {
    let (_tmp, f, c, h) = fixture(super::admission::request("first").ship);
    let handoff_id = authorized(&f, &c, &h);
    dispatch(&f, &c, "open", &handoff_id, Some("landing-run-1")).expect("open");
    f.boundary()
        .mutate_execution_claim(
            Some(&operator(&c)),
            "revoke",
            &ClaimMutation::RevokeHandoff {
                handoff_id: handoff_id.clone(),
                reason: "withdrawn".into(),
            },
        )
        .expect("revoke");

    assert!(dispatch(&f, &c, "after-revoke", &handoff_id, None).is_err());
    assert!(complete(&f, &c, &h, "after-revoke", &handoff_id, "merged").is_err());
    assert_eq!(f.task(&c.task_id).status, TaskStatus::Review);
    assert_eq!(starts(&f)[0].state, LandingStartState::Revoked);
    assert_eq!(
        attempt(&f, &handoff_id).state,
        LandingAttemptState::Dispatched,
        "the attempt is fenced by the revoked authority, not rewritten by it"
    );
}

#[test]
fn an_owner_local_candidate_lands_through_the_same_authority_boundary() {
    // Delivery variants differ in what the owner has to observe externally,
    // not in what this boundary decides; the variant-specific observation is
    // covered where it happens, in the landing activity.
    let mut ship = super::admission::request("first").ship;
    ship.mode = "local".into();
    let (_tmp, f, c, mut h) = fixture(ship);
    h.candidate.delivery = HandoffDelivery::LocalCandidate;
    let h = super::handoff::with_validation_logs(&f, &c, h);
    accept(&f, &c, &h).expect("local handoff");
    approve(&f, &c, &h, "approval").expect("approve");
    let handoff_id = starts(&f)[0].handoff_id.clone();

    dispatch(&f, &c, "open", &handoff_id, Some("landing-run-1")).expect("open");
    complete(
        &f,
        &c,
        &h,
        "complete",
        &handoff_id,
        "landing ref contains the candidate",
    )
    .expect("complete");
    assert_eq!(f.task(&c.task_id).status, TaskStatus::Done);
    assert_eq!(attempt(&f, &handoff_id).state, LandingAttemptState::Merged);
    assert_eq!(starts(&f)[0].state, LandingStartState::Completed);
}
