//! [ORB-24325] A package-wide required run that deferred a library unit test is
//! established only when owner-landing settlement receives a `--lib` run of
//! that exact test. Settlement is the public `review_gate_settle` action. A
//! different package, target or filter is not that receipt, and the host
//! runs nothing.

use std::os::unix::fs::PermissionsExt;

use orbit_types::workflow::{
    EvidenceHostOs, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, ReviewCertificate,
    ValidationOutcome,
};
use serde_json::{Value, json};

use super::super::review_before_landing::published;
use super::super::review_gate_audit::Fixture;

const PACKAGE: &str = "orbit-runtime";
const LIB_TEST: &str = "plugin::broker::tests::sandbox::lib_probe";
const MODULE: &str = "plugin::broker::tests::sandbox";
const NOTICE: &str = "DEFERRED: bubblewrap unavailable: plugin::broker::tests::sandbox::lib_probe: No permissions to create a new namespace";
const ARTIFACT: &str = "evidence/lib-sandbox.json";
const LOG: &str = "evidence/lib-sandbox.log.json";

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

/// The package-wide shapes a `--lib <exact test>` receipt establishes: the
/// plain run, arguments after `--`, one module filter, and non-selecting
/// options. Each receipt clears that deferral and passes the review.
fn a_package_wide_lib_deferral_is_received_when_settlement_runs_that_lib_test() {
    let requirement = lib_requirement(LIB_TEST);
    let covered = [
        format!("cargo test -p {PACKAGE}"),
        format!("cargo test -p {PACKAGE} -- --nocapture"),
        format!("cargo test -p {PACKAGE} {MODULE}"),
        format!("cargo test --locked -p {PACKAGE} --no-fail-fast"),
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
            orbit_core::TaskStatus::Review,
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
            "a record that itself carries --lib",
            format!("cargo test -p {PACKAGE} --lib"),
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

crate::scenario_table!(
    package_wide_host_evidence_scenarios,
    a_package_wide_lib_deferral_is_received_when_settlement_runs_that_lib_test,
    a_package_wide_lib_deferral_is_refused_for_a_mismatched_package_target_or_filter,
);
