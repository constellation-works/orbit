// Examples are binaries with no public API to document, so missing_docs does not apply.
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
//! `load_activity_asset`, re-serialize as a `ResourceEnvelope<ActivityV2>` and load
//! again through `load_activity_asset` to an equal typed activity. The comparison is
//! over the typed `ActivityV2` the loader returns (name plus spec), not over raw YAML:
//! a reference may omit a field the model defaults (`max_iterations`, `provider`,
//! the envelope booleans, `trustedHostExecution`, a deterministic `config`) and the
//! serialized form states that default explicitly, which is the same activity. Any
//! modeled field that the serialized form loses or changes (instruction, tools,
//! envelope flags, schemas, deterministic config, ...) still makes the two typed
//! values differ and fails the reference, with the differing spec keys named.
//! Byte identity would depend on `serde_yaml`'s quoting and key-order choices.
//!
//! The comparison itself is proven before the references run: a reference with its
//! defaults written out must compare equal, and a changed instruction, tool,
//! envelope flag or deterministic config must be reported as different.
//!
//! A reference directory that cannot be read, or that holds no YAML, is a failure,
//! never an empty pass. An in-memory YAML with `kind: Job` must fail to load as an
//! Activity with `AssetLoadError::KindMismatch`.
//!
//! Usage:
//!     cargo run -p orbit-engine --example v2_round_trip

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orbit_engine::activity_job::load_activity_asset;
use orbit_types::resource::{ResourceEnvelope, ResourceKind, ResourceMetadata};
use orbit_types::workflow::ActivityV2;

fn main() -> ExitCode {
    let mut failures: Vec<String> = Vec::new();

    check_comparison_detects_material_changes(&mut failures);

    let dir = reference_dir();
    println!("reference directory: {}", dir.display());
    let paths = match v2_reference_paths(&dir) {
        Ok(paths) if paths.is_empty() => {
            failures.push(format!("{}: no reference YAML files found", dir.display()));
            Vec::new()
        }
        Ok(paths) => paths,
        Err(err) => {
            failures.push(err);
            Vec::new()
        }
    };
    println!("references: {}", paths.len());

    for path in &paths {
        match round_trip_reference(path) {
            Ok(()) => println!("round-trip OK: {}", path.display()),
            Err(err) => failures.push(format!("{}: {err}", path.display())),
        }
    }

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

/// Load one reference, re-serialize its typed activity and compare the reload.
fn round_trip_reference(path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(path).map_err(|err| format!("read: {err}"))?;
    let asset = load_activity_asset(&source).map_err(|err| format!("load: {err}"))?;
    let envelope = ResourceEnvelope::<ActivityV2> {
        schema_version: 2,
        kind: ResourceKind::Activity,
        metadata: ResourceMetadata::named(asset.name),
        spec: asset.spec,
    };
    let re_serialized =
        serde_yaml::to_string(&envelope).map_err(|err| format!("serialize: {err}"))?;
    match semantic_difference(&source, &re_serialized)? {
        None => Ok(()),
        Some(diff) => Err(format!("semantic round-trip mismatch\n{diff}")),
    }
}

/// Compare two activity documents as the typed activities the public loader
/// returns, so a value omitted in one and defaulted explicitly in the other is
/// equal. `Ok(None)` means equal; `Ok(Some(report))` names what differs; `Err`
/// means a document did not load.
fn semantic_difference(left_yaml: &str, right_yaml: &str) -> Result<Option<String>, String> {
    let left = load_activity_asset(left_yaml).map_err(|err| format!("load left: {err}"))?;
    let right = load_activity_asset(right_yaml).map_err(|err| format!("load right: {err}"))?;
    if left.name == right.name && left.spec == right.spec {
        return Ok(None);
    }
    let left_spec = normalized_spec(&left.spec)?;
    let right_spec = normalized_spec(&right.spec)?;
    let mut keys: Vec<String> = Vec::new();
    for map in [&left_spec, &right_spec] {
        if let Some(map) = map.as_mapping() {
            for key in map.keys().filter_map(serde_yaml::Value::as_str) {
                if !keys.iter().any(|k| k == key) {
                    keys.push(key.to_string());
                }
            }
        }
    }
    keys.sort();
    let differing: Vec<&str> = keys
        .iter()
        .filter(|key| left_spec.get(key.as_str()) != right_spec.get(key.as_str()))
        .map(String::as_str)
        .collect();
    let mut report = String::new();
    if left.name != right.name {
        report.push_str(&format!("name: `{}` != `{}`\n", left.name, right.name));
    }
    report.push_str(&format!("differing spec keys: {differing:?}\n"));
    report.push_str(&format!(
        "--- left (normalized) ---\n{}--- right (normalized) ---\n{}",
        to_sorted_yaml(&left_spec),
        to_sorted_yaml(&right_spec),
    ));
    Ok(Some(report))
}

fn normalized_spec(spec: &ActivityV2) -> Result<serde_yaml::Value, String> {
    serde_yaml::to_value(spec).map_err(|err| format!("normalize spec: {err}"))
}

/// Prove `semantic_difference` ignores written-out defaults yet reports a
/// material modeled-field change, so a passing reference run is meaningful.
fn check_comparison_detects_material_changes(failures: &mut Vec<String>) {
    const BASE: &str = r#"schemaVersion: 2
kind: Activity
metadata:
  name: comparison_probe
spec:
  type: agent_loop
  description: probe
  instruction: do the work
  tools:
    - orbit.task.show
"#;
    let explicit_defaults = BASE.to_string()
        + r#"  max_iterations: 25
  provider: claude
  wall_clock_timeout_seconds: 300
  require_response_envelope: false
  require_completion_envelope: true
  trustedHostExecution: false
"#;
    let material = [
        ("instruction", BASE.replace("do the work", "do other work")),
        ("tools", BASE.replace("orbit.task.show", "orbit.task.list")),
        (
            "envelope flag",
            BASE.to_string() + "  require_completion_envelope: false\n",
        ),
        ("provider", BASE.to_string() + "  provider: codex\n"),
    ];
    match semantic_difference(BASE, &explicit_defaults) {
        Ok(None) => println!("comparison smoke OK: explicit defaults compare equal"),
        Ok(Some(diff)) => failures.push(format!(
            "comparison smoke: explicit defaults reported as different\n{diff}"
        )),
        Err(err) => failures.push(format!("comparison smoke: defaults variant: {err}")),
    }
    for (what, changed) in &material {
        match semantic_difference(BASE, changed) {
            Ok(Some(_)) => println!("comparison smoke OK: changed {what} is reported"),
            Ok(None) => failures.push(format!("comparison smoke: changed {what} compared equal")),
            Err(err) => failures.push(format!("comparison smoke: {what} variant: {err}")),
        }
    }

    const DETERMINISTIC: &str = r#"schemaVersion: 2
kind: Activity
metadata:
  name: comparison_probe_det
spec:
  type: deterministic
  description: probe
  action: noop
"#;
    let changed_config = DETERMINISTIC.to_string() + "  config:\n    key: value\n";
    match semantic_difference(DETERMINISTIC, &changed_config) {
        Ok(Some(_)) => println!("comparison smoke OK: changed config is reported"),
        Ok(None) => failures.push("comparison smoke: changed config compared equal".into()),
        Err(err) => failures.push(format!("comparison smoke: config variant: {err}")),
    }
}

fn reference_dir() -> PathBuf {
    // `cargo run` sets the manifest dir at run time; prefer it over the compile-time
    // value, which a compiler cache can carry over from a different checkout.
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    root.join("crates/orbit-core/assets/activities")
}

fn v2_reference_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: read_dir: {err}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("{}: read_dir entry: {err}", dir.display()))?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("yaml") {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}

fn to_sorted_yaml(v: &serde_yaml::Value) -> String {
    serde_yaml::to_string(v).unwrap_or_default()
}
