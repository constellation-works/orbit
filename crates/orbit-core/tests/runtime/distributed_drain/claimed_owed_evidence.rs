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
const SANDBOX: &str = "cargo test -p orbit-exec --test sandbox linux_sandbox";
const SANDBOX_ARTIFACT: &str = "evidence/sandbox-linux.json";
const CI: &str = "make ci";
const CI_ARTIFACT: &str = "evidence/ci.json";

/// The owner's rule: a change matching `paths` owes a Linux CodeQL run.
pub(super) fn codeql_rule(paths: &str) -> String {
    format!(
        "\n[[review.host_evidence]]\nkind = \"codeql\"\nname = \"Rust CodeQL (Linux)\"\n\
         paths = [\"{paths}\"]\nos = \"linux\"\ncommand = \"{CODEQL}\"\nartifact = \"{ARTIFACT}\"\n"
    )
}

/// The pull request's hosted check a [`hosted_codeql_rule`] declares runs
/// its command.
pub(super) const HOSTED_CHECK: &str = "CodeQL / Analyze (rust)";

/// [`codeql_rule`] declaring that [`HOSTED_CHECK`] runs its exact command.
pub(super) fn hosted_codeql_rule(paths: &str) -> String {
    format!("{}hosted_check = \"{HOSTED_CHECK}\"\n", codeql_rule(paths))
}

/// The requirement a [`hosted_codeql_rule`] owes.
pub(super) fn hosted_owed() -> ReviewEvidenceRequirement {
    ReviewEvidenceRequirement {
        hosted_check: Some(HOSTED_CHECK.into()),
        ..owed()
    }
}

/// The requirement the rule owes.
pub(super) fn owed() -> ReviewEvidenceRequirement {
    ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::CodeQl,
        name: "Rust CodeQL (Linux)".into(),
        command: CODEQL.into(),
        artifact: ARTIFACT.into(),
        os: Some(EvidenceHostOs::Linux),
        hosted_check: None,
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

pub(super) fn record(id: &str, command: &str, outcome: ValidationOutcome) -> ReviewValidation {
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
pub(super) fn report(
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

pub(super) fn ci_fast() -> ReviewValidation {
    record("V1", "make ci-fast", ValidationOutcome::Passed)
}

pub(super) fn artifact<T: serde::de::DeserializeOwned>(
    leaf: &ReviewedLeaf,
    path: &str,
) -> Option<T> {
    leaf.owner_artifact(path)
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

/// A Mac leaf's reviewer reports `incomplete` with no finding and no
/// structured external evidence, naming the CodeQL gap only in prose. The
/// reviewer's manifest named the owed check, and the gate holds for it
/// instead of blocking.
fn a_mac_leaf_whose_only_gap_is_owed_codeql_holds_for_it() {
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
fn a_mac_reviewers_unverified_codeql_pass_holds_instead_of_shipping() {
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
fn a_linux_leaf_or_an_unmatched_rule_owes_no_evidence() {
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
fn an_open_finding_on_a_mac_leaf_still_blocks() {
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

/// A Mac reviewer names its own Linux-only gaps and a hosted CI job as
/// external evidence, each record `not_run`. No host-evidence rule owes them,
/// so the reviewer's report alone holds the candidate for the owner's runs
/// instead of blocking it.
fn a_mac_reviewers_named_linux_only_evidence_holds_instead_of_blocking() {
    let (leaf, admitted) = leaf_on(HostOs::Macos, "docs/**/*.md");
    assert_eq!(admitted["owed_evidence"], json!([]), "{admitted}");
    let named = vec![
        owed(),
        ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::HostSandboxTest,
            name: "Sandbox probe (Linux)".into(),
            command: SANDBOX.into(),
            artifact: SANDBOX_ARTIFACT.into(),
            os: Some(EvidenceHostOs::Linux),
            hosted_check: None,
        },
        ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::HostedCi,
            name: "CI".into(),
            command: CI.into(),
            artifact: CI_ARTIFACT.into(),
            os: None,
            hosted_check: None,
        },
    ];
    let mut validation = vec![ci_fast()];
    for (index, required) in named.iter().enumerate() {
        let id = format!("V{}", index + 2);
        validation.push(record(&id, &required.command, ValidationOutcome::NotRun));
    }
    let mut reviewer_report = report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        validation,
        Some("Linux-only evidence is owed to the owner"),
    );
    reviewer_report.external_evidence = named.clone();
    leaf.put_report(&reviewer_report);
    let refused = leaf
        .settle()
        .expect_err("a held review opens no pull request");
    assert!(!refused.contains("review_gate_blocked"), "{refused}");

    let hold: ReviewEvidenceHold = artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT)
        .expect("the gate holds for the named evidence");
    assert_eq!(hold.requirements.len(), named.len(), "{hold:?}");
    for required in &named {
        assert!(hold.requirements.contains(required), "{hold:?}");
    }
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
}

/// A claimed before-landing review must release its lane with an evidence
/// hold just like a before-PR review, without approving an unverified head.
/// [ORB-15677] The owner's workspace declares no hosted check for Linux
/// CodeQL, so the run the Mac reviewer named holds the review beside the
/// hosted job instead of becoming the owner's.
fn a_mac_before_landing_review_holds_named_codeql_without_a_declared_hosted_check() {
    let mut leaf = ReviewedLeaf::admit_before_landing();
    leaf.bound = leaf.bound.clone().with_host_os(Some(HostOs::Macos));
    leaf.gate_input["before_landing"] = json!(true);
    let admitted = leaf.admit_review();
    assert_eq!(admitted["timing"], "before-landing", "{admitted}");
    let hosted = ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::HostedCi,
        name: "CI".into(),
        command: CI.into(),
        artifact: CI_ARTIFACT.into(),
        os: None,
        hosted_check: None,
    };
    let mut reviewer_report = report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![
            ci_fast(),
            record("V2", CODEQL, ValidationOutcome::NotRun),
            record("V3", CI, ValidationOutcome::NotRun),
        ],
        Some("Linux and hosted checks need owner-side evidence"),
    );
    reviewer_report.external_evidence = vec![owed(), hosted.clone()];
    leaf.put_report(&reviewer_report);
    let refused = leaf.settle().expect_err("missing evidence cannot land");
    assert!(
        refused.contains("review_awaiting_evidence"),
        "an evidence-only claimed before-landing review must hold: {refused}"
    );
    let hold: ReviewEvidenceHold = artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT)
        .expect("named evidence remains an owner-side follow-up");
    assert_eq!(hold.requirements, vec![owed(), hosted]);
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert!(
        certificate.owner_owed_evidence.is_empty(),
        "no hosted run stands in for an undeclared check: {:?}",
        certificate.owner_owed_evidence
    );
    let codeql = certificate
        .validation
        .iter()
        .find(|record| record.command == CODEQL)
        .expect("the CodeQL record stays on the certificate");
    assert_eq!(
        (codeql.role, codeql.outcome),
        (ValidationRole::Required, ValidationOutcome::NotRun)
    );
    leaf.leaf_holds(&hold);
    leaf.pair.pass(&leaf.drain);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
}

/// A check that failed on the candidate is an answer, not an evidence gap: a
/// later run of it on another OS cannot settle it, so the review blocks.
fn a_mac_reviewers_failed_linux_only_check_still_blocks() {
    let (leaf, admitted) = leaf_on(HostOs::Macos, "docs/**/*.md");
    let mut reviewer_report = report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![ci_fast(), record("V2", CI, ValidationOutcome::Failed)],
        Some("the hosted CI check failed on the candidate"),
    );
    reviewer_report.external_evidence = vec![ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::HostedCi,
        name: "CI".into(),
        command: CI.into(),
        artifact: CI_ARTIFACT.into(),
        os: None,
        hosted_check: None,
    }];
    leaf.put_report(&reviewer_report);
    let refused = leaf
        .settle()
        .expect_err("a failed check opens no pull request");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    assert!(
        artifact::<ReviewEvidenceHold>(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
        "no hold for a failed check"
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
    let (root, origin) = publish_origin(&leaf);
    let follower_repo = leaf.pair.follower_repo.clone();
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

    let owner = leaf.pair.wire.owner.clone();
    owner_checkout(&leaf, &origin);
    fulfil_or_attach(&leaf, &owner, &hold, &root);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "backlog");
    let receipt = owner.get_task_history(&leaf.task).unwrap();
    assert_eq!(
        receipt.last().unwrap().event,
        "review_evidence_received",
        "{receipt:?}"
    );

    let resumed = owner_resumes(&leaf, ReviewTiming::BeforePr);
    assert_eq!(
        resumed.output["outcome"], "resumed_held",
        "{}",
        resumed.output
    );
    assert_eq!(
        resumed.output["implement"], false,
        "no re-implementation: {}",
        resumed.output
    );
    assert_eq!(resumed.output["source_sha"], hold.candidate.commit.as_str());

    let mut gate_input = json!({
        "run_id": resumed.run, "job_run_id": resumed.run, "completed_task_ids": [leaf.task],
        "workspace_path": resumed.worktree, "base": "main", "base_sync": "local",
        "base_sha": resumed.base_sha, "mode": "pr", "skipped_no_diff_expected": false,
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
    let head = git(&resumed.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(settled["reviewed_head_sha"], head.trim(), "pr_open's head");
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(
        certificate.resumed_hold_attempt.as_deref(),
        Some(hold.attempt_id.as_str()),
        "settled without a reviewer"
    );
    assert_eq!(certificate.final_candidate.tree, hold.candidate.tree);
}

/// A bare `origin` beside the pair, which the leaf's follower checkout
/// pushes its base to; returns the pair's root and that remote.
#[cfg(unix)]
pub(super) fn publish_origin(leaf: &ReviewedLeaf) -> (PathBuf, PathBuf) {
    let root = leaf.pair._root.path().to_path_buf();
    let origin = root.join("origin.git");
    git(&root, &["init", "--bare", "-q", origin.to_str().unwrap()]);
    let follower_repo = &leaf.pair.follower_repo;
    git(
        follower_repo,
        &["remote", "set-url", "origin", origin.to_str().unwrap()],
    );
    git(follower_repo, &["push", "-q", "origin", "main"]);
    (root, origin)
}

/// The owner: a Linux checkout of the same `origin`, on the same base.
#[cfg(unix)]
pub(super) fn owner_checkout(leaf: &ReviewedLeaf, origin: &Path) {
    let owner_repo = &leaf.pair.owner_repo;
    git(owner_repo, &["init", "-q", "-b", "main"]);
    git(
        owner_repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(owner_repo, &["fetch", "-q", "origin", "main"]);
    git(owner_repo, &["reset", "-q", "--hard", "origin/main"]);
}

/// The owner's own `task_pr_pipeline` run of the task after its evidence
/// arrived, through `candidate_resume` and the commit of what it resumed.
#[cfg(unix)]
pub(super) struct ResumedRun {
    pub(super) run: String,
    pub(super) worktree: PathBuf,
    pub(super) base_sha: Value,
    /// `candidate_resume`'s output.
    pub(super) output: Value,
}

/// Start the owner's `task_pr_pipeline` run of the task as dispatch does,
/// with the workspace's captured review `timing`, set up its worktree and
/// resume the task's kept candidate; dispatch links the task to the run and
/// the commit step commits the resumed candidate.
#[cfg(unix)]
pub(super) fn owner_resumes(leaf: &ReviewedLeaf, timing: ReviewTiming) -> ResumedRun {
    let owner = &leaf.pair.wire.owner;
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let admission = orbit_types::workflow::ReviewAdmission {
        contract_version: orbit_types::workflow::REVIEW_CONTRACT_VERSION,
        policy_version: orbit_config::OPERATION_POLICY_VERSION,
        timing,
        timing_source: "workspace".into(),
        crew: Some("sol".into()),
        crew_source: "workspace".into(),
        crew_pool: Vec::new(),
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
    let setup = owner_action(
        owner,
        "worktree_setup",
        &json!({
            "job_run_id": run, "run_id": run, "task_ids": [leaf.task],
            "base": "main", "base_sync": "local", "dependency_delivery": "ignore",
        }),
    );
    let output = owner_action(
        owner,
        "candidate_resume",
        &json!({
            "job_run_id": run, "task_ids": [leaf.task],
            "workspace_path": setup["workspace_path"], "base_sha": setup["base_sha"],
            "prior_job_run_id": setup["prior_job_run_id"],
            "prior_foreign_run": setup["prior_foreign_run"],
        }),
    );
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
    ResumedRun {
        run,
        worktree,
        base_sha: setup["base_sha"].clone(),
        output,
    }
}

/// The owner's run of the deterministic action `name`, as its executor
/// dispatches it.
#[cfg(unix)]
pub(super) fn owner_action(owner: &OrbitRuntime, name: &str, input: &Value) -> Value {
    orbit_engine::execute_deterministic_action(
        owner,
        name,
        &json!({}),
        input,
        false,
        &Default::default(),
        None,
    )
    .unwrap_or_else(|error| panic!("{name}: {error}"))
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
        crate::worker_fixture::install(&released, "created");
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
    attach_result(leaf, owner, hold);
}

/// An operator's attachment of a passed result, and its log, for the one
/// owed check `hold` names: the other writer the hold accepts.
#[cfg(unix)]
pub(super) fn attach_result(leaf: &ReviewedLeaf, owner: &OrbitRuntime, hold: &ReviewEvidenceHold) {
    let [required] = hold.requirements.as_slice() else {
        panic!("one owed check: {hold:?}");
    };
    let log_artifact = format!("{}.log.json", required.artifact.trim_end_matches(".json"));
    let log_artifact = log_artifact.as_str();
    let result = orbit_types::workflow::ReviewExternalEvidence {
        schema_version: 1,
        attempt_id: hold.attempt_id.clone(),
        candidate: hold.candidate.clone(),
        kind: required.kind,
        name: required.name.clone(),
        command: required.command.clone(),
        outcome: ValidationOutcome::Passed,
        log_artifact: log_artifact.into(),
        os: required.os,
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
                    attach(&required.artifact, serde_json::to_vec(&result).unwrap()),
                ],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_mac_leaf_whose_only_gap_is_owed_codeql_holds_for_it_scenarios,
    a_mac_leaf_whose_only_gap_is_owed_codeql_holds_for_it,
    a_mac_reviewers_unverified_codeql_pass_holds_instead_of_shipping,
    a_linux_leaf_or_an_unmatched_rule_owes_no_evidence
);

crate::scenario_table!(
    an_open_finding_on_a_mac_leaf_still_blocks_scenarios,
    an_open_finding_on_a_mac_leaf_still_blocks,
    a_mac_reviewers_named_linux_only_evidence_holds_instead_of_blocking
);

crate::scenario_table!(
    a_mac_before_landing_review_holds_named_codeql_without_a_declared_hosted_check_scenarios,
    a_mac_before_landing_review_holds_named_codeql_without_a_declared_hosted_check,
    a_mac_reviewers_failed_linux_only_check_still_blocks
);
