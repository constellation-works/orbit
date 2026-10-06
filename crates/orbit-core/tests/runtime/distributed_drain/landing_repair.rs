//! A handoff whose landing stopped on its base is repaired once, without an
//! operator [ORB-14261].
//!
//! The owner's landing records a repairable stop when the candidate conflicts
//! with, or is stale against, its base. The claim then waits in
//! `repair_pending`, the next pull by its original executor admits a repair
//! claim whose leaf carries the preserved candidate, and that leaf's new
//! handoff lands under the same task. A second such stop blocks the task with
//! both attempts' evidence. The leaf itself does not run in this binary (see
//! the module root); the engine's `candidate_resume` tests cover what the
//! repair leaf does with `claim_repair`.

use super::*;

use orbit_engine::{HandoffLandingStep, HandoffLandingUpdate};

const FIRST_STOP: &str = "pull request #42 conflicts with its base";
const SECOND_STOP: &str = "pull request #43 is behind its base";

/// A base that gained a conflicting change stops the first landing. The
/// follower's next pull takes the repair, its leaf re-hands off a rebased
/// candidate under the same task, and that one lands: no operator acts
/// between the stop and the landing.
#[test]
fn a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task() {
    if !isolated(
        module_path!(),
        "a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task",
    ) {
        return;
    }
    let pair = Pair::new(1);
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let first = handed_off_leaf(&pair, &drain, None);
    let task = pair.claimed_task(&first);
    let stopped = pair.land(&task, Landing::Stop(FIRST_STOP));

    let claim = pair.owner_claim(&stopped.claim_id);
    assert_eq!(claim["phase"], "repair_pending", "{claim}");
    assert_eq!(pair.owner_status(&task), "in-progress");

    let repair = handed_off_leaf(&pair, &drain, Some(rebased));
    assert_eq!(
        pair.claimed_task(&repair),
        task,
        "the repair is the same task"
    );
    let input = pair
        .follower_jobs
        .get_job_run(&repair)
        .unwrap()
        .unwrap()
        .input
        .expect("leaf input");
    let carried = &input["claim_repair"];
    assert_eq!(carried["repairs_claim_id"], stopped.claim_id.as_str());
    assert_eq!(carried["handoff_id"], stopped.handoff_id.as_str());
    assert_eq!(carried["head_sha"], "a".repeat(40));
    assert_eq!(carried["branch"], format!("orbit/{task}"));
    assert!(
        carried["stop_evidence"]
            .as_str()
            .is_some_and(|evidence| evidence.contains(FIRST_STOP)),
        "the repair leaf sees why the landing stopped: {carried}"
    );
    assert_eq!(
        pair.owner_claim(&stopped.claim_id)["phase"],
        "revoked",
        "the repair claim supersedes the stopped one"
    );

    let landed = pair.land(&task, Landing::Complete);
    assert_ne!(landed.claim_id, stopped.claim_id);
    assert_eq!(pair.owner_claim(&landed.claim_id)["phase"], "landed");
    assert_eq!(pair.owner_status(&task), "done");
}

/// The repair's own landing stops on its base too: there is no second
/// automatic repair. The task blocks with a comment carrying both attempts'
/// candidates and stop evidence, and no further pull takes it.
#[test]
fn a_second_base_stop_blocks_the_task_with_both_attempts_evidence() {
    if !isolated(
        module_path!(),
        "a_second_base_stop_blocks_the_task_with_both_attempts_evidence",
    ) {
        return;
    }
    let pair = Pair::new(1);
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let first = handed_off_leaf(&pair, &drain, None);
    let task = pair.claimed_task(&first);
    let stopped = pair.land(&task, Landing::Stop(FIRST_STOP));
    handed_off_leaf(&pair, &drain, Some(rebased));

    let exhausted = pair.land(&task, Landing::Stop(SECOND_STOP));

    assert_eq!(pair.owner_claim(&exhausted.claim_id)["phase"], "failed");
    assert_eq!(pair.owner_status(&task), "blocked");
    let comments = comments_of(&pair.owner_task(&task));
    for evidence in [
        FIRST_STOP,
        SECOND_STOP,
        stopped.handoff_id.as_str(),
        exhausted.handoff_id.as_str(),
        &"a".repeat(40),
        &"e".repeat(40),
    ] {
        assert!(
            comments.contains(evidence),
            "{evidence} in the blocked task's comments: {comments}"
        );
    }
    let pass = pair.pass(&drain);
    assert!(pass["error"].is_null(), "{pass}");
    assert_eq!(
        pair.leaf_runs().len(),
        2,
        "a blocked task admits no further repair: {pass}"
    );
}

/// A repair leaf's candidate: rebased onto a base that moved.
fn rebased(handoff: &mut TaskHandoff) {
    handoff.candidate.candidate.commit = "e".repeat(40);
    handoff.candidate.base.commit = "f".repeat(40);
    handoff.candidate.delivery = HandoffDelivery::PullRequest { number: 43 };
    handoff.execution_summary = "Outcome: success\nRepaired a resumed candidate".into();
}

/// Admit the owner's next claim as a running leaf and settle its handoff,
/// shaped by `shape`, on the owner.
fn handed_off_leaf(pair: &Pair, drain: &str, shape: Option<fn(&mut TaskHandoff)>) -> String {
    let leaf = pair.running_leaf(drain, 1);
    let mut handed_off = handoff(&pair.admission(&leaf));
    if let Some(shape) = shape {
        shape(&mut handed_off);
    }
    pair.advance(
        &leaf,
        LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(handed_off))),
    );
    pair.follower_jobs
        .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .expect("leaf finished");
    let settled = pair.pass(drain);
    assert!(settled["error"].is_null(), "{settled}");
    leaf
}

/// How the owner's landing job ends one attempt.
enum Landing {
    Stop(&'static str),
    Complete,
}

/// The claim and handoff an owner landing attempt carried.
struct Landed {
    claim_id: String,
    handoff_id: String,
}

impl Pair {
    /// Approve `task`'s handed-off claim and end its landing attempt as the
    /// landing job would: a base conflict is a repairable stop, a merge is a
    /// completion over the accepted candidate.
    fn land(&self, task: &str, landing: Landing) -> Landed {
        let owner = &self.wire.owner;
        let claim_id = self
            .owner_claims()
            .into_iter()
            .find(|claim| {
                claim["claim"]["task_id"] == task && claim["claim"]["phase"] == "handed_off"
            })
            .and_then(|claim| claim["claim"]["claim_id"].as_str().map(str::to_string))
            .unwrap_or_else(|| panic!("no handed-off claim for {task}: {:?}", self.owner_claims()));
        let accepted = owner
            .accepted_task_handoff(&claim_id)
            .expect("accepted handoff");
        let (handoff_id, candidate) = (accepted.handoff_id, accepted.handoff.candidate);
        let operator = ClaimInvocation::trusted_operator(
            task.into(),
            claim_id.clone(),
            "owner-operator".into(),
        );
        owner
            .approve_task_handoff(
                &operator,
                &format!("approve:{handoff_id}"),
                handoff_id.clone(),
                candidate.clone(),
                HandoffObservation {
                    footprint_widening: vec![],
                    candidate: candidate.clone(),
                    required_commands: owner.workflow_required_validation_commands().to_vec(),
                    owner_completion_authority: None,
                    review: None,
                },
            )
            .expect("operator approves the handoff");
        let dispatched = owner
            .landing_attempts()
            .unwrap()
            .into_iter()
            .any(|attempt| attempt.handoff_id == handoff_id);
        assert!(dispatched, "approval dispatches the landing attempt");
        let (step, observed, evidence) = match landing {
            Landing::Stop(reason) => (
                HandoffLandingStep::Stop { repairable: true },
                None,
                serde_json::to_string(reason).unwrap(),
            ),
            Landing::Complete => (
                HandoffLandingStep::Complete,
                Some(candidate),
                "pull request merged with merge commit".to_string(),
            ),
        };
        RuntimeHost::record_handoff_landing(
            owner,
            &HandoffLandingUpdate {
                handoff_id: handoff_id.clone(),
                step,
                observed,
                evidence,
            },
        )
        .expect("the landing job records its outcome");
        Landed {
            claim_id,
            handoff_id,
        }
    }

    /// The owner's record of claim `claim_id`.
    fn owner_claim(&self, claim_id: &str) -> Value {
        self.owner_claims()
            .into_iter()
            .find(|claim| claim["claim"]["claim_id"] == claim_id)
            .map(|claim| claim["claim"].clone())
            .unwrap_or_else(|| panic!("no owner claim {claim_id}"))
    }
}
