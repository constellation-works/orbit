//! What a before-PR certificate establishes when the reviewer replaced its
//! report or recorded failures outside the task's scope [ORB-14192].
//!
//! Reports reach the gate the way a reviewer attaches them, through the
//! public `orbit.task.artifact.put` tool; the deterministic settlement then
//! issues the certificate, and the shared coverage rules spend it.

use orbit_automation::review::certificate_acceptable;
use orbit_core::OrbitRuntime;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{ArtifactManifestV2, TASK_ARTIFACT_MANIFEST_FILE_NAME};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, ReviewCertificate, ReviewReportHistory, ReviewVerdict,
    ValidationOutcome, ValidationRole,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

/// ORB-14191's required CodeQL run.
const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 \
     codeql/rust-queries:codeql-suites/rust-security-extended.qls";
/// ORB-14151's final-candidate workspace diagnostic and the unrelated
/// fixture it failed in.
const WORKSPACE: &str = "cargo test --workspace --no-fail-fast";
const UNRELATED_FIXTURE: &str = "crates/orbit-engine/tests/fixtures/f066_resume.rs";

/// The task-scoped regressions, all passing.
fn scoped_passes() -> Vec<Value> {
    (1..=9)
        .map(|index| {
            json!({
                "command": format!("cargo test -p fixture case_{index}"),
                "outcome": "passed",
                "role": "required",
            })
        })
        .collect()
}

fn report(fixture: &Fixture, verdict: &str, extra: Vec<Value>, findings: Value) -> Value {
    let mut validation = scoped_passes();
    validation.extend(extra);
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": verdict,
        "summary": "Checked the candidate.",
        "findings": findings,
        "validation": validation,
        "escalation": (verdict == "incomplete").then_some("CodeQL extraction was incomplete"),
    })
}

/// The first report of ORB-14191's attempt: incomplete, CodeQL failed.
fn codeql_failed_report(fixture: &Fixture) -> Value {
    report(
        fixture,
        "incomplete",
        vec![json!({
            "command": CODEQL,
            "outcome": "failed",
            "role": "required",
            "note": "semantic extraction was incomplete",
        })],
        json!([]),
    )
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
        .unwrap()
        .expect("settlement issues a certificate");
    serde_json::from_slice(&artifact.content).unwrap()
}

fn verdict_comment(fixture: &Fixture) -> String {
    fixture
        .runtime
        .get_task_comments(&fixture.task_id)
        .unwrap()
        .into_iter()
        .map(|comment| comment.message)
        .rfind(|message| message.starts_with("before-PR review settled attempt"))
        .expect("settlement comments its verdict")
}

/// Model a pre-retention workspace by removing the sidecar pointer from its
/// isolated task bundle while preserving the legitimate current report blob.
fn remove_history_sidecar(fixture: &Fixture) {
    let registry =
        TaskRegistryStore::open(&task_registry_path(&fixture.runtime.global_root())).unwrap();
    let bundle = registry
        .canonical_task_bundle_path(&fixture.runtime.workspace_id().unwrap(), &fixture.task_id)
        .unwrap();
    let manifest_path = bundle
        .join("artifacts")
        .join(TASK_ARTIFACT_MANIFEST_FILE_NAME);
    let bytes = std::fs::read(&manifest_path).unwrap();
    let mut manifest: ArtifactManifestV2 = serde_yaml::from_slice(&bytes).unwrap();
    assert!(
        manifest
            .files
            .iter()
            .any(|file| file.path == REVIEW_REPORT_HISTORY_ARTIFACT)
    );
    manifest
        .files
        .retain(|file| file.path != REVIEW_REPORT_HISTORY_ARTIFACT);
    std::fs::write(&manifest_path, serde_yaml::to_string(&manifest).unwrap()).unwrap();
}

/// ORB-14191 attempt rvw-368054c058a1-1: the first report is incomplete with
/// the required CodeQL run failed, the replacement says `accept_with_fixes`
/// and silently omits it. No settlement runs between the two writes, the
/// replacement is retried after a lost response, and the host restarts
/// before settling; the gate still refuses the false completeness, keeps
/// both revisions, and the certificate it issues is no coverage.
#[test]
fn a_replacement_report_cannot_drop_a_required_check_an_earlier_revision_failed() {
    if !super::dispatch_admission::isolated(
        "review_report_revisions::a_replacement_report_cannot_drop_a_required_check_an_earlier_revision_failed",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&codeql_failed_report(&fixture));

    // The reviewer then fixes a finding and replaces its report.
    std::fs::write(fixture.repo.join("candidate.txt"), "after\nfixed\n").unwrap();
    let replacement = report(
        &fixture,
        "accept_with_fixes",
        Vec::new(),
        json!([{
            "id": "F1", "severity": "low", "summary": "Trailing line missing",
            "paths": ["candidate.txt"], "disposition": {"kind": "repaired"},
            "change": "Added the trailing line",
        }]),
    );
    fixture.put_report(&replacement);
    // Its response was lost, so it puts the same bytes again.
    fixture.put_report(&replacement);

    // The host restarts before settling.
    let workspace = fixture.repo.join(".orbit");
    fixture.runtime =
        OrbitRuntime::from_roots(&fixture._root.path().join("global"), &workspace).unwrap();

    let refused = fixture
        .settle()
        .expect_err("a dropped required check never opens a PR");
    let certificate = certificate(&fixture);
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete, "{refused}");
    assert!(!certificate.validation_complete);
    let escalation = certificate.escalation.clone().unwrap_or_default();
    assert!(
        escalation.contains(&format!(
            "required check `{CODEQL}` was recorded failed by an earlier report revision of this \
             attempt and the final report omits it"
        )),
        "{escalation}"
    );
    assert_eq!(certificate.retained_obligations.len(), 1);
    let retained = &certificate.retained_obligations[0].validation;
    assert_eq!(
        (retained.command.as_str(), retained.outcome, retained.role),
        (CODEQL, ValidationOutcome::Failed, ValidationRole::Required),
        "the earlier failure stays visible on the certificate"
    );
    assert!(verdict_comment(&fixture).contains(&format!(
        "Required checks retained from earlier report revisions: `{CODEQL}` failed"
    )));
    assert!(
        certificate_acceptable(&certificate).is_err(),
        "coverage refuses the certificate too"
    );

    // Both revisions are retained; the retry added none.
    let history = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("report history");
    let history = ReviewReportHistory::parse(&history.content).unwrap();
    let verdicts = history
        .revisions
        .iter()
        .map(|revision| revision.verdict)
        .collect::<Vec<_>>();
    assert_eq!(
        verdicts,
        vec![ReviewVerdict::Incomplete, ReviewVerdict::AcceptWithFixes]
    );
    let current = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&current.content).unwrap(),
        replacement,
        "the current report is still the replacement; nothing was rewritten"
    );
    let refused = fixture.runtime.run_tool(
        "orbit.task.artifact.put",
        json!({
            "id": fixture.task_id, "model": "codex",
            "path": REVIEW_REPORT_HISTORY_ARTIFACT, "source_path": fixture.repo.join("candidate.txt"),
        }),
    );
    assert!(
        refused.is_err(),
        "only the artifact store writes the report history"
    );
}

/// A first replacement after upgrading must import the held report when no
/// history sidecar existed yet. This is the legacy branch of the same
/// two-report omission case, before any settlement has run.
#[test]
fn first_replacement_imports_a_legacy_report_without_a_history_sidecar() {
    if !super::dispatch_admission::isolated(
        "review_report_revisions::first_replacement_imports_a_legacy_report_without_a_history_sidecar",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&codeql_failed_report(&fixture));
    remove_history_sidecar(&fixture);

    std::fs::write(fixture.repo.join("candidate.txt"), "after\nfixed\n").unwrap();
    let replacement = report(
        &fixture,
        "accept_with_fixes",
        Vec::new(),
        json!([{
            "id": "F1", "severity": "low", "summary": "Trailing line missing",
            "paths": ["candidate.txt"], "disposition": {"kind": "repaired"},
            "change": "Added the trailing line",
        }]),
    );
    fixture.put_report(&replacement);
    fixture.put_report(&replacement);
    let workspace = fixture.repo.join(".orbit");
    fixture.runtime =
        OrbitRuntime::from_roots(&fixture._root.path().join("global"), &workspace).unwrap();

    fixture
        .settle()
        .expect_err("a legacy required failure cannot vanish on first replacement");
    let certificate = certificate(&fixture);
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(!certificate.validation_complete);
    assert_eq!(certificate.retained_obligations.len(), 1);
    assert_eq!(
        certificate.retained_obligations[0].validation.command,
        CODEQL
    );
    assert_eq!(
        certificate.retained_obligations[0].validation.outcome,
        ValidationOutcome::Failed
    );
    let history = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("first replacement creates report history");
    let history = ReviewReportHistory::parse(&history.content).unwrap();
    assert_eq!(history.revisions.len(), 2);
    assert_eq!(history.revisions[0].validation[9].command, CODEQL);
    assert!(certificate_acceptable(&certificate).is_err());
}

/// ORB-14151's shape: every scoped regression passed and the final-candidate
/// workspace run failed only in an unrelated fixture. Recorded as an honest
/// diagnostic it keeps its failed outcome and sources and the bounded task
/// passes; a CodeQL failure an earlier revision recorded is legitimately
/// resolved by its passing rerun. The certificate, comment, and PR body say
/// what was not established instead of claiming the workspace passed.
#[test]
fn an_unrelated_diagnostic_failure_passes_the_bounded_task_with_its_limits_disclosed() {
    if !super::dispatch_admission::isolated(
        "review_report_revisions::an_unrelated_diagnostic_failure_passes_the_bounded_task_with_its_limits_disclosed",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&codeql_failed_report(&fixture));
    fixture.put_report(&report(
        &fixture,
        "accept",
        vec![
            json!({"command": CODEQL, "outcome": "passed", "role": "required"}),
            json!({
                "command": WORKSPACE, "outcome": "failed", "role": "diagnostic",
                "sources": [UNRELATED_FIXTURE],
                "note": "fails only in engine fixtures this task does not touch",
            }),
        ],
        json!([]),
    ));

    let settled = fixture.settle().expect("the bounded task passes");
    assert_eq!(settled["gate"], "passed");
    let limits = settled["review_fixes"].as_str().unwrap_or_default();
    assert!(
        limits.contains("## Review validation limits")
            && limits.contains(&format!(
                "diagnostic `{WORKSPACE}` failed in {UNRELATED_FIXTURE}"
            )),
        "the PR body names the diagnostic limit: {limits}"
    );

    let certificate = certificate(&fixture);
    assert!(certificate.validation_complete);
    assert!(
        certificate.validation.iter().any(|record| {
            record.command == WORKSPACE
                && record.role == ValidationRole::Diagnostic
                && record.outcome == ValidationOutcome::Failed
                && record.sources == [UNRELATED_FIXTURE]
        }),
        "the raw failure and its source binding are kept"
    );
    assert_eq!(
        certificate.retained_obligations[0].validation.outcome,
        ValidationOutcome::Failed,
        "the earlier CodeQL failure stays in history beside its passing rerun"
    );
    assert!(
        certificate
            .validation_scope
            .contains(&"file:candidate.txt".to_string())
    );
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    assert!(verdict_comment(&fixture).contains(&format!(
        "Not established by this review: diagnostic `{WORKSPACE}` failed in {UNRELATED_FIXTURE}"
    )));
}

/// A diagnostic whose failure lies in the candidate's own changed path is a
/// required failure under another name: settlement refuses it.
#[test]
fn a_diagnostic_failing_inside_the_candidate_scope_is_refused() {
    if !super::dispatch_admission::isolated(
        "review_report_revisions::a_diagnostic_failing_inside_the_candidate_scope_is_refused",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&report(
        &fixture,
        "accept",
        vec![json!({
            "command": WORKSPACE, "outcome": "failed", "role": "diagnostic",
            "sources": ["candidate.txt"], "note": "unrelated, the reviewer claims",
        })],
        json!([]),
    ));
    fixture
        .settle()
        .expect_err("an in-scope failure blocks delivery");
    let certificate = certificate(&fixture);
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    assert!(
        certificate
            .escalation
            .as_deref()
            .is_some_and(|reason| reason.contains(&format!(
                "diagnostic `{WORKSPACE}` failed in `candidate.txt`, inside the candidate's scope"
            ))),
        "{:?}",
        certificate.escalation
    );
}

/// The admitted owner command list survives restart and mutable config
/// changes, and a diagnostic cannot replace one of those required checks.
#[test]
fn owner_required_commands_are_bound_at_admission_and_rechecked_by_consumers() {
    if !super::dispatch_admission::isolated(
        "review_report_revisions::owner_required_commands_are_bound_at_admission_and_rechecked_by_consumers",
    ) {
        return;
    }

    let mut fixture = Fixture::new_with_required_commands(&["make ci-fast"]);
    fixture.admit();
    let manifest = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, "review-manifest.json")
        .unwrap()
        .expect("admission manifest");
    let manifest: Value = serde_json::from_slice(&manifest.content).unwrap();
    assert_eq!(
        manifest["required_validation_commands"],
        json!(["make ci-fast"])
    );
    fixture.put_report(&report(
        &fixture,
        "accept",
        vec![json!({
            "command": WORKSPACE,
            "outcome": "failed",
            "role": "diagnostic",
            "sources": [UNRELATED_FIXTURE],
            "note": "the failure is outside this task's scope",
        })],
        json!([]),
    ));

    // A new host config cannot rewrite the admitted run's check authority.
    let config = fixture.repo.join(".orbit/config.toml");
    let contents = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        contents.replace("make ci-fast", "make ci-fast --locked"),
    )
    .unwrap();
    let workspace = fixture.repo.join(".orbit");
    fixture.runtime =
        OrbitRuntime::from_roots(&fixture._root.path().join("global"), &workspace).unwrap();

    fixture
        .settle()
        .expect_err("a bounded diagnostic cannot clear a host-required command");
    let failed_certificate = certificate(&fixture);
    assert_eq!(
        failed_certificate.required_validation_commands,
        Some(vec!["make ci-fast".to_string()])
    );
    assert!(!failed_certificate.validation_complete);
    assert!(
        failed_certificate
            .escalation
            .as_deref()
            .unwrap_or_default()
            .contains("host-required check `make ci-fast` is not established")
    );
    assert!(certificate_acceptable(&failed_certificate).is_err());

    let mut passing = Fixture::new_with_required_commands(&["make ci-fast"]);
    passing.admit();
    passing.put_report(&report(
        &passing,
        "accept",
        vec![json!({
            "command": "make ci-fast",
            "outcome": "passed",
            "role": "required",
        })],
        json!([]),
    ));
    let settled = passing.settle().expect("the host-required check passes");
    let certificate = certificate(&passing);
    assert!(certificate.validation_complete);
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    let body = settled["review_fixes"].as_str().unwrap_or_default();
    assert!(body.contains("## Review validation"));
    assert!(body.contains("`make ci-fast` — passed (required)"));
}
