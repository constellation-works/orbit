// Examples are binaries and export no API, so missing_docs has nothing to check.
#![allow(missing_docs)]
// Examples are user-facing smoke binaries that print progress and may unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

use orbit_engine::activity_job::load_job_asset;
use orbit_types::workflow::{JobKind, JobV2};

fn main() -> Result<(), String> {
    run_case("workflow", workflow_yaml(), JobKind::Workflow)?;
    run_case("subroutine", subroutine_yaml(), JobKind::Subroutine)?;
    run_case("omitted-default", omitted_kind_yaml(), JobKind::Workflow)?;
    println!("v2 job kind smoke: 3 scenarios passed");
    Ok(())
}

fn run_case(name: &str, yaml: &str, expected_kind: JobKind) -> Result<(), String> {
    let asset = load_job_asset(yaml).map_err(|err| format!("{name}: load failed: {err}"))?;
    if asset.spec.kind != expected_kind {
        return Err(format!(
            "{name}: expected kind {}, got {}",
            expected_kind, asset.spec.kind
        ));
    }

    let roundtrip_yaml = serde_yaml::to_string(&asset.spec)
        .map_err(|err| format!("{name}: serialize failed: {err}"))?;
    let roundtrip: JobV2 = serde_yaml::from_str(&roundtrip_yaml)
        .map_err(|err| format!("{name}: round-trip parse failed: {err}"))?;
    if roundtrip.kind != expected_kind {
        return Err(format!(
            "{name}: round-trip expected kind {}, got {}",
            expected_kind, roundtrip.kind
        ));
    }
    Ok(())
}

fn workflow_yaml() -> &'static str {
    r#"schemaVersion: 2
kind: Job
metadata:
  name: workflow_case
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  steps:
    - id: echo
      spec:
        type: deterministic
        description: workflow step
        action: noop
        config: {}
"#
}

fn subroutine_yaml() -> &'static str {
    r#"schemaVersion: 2
kind: Job
metadata:
  name: subroutine_case
spec:
  state: enabled
  kind: subroutine
  max_active_runs: 1
  steps:
    - id: echo
      spec:
        type: deterministic
        description: subroutine step
        action: noop
        config: {}
"#
}

fn omitted_kind_yaml() -> &'static str {
    r#"schemaVersion: 2
kind: Job
metadata:
  name: omitted_kind_case
spec:
  state: enabled
  max_active_runs: 1
  steps:
    - id: echo
      spec:
        type: deterministic
        description: omitted-kind step
        action: noop
        config: {}
"#
}
