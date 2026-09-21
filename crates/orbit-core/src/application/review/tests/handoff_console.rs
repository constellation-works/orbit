//! The owner handoff console [ORB-12516]: what an authorized owner surface
//! reads, and what it is refused.
//!
//! Every scenario runs against the real coordination store over a real
//! admission, because the whole point of the console is that its refusals are
//! the store's rather than a second copy of them. Nothing here mutates a live
//! host: the claim is admitted for a fictitious `follower` machine inside a
//! temporary workspace.

use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::*;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::workflow::handoff::*;
use orbit_types::workflow::{ReviewTiming, automation::SourceRevision};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::tool_host::test_support::{create_context_task, test_runtime};
use crate::application::review::ExpectedCandidate;

/// One owner holding a claim on a `follower` machine, optionally already
/// handed off.
struct Console {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    task_id: String,
    claim_id: String,
    candidate: HandoffCandidate,
    worker: ClaimInvocation,
}

fn console(handed_off: bool) -> Console {
    let (root, runtime, repo) = test_runtime();
    let task = create_context_task(&runtime, &repo, TaskStatus::Backlog, &["src/a.rs"]);
    let workspace_id = runtime.workspace_id().expect("workspace");
    let boundary = TaskCommitBoundary::new(
        runtime.sqlite_store().expect("store"),
        TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).expect("registry"),
        workspace_id.clone(),
    )
    .expect("boundary");
    let ship = AdmissionShipContract {
        mode: "pr".into(),
        base_branch: "agent-main".into(),
        landing_branch: "agent-main".into(),
        review_policy: "none".into(),
        completion: "review".into(),
        authorization_reference: None,
    };
    let admission = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(ExecutionLocation {
                machine_id: "follower".into(),
                machine_name: Some("runner-2".into()),
            }),
            &AdmissionRequest {
                request_id: "pull".into(),
                caller_version: "test".into(),
                caller_schema: 1,
                caller_review_policy: "none".into(),
                run_context: AdmissionRunContext {
                    run_id: "drain".into(),
                    job_name: "auto".into(),
                    machine_name: None,
                },
                ship: ship.clone(),
            },
            "test",
            &repo,
            &runtime.data_root(),
        )
        .expect("admission");
    let AdmissionLookup::Found { receipt, .. } = admission else {
        panic!("receipt")
    };
    let claim = receipt.claim.expect("claim");
    let run = ClaimRun {
        machine_id: "follower".into(),
        run_id: "leaf".into(),
    };
    runtime
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task.id.clone(),
                claim.claim_id.clone(),
                "follower".into(),
                None,
            )),
            "bind",
            &ClaimMutation::Bind {
                run: run.clone(),
                ship,
            },
        )
        .expect("bind");
    let worker = ClaimInvocation::trusted_worker(
        task.id.clone(),
        claim.claim_id.clone(),
        "follower".into(),
        Some(run),
    );
    let candidate = HandoffCandidate {
        repository: "owner/repository".into(),
        source_branch: "attempt/leaf".into(),
        base_branch: "agent-main".into(),
        landing_branch: "agent-main".into(),
        candidate: SourceRevision {
            commit: "a".repeat(40),
            tree: "b".repeat(40),
        },
        base: SourceRevision {
            commit: "c".repeat(40),
            tree: "d".repeat(40),
        },
        delivery: HandoffDelivery::PullRequest { number: 7 },
    };
    if handed_off {
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: workspace_id.clone(),
            task_id: task.id.clone(),
            claim_id: claim.claim_id.clone(),
            machine_id: "follower".into(),
            run_id: "leaf".into(),
            candidate: candidate.clone(),
            tested_head: candidate.candidate.commit.clone(),
            command: "make ci".into(),
            exit_code: 0,
            output: "captured successful checks".into(),
        };
        let content = serde_json::to_vec(&log).expect("log");
        let reference = HandoffArtifactRef {
            path: "checks.json".into(),
            sha256: format!("{:x}", Sha256::digest(&content)),
        };
        runtime
            .mutate_execution_claim(
                Some(&worker),
                "evidence",
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts: vec![TaskArtifact {
                        path: reference.path.clone(),
                        content,
                        media_type: "application/json".into(),
                        created_by: None,
                    }],
                    ..Default::default()
                }),
            )
            .expect("owner artifact");
        runtime
            .accept_task_handoff(
                &worker,
                "handoff",
                TaskHandoff {
                    schema_version: 1,
                    workspace_id,
                    task_id: task.id.clone(),
                    claim_id: claim.claim_id.clone(),
                    machine_id: "follower".into(),
                    run_id: "leaf".into(),
                    candidate: candidate.clone(),
                    review: HandoffReview {
                        policy: ReviewTiming::None,
                        disposition: HandoffReviewDisposition::NotRequired,
                    },
                    execution_summary: "Outcome: success".into(),
                    validation: vec![reference],
                },
                HandoffObservation {
                    candidate: candidate.clone(),
                    required_commands: vec!["make ci".into()],
                },
            )
            .expect("accept");
    }
    Console {
        _root: root,
        runtime,
        task_id: task.id,
        claim_id: claim.claim_id,
        candidate,
        worker,
    }
}

impl Console {
    fn read(&self) -> Value {
        self.runtime
            .distributed_claim_console()
            .expect("console read")
    }

    fn claim(&self) -> Value {
        self.read()["claims"]
            .as_array()
            .expect("claims")
            .first()
            .cloned()
            .expect("one claim")
    }

    fn handoff_id(&self) -> String {
        self.runtime
            .accepted_task_handoff(&self.claim_id)
            .expect("accepted")
            .handoff_id
    }

    fn expected(&self) -> ExpectedCandidate {
        ExpectedCandidate {
            candidate_commit: self.candidate.candidate.commit.clone(),
            base_commit: self.candidate.base.commit.clone(),
        }
    }
}

#[test]
fn the_console_reports_machine_qualified_execution_and_a_live_claim() {
    let console = console(false);
    let claim = console.claim();

    assert_eq!(claim["executed_on"]["known"], true);
    assert_eq!(claim["executed_on"]["machine_id"], "follower");
    assert_eq!(claim["executed_on"]["machine_name"], "runner-2");
    assert_eq!(claim["phase"], "running");
    assert_eq!(claim["authorizes_execution"], true);
    assert_eq!(claim["bound_run"]["machine_id"], "follower");
    // The run lives in the follower's job store, so there is nothing local to
    // open — the console says where to look instead of inventing a link.
    assert_eq!(claim["bound_run_navigable"], false);
    assert!(
        claim["inspect_on"]
            .as_str()
            .expect("inspect hint")
            .contains("follower"),
        "{claim}"
    );
    assert_eq!(claim["handoff"], Value::Null);
    assert_eq!(claim["footprint_protected"], true);
    assert_eq!(claim["unresolved_merge_intent"], Value::Null);
}

/// An elapsed reservation is a diagnostic. The claim is still live, its frozen
/// footprint still protects its files, and nothing about expiry says the
/// attempt was revoked or died.
#[test]
fn an_expired_reservation_is_reported_without_implying_revocation() {
    let console = console(false);
    let claim = console.claim();
    let reservation = &claim["reservation"];

    assert!(reservation["expires_at"].is_string());
    assert_eq!(reservation["expired"], false);
    assert_eq!(claim["phase"], "running");

    let note = reservation["note"].as_str().expect("note");
    assert!(!note.contains("revok"), "{note}");

    // The projection's own vocabulary: no phase reads as revoked here, and the
    // expiry language never promotes itself into a settlement.
    let rendered = console.read().to_string();
    assert!(!rendered.contains("\"phase\":\"revoked\""), "{rendered}");
}

#[test]
fn a_handed_off_claim_reports_its_candidate_typed_review_and_pending_authority() {
    let console = console(true);
    let claim = console.claim();
    let handoff = &claim["handoff"];

    assert_eq!(claim["phase"], "handed_off");
    assert_eq!(handoff["candidate"]["repository"], "owner/repository");
    assert_eq!(
        handoff["candidate"]["candidate"]["commit"],
        console.candidate.candidate.commit
    );
    assert_eq!(
        handoff["candidate"]["base"]["commit"],
        console.candidate.base.commit
    );
    assert_eq!(handoff["candidate"]["delivery"]["kind"], "pull_request");

    // A typed not-required disposition is not a review that passed.
    assert_eq!(handoff["review"]["disposition"], "not_required");
    assert_eq!(handoff["review"]["is_code_review"], false);
    assert_eq!(handoff["required_commands"][0], "make ci");
    assert_eq!(handoff["validation"][0]["path"], "checks.json");

    assert_eq!(handoff["authority"]["state"], "not_authorized");
    assert_eq!(handoff["landing"]["state"], "none");
    // Merged is a fact about the landing branch, never about a deployment.
    assert_eq!(handoff["landing"]["merged"], false);
    assert_eq!(handoff["landing"]["deployed"], Value::Null);
}

#[test]
fn approval_records_authority_and_revocation_withdraws_it_without_completing_the_task() {
    let console = console(true);
    let handoff_id = console.handoff_id();

    let approved = console
        .runtime
        .approve_handoff_as_operator(&handoff_id, &console.expected(), "human", "req-1")
        .expect("approve");
    assert_eq!(approved["handoff_id"], handoff_id);
    assert_eq!(approved["task_status"], "review");

    let claim = console.claim();
    assert_eq!(claim["handoff"]["authority"]["state"], "authorized");
    assert!(
        claim["handoff"]["authority"]["authorization_id"].is_string(),
        "{claim}"
    );

    // A retry of the same decision replays the recorded outcome rather than
    // recording a second authorization.
    let replayed = console
        .runtime
        .approve_handoff_as_operator(&handoff_id, &console.expected(), "human", "req-1")
        .expect("replay");
    assert_eq!(replayed["handoff_id"], handoff_id);
    assert_eq!(
        console
            .runtime
            .landing_start_requests()
            .expect("outbox")
            .len(),
        1
    );

    console
        .runtime
        .revoke_handoff_as_operator(
            &handoff_id,
            &console.expected(),
            "human",
            "withdrawn for diagnosis",
            "req-2",
        )
        .expect("revoke");
    let claim = console.claim();
    assert_eq!(claim["handoff"]["authority"]["state"], "revoked");
    // Revocation withdraws authority; it does not decide what happens to the
    // work, so the task stays where the handoff left it.
    assert_eq!(
        console
            .runtime
            .get_task(&console.task_id)
            .expect("task")
            .status,
        TaskStatus::Review
    );
}

/// The candidate the operator was shown is an expectation, not the source of
/// what gets approved. A stale page is refused rather than approving whatever
/// the owner happens to hold now.
#[test]
fn a_stale_candidate_expectation_cannot_approve() {
    let console = console(true);
    let handoff_id = console.handoff_id();
    let stale = ExpectedCandidate {
        candidate_commit: "f".repeat(40),
        base_commit: console.candidate.base.commit.clone(),
    };

    let error = console
        .runtime
        .approve_handoff_as_operator(&handoff_id, &stale, "human", "req-1")
        .expect_err("stale expectation refused");
    assert!(error.to_string().contains("stale_claim"), "{error}");
    assert!(
        console
            .runtime
            .landing_start_requests()
            .expect("outbox")
            .is_empty()
    );
}

#[test]
fn an_unknown_handoff_is_refused_rather_than_matched_to_another_claim() {
    let console = console(true);
    let error = console
        .runtime
        .approve_handoff_as_operator(&"0".repeat(64), &console.expected(), "human", "req-1")
        .expect_err("unknown handoff refused");
    assert!(
        error.to_string().contains("is current on this owner"),
        "{error}"
    );
}

#[test]
fn recovery_requires_the_phase_the_operator_saw_a_reason_and_a_permitted_target() {
    let console = console(false);

    let wrong_phase = console
        .runtime
        .recover_claim_as_operator(
            &console.claim_id,
            "handed_off",
            TaskStatus::Blocked,
            "human",
            "attempt is over",
            "req-1",
        )
        .expect_err("phase expectation enforced");
    assert!(
        wrong_phase.to_string().contains("stale_claim"),
        "{wrong_phase}"
    );

    let no_reason = console
        .runtime
        .recover_claim_as_operator(
            &console.claim_id,
            "running",
            TaskStatus::Blocked,
            "human",
            "   ",
            "req-2",
        )
        .expect_err("reason required");
    assert!(no_reason.to_string().contains("reason"), "{no_reason}");

    let wrong_target = console
        .runtime
        .recover_claim_as_operator(
            &console.claim_id,
            "running",
            TaskStatus::Done,
            "human",
            "attempt is over",
            "req-3",
        )
        .expect_err("target enforced");
    assert!(
        wrong_target.to_string().contains("blocked or backlog"),
        "{wrong_target}"
    );

    let recovered = console
        .runtime
        .recover_claim_as_operator(
            &console.claim_id,
            "running",
            TaskStatus::Backlog,
            "human",
            "host was rebuilt",
            "req-4",
        )
        .expect("recover");
    assert_eq!(recovered["phase"], "revoked");
    assert_eq!(recovered["task_status"], "backlog");
}

/// A sleeping worker that returns after recovery gets `stale_claim`: its old
/// candidate cannot become authoritative task state.
#[test]
fn a_recovered_claim_refuses_the_old_attempts_writes() {
    let console = console(false);
    console
        .runtime
        .recover_claim_as_operator(
            &console.claim_id,
            "running",
            TaskStatus::Blocked,
            "human",
            "host lost",
            "req-1",
        )
        .expect("recover");

    let error = console
        .runtime
        .mutate_execution_claim(
            Some(&console.worker),
            "late-evidence",
            &ClaimMutation::Evidence(ClaimEvidence {
                summary: Some("work from the fenced attempt".into()),
                ..Default::default()
            }),
        )
        .expect_err("fenced attempt refused");
    assert!(error.to_string().contains("stale_claim"), "{error}");
}

/// An external merge whose reply was lost blocks revocation and recovery until
/// it is reconciled: a database row cannot cancel a request already sent.
#[test]
fn an_unresolved_merge_intent_blocks_revocation_and_recovery() {
    let console = console(true);
    let handoff_id = console.handoff_id();
    console
        .runtime
        .approve_handoff_as_operator(&handoff_id, &console.expected(), "human", "req-1")
        .expect("approve");

    // The landing consumer's own seam: recording a send intent carries the
    // owner's observation of the accepted candidate, exactly as the landing job
    // supplies it.
    let operator = ClaimInvocation::trusted_operator(
        console.task_id.clone(),
        console.claim_id.clone(),
        "owner-landing".into(),
    )
    .with_handoff_observation(HandoffObservation {
        candidate: console.candidate.clone(),
        required_commands: vec!["make ci".into()],
    });
    console
        .runtime
        .mutate_execution_claim(
            Some(&operator),
            "intent",
            &ClaimMutation::MergeIntent {
                intent_id: "intent-1".into(),
                resolved: false,
                evidence: "merge request sent to the provider".into(),
            },
        )
        .expect("record intent");

    let claim = console.claim();
    assert_eq!(claim["unresolved_merge_intent"], "intent-1");
    assert_eq!(claim["handoff"]["uncertain_merge_intent"], "intent-1");

    for error in [
        console
            .runtime
            .revoke_handoff_as_operator(
                &handoff_id,
                &console.expected(),
                "human",
                "withdraw",
                "req-2",
            )
            .expect_err("revocation blocked"),
        console
            .runtime
            .recover_claim_as_operator(
                &console.claim_id,
                "handed_off",
                TaskStatus::Blocked,
                "human",
                "give up",
                "req-3",
            )
            .expect_err("recovery blocked"),
    ] {
        assert!(
            error
                .to_string()
                .contains("unresolved external merge intent"),
            "{error}"
        );
        assert_eq!(
            crate::application::review::HandoffConsoleRefusal::classify(&error),
            Some(crate::application::review::HandoffConsoleRefusal::UncertainMerge),
        );
    }
}

/// A replica checkout holds no claim state and may not write any. The read
/// answers honestly instead of erroring so a workspace switch is not a fault.
#[test]
fn a_replica_checkout_serves_no_claims_and_refuses_every_action() {
    let console = console(true);
    let handoff_id = console.handoff_id();
    let expected = console.expected();
    let Console {
        _root,
        runtime,
        claim_id,
        ..
    } = console;
    let replica = runtime.with_coordination_write_owner(Some("owner-machine".into()));

    let read = replica.distributed_claim_console().expect("replica read");
    assert_eq!(read["owner_workspace"], false);
    assert_eq!(read["refusal"], "replica_checkout");
    assert_eq!(read["claims"].as_array().expect("claims").len(), 0);

    for error in [
        replica
            .approve_handoff_as_operator(&handoff_id, &expected, "human", "req-1")
            .expect_err("replica approval refused"),
        replica
            .revoke_handoff_as_operator(&handoff_id, &expected, "human", "withdraw", "req-2")
            .expect_err("replica revocation refused"),
        replica
            .recover_claim_as_operator(
                &claim_id,
                "handed_off",
                TaskStatus::Blocked,
                "human",
                "give up",
                "req-3",
            )
            .expect_err("replica recovery refused"),
    ] {
        assert_eq!(
            crate::application::review::HandoffConsoleRefusal::classify(&error),
            Some(crate::application::review::HandoffConsoleRefusal::ReplicaCheckout),
            "{error}"
        );
    }
}
