//! An owner that accepts a claimed leaf's before-PR review records its
//! certificate, so its after-landing review excludes the reviewed tree
//! [ORB-13895].
//!
//! The claim is admitted on the owner's commit boundary directly, as a
//! gate-running leaf's pull is; `claimed_review` drives the leaf's gate
//! itself [ORB-13908].

use std::collections::BTreeMap;

use super::*;

use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_core::application::automation::evaluate_auto_task;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{
    AdmissionIdentity, AdmissionLookup, AdmissionRequest, AdmissionReviewContract,
    AdmissionRunContext, AdmissionShipContract, ClaimEvidence, DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
    ExecutionLocation, HandoffReviewObservation,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffReviewEvidence};
use orbit_types::workflow::{
    CommitIdentity, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ReviewBudget, ReviewCertificate,
    ReviewConsumption, ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome,
    ValidationRole,
};
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "owner/repository";
const CONSUMER: &str = "landed-review";

fn revision(repo: &Path, spec: &str) -> SourceRevision {
    SourceRevision {
        commit: git(repo, &["rev-parse", spec]).trim().to_string(),
        tree: git(repo, &["rev-parse", &format!("{spec}^{{tree}}")])
            .trim()
            .to_string(),
    }
}

fn commit_identity(revision: &SourceRevision, subject: &str) -> CommitIdentity {
    CommitIdentity {
        commit: revision.commit.clone(),
        tree: revision.tree.clone(),
        author: "Orbit Test".into(),
        committer: "Orbit Test".into(),
        subject: subject.into(),
    }
}

/// A follower's reviewed candidate — an implementation commit and the
/// reviewer's fix commit on the owner's base — is accepted under a
/// before-PR claim. The owner's review store then holds the certificate the
/// handoff pinned, and once the pull request squash-merges onto the reviewed
/// base, the owner's after-landing consumer excludes that delivery instead of
/// queueing it for review.
#[test]
fn an_accepted_before_pr_handoff_covers_its_landing() {
    if !isolated(
        module_path!(),
        "an_accepted_before_pr_handoff_covers_its_landing",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let (owner, repo) = open_runtime(root.path(), OWNER);
    let task_id = backlog_task(&owner, &repo, "src/work.rs", None);

    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "baseline"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{REPOSITORY}.git"),
        ],
    );
    owner
        .run_tool(
            "orbit.auto_task.add",
            json!({
                "name": CONSUMER,
                "schedule": {"deliveries_landed": {
                    "branch": "main",
                    "owner_machine": OWNER,
                    "threshold": 1,
                    "max_wait_minutes": 60,
                    "coverage": "landed_code_review_v1",
                    "max_items": 20,
                    "retries": 0,
                }},
                "template": {"title": "Review landed deliveries"},
            }),
        )
        .expect("consumer");
    owner.auto_task_toggle(CONSUMER, true).unwrap();
    let definition = owner.auto_task_show(CONSUMER).unwrap().unwrap();
    publish_origin(&repo);
    evaluate_auto_task(&owner, &definition, false, Utc::now()).expect("baseline");

    let base = revision(&repo, "HEAD");
    let branch = format!("orbit/{task_id}");
    git(&repo, &["checkout", "-q", "-b", &branch]);
    std::fs::write(repo.join("src/work.rs"), "fn work() { todo!() }\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "Implement"]);
    let implementation = revision(&repo, "HEAD");
    std::fs::write(repo.join("src/work.rs"), "fn work() {}\n// reviewed\n").unwrap();
    git(&repo, &["commit", "-q", "-am", "Reviewer fixes"]);
    let candidate = revision(&repo, "HEAD");
    git(&repo, &["checkout", "-q", "main"]);

    // The claim a gate-running leaf's pull admits under the owner's
    // captured review contract.
    let ship = AdmissionShipContract {
        mode: "pr".into(),
        base_branch: "main".into(),
        landing_branch: "main".into(),
        before_pr: true,
        completion: "review".into(),
        authorization_reference: None,
        review: Some(AdmissionReviewContract {
            contract_version: REVIEW_CONTRACT_VERSION,
            crew: Some("reviewer".into()),
            budget: ReviewBudget::default(),
            required_validation_commands: Some(vec![]),
            baseline_commands: Vec::new(),
            host_evidence: Vec::new(),
        }),
    };
    let boundary = TaskCommitBoundary::new(
        owner.sqlite_store().unwrap(),
        TaskRegistryStore::open(&task_registry_path(&owner.global_root())).unwrap(),
        owner.workspace_id().unwrap(),
    )
    .unwrap();
    let request = AdmissionRequest {
        request_id: "reviewed-pull".into(),
        caller_version: "test".into(),
        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: true,
        run_context: AdmissionRunContext {
            run_id: "drain".into(),
            job_name: "workspace_pull_pipeline".into(),
            machine_name: None,
        },
        ship: ship.clone(),
        crews: None,
        os: None,
    };
    let AdmissionLookup::Found { receipt, .. } = boundary
        .admit_task(
            &AdmissionIdentity::trusted_remote(ExecutionLocation {
                machine_id: FOLLOWER.into(),
                machine_name: None,
            }),
            &request,
            "test",
            &repo,
            &repo.join(".orbit"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &|_| Ok(None),
        )
        .expect("admit")
    else {
        panic!("no receipt");
    };
    let claim = receipt.claim.expect("claim");
    assert_eq!(claim.task_id, task_id);
    let run = ClaimRun {
        machine_id: FOLLOWER.into(),
        run_id: "leaf".into(),
    };
    owner
        .mutate_execution_claim(
            Some(&ClaimInvocation::trusted_worker(
                task_id.clone(),
                claim.claim_id.clone(),
                FOLLOWER.into(),
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
        task_id.clone(),
        claim.claim_id.clone(),
        FOLLOWER.into(),
        Some(run),
    );

    // The leaf's gate issued this certificate and sent it as claim evidence.
    let task = owner.get_task(&task_id).unwrap();
    let certificate = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: "follower-attempt".into(),
        lineage_key: "follower-lineage".into(),
        task_ids: vec![task_id.clone()],
        task_meaning_digest: combined_task_meaning_digest(&[(
            task_id.clone(),
            task_meaning_digest(&task).unwrap(),
        )])
        .unwrap(),
        repository: REPOSITORY.into(),
        base: base.clone(),
        reviewed_candidate: implementation.clone(),
        final_candidate: candidate.clone(),
        implementation_commits: vec![commit_identity(&implementation, "Implement")],
        repair_commits: vec![commit_identity(&candidate, "Reviewer fixes")],
        verdict: ReviewVerdict::AcceptWithFixes,
        assurance: ReviewVerdict::AcceptWithFixes.assurance(),
        findings: vec![],
        validation: vec![ReviewValidation {
            id: None,
            command: "cargo test".into(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: None,
            control: None,
            sources: Vec::new(),
            mutation_target: Vec::new(),
            baseline: None,
        }],
        required_validation_commands: Some(vec![]),
        baseline_commands: Vec::new(),
        validation_complete: true,
        reviewer: ReviewerIdentity {
            crew: "reviewer".into(),
            provider: "provider".into(),
            model: "model".into(),
            reasoning_effort: None,
            implementer_model: None,
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption::default(),
        budget: ReviewBudget::default(),
        escalation: None,
        retained_obligations: vec![],
        retired_validation: vec![],
        validation_scope: vec![],
        selectors_widened: vec![],
        evidence_carried: None,
        baseline_red: Vec::new(),
        host_evidence: Vec::new(),
        issued_at: Utc::now(),
        owed_evidence: Vec::new(),
        resumed_hold_attempt: None,
    };
    let content = serde_json::to_vec(&certificate).unwrap();
    let reference = HandoffArtifactRef {
        path: REVIEW_GATE_ARTIFACT.into(),
        sha256: format!("{:x}", Sha256::digest(&content)),
    };
    owner
        .mutate_execution_claim(
            Some(&worker),
            "review-certificate",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts: vec![TaskArtifact {
                    path: reference.path.clone(),
                    content,
                    media_type: "application/json".into(),
                    created_by: None,
                }],
                ..ClaimEvidence::default()
            }),
        )
        .expect("certificate evidence");

    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: owner.workspace_id().unwrap(),
        task_id: task_id.clone(),
        claim_id: claim.claim_id.clone(),
        machine_id: FOLLOWER.into(),
        run_id: "leaf".into(),
        candidate: HandoffCandidate {
            repository: REPOSITORY.into(),
            source_branch: branch,
            base_branch: "main".into(),
            landing_branch: "main".into(),
            candidate: candidate.clone(),
            base: base.clone(),
            delivery: HandoffDelivery::PullRequest { number: 42 },
        },
        review: HandoffReview {
            policy: ReviewTiming::BeforePr,
            disposition: HandoffReviewDisposition::BeforePr(Box::new(HandoffReviewEvidence {
                attempt_id: certificate.attempt_id.clone(),
                verdict: certificate.verdict,
                reviewed_head_sha: candidate.commit.clone(),
                reviewed_base_sha: base.commit.clone(),
                reviewer_commit: Some(candidate.commit.clone()),
                reviewer_crew: "reviewer".into(),
                reviewer_run_id: "leaf".into(),
                certificate: reference,
                artifacts: vec![],
                host_evidence: vec![],
            })),
        },
        execution_summary: "Outcome: success".into(),
        validation: vec![],
        footprint_widening: vec![],
    };
    // The provider read and Git observation the settle tool makes, standing
    // in for a published pull request no test here has.
    let observation = HandoffObservation {
        footprint_widening: vec![],
        candidate: handoff.candidate.clone(),
        required_commands: vec![],
        owner_completion_authority: None,
        review: Some(HandoffReviewObservation {
            reviewed_base_sha: base.commit.clone(),
            reviewed_base_is_ancestor: true,
            repository: REPOSITORY.into(),
        }),
    };
    owner
        .accept_task_handoff(&worker, "handoff", handoff, observation)
        .expect("reviewed handoff accepted");
    assert_eq!(
        owner.get_task(&task_id).unwrap().status.to_string(),
        "review"
    );
    assert_eq!(
        owner
            .review_store()
            .unwrap()
            .review_certificate(&owner.workspace_id().unwrap(), "follower-attempt")
            .unwrap(),
        Some(certificate),
        "the owner's review store holds the follower's certificate"
    );

    // The reviewed pull request squash-merges onto the reviewed base.
    git(
        &repo,
        &["merge", "-q", "--squash", &format!("orbit/{task_id}")],
    );
    git(&repo, &["commit", "-q", "-m", "Squash-merge #42"]);
    let landed = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    std::fs::create_dir_all(home.join("bin")).unwrap();
    std::fs::write(
        home.join("pull.json"),
        json!([{
            "number": 42,
            "html_url": format!("https://github.com/{REPOSITORY}/pull/42"),
            "merge_commit_sha": landed,
            "merged_at": "2026-10-04T00:00:00Z",
            "base": {"ref": "main", "repo": {"full_name": REPOSITORY}},
        }])
        .to_string(),
    )
    .unwrap();
    let gh = home.join("bin/gh");
    std::fs::write(&gh, "#!/bin/sh\nexec cat \"$HOME/pull.json\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    publish_origin(&repo);
    let diagnostic = evaluate_auto_task(&owner, &definition, false, Utc::now()).unwrap();
    let state = diagnostic
        .state
        .clone()
        .unwrap_or_else(|| panic!("no consumer state: {diagnostic:#?}"));
    // Only an exclusion retires a landing without examining it: the
    // certificate covers the squash tree, so the cursor passes it with no
    // review debt and no review job.
    assert_eq!(state.covered.commit, landed, "{state:#?}");
    assert_eq!(state.covered.tree, candidate.tree, "{state:#?}");
    assert!(state.pending_commits.is_empty(), "{state:#?}");
    assert!(state.pending.is_empty(), "{state:#?}");
    assert!(state.active.is_none(), "{state:#?}");
}
