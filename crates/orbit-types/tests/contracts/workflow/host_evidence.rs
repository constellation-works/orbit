//! [ORB-14478] The closed `host_sandbox_test` grammar, the judgement of a
//! host run's output, and the OS binding both hosts share.

use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    EvidenceHostOs, HostEvidenceReason, HostSandboxCommand, ReviewEvidenceKind,
    ReviewEvidenceRequirement, ReviewExternalEvidence, ValidationOutcome, judge_host_test_output,
};

fn required() -> Vec<String> {
    vec![
        "make ci-test-affected".to_string(),
        "make ci-lint".to_string(),
    ]
}

#[test]
fn only_the_cargo_test_shape_and_owner_required_commands_are_admitted() {
    assert_eq!(
        HostSandboxCommand::admit(
            "cargo test -p orbit-exec --test sandbox macos_sandbox::",
            &required()
        ),
        Ok(HostSandboxCommand::CargoTest {
            package: "orbit-exec".into(),
            target: "sandbox".into(),
            filter: Some("macos_sandbox::".into()),
        })
    );
    assert_eq!(
        HostSandboxCommand::admit("cargo test --test sandbox -p orbit-exec", &required())
            .map(|command| command.host_command()),
        Ok("cargo test -p orbit-exec --test sandbox -- --nocapture".to_string())
    );
    assert_eq!(
        HostSandboxCommand::admit("make  ci-test-affected", &required()),
        Ok(HostSandboxCommand::RequiredValidation(
            "make ci-test-affected".into()
        ))
    );

    let refused = |command: &str| {
        HostSandboxCommand::admit(command, &required())
            .expect_err(command)
            .reason
    };
    for command in [
        "cargo test -p orbit-exec --test sandbox; rm -rf /",
        "cargo test -p orbit-exec --test sandbox && touch pwned",
        "cargo test -p orbit-exec --test sandbox | tee out",
        "cargo test -p orbit-exec --test $(whoami)",
        "cargo test -p orbit-exec --test `id`",
        "cargo test -p orbit-exec --test sandbox > out",
        "cargo test -p 'orbit-exec' --test sandbox",
        "cargo test -p orbit-exec --test sandbox\nrm -rf /",
        "cargo test -p orbit-exec --test sandbox\tfilter",
        "make ci-lint; true",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::ShellMetacharacter,
            "{command}"
        );
    }
    for command in [
        "",
        "make ci",
        "cargo build -p orbit-exec",
        "cargo",
        "sh -c true",
        "/usr/bin/cargo test -p orbit-exec --test sandbox",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::CommandNotAllowed,
            "{command}"
        );
    }
    for command in [
        "cargo test -p orbit-exec",
        "cargo test --test sandbox",
        "cargo test -p orbit-exec --test sandbox --workspace",
        "cargo test -p orbit-exec --test sandbox --features=x",
        "cargo test -p orbit-exec --test sandbox one two",
        "cargo test -p orbit-exec -p orbit-core --test sandbox",
        "cargo test -p --test sandbox",
        "cargo test -p orbit-exec --test sandbox -- --ignored",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::ArgumentNotAllowed,
            "{command}"
        );
    }
}

#[test]
fn a_cargo_filter_after_the_separator_has_the_same_admitted_host_command() {
    let plain = HostSandboxCommand::admit(
        "cargo test -p orbit-engine --test engine v2_cli_sandbox::dispatch_registration_failure_ends_the_provider_group",
        &[],
    )
    .unwrap();
    let separated = HostSandboxCommand::admit(
        "cargo test -p orbit-engine --test engine -- v2_cli_sandbox::dispatch_registration_failure_ends_the_provider_group",
        &[],
    )
    .expect("a reviewer-named filter after -- must reach owner fulfilment");
    assert_eq!(separated, plain);
    assert_eq!(separated.host_command(), plain.host_command());
    for command in [
        "cargo test -p orbit-engine --test engine --",
        "cargo test -p orbit-engine --test engine -- --ignored",
        "cargo test -p orbit-engine --test engine -- probe --nocapture",
        "cargo test -p orbit-engine --test engine -- one two",
        "cargo test -p orbit-engine --test engine one -- two",
        "cargo test -p orbit-engine -- probe --test engine",
    ] {
        assert_eq!(
            HostSandboxCommand::admit(command, &[])
                .expect_err(command)
                .reason,
            HostEvidenceReason::ArgumentNotAllowed,
            "{command}"
        );
    }
}

#[test]
fn a_skipped_deferred_or_empty_run_is_missing_evidence_never_a_pass() {
    let cargo = HostSandboxCommand::admit("cargo test -p a --test b", &[]).unwrap();
    let required = HostSandboxCommand::RequiredValidation("make ci-test-affected".into());
    let passed = "running 1 test\ntest probe ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    assert_eq!(judge_host_test_output(&cargo, true, false, passed), Ok(1));
    assert_eq!(
        judge_host_test_output(&required, true, false, "lint clean\n"),
        Ok(0)
    );

    let reason = |command: &HostSandboxCommand, success: bool, timed_out: bool, output: &str| {
        judge_host_test_output(command, success, timed_out, output)
            .expect_err(output)
            .reason
    };
    for (output, expected) in [
        (
            "SKIP: sandbox-exec cannot apply a profile on this host: denied\ntest result: ok. 1 passed; 0 failed",
            HostEvidenceReason::SandboxUnavailable,
        ),
        (
            "sandbox-exec: sandbox_apply: Operation not permitted\ntest result: ok. 1 passed; 0 failed",
            HostEvidenceReason::SandboxUnavailable,
        ),
        (
            "SKIP: recovery agent kernel launch\ntest result: ok. 1 passed; 0 failed",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "skipping real Bubblewrap test: no namespaces\ntest result: ok. 1 passed; 0 failed",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "  DEFERRED: bubblewrap unavailable\ntest result: ok. 1 passed; 0 failed",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out",
            HostEvidenceReason::NoTestsRan,
        ),
    ] {
        assert_eq!(reason(&cargo, true, false, output), expected, "{output}");
    }
    assert_eq!(
        reason(&required, true, false, "SKIP: kernel check\n"),
        HostEvidenceReason::SelfSkipped
    );
    assert_eq!(
        reason(&cargo, false, true, passed),
        HostEvidenceReason::TimedOut
    );
    assert_eq!(
        reason(
            &cargo,
            false,
            false,
            "test result: FAILED. 0 passed; 1 failed"
        ),
        HostEvidenceReason::TestFailed
    );
    // A test that panics because the required sandbox cannot apply is the
    // host's condition, not a failed candidate.
    assert_eq!(
        reason(
            &cargo,
            false,
            false,
            "ORBIT_REQUIRE_SANDBOX_EXEC=1 but sandbox-exec cannot apply a profile here: denied"
        ),
        HostEvidenceReason::SandboxUnavailable
    );
}

/// [ORB-24325] `cargo test -p <crate> --lib [<filter>]` is admitted and the
/// host replays that argv with `-- --nocapture`. A run of that shape that
/// skips, defers or passes no test is not evidence.
#[test]
fn a_lib_unit_test_target_is_admitted_and_replayed() {
    let command = "cargo test -p orbit-runtime --lib plugin::broker::tests::sandbox";
    let admitted = HostSandboxCommand::admit(command, &[])
        .unwrap_or_else(|refusal| panic!("lib unit target is admitted: {refusal:?}"));
    assert!(
        matches!(
            admitted,
            HostSandboxCommand::CargoLibTest {
                ref package,
                filter: Some(ref filter),
            } if package == "orbit-runtime" && filter == "plugin::broker::tests::sandbox"
        ),
        "{admitted:?}"
    );
    let host = "cargo test -p orbit-runtime --lib plugin::broker::tests::sandbox -- --nocapture";
    assert_eq!(admitted.host_command(), host);
    assert_eq!(
        HostSandboxCommand::admit(
            "cargo test --lib plugin::broker::tests::sandbox -p orbit-runtime",
            &[]
        )
        .map(|command| command.host_command()),
        Ok(host.to_string())
    );
    assert_eq!(
        HostSandboxCommand::admit(
            "cargo test -p orbit-runtime --lib -- plugin::broker::tests::sandbox",
            &[]
        )
        .map(|command| command.host_command()),
        Ok(host.to_string())
    );
    assert_eq!(
        HostSandboxCommand::admit(
            "cargo test --package orbit-runtime --lib plugin::broker::tests::sandbox",
            &[]
        )
        .map(|command| command.host_command()),
        Ok(host.to_string())
    );
    assert_eq!(
        HostSandboxCommand::admit("cargo test -p orbit-runtime --lib", &[])
            .map(|command| command.host_command()),
        Ok("cargo test -p orbit-runtime --lib -- --nocapture".to_string())
    );

    let passed = "running 1 test\ntest probe ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    assert_eq!(
        judge_host_test_output(&admitted, true, false, passed),
        Ok(1)
    );
    let reason = |output: &str| {
        judge_host_test_output(&admitted, true, false, output)
            .expect_err(output)
            .reason
    };
    assert_eq!(
        reason(
            "  DEFERRED: bubblewrap unavailable: plugin::broker::tests::sandbox: denied\ntest result: ok. 1 passed; 0 failed"
        ),
        HostEvidenceReason::SelfSkipped
    );
    assert_eq!(
        reason(
            "DEFERRED: bubblewrap unavailable: plugin::broker::tests::sandbox: No permissions to create a new namespace\ntest result: ok. 1 passed"
        ),
        HostEvidenceReason::SandboxUnavailable
    );
    assert_eq!(
        reason("SKIP: recovery agent kernel launch\ntest result: ok. 1 passed; 0 failed"),
        HostEvidenceReason::SelfSkipped
    );
    // The seatbelt wording is also a sandbox-unavailable marker, so it is
    // refused before the skip prefix, as an integration target is.
    assert_eq!(
        reason(
            "SKIP: sandbox-exec cannot apply a profile: plugin::broker::tests::sandbox: denied\ntest result: ok. 1 passed"
        ),
        HostEvidenceReason::SandboxUnavailable
    );
    assert_eq!(
        reason(
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out"
        ),
        HostEvidenceReason::NoTestsRan
    );
}

/// Neighbouring `--lib` shapes stay outside the allowlist. An OS mismatch is
/// not an admit decision: `host_evidence_satisfies_only_the_os_its_requirement_names`
/// still refuses evidence from the other host.
#[test]
fn a_lib_unit_shape_refuses_its_neighbours() {
    let refused = |command: &str| {
        HostSandboxCommand::admit(command, &[])
            .expect_err(command)
            .reason
    };
    for command in [
        "cargo test -p orbit-runtime --lib filter; true",
        "cargo test -p orbit-runtime --lib filter && true",
        "cargo test -p orbit-runtime --lib $(whoami)",
        "cargo test -p 'orbit-runtime' --lib filter",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::ShellMetacharacter,
            "{command}"
        );
    }
    for command in [
        "make test",
        "cargo build -p orbit-runtime --lib",
        "sh -c true",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::CommandNotAllowed,
            "{command}"
        );
    }
    for command in [
        "cargo test -p orbit-runtime --lib --bins",
        "cargo test -p orbit-runtime --lib --all-targets",
        "cargo test -p orbit-runtime --lib --workspace",
        "cargo test -p orbit-runtime --lib --features=x",
        "cargo test -p orbit-runtime --lib --release",
        "cargo test -p orbit-runtime --lib -Z",
        "cargo test -p orbit-runtime --lib --test sandbox",
        "cargo test -p orbit-runtime --test sandbox --lib",
        "cargo test --lib plugin::broker::tests::sandbox",
        "cargo test --lib",
        "cargo test -p orbit-runtime -p orbit-core --lib filter",
        "cargo test -p orbit-runtime --lib --lib filter",
        "cargo test -p orbit-runtime --lib one two",
        "cargo test -p orbit-runtime --lib -filter",
        "cargo test -p orbit-runtime --lib -- -filter",
        "cargo test -p orbit-runtime --lib -- one two",
    ] {
        assert_eq!(
            refused(command),
            HostEvidenceReason::ArgumentNotAllowed,
            "{command}"
        );
    }
}

/// One requirement shape serves both hosts: a result counts only for the OS
/// its requirement names, so a Linux run never satisfies a macOS requirement
/// and the reverse.
#[test]
fn host_evidence_satisfies_only_the_os_its_requirement_names() {
    let candidate = SourceRevision {
        commit: "c".repeat(40),
        tree: "t".repeat(40),
    };
    let requirement = |os| ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::HostSandboxTest,
        name: "sandbox".into(),
        command: "cargo test -p orbit-exec --test sandbox".into(),
        artifact: "evidence/sandbox.json".into(),
        os: Some(os),
        hosted_check: None,
    };
    let evidence = |os| ReviewExternalEvidence {
        schema_version: 1,
        attempt_id: "attempt".into(),
        candidate: candidate.clone(),
        kind: ReviewEvidenceKind::HostSandboxTest,
        name: "sandbox".into(),
        command: "cargo test -p orbit-exec --test sandbox".into(),
        outcome: ValidationOutcome::Passed,
        log_artifact: "evidence/sandbox.log.json".into(),
        os,
    };
    for os in [EvidenceHostOs::Linux, EvidenceHostOs::Macos] {
        let other = match os {
            EvidenceHostOs::Linux => EvidenceHostOs::Macos,
            EvidenceHostOs::Macos => EvidenceHostOs::Linux,
        };
        assert!(evidence(Some(os)).matches_requirement(&requirement(os), &candidate));
        assert!(!evidence(Some(other)).matches_requirement(&requirement(os), &candidate));
        assert!(!evidence(None).matches_requirement(&requirement(os), &candidate));
    }
}
