//! Shared review coverage rules [ORB-11333].

mod validation;

use super::*;
use chrono::{TimeZone, Utc};
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::{
    LandingTransformation, REVIEW_CONTRACT_VERSION, ReviewAssurance, ReviewBudget,
    ReviewConsumption, ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome,
    ValidationRole,
};

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap()
}

fn revision(label: &str) -> SourceRevision {
    SourceRevision {
        commit: format!("commit-{label}"),
        tree: format!("tree-{label}"),
    }
}

fn task() -> Task {
    Task {
        id: "ORB-1".into(),
        title: "Title".into(),
        description: "Description".into(),
        acceptance_criteria: vec!["Criterion".into()],
        tags: vec!["b".into(), "a".into()],
        required_tools: vec![],
        plan: "Plan".into(),
        execution_summary: String::new(),
        context_files: vec!["file:src/lib.rs".into()],
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Feature,
        pr_status: None,
        external_refs: vec![],
        relations: vec![],
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn certificate(verdict: ReviewVerdict) -> ReviewCertificate {
    ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: "rvw-1".into(),
        lineage_key: "ws/ORB-1/agent-main".into(),
        task_ids: vec!["ORB-1".into()],
        task_meaning_digest: "meaning".into(),
        repository: "owner/repo".into(),
        base: revision("base"),
        reviewed_candidate: revision("impl"),
        final_candidate: revision("final"),
        implementation_commits: vec![],
        repair_commits: vec![],
        verdict,
        assurance: verdict.assurance(),
        findings: vec![],
        validation: vec![ReviewValidation {
            command: "make ci-fast".into(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: None,
        }],
        validation_complete: true,
        reviewer: ReviewerIdentity {
            crew: "reviewers".into(),
            provider: "codex".into(),
            model: "gpt-5".into(),
            reasoning_effort: None,
            implementer_model: Some("gpt-5".into()),
            same_model_as_implementer: true,
        },
        consumed: ReviewConsumption::default(),
        budget: ReviewBudget::default(),
        escalation: None,
        selectors_widened: Vec::new(),
        issued_at: now(),
    }
}

fn delivery(before: &str, after: &str) -> Delivery {
    Delivery {
        key: "pr:owner/repo:agent-main:7".into(),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        before: revision(before),
        after: revision(after),
        commits: vec!["landed".into()],
        task_ids: vec![],
        evidence_reference: "https://github.com/owner/repo/pull/7".into(),
        evidence_digest: "digest".into(),
        landed_at: now(),
    }
}

fn facts() -> LandingFacts {
    LandingFacts {
        objects_present: true,
        task_meaning_current: true,
        managed_landing: None,
    }
}

#[test]
fn task_meaning_digest_ignores_audit_writes_but_not_criteria() {
    let base = task();
    let digest = task_meaning_digest(&base).unwrap();

    let mut commented = base.clone();
    commented.execution_summary = "Outcome: success".into();
    commented.status = TaskStatus::Review;
    commented.priority = TaskPriority::High;
    commented.tags = vec!["a".into(), "b".into()];
    assert_eq!(task_meaning_digest(&commented).unwrap(), digest);

    let mut edited = base.clone();
    edited.acceptance_criteria.push("Another criterion".into());
    assert_ne!(task_meaning_digest(&edited).unwrap(), digest);

    let mut rescoped = base;
    rescoped.context_files.push("dir:crates".into());
    assert_ne!(task_meaning_digest(&rescoped).unwrap(), digest);

    let combined =
        combined_task_meaning_digest(&[("ORB-2".into(), "y".into()), ("ORB-1".into(), "x".into())])
            .unwrap();
    let reordered =
        combined_task_meaning_digest(&[("ORB-1".into(), "x".into()), ("ORB-2".into(), "y".into())])
            .unwrap();
    assert_eq!(combined, reordered);
}

#[test]
fn only_passed_and_fully_validated_certificates_are_acceptable() {
    assert!(certificate_acceptable(&certificate(ReviewVerdict::PassedWithoutRepairs)).is_ok());
    assert!(certificate_acceptable(&certificate(ReviewVerdict::PassedWithRepairs)).is_ok());
    assert_eq!(
        certificate_acceptable(&certificate(ReviewVerdict::ChangesRequired)),
        Err(ReviewInvalidation::VerdictNotPassed)
    );
    assert_eq!(
        certificate_acceptable(&certificate(ReviewVerdict::Incomplete)),
        Err(ReviewInvalidation::VerdictNotPassed)
    );

    let mut denied = certificate(ReviewVerdict::PassedWithoutRepairs);
    denied.validation[0].outcome = ValidationOutcome::Denied;
    assert_eq!(
        certificate_acceptable(&denied),
        Err(ReviewInvalidation::ValidationIncomplete)
    );

    let mut unvalidated = certificate(ReviewVerdict::PassedWithoutRepairs);
    unvalidated.validation.clear();
    assert_eq!(
        certificate_acceptable(&unvalidated),
        Err(ReviewInvalidation::ValidationIncomplete)
    );

    let mut stale_contract = certificate(ReviewVerdict::PassedWithoutRepairs);
    stale_contract.schema_version = REVIEW_CONTRACT_VERSION + 1;
    assert_eq!(
        certificate_acceptable(&stale_contract),
        Err(ReviewInvalidation::MappingUnknown)
    );
}

#[test]
fn coverage_reads_the_same_validation_contract_the_gate_settled_under() {
    // A certificate the gate passed under the repaired contract stays
    // acceptable coverage: the negative control and the excluded deployment
    // do not make it incomplete.
    let mut classified = certificate(ReviewVerdict::PassedWithoutRepairs);
    classified.validation.extend([
        ReviewValidation {
            command: "grep -q 'Strict-Transport-Security' old-config".into(),
            outcome: ValidationOutcome::Failed,
            role: ValidationRole::ExpectedFailure,
            note: Some("negative control: the superseded assertion must fail".into()),
            check: None,
        },
        ReviewValidation {
            command: "wrangler deploy".into(),
            outcome: ValidationOutcome::NotRun,
            role: ValidationRole::Excluded,
            note: Some("live deployment is outside the authorized scope".into()),
            check: None,
        },
    ]);
    assert!(certificate_acceptable(&classified).is_ok());

    // The flag alone never carries a certificate whose records contradict it.
    let mut contradicted = classified.clone();
    contradicted.validation[1].outcome = ValidationOutcome::Passed;
    assert!(contradicted.validation_complete);
    assert_eq!(
        certificate_acceptable(&contradicted),
        Err(ReviewInvalidation::ValidationIncomplete)
    );

    let mut unexplained = classified;
    unexplained.validation[2].note = None;
    assert_eq!(
        certificate_acceptable(&unexplained),
        Err(ReviewInvalidation::ValidationIncomplete)
    );
}

#[test]
fn coverage_refuses_a_superseded_test_replaced_only_by_an_unrelated_formatter() {
    let mut loophole = certificate(ReviewVerdict::PassedWithoutRepairs);
    loophole.validation = vec![
        ReviewValidation {
            command: "cargo test".into(),
            outcome: ValidationOutcome::Failed,
            role: ValidationRole::Superseded,
            note: Some("rerun after repair".into()),
            check: None,
        },
        ReviewValidation {
            command: "cargo fmt --check".into(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: None,
        },
    ];
    assert_eq!(
        certificate_acceptable(&loophole),
        Err(ReviewInvalidation::ValidationIncomplete),
        "an unrelated later formatter cannot certify a superseded test"
    );
}

#[test]
fn coverage_accepts_a_superseded_attempt_replaced_by_a_related_corrected_rerun() {
    let mut related = certificate(ReviewVerdict::PassedWithoutRepairs);
    related.validation = vec![
        ReviewValidation {
            command: "cargo test --package orbit-core".into(),
            outcome: ValidationOutcome::Failed,
            role: ValidationRole::Superseded,
            note: Some("sandbox allowlist leak; rerun below in a corrected environment".into()),
            check: Some("orbit-core-tests".into()),
        },
        ReviewValidation {
            command: "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core".into(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: Some("orbit-core-tests".into()),
        },
    ];
    assert!(
        certificate_acceptable(&related).is_ok(),
        "a shared check identity binds a corrected command as the replacement"
    );
    assert_eq!(
        related.validation[0].command, "cargo test --package orbit-core",
        "the failed observation remains on the certificate"
    );
    assert_eq!(related.validation[0].outcome, ValidationOutcome::Failed);
}

#[test]
fn exact_tree_landings_are_excluded_and_everything_else_stays_uncovered() {
    let cert = certificate(ReviewVerdict::PassedWithRepairs);
    let covered = exclusion(&delivery("base", "final"), &cert, &facts()).unwrap();
    assert_eq!(covered.attempt_id, "rvw-1");
    assert_eq!(
        covered.assurance,
        ReviewAssurance::IndependentReviewWithSelfAuthoredRepairs.as_str()
    );

    assert_eq!(
        exclusion(&delivery("moved", "final"), &cert, &facts()),
        Err(ReviewInvalidation::BaseChanged)
    );
    assert_eq!(
        exclusion(&delivery("base", "edited"), &cert, &facts()),
        Err(ReviewInvalidation::CandidateChanged)
    );
    assert_eq!(
        exclusion(
            &delivery("base", "final"),
            &cert,
            &LandingFacts {
                objects_present: false,
                ..facts()
            }
        ),
        Err(ReviewInvalidation::ObjectsMissing)
    );
    assert_eq!(
        exclusion(
            &delivery("base", "final"),
            &cert,
            &LandingFacts {
                task_meaning_current: false,
                ..facts()
            }
        ),
        Err(ReviewInvalidation::TaskMeaningChanged)
    );
    let raced = ReviewLanding {
        attempt_id: "rvw-1".into(),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        pr_number: Some("7".into()),
        landed: revision("other"),
        base_at_landing: revision("base"),
        transformation: LandingTransformation::Squash,
        covered: true,
        reason: None,
        recorded_at: now(),
    };
    assert_eq!(
        exclusion(
            &delivery("base", "final"),
            &cert,
            &LandingFacts {
                managed_landing: Some(raced),
                ..facts()
            }
        ),
        Err(ReviewInvalidation::ExternalLandingRace)
    );
    assert_eq!(
        exclusion(
            &delivery("base", "final"),
            &certificate(ReviewVerdict::ChangesRequired),
            &facts()
        ),
        Err(ReviewInvalidation::VerdictNotPassed)
    );
}

#[test]
fn managed_landings_classify_by_transformation_and_stay_exact_tree() {
    let cert = certificate(ReviewVerdict::PassedWithoutRepairs);
    let landed =
        |tree: &str, base: &str, candidate: bool, parents: usize, span: usize| LandedCandidate {
            landed_commit: "landed".into(),
            landed_tree: format!("tree-{tree}"),
            base_at_landing_tree: format!("tree-{base}"),
            is_candidate_commit: candidate,
            parents,
            span_commits: span,
        };
    assert_eq!(
        classify_landing(&cert, &landed("final", "base", true, 1, 1)),
        (LandingTransformation::FastForward, true, None)
    );
    assert_eq!(
        classify_landing(&cert, &landed("final", "base", false, 1, 1)),
        (LandingTransformation::Squash, true, None)
    );
    assert_eq!(
        classify_landing(&cert, &landed("final", "base", false, 2, 1)),
        (LandingTransformation::MergeCommit, true, None)
    );
    assert_eq!(
        classify_landing(&cert, &landed("final", "base", false, 1, 3)),
        (LandingTransformation::Rebase, true, None)
    );
    assert_eq!(
        classify_landing(&cert, &landed("final", "moved", false, 1, 1)),
        (
            LandingTransformation::Unknown,
            false,
            Some(ReviewInvalidation::BaseChanged)
        )
    );
    assert_eq!(
        classify_landing(&cert, &landed("conflict-repaired", "base", false, 1, 1)),
        (
            LandingTransformation::Unknown,
            false,
            Some(ReviewInvalidation::CandidateChanged)
        )
    );
}
