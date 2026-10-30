//! Script steps phase 1 at the public catalog/dispatch boundary: a
//! `target: script:<name>` step load-validates its manifest (missing,
//! escaping entry and inline-code interpreter flags all fail naming the
//! manifest file), resolves, and then refuses at dispatch with
//! `script_steps_unavailable` without spawning anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_agent::loop_engine::InMemorySink;
use orbit_engine::activity_job::{
    V2ActivityCatalog, V2ScriptCatalog, load_job_asset, load_script_asset, resolve_job_target_refs,
    resolve_job_target_refs_with_scripts,
};
use orbit_engine::{V2AuditWriter, execute_job_with_resume};
use serde_json::json;

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join(".orbit/resources/scripts")).unwrap();
        fs::create_dir_all(root.path().join("tools")).unwrap();
        Self { root }
    }

    fn workspace(&self) -> &Path {
        self.root.path()
    }

    fn scripts_dir(&self) -> PathBuf {
        self.root.path().join(".orbit/resources/scripts")
    }

    fn write_entry(&self, relative: &str, body: &str) {
        let path = self.root.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_manifest(&self, name: &str, spec_yaml: &str) -> PathBuf {
        let path = self.scripts_dir().join(format!("{name}.yaml"));
        fs::write(
            &path,
            format!(
                "schemaVersion: 1\nkind: Script\nmetadata:\n  name: {name}\nspec:\n{spec_yaml}"
            ),
        )
        .unwrap();
        path
    }

    fn catalog(&self) -> V2ScriptCatalog {
        let mut catalog = V2ScriptCatalog::new(self.scripts_dir(), self.workspace().to_path_buf());
        catalog.load_dir().expect("script catalog loads");
        catalog
    }

    fn package_script(&self) -> PathBuf {
        self.write_entry(
            "tools/package.sh",
            "#!/usr/bin/env bash\ntouch spawned-marker\n",
        );
        self.write_manifest(
            "package",
            "  description: Package the built site.\n  entry: tools/package.sh\n  args: [\"--label={{ input.label }}\"]\n",
        )
    }
}

fn script_job() -> orbit_types::workflow::JobV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": {"name": "script_fixture"},
        "spec": {
            "state": "enabled",
            "steps": [
                {"id": "package", "target": "script:package"},
            ],
        },
    });
    load_job_asset(&asset.to_string()).unwrap().spec
}

#[test]
fn a_valid_script_manifest_loads_with_shebang_interpreter_and_digests() {
    let fixture = Fixture::new();
    let manifest = fixture.package_script();
    let asset = load_script_asset(&manifest, fixture.workspace()).expect("manifest loads");
    assert_eq!(asset.name, "package");
    assert_eq!(asset.interpreter, vec!["bash".to_string()]);
    assert_eq!(
        asset.entry_path,
        fixture
            .workspace()
            .join("tools/package.sh")
            .canonicalize()
            .unwrap()
    );
    assert_eq!(asset.digest.len(), 64);
    assert_ne!(asset.digest, asset.manifest_digest);
    assert_ne!(asset.digest, asset.entry_digest);
}

#[test]
fn a_missing_script_manifest_fails_resolution_naming_the_file() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog();
    let mut job = script_job();
    let error = resolve_job_target_refs_with_scripts(&mut job, &V2ActivityCatalog::new(), &catalog)
        .expect_err("missing manifest refuses");
    let expected = fixture.scripts_dir().join("package.yaml");
    assert!(
        error.to_string().contains(&expected.display().to_string()),
        "error names the manifest file: {error}"
    );
}

#[test]
fn an_entry_escaping_the_workspace_fails_to_load_naming_the_manifest() {
    let fixture = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let outside_entry = outside.path().join("evil.sh");
    fs::write(&outside_entry, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_entry, fixture.workspace().join("tools/evil.sh")).unwrap();
    let manifest =
        fixture.write_manifest("evil", "  description: Escapes.\n  entry: tools/evil.sh\n");
    let error =
        load_script_asset(&manifest, fixture.workspace()).expect_err("escaping entry refuses");
    assert!(
        error.to_string().contains(&manifest.display().to_string()),
        "error names the manifest file: {error}"
    );
}

#[test]
fn an_entry_under_orbit_state_fails_to_load_naming_the_manifest() {
    let fixture = Fixture::new();
    fixture.write_entry(".orbit/state/hidden.sh", "#!/bin/sh\nexit 0\n");
    let manifest = fixture.write_manifest(
        "hidden",
        "  description: Lives in state.\n  entry: .orbit/state/hidden.sh\n",
    );
    let error = load_script_asset(&manifest, fixture.workspace()).expect_err("state entry refuses");
    assert!(
        error.to_string().contains(&manifest.display().to_string()),
        "error names the manifest file: {error}"
    );
}

#[test]
fn inline_code_interpreter_flags_fail_to_load_naming_the_manifest() {
    let fixture = Fixture::new();
    fixture.write_entry("tools/run.sh", "echo hi\n");
    for (name, interpreter) in [
        ("inline_bash", "[bash, -c]"),
        ("inline_python", "[python3, -c]"),
        ("inline_node", "[node, -e]"),
    ] {
        let manifest = fixture.write_manifest(
            name,
            &format!(
                "  description: Inline.\n  entry: tools/run.sh\n  interpreter: {interpreter}\n"
            ),
        );
        let error = load_script_asset(&manifest, fixture.workspace())
            .expect_err("inline-code flag refuses");
        assert!(
            error.to_string().contains(&manifest.display().to_string()),
            "error names the manifest file: {error}"
        );
        assert!(
            error.to_string().contains("inline code"),
            "error names the inline-code refusal: {error}"
        );
    }
}

#[test]
fn an_inline_code_field_in_the_manifest_fails_to_load() {
    let fixture = Fixture::new();
    fixture.write_entry("tools/run.sh", "echo hi\n");
    let manifest = fixture.write_manifest(
        "smuggled",
        "  description: Inline text field.\n  entry: tools/run.sh\n  command: rm -rf /\n",
    );
    let error = load_script_asset(&manifest, fixture.workspace())
        .expect_err("inline command field refuses");
    assert!(
        error.to_string().contains(&manifest.display().to_string()),
        "error names the manifest file: {error}"
    );
}

#[test]
fn a_valid_script_step_resolves_and_refuses_at_dispatch_spawning_nothing() {
    let fixture = Fixture::new();
    fixture.package_script();
    let catalog = fixture.catalog();
    let mut job = script_job();
    resolve_job_target_refs_with_scripts(&mut job, &V2ActivityCatalog::new(), &catalog)
        .expect("valid script step resolves");

    #[derive(Default)]
    struct NullHost;
    impl orbit_engine::test_support::NullRuntimeHost for NullHost {}

    let blobs = tempfile::tempdir().unwrap();
    let audit = Arc::new(V2AuditWriter::new(
        "jrun-script-steps",
        "test-agent",
        Arc::new(InMemorySink::new(blobs.path().join("blobs"))),
    ));
    let error = execute_job_with_resume(
        &job,
        json!({"label": "v1"}),
        "jrun-script-steps",
        audit,
        &NullHost,
        None,
    )
    .expect_err("the script step refuses at dispatch");
    assert!(
        matches!(
            error,
            orbit_engine::DispatchError::ScriptStepsUnavailable { ref script, .. }
                if script == "package"
        ),
        "dispatch refuses with the typed script_steps_unavailable error: {error:?}"
    );
    assert_eq!(error.failure_code(), "script_steps_unavailable");
    assert!(error.is_non_retryable());
    assert!(
        !fixture.workspace().join("spawned-marker").exists(),
        "nothing was spawned: the entry's marker file must not exist"
    );
}

#[test]
fn a_routine_script_target_stays_a_load_error() {
    let error = serde_json::from_str::<orbit_types::workflow::RoutineTarget>("\"script:package\"")
        .expect_err("routine script target refuses");
    assert!(
        error
            .to_string()
            .contains("wrap the script in a one-step job"),
        "routine refusal guides to a job: {error}"
    );
}

#[test]
fn resolution_without_a_script_catalog_refuses_a_script_step() {
    // The older-binary shape: a resolver that knows no scripts must fail
    // the load rather than admit the step.
    let mut job = script_job();
    let error = resolve_job_target_refs(&mut job, &V2ActivityCatalog::new())
        .expect_err("script step without a script catalog refuses");
    assert!(
        error.to_string().contains("script `package`"),
        "error names the script: {error}"
    );
}
