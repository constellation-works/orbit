//! [ORB-14478] The closed `host_sandbox_test` grammar, the judgement of a
//! host run's output, and the OS binding both hosts share.

use crate::workflow::automation::SourceRevision;
use crate::workflow::{
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
