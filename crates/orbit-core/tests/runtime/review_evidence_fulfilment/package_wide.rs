//! [ORB-24325] A single-package library (`--lib`) or package-wide required run
//! (optionally wrapped with `scripts/build-budget.py --`) that deferred a
//! library unit test is established only when settlement receives a `--lib` run
//! of that exact test. Settlement is the public `review_gate_settle` action. A
//! different package, target, filter, revision, unknown wrapper, or
//! build-changing flag is not that receipt, and the host runs nothing.

use std::os::unix::fs::PermissionsExt;

use chrono::Utc;
use orbit_engine::RunLifecycle;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    EvidenceHostOs, HostEvidenceReason, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT,
    ReviewCertificate, ReviewEvidenceHold, ReviewEvidenceKind, ValidationOutcome,
};
use serde_json::{Value, json};

use crate::review_before_landing::published;
use crate::review_continuation::{attach, attach_as_operator};
use crate::review_gate_audit::Fixture;

const PACKAGE: &str = "orbit-runtime";

const LIB_TEST: &str = "plugin::broker::tests::sandbox::lib_probe";

const BROKER_LIB_TEST: &str = "another_runs_sandbox_and_the_host_are_refused_without_a_reply";

const MODULE: &str = "plugin::broker::tests::sandbox";

const NOTICE: &str = "DEFERRED: bubblewrap unavailable: plugin::broker::tests::sandbox::lib_probe: No permissions to create a new namespace";

const BROKER_NOTICE: &str = "\
    DEFERRED: bubblewrap unavailable: \
    another_runs_sandbox_and_the_host_are_refused_without_a_reply: \
    Bubblewrap capability probe failed: bwrap: No permissions to create a new namespace";

const SECOND_NOTICE: &str = "\
    DEFERRED: bubblewrap unavailable: another_deferred_lib_test: \
    Bubblewrap capability probe failed: bwrap: No permissions to create a new namespace";

const ARTIFACT: &str = "evidence/lib-sandbox.json";

const LOG: &str = "evidence/lib-sandbox.log.json";

const BROKER_ARTIFACT: &str = "host-checks/orbit-runtime-broker-sandbox.json";

const BROKER_LOG: &str = "host-checks/orbit-runtime-broker-sandbox.log.json";

/// `cargo test -p <package> --lib <filter> -- --nocapture`. Refuse any other
/// selector so a `--test` replay cannot pass by accident.
const CARGO_STUB: &str = "#!/bin/sh\n\
if [ \"$4\" != \"--lib\" ]; then\n\
  echo \"unexpected argv: $*\" >&2\n\
  exit 2\n\
fi\n\
exec sh tests/host/lib.sh \"$5\"\n";

const LIB_PROBE: &str = "\
echo \"ORBIT_HOST_LIB_RAN $1 $(git rev-parse HEAD)\"\n\
echo \"test $1 ... ok\"\n\
echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n";

fn lib_requirement(filter: &str) -> String {
    format!("cargo test -p {PACKAGE} --lib {filter}")
}

fn host_os() -> EvidenceHostOs {
    EvidenceHostOs::current().expect("a supported host OS")
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// An owner landing review whose `cargo` is a stand-in for `--lib`, and whose
/// candidate commits the probe that stand-in runs.
fn fixture() -> (Fixture, tempfile::TempDir) {
    let bin = tempfile::TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let cargo = bin.path().join("cargo");
    std::fs::write(&cargo, CARGO_STUB).unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut fixture = Fixture::before_landing_with_config(&format!(
        "[workflow.validation_env]\npath = [\"{}\"]\n",
        bin.path().display()
    ));
    std::fs::create_dir_all(fixture.repo.join("tests/host")).unwrap();
    std::fs::write(fixture.repo.join("tests/host/lib.sh"), LIB_PROBE).unwrap();
    git(&fixture.repo, &["add", "tests"]);
    git(&fixture.repo, &["commit", "-qm", "library probe"]);
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    (fixture, bin)
}

fn report(fixture: &Fixture, broad: &str, requirement: &str) -> Value {
    json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete",
        "summary": "The package-wide run deferred its library sandbox test.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {"id": "V2", "command": broad, "outcome": "passed", "role": "required",
             "deferred": [NOTICE]},
            {"id": "V3", "command": requirement, "outcome": "not_run", "role": "required"},
        ],
        "external_evidence": [{
            "kind": "host_sandbox_test",
            "name": "Library sandbox",
            "command": requirement,
            "artifact": ARTIFACT,
            "os": host_os().as_str(),
        }],
        "escalation": "The library sandbox test needs the host.",
    })
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
            .unwrap()
            .expect("the certificate is on the task")
            .content,
    )
    .unwrap()
}

fn broad_record<'a>(
    certificate: &'a ReviewCertificate,
    broad: &str,
) -> &'a orbit_types::workflow::ReviewValidation {
    certificate
        .validation
        .iter()
        .find(|record| record.command == broad)
        .unwrap_or_else(|| panic!("the package-wide record stays on the certificate: {broad}"))
}

/// The library and package-wide shapes a `--lib <exact test>` receipt
/// establishes: plain and `scripts/build-budget.py --` runs, explicit `--lib`,
/// arguments after `--`, one module filter, and non-selecting options. Each
/// receipt clears that deferral and passes the review.
fn a_package_wide_lib_deferral_is_received_when_settlement_runs_that_lib_test() {
    let requirement = lib_requirement(LIB_TEST);
    let covered = [
        format!("cargo test -p {PACKAGE}"),
        format!("cargo test -p {PACKAGE} --lib"),
        format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib"),
        format!("./scripts/build-budget.py -- cargo test -p {PACKAGE} --lib"),
        format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib -- --nocapture"),
        format!("cargo test -p {PACKAGE} -- --nocapture"),
        format!("cargo test -p {PACKAGE} {MODULE}"),
        format!("cargo test --locked -p {PACKAGE} --lib --no-fail-fast"),
    ];
    for broad in covered {
        let (fixture, _bin) = fixture();
        fixture.put_report(&report(&fixture, &broad, &requirement));
        let settled = fixture
            .settle()
            .unwrap_or_else(|refused| panic!("{broad}: {refused}"));
        assert_eq!(settled["gate"], "passed", "{broad}: {settled}");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none(),
            "{broad}: a received host run does not hold"
        );
        let certificate = certificate(&fixture);
        assert!(certificate.validation_complete, "{broad}");
        assert!(
            certificate.verdict.passed(),
            "{broad}: {:?}",
            certificate.verdict
        );
        let record = broad_record(&certificate, &broad);
        assert!(record.deferred.is_empty(), "{broad}: {record:?}");
        assert_eq!(
            record.executed_outcome(),
            ValidationOutcome::Passed,
            "{broad}"
        );
        let narrow = certificate
            .validation
            .iter()
            .find(|record| record.command == requirement)
            .unwrap_or_else(|| panic!("{broad}: the --lib record"));
        assert_eq!(narrow.outcome, ValidationOutcome::Passed, "{broad}");
        let [host] = certificate.host_evidence.as_slice() else {
            panic!("{broad}: one host receipt: {:?}", certificate.host_evidence);
        };
        assert!(host.passed, "{broad}: {host:?}");
        assert_eq!(host.command, requirement, "{broad}");
        let log: Value = serde_json::from_slice(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .expect("log attached")
                .content,
        )
        .unwrap();
        assert_eq!(
            log["host_command"],
            format!("cargo test -p {PACKAGE} --lib {LIB_TEST} -- --nocapture"),
            "{broad}"
        );
        assert_eq!(log["tests_passed"], 1, "{broad}: {log}");
        let head = certificate.final_candidate.commit.as_str();
        assert!(
            log["output"]
                .as_str()
                .unwrap_or_default()
                .lines()
                .any(|line| { line == format!("ORBIT_HOST_LIB_RAN {LIB_TEST} {head}") }),
            "{broad}: the host ran the exact library test at the final candidate: {log}"
        );
        let evidence: Value = serde_json::from_slice(
            &fixture
                .runtime
                .get_task_artifact(&fixture.task_id, ARTIFACT)
                .unwrap()
                .expect("result attached")
                .content,
        )
        .unwrap();
        assert_eq!(evidence["outcome"], "passed", "{broad}");
        assert_eq!(evidence["command"], requirement, "{broad}");
        assert_eq!(
            fixture.runtime.get_task(&fixture.task_id).unwrap().status,
            orbit_types::task::TaskStatus::Review,
            "{broad}"
        );
    }
}

/// A selector that does not name this package's deferred library test is not
/// a receipt: settlement blocks, the deferral stays, and the host runs nothing.
fn a_package_wide_lib_deferral_is_refused_for_a_mismatched_package_target_or_filter() {
    let exact = lib_requirement(LIB_TEST);
    let package_wide = format!("cargo test -p {PACKAGE}");
    let cases = [
        (
            "a different package",
            package_wide.clone(),
            format!("cargo test -p orbit-exec --lib {LIB_TEST}"),
        ),
        (
            "a --test requirement",
            package_wide.clone(),
            format!("cargo test -p {PACKAGE} --test sandbox {LIB_TEST}"),
        ),
        (
            "a --lib requirement against a --test record",
            format!("cargo test -p {PACKAGE} --test sandbox"),
            exact.clone(),
        ),
        (
            "a record combining --lib with --test",
            format!("cargo test -p {PACKAGE} --lib --test sandbox"),
            exact.clone(),
        ),
        (
            "a record repeating --lib",
            format!("cargo test -p {PACKAGE} --lib --lib"),
            exact.clone(),
        ),
        (
            "a record that names --bins",
            format!("cargo test -p {PACKAGE} --bins"),
            exact.clone(),
        ),
        (
            "a record that names --all-targets",
            format!("cargo test -p {PACKAGE} --all-targets"),
            exact.clone(),
        ),
        (
            "an unknown wrapper around --lib",
            format!("scripts/other-wrapper.py -- cargo test -p {PACKAGE} --lib"),
            exact.clone(),
        ),
        (
            "build-budget without --",
            format!("scripts/build-budget.py cargo test -p {PACKAGE} --lib"),
            exact.clone(),
        ),
        (
            "nested wrapper inside build-budget",
            format!(
                "scripts/build-budget.py -- scripts/with-test-build-lint.sh --check cargo test -p {PACKAGE} --lib"
            ),
            exact.clone(),
        ),
        (
            "a build-changing --release flag",
            format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib --release"),
            exact.clone(),
        ),
        (
            "a build-changing --features flag",
            format!(
                "scripts/build-budget.py -- cargo test -p {PACKAGE} --lib --features test-support"
            ),
            exact.clone(),
        ),
        (
            "a build-changing --profile flag",
            format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib --profile ci"),
            exact.clone(),
        ),
        (
            "a build-changing --target flag",
            format!(
                "scripts/build-budget.py -- cargo test -p {PACKAGE} --lib --target x86_64-unknown-linux-gnu"
            ),
            exact.clone(),
        ),
        (
            "a multi-package library record",
            format!("scripts/build-budget.py -- cargo test -p orbit-cli -p {PACKAGE} --lib"),
            exact.clone(),
        ),
        (
            "a module filter is not the deferred test",
            package_wide.clone(),
            lib_requirement(MODULE),
        ),
        (
            "a --lib requirement without a filter",
            package_wide,
            format!("cargo test -p {PACKAGE} --lib"),
        ),
    ];
    for (why, broad, requirement) in cases {
        let (fixture, _bin) = fixture();
        fixture.put_report(&report(&fixture, &broad, &requirement));
        let refused = fixture.settle().expect_err(why).to_string();
        assert!(refused.contains("review_gate_blocked"), "{why}: {refused}");
        let certificate = certificate(&fixture);
        assert!(
            certificate.host_evidence.is_empty(),
            "{why}: {:?}",
            certificate.host_evidence
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, LOG)
                .unwrap()
                .is_none(),
            "{why}: the host ran"
        );
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, ARTIFACT)
                .unwrap()
                .is_none(),
            "{why}: a result was accepted"
        );
        let record = broad_record(&certificate, &broad);
        assert_eq!(record.deferred, [NOTICE.to_string()], "{why}");
        assert_eq!(
            record.executed_outcome(),
            ValidationOutcome::NotRun,
            "{why}"
        );
        assert!(!certificate.verdict.passed(), "{why}");
    }
}

fn git_output(repo: &std::path::Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn v9_v12_report(
    fixture: &Fixture,
    v9_command: &str,
    v9_outcome: &str,
    v9_deferred: &[&str],
    v12_record: Option<(&str, &str)>,
    requirement_command: &str,
) -> Value {
    let mut validation = vec![
        json!({"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}),
        json!({
            "id": "V9",
            "command": v9_command,
            "outcome": v9_outcome,
            "role": "required",
            "deferred": v9_deferred,
        }),
    ];
    if let Some((command, outcome)) = v12_record {
        validation.push(json!({
            "id": "V12",
            "command": command,
            "outcome": outcome,
            "role": "required",
        }));
    }
    json!({
        "schema_version": 1,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete",
        "summary": "No findings; only the Bubblewrap-deferred orbit-runtime library test remains.",
        "findings": [],
        "validation": validation,
        "external_evidence": [{
            "kind": "host_sandbox_test",
            "name": BROKER_LIB_TEST,
            "command": requirement_command,
            "artifact": BROKER_ARTIFACT,
            "os": "linux",
        }],
        "escalation": format!(
            "Decision needed: run `{requirement_command}` and attach `{BROKER_ARTIFACT}`."
        ),
    })
}

/// Modeled on ORB-29024's V9/V12 pair: V9 runs `scripts/build-budget.py --
/// cargo test -p orbit-runtime --lib` with one Bubblewrap deferral and V12
/// names the exact library test. Settlement issues a native Linux hold for V12;
/// untrusted or wrong-revision evidence does not settle it, and authenticated
/// Linux evidence for the pinned candidate tree releases the hold and settles
/// the matching single deferral without deferral.
fn budgeted_library_v9_v12_pair_holds_and_settles_with_authenticated_linux_evidence() {
    let v9 = format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib");
    let v12 = lib_requirement(BROKER_LIB_TEST);

    let mut fixture = Fixture::new();
    fixture.admit();
    fixture.put_report(&v9_v12_report(
        &fixture,
        &v9,
        "passed",
        &[BROKER_NOTICE],
        Some((&v12, "not_run")),
        &v12,
    ));

    let held = fixture
        .settle()
        .expect_err("the V9/V12 pair issues a native hold")
        .to_string();
    assert!(
        held.contains("review_awaiting_evidence"),
        "expected evidence hold, got: {held}"
    );
    let hold: ReviewEvidenceHold = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .expect("review-evidence-hold.json attached")
            .content,
    )
    .unwrap();
    assert_eq!(hold.requirements.len(), 1, "{hold:?}");
    assert_eq!(
        hold.requirements[0].kind,
        ReviewEvidenceKind::HostSandboxTest
    );
    assert_eq!(hold.requirements[0].os, Some(EvidenceHostOs::Linux));
    assert_eq!(hold.requirements[0].command, v12);
    assert_eq!(hold.requirements[0].artifact, BROKER_ARTIFACT);
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::InProgress
    );

    let log_payload = json!({
        "schema_version": 1,
        "tested_head": hold.candidate.commit,
        "tested_tree": hold.candidate.tree,
        "command": v12,
        "outcome": "passed",
        "output": format!("test {BROKER_LIB_TEST} ... ok\ntest result: ok. 1 passed; 0 failed"),
    });

    // Wrong revision (different tree) from an operator does not release the hold.
    let wrong_tree_result = json!({
        "schema_version": 1,
        "attempt_id": hold.attempt_id,
        "candidate": {
            "commit": hold.candidate.commit,
            "tree": "0000000000000000000000000000000000000000",
        },
        "kind": "host_sandbox_test",
        "name": BROKER_LIB_TEST,
        "command": v12,
        "outcome": "passed",
        "log_artifact": BROKER_LOG,
        "os": "linux",
    });
    attach_as_operator(&fixture, BROKER_LOG, &log_payload);
    attach_as_operator(&fixture, BROKER_ARTIFACT, &wrong_tree_result);
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::InProgress,
        "wrong-revision evidence must not release the hold"
    );

    // Agent-written evidence for the pinned candidate tree does not release the hold.
    let pinned_result = json!({
        "schema_version": 1,
        "attempt_id": hold.attempt_id,
        "candidate": hold.candidate,
        "kind": "host_sandbox_test",
        "name": BROKER_LIB_TEST,
        "command": v12,
        "outcome": "passed",
        "log_artifact": BROKER_LOG,
        "os": "linux",
    });
    attach(&fixture, BROKER_ARTIFACT, &pinned_result);
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::InProgress,
        "unauthenticated agent evidence must not release the hold"
    );

    // Authenticated Linux operator evidence at the pinned candidate tree releases
    // the hold and settles V9/V12 cleanly on fresh settlement.
    attach_as_operator(&fixture, BROKER_ARTIFACT, &pinned_result);
    assert_eq!(
        fixture.runtime.get_task(&fixture.task_id).unwrap().status,
        TaskStatus::Backlog,
        "authenticated Linux evidence releases the hold"
    );

    // Fresh review on the same pinned candidate tree settles V9 and V12 to accept.
    let head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
    assert_eq!(head, hold.candidate.commit);
    let previous = fixture.runtime.show_job_run(&hold.run_id).unwrap();
    let next = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), previous.input, None)
        .unwrap();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .apply_task_automation_update(
            &fixture.task_id,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(next.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    fixture.input["job_run_id"] = json!(next.run_id);
    fixture.admit();
    assert_ne!(fixture.input["admission"]["attempt_id"], hold.attempt_id);
    fixture.put_report(&v9_v12_report(
        &fixture,
        &v9,
        "passed",
        &[BROKER_NOTICE],
        Some((&v12, "not_run")),
        &v12,
    ));
    let settled = fixture
        .settle()
        .expect("authenticated Linux evidence settles the matching V9 deferral");
    assert_eq!(settled["gate"], "passed", "{settled}");
    let cert = certificate(&fixture);
    assert!(cert.verdict.passed(), "{:?}", cert.verdict);
    assert!(cert.validation_complete);
    let v9_record = broad_record(&cert, &v9);
    assert!(v9_record.deferred.is_empty(), "{v9_record:?}");
    assert_eq!(v9_record.executed_outcome(), ValidationOutcome::Passed);
    let v12_record = broad_record(&cert, &v12);
    assert_eq!(v12_record.outcome, ValidationOutcome::Passed);

    // Synthesized hold when reviewer accepted with V9 alone (no V12 named).
    let mut synth = Fixture::new();
    synth.admit();
    synth.put_report(&json!({
        "schema_version": 1,
        "attempt_id": synth.input["admission"]["attempt_id"],
        "verdict": "accept",
        "summary": "No findings.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {
                "id": "V9",
                "command": v9,
                "outcome": "passed",
                "role": "required",
                "deferred": [BROKER_NOTICE],
            },
        ],
    }));
    let synth_held = synth
        .settle()
        .expect_err("accepted budgeted --lib deferral synthesizes a native hold")
        .to_string();
    assert!(
        synth_held.contains("review_awaiting_evidence"),
        "{synth_held}"
    );
    let synth_hold: ReviewEvidenceHold = serde_json::from_slice(
        &synth
            .runtime
            .get_task_artifact(&synth.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .expect("synthesized hold attached")
            .content,
    )
    .unwrap();
    assert_eq!(synth_hold.requirements.len(), 1);
    assert_eq!(synth_hold.requirements[0].command, v12);
    assert_eq!(synth_hold.requirements[0].os, Some(EvidenceHostOs::Linux));
}

/// When the V9/V12 library pair cannot be safely reconciled, settlement stays
/// fail-closed (no hold, no host run), names V9 in the operator diagnostic, and
/// gives a runnable native command and fresh-review route. Also confirms
/// HostSandboxCommand does not broaden to admit wrapped commands directly.
fn unreconciled_v9_v12_library_pairs_refuse_and_name_v9_with_runnable_route() {
    let exact_v9 = format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib");
    let exact_v12 = lib_requirement(BROKER_LIB_TEST);
    let cases = [
        (
            "unknown wrapper on V9",
            format!("scripts/other-wrapper.py -- cargo test -p {PACKAGE} --lib"),
            "passed",
            vec![BROKER_NOTICE],
            Some((exact_v12.clone(), "not_run")),
            exact_v12.clone(),
            false,
        ),
        (
            "build-changing flag on V9",
            format!("scripts/build-budget.py -- cargo test -p {PACKAGE} --lib --release"),
            "passed",
            vec![BROKER_NOTICE],
            Some((exact_v12.clone(), "not_run")),
            exact_v12.clone(),
            false,
        ),
        (
            "different package on V12",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE],
            Some((
                format!("cargo test -p orbit-core --lib {BROKER_LIB_TEST}"),
                "not_run",
            )),
            format!("cargo test -p orbit-core --lib {BROKER_LIB_TEST}"),
            true,
        ),
        (
            "different target on V12",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE],
            Some((
                format!("cargo test -p {PACKAGE} --test sandbox {BROKER_LIB_TEST}"),
                "not_run",
            )),
            format!("cargo test -p {PACKAGE} --test sandbox {BROKER_LIB_TEST}"),
            true,
        ),
        (
            "different named test on V12",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE],
            Some((lib_requirement("other_lib_test"), "not_run")),
            lib_requirement("other_lib_test"),
            true,
        ),
        (
            "second uncovered deferral notice on V9",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE, SECOND_NOTICE],
            Some((exact_v12.clone(), "not_run")),
            exact_v12.clone(),
            true,
        ),
        (
            "missing required V12 record",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE],
            None,
            exact_v12.clone(),
            true,
        ),
        (
            "failed required V12 record",
            exact_v9.clone(),
            "passed",
            vec![BROKER_NOTICE],
            Some((exact_v12.clone(), "failed")),
            exact_v12.clone(),
            true,
        ),
    ];

    for (why, v9_cmd, v9_outcome, deferred, v12_rec, req_cmd, expect_focused_native) in cases {
        let mut fixture = Fixture::new();
        fixture.admit();
        let v12_pair = v12_rec
            .as_ref()
            .map(|(cmd, outcome)| (cmd.as_str(), *outcome));
        fixture.put_report(&v9_v12_report(
            &fixture, &v9_cmd, v9_outcome, &deferred, v12_pair, &req_cmd,
        ));
        let refused = fixture.settle().expect_err(why).to_string();
        assert!(refused.contains("review_gate_blocked"), "{why}: {refused}");
        assert!(
            fixture
                .runtime
                .get_task_artifact(&fixture.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
                .unwrap()
                .is_none(),
            "{why}: unreconcilable pair must not issue a hold"
        );
        let cert = certificate(&fixture);
        assert!(!cert.verdict.passed(), "{why}");
        assert!(cert.host_evidence.is_empty(), "{why}");
        let escalation = cert.escalation.as_deref().unwrap_or_default();
        assert!(
            escalation.contains("unreconciled_library_deferral")
                && escalation.contains("`V9`")
                && escalation.contains(&v9_cmd)
                && escalation.contains("fresh review"),
            "{why}: diagnostic must name V9, its command, and fresh-review route: {escalation}"
        );
        if expect_focused_native {
            let expected_test = if deferred.len() > 1 {
                "another_deferred_lib_test"
            } else {
                BROKER_LIB_TEST
            };
            assert!(
                escalation.contains(&format!(
                    "cargo test -p {PACKAGE} --lib {expected_test} -- --nocapture"
                )),
                "{why}: diagnostic must give runnable native command: {escalation}"
            );
        } else {
            assert!(
                escalation.contains(&format!("run `{v9_cmd}` natively")),
                "{why}: diagnostic must give runnable native command for V9: {escalation}"
            );
        }
        assert!(
            refused.contains("`V9`"),
            "{why}: refusal message must surface V9 diagnostic: {refused}"
        );
    }

    // Failed V9 stays blocked and uncovered even when V12 matches.
    let mut failed_v9 = Fixture::new();
    failed_v9.admit();
    failed_v9.put_report(&v9_v12_report(
        &failed_v9,
        &exact_v9,
        "failed",
        &[BROKER_NOTICE],
        Some((&exact_v12, "not_run")),
        &exact_v12,
    ));
    let refused = failed_v9
        .settle()
        .expect_err("failed V9 stays uncovered")
        .to_string();
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    assert!(
        failed_v9
            .runtime
            .get_task_artifact(&failed_v9.task_id, REVIEW_EVIDENCE_HOLD_ARTIFACT)
            .unwrap()
            .is_none()
    );

    // Admission of arbitrary/wrapped commands in HostSandboxCommand stays refused.
    let (wrapped_req_fixture, _bin) = fixture();
    let wrapped_req = format!("scripts/build-budget.py -- {exact_v12}");
    wrapped_req_fixture.put_report(&json!({
        "schema_version": 1,
        "attempt_id": wrapped_req_fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete",
        "summary": "Attempted wrapped host requirement.",
        "findings": [],
        "validation": [
            {"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"},
            {"id": "V12", "command": wrapped_req, "outcome": "not_run", "role": "required"},
        ],
        "external_evidence": [{
            "kind": "host_sandbox_test",
            "name": BROKER_LIB_TEST,
            "command": wrapped_req,
            "artifact": BROKER_ARTIFACT,
            "os": host_os().as_str(),
        }],
        "escalation": "Needs host run.",
    }));
    wrapped_req_fixture
        .settle()
        .expect_err("wrapped host_sandbox_test command stays refused");
    let cert = certificate(&wrapped_req_fixture);
    let [host_refusal] = cert.host_evidence.as_slice() else {
        panic!("expected one host refusal: {:?}", cert.host_evidence);
    };
    assert_eq!(
        host_refusal.reason,
        Some(HostEvidenceReason::CommandNotAllowed)
    );
    assert!(host_refusal.host_command.is_none());
}

crate::scenario_table!(
    package_wide_host_evidence_scenarios,
    a_package_wide_lib_deferral_is_received_when_settlement_runs_that_lib_test,
    a_package_wide_lib_deferral_is_refused_for_a_mismatched_package_target_or_filter,
    budgeted_library_v9_v12_pair_holds_and_settles_with_authenticated_linux_evidence,
    unreconciled_v9_v12_library_pairs_refuse_and_name_v9_with_runnable_route,
);
