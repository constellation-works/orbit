// Fault injection is needed to exercise Seatbelt refusal classification on Linux.
use super::super::probe::{ApplyResult, SandboxExecApplyProbe, classify_apply_result};

#[test]
fn only_exit_71_with_sandbox_apply_can_skip_a_test() {
    let marker = "sandbox-exec: sandbox_apply: Operation not permitted";
    let apply = |success, exit_code, stderr: &str| {
        Some(Ok(ApplyResult {
            success,
            exit_code,
            stderr: stderr.to_string(),
        }))
    };
    let cases = [
        (apply(true, Some(0), ""), "applied", Ok(true)),
        (apply(false, Some(71), marker), "refused", Ok(false)),
        (None, "absent", Err(())),
        (apply(false, Some(71), "invalid profile"), "failed", Err(())),
        (apply(false, Some(1), marker), "failed", Err(())),
        (apply(false, None, marker), "failed", Err(())),
        (
            Some(Err("spawn refused: Operation not permitted".to_string())),
            "failed",
            Err(()),
        ),
        (
            Some(Err("probe deadline elapsed".to_string())),
            "failed",
            Err(()),
        ),
    ];
    for (result, expected_kind, expected_guard) in cases {
        let outcome = classify_apply_result(result);
        let kind = match &outcome {
            SandboxExecApplyProbe::Applied => "applied",
            SandboxExecApplyProbe::ApplyRefused(_) => "refused",
            SandboxExecApplyProbe::BinaryAbsent(_) => "absent",
            SandboxExecApplyProbe::Failed(_) => "failed",
            SandboxExecApplyProbe::NotMacos => panic!("injected apply cannot be non-macOS"),
        };
        assert_eq!(
            kind, expected_kind,
            "distinct apply outcomes must remain distinguishable"
        );
        let mut output = Vec::new();
        let guard = outcome.guard_with_writer("fixture", false, &mut output);
        assert_eq!(
            guard.map_err(|_| ()),
            expected_guard,
            "only the nested apply refusal may skip: {outcome:?}"
        );
        if expected_kind == "refused" {
            assert!(
                outcome
                    .guard_with_writer("fixture", true, &mut Vec::new())
                    .is_err(),
                "required native coverage must refuse a skip"
            );
        } else {
            assert!(
                output.is_empty(),
                "running and failing prerequisites must not print SKIP"
            );
        }
    }
}
