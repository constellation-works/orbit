//! Exercise the exported test guard in a child, including visible skip output.
use orbit_exec::SandboxExecApplyProbe;

const CHILD: &str = "apply_probe::refused_apply_fixture";
const FAILURE: &str =
    "permissive profile exited with Some(71): sandbox-exec: sandbox_apply: Operation not permitted";

#[test]
fn refused_apply_prints_a_named_skip_and_required_coverage_fails() {
    let root = tempfile::tempdir().expect("fixture logs");
    for required in [false, true] {
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        child.args(["--ignored", "--exact", CHILD, "--nocapture"]);
        child.env_remove("ORBIT_REQUIRE_SANDBOX_EXEC");
        if required {
            child.env("ORBIT_REQUIRE_SANDBOX_EXEC", "1");
        }
        let output = orbit_common::test_env::run_child_test(&mut child, CHILD, root.path());
        let stderr = String::from_utf8_lossy(&output.stderr);
        if required {
            assert!(
                !output.status.success(),
                "host coverage must fail on a refused apply"
            );
            assert!(
                stderr.contains("ORBIT_REQUIRE_SANDBOX_EXEC=1") && stderr.contains(FAILURE),
                "required failure must name its cause: {stderr}"
            );
            assert!(!stderr.contains("SKIP:"), "required coverage must not skip");
        } else {
            orbit_common::test_env::assert_child_test_passed(
                CHILD,
                output.status,
                &output.stdout,
                &output.stderr,
            );
            assert!(
                stderr.lines().any(|line| line.starts_with("SKIP:")
                    && line.contains(CHILD)
                    && line.contains(FAILURE)),
                "the skipped fixture must expose its name and probe failure: {stderr}"
            );
        }
    }
}

#[test]
#[ignore = "child fixture with an injected Seatbelt apply refusal"]
fn refused_apply_fixture() {
    if !SandboxExecApplyProbe::ApplyRefused(FAILURE.to_string()).test_guard(CHILD) {
        return;
    }
    panic!("a refused apply must never enter the sandbox-dependent body");
}
