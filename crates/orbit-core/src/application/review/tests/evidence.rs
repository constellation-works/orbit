//! [ORB-24325] A package-wide required run that deferred a library unit test
//! is covered only by a `--lib` requirement filtered to that exact test.

use orbit_types::workflow::{
    EvidenceHostOs, ReviewEvidenceKind, ReviewEvidenceRequirement, ReviewValidation,
    ValidationOutcome, ValidationRole,
};

use super::super::evidence::covers_deferral;

const LIB_TEST: &str =
    "plugin::broker::tests::sandbox::another_runs_sandbox_and_the_host_are_refused_without_a_reply";
const LIB_NOTICE: &str = "DEFERRED: bubblewrap unavailable: plugin::broker::tests::sandbox::another_runs_sandbox_and_the_host_are_refused_without_a_reply: No permissions to create a new namespace";

fn requirement(command: &str) -> ReviewEvidenceRequirement {
    ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::HostSandboxTest,
        name: "lib sandbox".into(),
        command: command.into(),
        artifact: "evidence/lib-sandbox.json".into(),
        os: Some(EvidenceHostOs::Linux),
        hosted_check: None,
    }
}

fn record(command: &str, notice: &str) -> ReviewValidation {
    ReviewValidation {
        id: None,
        command: command.into(),
        outcome: ValidationOutcome::Passed,
        role: ValidationRole::Required,
        note: None,
        check: None,
        control: None,
        sources: Vec::new(),
        mutation_target: Vec::new(),
        repaired_blob: Default::default(),
        deferred: vec![notice.into()],
        baseline: None,
    }
}

fn lib_requirement(filter: &str) -> String {
    format!("cargo test -p orbit-runtime --lib {filter}")
}

/// A required `cargo test -p <crate>` record with no target selector is
/// matched by `--lib` and that exact filter. A different package, a `--test`
/// requirement, a `--lib` requirement against a `--test` record, and a record
/// that itself names `--lib` or another target selector are not.
#[test]
fn a_package_wide_deferral_is_covered_by_the_same_lib_filter() {
    let lib = lib_requirement(LIB_TEST);
    let covered = [
        "cargo test -p orbit-runtime",
        "cargo test -p orbit-runtime -- --nocapture",
        "cargo test -p orbit-runtime plugin::broker::tests::sandbox",
        "cargo test --locked -p orbit-runtime --no-fail-fast",
    ];
    for command in covered {
        assert!(
            covers_deferral(&requirement(&lib), &record(command, LIB_NOTICE), LIB_NOTICE),
            "{command}"
        );
    }

    let uncovered = [
        (
            "cargo test -p orbit-runtime",
            "cargo test -p orbit-exec --lib {LIB_TEST}",
            "a different package",
        ),
        (
            "cargo test -p orbit-runtime",
            "cargo test -p orbit-runtime --test sandbox {LIB_TEST}",
            "a --test requirement",
        ),
        (
            "cargo test -p orbit-runtime --test sandbox",
            "cargo test -p orbit-runtime --lib {LIB_TEST}",
            "a --lib requirement against a --test record",
        ),
        (
            "cargo test -p orbit-runtime --lib",
            "cargo test -p orbit-runtime --lib {LIB_TEST}",
            "a record that itself carries --lib",
        ),
        (
            "cargo test -p orbit-runtime --bins",
            "cargo test -p orbit-runtime --lib {LIB_TEST}",
            "a record that names --bins",
        ),
        (
            "cargo test -p orbit-runtime --all-targets",
            "cargo test -p orbit-runtime --lib {LIB_TEST}",
            "a record that names --all-targets",
        ),
        (
            "cargo test -p orbit-runtime",
            "cargo test -p orbit-runtime --lib plugin::broker::tests::sandbox",
            "a module filter is not the deferred test",
        ),
        (
            "cargo test -p orbit-runtime",
            "cargo test -p orbit-runtime --lib",
            "a --lib requirement without a filter",
        ),
    ];
    for (command, required, why) in uncovered {
        let required = required.replace("{LIB_TEST}", LIB_TEST);
        assert!(
            !covers_deferral(
                &requirement(&required),
                &record(command, LIB_NOTICE),
                LIB_NOTICE
            ),
            "{why}: record `{command}` requirement `{required}`"
        );
    }

    // The integration shape [ORB-17043] still matches, and only that shape.
    let integration = "fixture_children::a_killed_parent_takes_its_group_with_it";
    let integration_notice =
        format!("DEFERRED: ps denied: {integration}: ps -o lstart= -p 1: Operation not permitted");
    assert!(covers_deferral(
        &requirement(&format!(
            "cargo test -p orbit-exec --test probe {integration}"
        )),
        &record(
            "cargo test -p orbit-exec --test probe fixture_children",
            &integration_notice
        ),
        &integration_notice,
    ));
    assert!(!covers_deferral(
        &requirement(&format!("cargo test -p orbit-exec --lib {integration}")),
        &record(
            "cargo test -p orbit-exec --test probe fixture_children",
            &integration_notice
        ),
        &integration_notice,
    ));
}
