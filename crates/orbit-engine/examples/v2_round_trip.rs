#![allow(missing_docs)]
// Examples are user-facing smoke binaries that print progress and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! Round-trip smoke for the v2 activity reference YAMLs, plus a kind-mismatch check.
//!
//! Each reference under `crates/orbit-core/assets/activities/` must load through
//! `load_activity_asset`, re-serialize as a `ResourceEnvelope<ActivityV2>` and parse
//! back to an equal YAML value. The comparison is semantic, not byte-level: byte
//! identity would depend on `serde_yaml`'s quoting and key-order choices.
//!
//! An in-memory YAML with `kind: Job` must fail to load as an Activity with
//! `AssetLoadError::KindMismatch`.
//!
//! Usage:
//!     cargo run -p orbit-engine --example v2_round_trip

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use orbit_engine::activity_job::load_activity_asset;
use orbit_types::resource::{ResourceEnvelope, ResourceKind, ResourceMetadata};
use orbit_types::workflow::ActivityV2;

fn main() -> ExitCode {
    let mut failures: Vec<String> = Vec::new();

    for path in v2_reference_paths() {
        let source = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(err) => {
                failures.push(format!("{}: read: {err}", path.display()));
                continue;
            }
        };
        let asset = match load_activity_asset(&source) {
            Ok(asset) => asset,
            Err(err) => {
                failures.push(format!("{}: load: {err}", path.display()));
                continue;
            }
        };

        let envelope = ResourceEnvelope::<ActivityV2> {
            schema_version: 2,
            kind: ResourceKind::Activity,
            metadata: ResourceMetadata::named(asset.name.clone()),
            spec: asset.spec.clone(),
        };
        let re_serialized = match serde_yaml::to_string(&envelope) {
            Ok(s) => s,
            Err(err) => {
                failures.push(format!("{}: serialize: {err}", path.display()));
                continue;
            }
        };

        let source_value: serde_yaml::Value = match serde_yaml::from_str(&source) {
            Ok(v) => v,
            Err(err) => {
                failures.push(format!("{}: parse source: {err}", path.display()));
                continue;
            }
        };
        let round_value: serde_yaml::Value = match serde_yaml::from_str(&re_serialized) {
            Ok(v) => v,
            Err(err) => {
                failures.push(format!("{}: parse round: {err}", path.display()));
                continue;
            }
        };

        if source_value != round_value {
            failures.push(format!(
                "{}: semantic round-trip mismatch\n--- source ---\n{}\n--- round ---\n{}",
                path.display(),
                to_sorted_yaml(&source_value),
                to_sorted_yaml(&round_value),
            ));
        } else {
            println!("round-trip OK: {}", path.display());
        }
    }

    // Kind-mismatch smoke.
    let mismatch_yaml = r#"schemaVersion: 2
kind: Job
metadata:
  name: mismatched
spec:
  type: deterministic
  description: oops
  action: noop
  config: {}
"#;
    match load_activity_asset(mismatch_yaml) {
        Err(orbit_engine::activity_job::AssetLoadError::KindMismatch { expected, actual }) => {
            println!(
                "kind-mismatch smoke OK: expected={} actual={}",
                expected, actual
            );
        }
        Ok(_) => failures.push("kind-mismatch smoke: load succeeded but should have failed".into()),
        Err(err) => failures.push(format!("kind-mismatch smoke: wrong error variant: {err}")),
    }

    if failures.is_empty() {
        println!("\nall v2 round-trip + kind-mismatch smokes passed");
        ExitCode::SUCCESS
    } else {
        eprintln!("\n{} failure(s):", failures.len());
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

fn v2_reference_paths() -> Vec<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let dir = root.join("crates/orbit-core/assets/activities");
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("yaml") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn to_sorted_yaml(v: &serde_yaml::Value) -> String {
    serde_yaml::to_string(v).unwrap_or_default()
}
