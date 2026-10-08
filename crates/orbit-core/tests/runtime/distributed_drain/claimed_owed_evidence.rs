//! A workspace host-evidence rule makes a claimed leaf's owed checks a
//! deterministic evidence hold, whatever its reviewer reports.
//!
//! The owner's `[[review.host_evidence]]` rule says a change to a Rust file
//! owes a Linux CodeQL run. A claimed leaf on a macOS follower cannot run it,
//! so its review gate synthesizes the requirement: the reviewer is handed it
//! as owner-fulfilled, a verdict whose only gap it is holds for it instead of
//! blocking, and a reviewer that skips or claims it cannot ship the candidate
//! unverified. Once the owner's run of the check arrives, the owner's next run
//! resumes the held candidate and settles the held review without
//! re-implementing or re-reviewing it.

use super::claimed_review::ReviewedLeaf;
use super::*;

use orbit_types::task::HostOs;
use orbit_types::workflow::{
    EvidenceHostOs, FindingDisposition, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, ReviewCertificate, ReviewEvidenceHold, ReviewEvidenceKind,
    ReviewEvidenceRequirement, ReviewFinding, ReviewManifest, ReviewReport, ReviewValidation,
    ReviewVerdict, ValidationOutcome, ValidationRole,
};

const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 \
                      codeql/rust-queries:codeql-suites/rust-security-extended.qls";
const ARTIFACT: &str = "evidence/codeql-rust-linux.json";

/// The owner's rule: a change matching `paths` owes a Linux CodeQL run.
fn codeql_rule(paths: &str) -> String {
    format!(
        "\n[[review.host_evidence]]\nkind = \"codeql\"\nname = \"Rust CodeQL (Linux)\"\n\
         paths = [\"{paths}\"]\nos = \"linux\"\ncommand = \"{CODEQL}\"\nartifact = \"{ARTIFACT}\"\n"
    )
}

/// The requirement the rule owes.
fn owed() -> ReviewEvidenceRequirement {
    ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::CodeQl,
        name: "Rust CodeQL (Linux)".into(),
        command: CODEQL.into(),
        artifact: ARTIFACT.into(),
        os: Some(EvidenceHostOs::Linux),
    }
}

/// A claimed leaf whose candidate changes `src/f0.rs`, from an owner whose
/// rule covers `paths`, on a follower running `os`; its review admitted.
fn leaf_on(os: HostOs, paths: &str) -> (ReviewedLeaf, Value) {
    let mut leaf = ReviewedLeaf::admit_with_configs(&codeql_rule(paths), "");
    leaf.bound = leaf.bound.clone().with_host_os(Some(os));
    let admitted = leaf.admit_review();
    (leaf, admitted)
}

fn record(id: &str, command: &str, outcome: ValidationOutcome) -> ReviewValidation {
    ReviewValidation {
        id: Some(id.into()),
        command: command.into(),
        outcome,
        role: ValidationRole::Required,
        note: None,
        check: None,
        control: None,
        sources: Vec::new(),
        mutation_target: Vec::new(),
        deferred: Vec::new(),
        baseline: None,
    }
}

/// A report naming no external evidence: everything else checked.
fn report(
    admitted: &Value,
    verdict: ReviewVerdict,
    findings: Vec<ReviewFinding>,
    validation: Vec<ReviewValidation>,
    escalation: Option<&str>,
) -> ReviewReport {
    ReviewReport {
        external_evidence: Vec::new(),
        schema_version: orbit_types::workflow::REVIEW_CONTRACT_VERSION,
        attempt_id: admitted["attempt_id"].as_str().unwrap().into(),
        verdict,
        summary: "Checked the change against the criteria.".into(),
        findings,
        validation,
        retired_validation: Vec::new(),
        escalation: escalation.map(Into::into),
    }
}

fn ci_fast() -> ReviewValidation {
    record("V1", "make ci-fast", ValidationOutcome::Passed)
}

fn artifact<T: serde::de::DeserializeOwned>(leaf: &ReviewedLeaf, path: &str) -> Option<T> {
    leaf.owner_artifact(path)
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

/// A Mac leaf's reviewer reports `incomplete` with no finding and no
/// structured external evidence, naming the CodeQL gap only in prose. The
/// reviewer's manifest named the owed check, and the gate holds for it
/// instead of blocking.
#[test]
fn a_mac_leaf_whose_only_gap_is_owed_codeql_holds_for_it() {
    if !isolated(
        module_path!(),
        "a_mac_leaf_whose_only_gap_is_owed_codeql_holds_for_it",
    ) {
        return;
    }
    let (leaf, admitted) = leaf_on(HostOs::Macos, "**/*.rs");
    assert_eq!(
        admitted["owed_evidence"],
        json!([owed()]),
        "admission names the owed check: {admitted}"
    );
    let manifest: ReviewManifest = artifact(&leaf, REVIEW_MANIFEST_ARTIFACT).unwrap();
    assert_eq!(manifest.owed_external_evidence, vec![owed()]);

    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![ci_fast()],
        Some("Rust CodeQL cannot run on macOS; a Linux run is owed"),
    ));
    let refused = leaf
        .settle()
        .expect_err("a held review opens no pull request");
    assert!(!refused.contains("review_gate_blocked"), "{refused}");

    let hold: ReviewEvidenceHold =
        artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).expect("the gate holds for the owed check");
    assert_eq!(hold.requirements, vec![owed()]);
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert_eq!(certificate.owed_evidence, vec![owed()]);
    let owed_record = certificate
        .validation
        .iter()
        .find(|record| record.command == CODEQL)
        .expect("the owed check is a required record");
    assert_eq!(owed_record.role, ValidationRole::Required);
    assert_eq!(owed_record.outcome, ValidationOutcome::NotRun);
}

/// A reviewer that accepts while reporting the owed check passed — a run its
/// host cannot perform — never ships the candidate unverified: the gate
/// holds for the owner's run instead.
#[test]
fn a_mac_reviewers_unverified_codeql_pass_holds_instead_of_shipping() {
    if !isolated(
        module_path!(),
        "a_mac_reviewers_unverified_codeql_pass_holds_instead_of_shipping",
    ) {
        return;
    }
    let (leaf, admitted) = leaf_on(HostOs::Macos, "**/*.rs");
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Accept,
        Vec::new(),
        vec![ci_fast(), record("V2", CODEQL, ValidationOutcome::Passed)],
        None,
    ));
    let refused = leaf
        .settle()
        .expect_err("an unverified pass opens no pull request");
    assert!(!refused.contains("review_gate_blocked"), "{refused}");

    let hold: ReviewEvidenceHold =
        artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).expect("the gate holds for the owed check");
    assert_eq!(hold.requirements, vec![owed()]);
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(
        certificate
            .escalation
            .as_deref()
            .is_some_and(|escalation| escalation.contains("owed_evidence")),
        "{:?}",
        certificate.escalation
    );
}

/// A Linux leaf runs CodeQL itself, and a rule whose paths the candidate
/// does not touch owes nothing: neither synthesizes a requirement, so a
/// complete review passes as before.
#[test]
fn a_linux_leaf_or_an_unmatched_rule_owes_no_evidence() {
    if !isolated(
        module_path!(),
        "a_linux_leaf_or_an_unmatched_rule_owes_no_evidence",
    ) {
        return;
    }
    for (os, paths) in [
        (HostOs::Linux, "**/*.rs"),
        (HostOs::Macos, "crates/**/*.rs"),
    ] {
        let (leaf, admitted) = leaf_on(os, paths);
        assert_eq!(admitted["owed_evidence"], json!([]), "{admitted}");
        leaf.put_report(&report(
            &admitted,
            ReviewVerdict::Accept,
            Vec::new(),
            vec![ci_fast()],
            None,
        ));
        let settled = leaf
            .settle()
            .unwrap_or_else(|error| panic!("{os:?} {paths}: {error}"));
        assert_eq!(settled["verdict"], "accept", "{settled}");
        let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
        assert!(certificate.owed_evidence.is_empty());
        assert!(
            artifact::<ReviewEvidenceHold>(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
            "no hold"
        );
    }
}

/// An open finding is a verdict on the candidate: owed evidence never turns
/// it into a hold.
#[test]
fn an_open_finding_on_a_mac_leaf_still_blocks() {
    if !isolated(module_path!(), "an_open_finding_on_a_mac_leaf_still_blocks") {
        return;
    }
    let (leaf, admitted) = leaf_on(HostOs::Macos, "**/*.rs");
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Incomplete,
        vec![ReviewFinding {
            id: "F1".into(),
            severity: "medium".into(),
            summary: "The stub panics".into(),
            paths: vec!["src/f0.rs".into()],
            disposition: FindingDisposition::Open,
            change: None,
        }],
        vec![ci_fast()],
        Some("the stub must be decided"),
    ));
    let refused = leaf.settle().expect_err("an open finding blocks");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    assert!(
        artifact::<ReviewEvidenceHold>(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
        "no hold for an open finding"
    );
}

/// The owner fulfils the held check with the candidate's CodeQL entry point
/// (a stub that runs once at the held commit). Receipt returns the task to
/// the backlog, and the owner's own next run resumes the held candidate
/// without the implementer and settles the held review without a reviewer,
/// passing it on to push and `pr_open`.
///
/// Owner fulfilment runs the script only inside a working Bubblewrap
/// namespace. Where the host refuses one, an operator attaches the same
/// result and log, the other writer the hold accepts.
#[cfg(target_os = "linux")]
#[test]
fn owner_fulfilment_resumes_the_held_mac_candidate_without_rework() {
    if !isolated(
        module_path!(),
        "owner_fulfilment_resumes_the_held_mac_candidate_without_rework",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let mut leaf = ReviewedLeaf::admit_with_configs(&codeql_rule("**/*.rs"), "");
    leaf.bound = leaf.bound.clone().with_host_os(Some(HostOs::Macos));
    let root = leaf.pair._root.path().to_path_buf();
    let origin = root.join("origin.git");
    git(&root, &["init", "--bare", "-q", origin.to_str().unwrap()]);
    let follower_repo = leaf.pair.follower_repo.clone();
    git(
        &follower_repo,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
    git(&follower_repo, &["push", "-q", "origin", "main"]);
    let script = follower_repo.join("scripts/codeql-rust-local.sh");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\nset -euo pipefail\n\
         head=\"$(git rev-parse HEAD)\"\n\
         echo \"ORBIT_CODEQL_STUB_RUN: $head\"\n\
         run_dir=\"$(mktemp -d \"$ORBIT_SCRATCH_DIR/codeql-rust-local.XXXXXX\")\"\n\
         echo \"codeql-rust-local: run directory: $run_dir\" >&2\n\
         printf '{\"runs\":[{\"results\":[]}]}' >\"$run_dir/results.sarif\"\n\
         echo \"codeql-rust-local: analysis completed\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&follower_repo, &["add", "scripts"]);
    git(
        &follower_repo,
        &["commit", "-q", "-m", "Add the CodeQL entry point"],
    );

    let admitted = leaf.admit_review();
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![ci_fast()],
        Some("Rust CodeQL cannot run on macOS"),
    ));
    leaf.settle().expect_err("held");
    let hold: ReviewEvidenceHold = artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap();
    assert_eq!(hold.requirements, vec![owed()]);
    let published = format!("refs/heads/orbit-evidence/orbit/{}", leaf.task);
    assert_eq!(hold.published_ref.as_deref(), Some(published.as_str()));
    leaf.leaf_holds(&hold);
    leaf.pair.pass(&leaf.drain);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");

    // The owner: a Linux checkout of the same `origin`, on the same base.
    let owner = leaf.pair.wire.owner.clone();
    let owner_repo = leaf.pair.owner_repo.clone();
    git(&owner_repo, &["init", "-q", "-b", "main"]);
    git(
        &owner_repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&owner_repo, &["fetch", "-q", "origin", "main"]);
    git(&owner_repo, &["reset", "-q", "--hard", "origin/main"]);
    fulfil_or_attach(&leaf, &owner, &hold, &root);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "backlog");
    let receipt = owner.get_task_history(&leaf.task).unwrap();
    assert_eq!(
        receipt.last().unwrap().event,
        "review_evidence_received",
        "{receipt:?}"
    );

    // The owner's own `task_pr_pipeline` run of the task, as dispatch
    // starts it with the workspace's captured before-PR admission.
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let admission = orbit_types::workflow::ReviewAdmission {
        contract_version: orbit_types::workflow::REVIEW_CONTRACT_VERSION,
        policy_version: orbit_config::OPERATION_POLICY_VERSION,
        timing: ReviewTiming::BeforePr,
        timing_source: "workspace".into(),
        crew: Some("sol".into()),
        crew_source: "workspace".into(),
        budget: orbit_types::workflow::ReviewBudget::default(),
        required_validation_commands: Some(Vec::new()),
        baseline_commands: Vec::new(),
        host_evidence: Vec::new(),
        captured_at: Utc::now(),
    };
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({ orbit_types::workflow::REVIEW_ADMISSION_KEY: admission })),
            None,
        )
        .unwrap()
        .run_id;
    let action = |name: &str, input: &Value| {
        orbit_engine::execute_deterministic_action(
            &owner,
            name,
            &json!({}),
            input,
            false,
            &Default::default(),
            None,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let setup = action(
        "worktree_setup",
        &json!({
            "job_run_id": run, "run_id": run, "task_ids": [leaf.task],
            "base": "main", "base_sync": "local", "dependency_delivery": "ignore",
        }),
    );
    let resumed = action(
        "candidate_resume",
        &json!({
            "job_run_id": run, "task_ids": [leaf.task],
            "workspace_path": setup["workspace_path"], "base_sha": setup["base_sha"],
            "prior_job_run_id": setup["prior_job_run_id"],
            "prior_foreign_run": setup["prior_foreign_run"],
        }),
    );
    assert_eq!(resumed["outcome"], "resumed_held", "{resumed}");
    assert_eq!(
        resumed["implement"], false,
        "no re-implementation: {resumed}"
    );
    assert_eq!(resumed["source_sha"], hold.candidate.commit.as_str());

    // Dispatch links the task to the run; the commit step commits the
    // resumed candidate.
    owner
        .update_task_with_identity(
            &leaf.task,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(orbit_types::task::TaskStatus::InProgress),
                job_run_id: Some(Some(run.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let worktree = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    git(&worktree, &["add", "-A"]);
    git(
        &worktree,
        &["commit", "-q", "-m", "Resume the held candidate"],
    );

    let mut gate_input = json!({
        "run_id": run, "job_run_id": run, "completed_task_ids": [leaf.task],
        "workspace_path": worktree, "base": "main", "base_sync": "local",
        "base_sha": setup["base_sha"], "mode": "pr", "skipped_no_diff_expected": false,
    });
    let admitted = owner
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &gate_input,
            ToolContext::default(),
        )
        .expect("admitted");
    assert_eq!(admitted["decision"], "evidence_received", "{admitted}");
    assert_eq!(admitted["held_attempt_id"], hold.attempt_id.as_str());
    gate_input["admission"] = admitted;
    let settled = owner
        .run_deterministic(
            "review_gate_settle",
            &json!({}),
            &gate_input,
            ToolContext::default(),
        )
        .expect("the held review settles once its owed check arrived");
    assert_eq!(settled["gate"], "passed", "{settled}");
    assert_eq!(settled["verdict"], "accept", "{settled}");
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(settled["reviewed_head_sha"], head.trim(), "pr_open's head");
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(
        certificate.resumed_hold_attempt.as_deref(),
        Some(hold.attempt_id.as_str()),
        "settled without a reviewer"
    );
    assert_eq!(certificate.final_candidate.tree, hold.candidate.tree);
}

/// The owner's fulfilment of `hold`, or an operator's attachment of the same
/// result where this host refuses the Bubblewrap namespace fulfilment needs.
#[cfg(target_os = "linux")]
fn fulfil_or_attach(
    leaf: &ReviewedLeaf,
    owner: &OrbitRuntime,
    hold: &ReviewEvidenceHold,
    root: &Path,
) {
    let log_artifact = "evidence/codeql-rust-linux.log.json";
    let probe = orbit_exec::probe_bwrap();
    if probe.available {
        let resources = owner.paths().global_dir.join("resources");
        std::fs::create_dir_all(resources.join("activities")).unwrap();
        std::fs::create_dir_all(resources.join("jobs")).unwrap();
        std::fs::write(
            resources.join("activities/fulfil_review_evidence.yaml"),
            include_str!("../../../assets/activities/fulfil_review_evidence.yaml"),
        )
        .unwrap();
        std::fs::write(
            resources.join("jobs/review_evidence_fulfilment_pipeline.yaml"),
            include_str!("../../../assets/jobs/review_evidence_fulfilment_pipeline.yaml")
                .replace("min_free_mib: 30720", "min_free_mib: 1"),
        )
        .unwrap();
        let released = root.join("released");
        orbit_core::test_support::install_substitute_pipeline_worker([
            "sh".to_string(),
            "-c".to_string(),
            "i=0; while [ ! -e \"$1\" ] && [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done"
                .to_string(),
            "worker".to_string(),
            released.to_string_lossy().into_owned(),
        ]);
        let tick = owner
            .run_review_evidence_fulfilment_tick(Utc::now())
            .unwrap();
        assert_eq!(tick.dispatched.len(), 1, "{tick:?}");
        let executed = owner.execute_pipeline_run_worker(&tick.dispatched[0].1);
        std::fs::write(&released, "").unwrap();
        executed.unwrap();
        let log: Value =
            serde_json::from_slice(&leaf.owner_artifact(log_artifact).unwrap()).unwrap();
        assert_eq!(log["tested_head"], hold.candidate.commit.as_str());
        return;
    }
    orbit_exec::report_bwrap_deferral(
        "owner CodeQL fulfilment (an operator attaches the result instead)",
        &probe.detail,
    );
    let result = orbit_types::workflow::ReviewExternalEvidence {
        schema_version: 1,
        attempt_id: hold.attempt_id.clone(),
        candidate: hold.candidate.clone(),
        kind: ReviewEvidenceKind::CodeQl,
        name: owed().name,
        command: CODEQL.into(),
        outcome: ValidationOutcome::Passed,
        log_artifact: log_artifact.into(),
        os: Some(EvidenceHostOs::Linux),
    };
    let attach = |path: &str, content: Vec<u8>| TaskArtifact {
        path: path.into(),
        media_type: "application/json".into(),
        content,
        created_by: None,
    };
    owner
        .clone()
        .with_actor(orbit_core::ActorIdentity::human("human:operator"))
        .update_task_with_identity(
            &leaf.task,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![
                    attach(
                        log_artifact,
                        json!({"tested_head": hold.candidate.commit, "outcome": "passed"})
                            .to_string()
                            .into_bytes(),
                    ),
                    attach(ARTIFACT, serde_json::to_vec(&result).unwrap()),
                ],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}
