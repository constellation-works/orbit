//! A claimed leaf resuming a held candidate reads the owner's evidence
//! findings on it between the nested `orbit`'s bridge and [`RunDispatch`]
//! over the broker's real socket [ORB-18667], on the final-recovery
//! fixture's leaf admitted, created, bound and launched through its durable
//! pull admission.
//!
//! Admitted as a reproduced defect and a security invariant: the owner's
//! comment carries findings only up to its 4096-byte budget, and before the
//! fix the reviewer-only scope refused the claimed implementer's read of the
//! whole artifact. The read is bound to the kept candidate the claim's
//! receipt offered, and every other `review-*` artifact stays refused without
//! reaching the owner.
//!
//! [`RunDispatch`]: super::super::brokered::RunDispatch

use base64::Engine as _;
use orbit_store::contracts::{ClaimCandidateRef, ClaimEvidence, ClaimMutation, LocalPullMutation};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    FINAL_RECOVERY_ACTIVITY, REVIEW_EVIDENCE_FINDINGS_ARTIFACT, REVIEW_EVIDENCE_HOLD_ARTIFACT,
    REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, ReviewEvidenceFinding, ReviewEvidenceFindings,
    ReviewEvidenceFlowStep,
};
use serde_json::{Value, json};

use super::claimed_recovery::{Leaf, refused_with};
use super::claimed_review::{GET, PUT, TASK};

const FINDINGS: &str = REVIEW_EVIDENCE_FINDINGS_ARTIFACT;
const IMPLEMENT: &str = "agent_implement";
const HELD_RUN: &str = "jrun-held-1";
const HELD_COMMIT: &str = "1111111111111111111111111111111111111111";

/// The held candidate an earlier claim's evidence hold kept, as the owner
/// offers it to the next claim.
fn kept() -> ClaimCandidateRef {
    ClaimCandidateRef {
        branch: format!("orbit-evidence/orbit/{TASK}"),
        head_sha: HELD_COMMIT.into(),
        pull_request: None,
        pull_request_head: None,
        source_run_id: Some(HELD_RUN.into()),
        failed_step_id: None,
        published: true,
        durable_ref: None,
        carry_failure: None,
        interrupted_review: None,
        landing_stop: None,
    }
}

/// An owner evidence run's findings on `commit` of held run `hold_run`: a
/// first result whose eight flow steps each carry a 500-byte message, then a
/// second result at `src/later.rs:99`. Both fit the per-result limits.
fn findings(hold_run: &str, commit: &str) -> ReviewEvidenceFindings {
    let step = |line: u64| ReviewEvidenceFlowStep {
        file: Some("src/first.rs".into()),
        line: Some(line),
        message: Some("m".repeat(500)),
    };
    ReviewEvidenceFindings {
        schema_version: 1,
        hold_run_id: hold_run.into(),
        run_id: "jrun-fulfil-1".into(),
        attempt_id: "attempt-1".into(),
        candidate: SourceRevision {
            commit: commit.into(),
            tree: "2".repeat(40),
        },
        results: vec![
            ReviewEvidenceFinding {
                check: "Rust CodeQL (Linux)".into(),
                rule: "rust/cleartext-logging".into(),
                message: "Sensitive data reaches a log.".into(),
                file: Some("src/first.rs".into()),
                line: Some(1),
                code_flow: (1..=8).map(step).collect(),
            },
            ReviewEvidenceFinding {
                check: "Rust CodeQL (Linux)".into(),
                rule: "rust/sql-injection".into(),
                message: "A request parameter reaches a query.".into(),
                file: Some("src/later.rs".into()),
                line: Some(99),
                code_flow: vec![
                    ReviewEvidenceFlowStep {
                        file: Some("src/request.rs".into()),
                        line: Some(12),
                        message: Some("source".into()),
                    },
                    ReviewEvidenceFlowStep {
                        file: Some("src/later.rs".into()),
                        line: Some(99),
                        message: Some("sink".into()),
                    },
                ],
            },
        ],
        omitted: 0,
    }
}

fn read_findings(
    fixture: &Leaf,
    broker: &crate::runtime::plugin::broker::PluginBroker,
) -> Result<Value, String> {
    fixture.forwarded(
        broker,
        GET,
        json!({"id": TASK, "path": FINDINGS, "model": "codex"}),
    )
}

#[test]
fn a_claimed_implementer_reads_every_finding_on_the_candidate_it_resumes() {
    let reported = findings(HELD_RUN, HELD_COMMIT);
    let summary = reported.summary_text();
    let later = summary
        .find("`rust/sql-injection` at src/later.rs:99")
        .unwrap();
    assert!(
        later > 4096,
        "the later result lies past the owner comment's 4096-byte budget: at byte {later} of {}",
        summary.len()
    );

    let fixture = Leaf::admit_resuming(Some(kept()));
    fixture
        .owner
        .hold_as_system(FINDINGS, &serde_json::to_vec(&reported).unwrap());
    let broker = fixture.serve(IMPLEMENT);
    let read = read_findings(&fixture, &broker).expect("the resuming implementer reads them");
    let carried: ReviewEvidenceFindings =
        serde_json::from_str(read["content"].as_str().unwrap()).unwrap();
    assert_eq!(carried, reported, "every retained result crosses whole");
    let later = &carried.results[1];
    assert_eq!(later.rule, "rust/sql-injection");
    assert_eq!(later.location(), "src/later.rs:99");
    assert_eq!(later.code_flow, reported.results[1].code_flow);

    assert_eq!(fixture.owner.reads(), vec![FINDINGS.to_string()]);
    for (_, input, session) in fixture.owner.calls.lock().unwrap().iter() {
        assert_eq!(input["id"], TASK, "{input}");
        assert_eq!(session.worker_invocation.as_ref(), Some(&fixture.binding));
    }
    let rows = fixture
        .runtime
        .list_audit_events(None, Some(GET.to_string()), None, None, 20)
        .unwrap();
    assert!(
        !rows.is_empty()
            && rows.iter().all(|row| row.brokered
                && row.task_id.as_deref() == Some(TASK)
                && row.activity_id.as_deref() == Some(IMPLEMENT)),
        "the broker audits the read for the implementer: {rows:?}"
    );
}

/// Security invariant: the findings read widens nothing else of the review
/// namespace and is bound to the claim's kept candidate and a live leaf. The
/// gate's other artifacts, a write, a respelled name, another task and a
/// final recovery stay refused without reaching the owner, as do a claim that
/// resumes nothing, another run and a settling claim; findings on another
/// held run or commit, or not attached by Orbit's system writer, are refused.
#[test]
fn the_findings_read_is_bound_to_the_kept_candidate_and_opens_nothing_else() {
    let fixture = Leaf::admit_resuming(Some(kept()));
    fixture.owner.hold_as_system(
        FINDINGS,
        &serde_json::to_vec(&findings(HELD_RUN, HELD_COMMIT)).unwrap(),
    );
    let broker = fixture.serve(IMPLEMENT);
    for path in [
        REVIEW_MANIFEST_ARTIFACT,
        REVIEW_REPORT_ARTIFACT,
        REVIEW_REPORT_HISTORY_ARTIFACT,
        REVIEW_EVIDENCE_HOLD_ARTIFACT,
        REVIEW_GATE_ARTIFACT,
        "review-certificate.json",
    ] {
        fixture.owner.hold_as_system(path, b"{}");
        refused_with(
            fixture.forwarded(&broker, GET, json!({"id": TASK, "path": path})),
            "only for the before-PR reviewer",
            path,
        );
    }
    let forged = base64::engine::general_purpose::STANDARD.encode(b"{}");
    refused_with(
        fixture.raw(
            &broker,
            PUT,
            json!({"id": TASK, "path": FINDINGS, "content_base64": forged}),
        ),
        "only for the before-PR reviewer",
        "a findings write",
    );
    // A name that only normalises to the artifact is refused by this scope;
    // another case is another `review-*` path, which stays the reviewer's.
    for (path, reason) in [
        (format!(" {FINDINGS}"), "findings_read_refused"),
        (format!("./{FINDINGS}"), "findings_read_refused"),
        (
            FINDINGS.to_ascii_uppercase(),
            "only for the before-PR reviewer",
        ),
    ] {
        refused_with(
            fixture.raw(&broker, GET, json!({"id": TASK, "path": path})),
            reason,
            &format!("{path:?}"),
        );
    }
    refused_with(
        fixture.forwarded(&broker, GET, json!({"id": "TSO-2", "path": FINDINGS})),
        "a task other than the claimed task",
        "another task's findings",
    );
    let recovery = fixture.serve(FINAL_RECOVERY_ACTIVITY);
    refused_with(
        read_findings(&fixture, &recovery),
        "review_read_refused",
        "a final recovery",
    );
    fixture.owner_untouched("another review artifact");

    let fixture = Leaf::admit_resuming(None);
    fixture.owner.hold_as_system(
        FINDINGS,
        &serde_json::to_vec(&findings(HELD_RUN, HELD_COMMIT)).unwrap(),
    );
    let broker = fixture.serve(IMPLEMENT);
    refused_with(
        read_findings(&fixture, &broker),
        "no_kept_candidate",
        "a claim without a kept candidate",
    );
    fixture.owner_untouched("a claim without a kept candidate");

    let fixture = Leaf::admit_resuming(Some(kept()));
    let mut elsewhere = fixture.run(IMPLEMENT);
    elsewhere.job_run_id = Some("jrun-elsewhere".into());
    let other = fixture.serve_run(&fixture.runtime, elsewhere);
    refused_with(
        read_findings(&fixture, &other),
        "not_claimed_leaf",
        "another run",
    );
    fixture.owner_untouched("another run");

    let broker = fixture.serve(IMPLEMENT);
    for (case, stored) in [
        ("another held run", findings("jrun-other", HELD_COMMIT)),
        ("another commit", findings(HELD_RUN, &"3".repeat(40))),
    ] {
        fixture
            .owner
            .hold_as_system(FINDINGS, &serde_json::to_vec(&stored).unwrap());
        refused_with(
            read_findings(&fixture, &broker),
            "findings_candidate_mismatch",
            case,
        );
    }
    fixture.owner.forget(FINDINGS);
    fixture.owner.hold(
        FINDINGS,
        &serde_json::to_vec(&findings(HELD_RUN, HELD_COMMIT)).unwrap(),
    );
    refused_with(
        read_findings(&fixture, &broker),
        "findings_unverified",
        "findings not written by the system",
    );

    let fixture = Leaf::admit_resuming(Some(kept()));
    let broker = fixture.serve(IMPLEMENT);
    fixture.advance(LocalPullMutation::Settle(Box::new(ClaimMutation::Release(
        ClaimEvidence {
            summary: Some("The executor gave the claim back.".into()),
            ..ClaimEvidence::default()
        },
    ))));
    refused_with(
        read_findings(&fixture, &broker),
        "claim_not_live",
        "a settling claim",
    );
    fixture.owner_untouched("a settling claim");
}
