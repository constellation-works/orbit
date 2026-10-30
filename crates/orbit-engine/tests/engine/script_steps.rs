//! Script steps at the public catalog/dispatch boundary: a `target:
//! script:<name>` step load-validates its manifest (missing, escaping entry
//! and inline-code interpreter flags all fail naming the manifest file),
//! resolves, and at dispatch hands the attempt to its host. A host refusal
//! fails the step without retry; a nonzero exit is a failed attempt the
//! step's `retry` repeats; exit 0 must answer with JSON. Every attempt
//! leaves one `script.execution` audit entry.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_agent::loop_engine::InMemorySink;
use orbit_engine::activity_job::{
    V2ActivityCatalog, V2ScriptCatalog, load_job_asset, load_script_asset, resolve_job_target_refs,
    resolve_job_target_refs_with_scripts,
};
use orbit_engine::{
    ScriptStepEnd, ScriptStepRan, ScriptStepRequest, ScriptStepRun, V2AuditWriter, V2SqliteSink,
    execute_job_with_resume,
};
use orbit_store::V2AuditEventFilter;
use orbit_types::workflow::activity_job::{
    ScriptExecutionRecord, V2_EVENT_TYPE_SCRIPT_EXECUTION, script_failure, script_refusal,
};
use serde_json::{Value, json};

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

/// A job writer whose envelopes land in an in-memory store the test reads.
fn audited_writer(
    run_id: &str,
) -> (
    Arc<V2AuditWriter>,
    Arc<orbit_store::Store>,
    tempfile::TempDir,
) {
    let audit_root = tempfile::tempdir().unwrap();
    let blob_dir = audit_root.path().join("blobs");
    fs::create_dir_all(&blob_dir).unwrap();
    let store = Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink"));
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        Arc::clone(&store) as Arc<dyn orbit_store::contracts::V2AuditStoreBackend>,
        "ws_scripts",
        run_id,
        "test-agent",
        None,
        audit_root.path(),
    ));
    let writer = Arc::new(
        V2AuditWriter::new(run_id, "test-agent", Arc::new(InMemorySink::new(blob_dir)))
            .with_envelope_sink(envelope),
    );
    (writer, store, audit_root)
}

/// Every `script.execution` payload, oldest first.
fn script_executions(store: &orbit_store::Store) -> Vec<Value> {
    let mut rows = store
        .list_v2_audit_events(&V2AuditEventFilter {
            workspace_id: "ws_scripts".to_string(),
            event_type: Some(V2_EVENT_TYPE_SCRIPT_EXECUTION.to_string()),
            oldest_first: true,
            ..Default::default()
        })
        .expect("read script executions");
    rows.sort_by_key(|row| row.id);
    rows.iter()
        .map(|row| serde_json::from_str(&row.payload_json).expect("payload is JSON"))
        .collect()
}

fn resolved_script_job(fixture: &Fixture, retry: Option<Value>) -> orbit_types::workflow::JobV2 {
    let mut step = json!({"id": "package", "target": "script:package"});
    if let Some(retry) = retry {
        step["retry"] = retry;
    }
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": {"name": "script_fixture"},
        "spec": {"state": "enabled", "steps": [step]},
    });
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    resolve_job_target_refs_with_scripts(&mut job, &V2ActivityCatalog::new(), &fixture.catalog())
        .expect("valid script step resolves");
    job
}

/// A host that answers every attempt with the same scripted run.
struct ScriptedRunHost {
    end: ScriptStepEnd,
    stdout: &'static str,
    log: &'static str,
    requests: Mutex<Vec<ScriptStepRequest>>,
}

impl ScriptedRunHost {
    fn new(end: ScriptStepEnd, stdout: &'static str, log: &'static str) -> Self {
        Self {
            end,
            stdout,
            log,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for ScriptedRunHost {
    fn run_script_step(&self, request: &ScriptStepRequest) -> ScriptStepRun {
        self.requests.lock().unwrap().push(request.clone());
        ScriptStepRun::Ran(Box::new(ScriptStepRan {
            end: self.end,
            stdout: self.stdout.as_bytes().to_vec(),
            stdout_over_cap: false,
            output_schema: Value::Null,
            log: self.log.as_bytes().to_vec(),
            record: ScriptExecutionRecord {
                script: request.script.clone(),
                digest: "d".repeat(64),
                grant_id: Some("grant-1".to_string()),
                argv: vec!["/usr/bin/bash".to_string()],
                duration_ms: Some(7),
                ..ScriptExecutionRecord::default()
            },
        }))
    }
}

const FAST_RETRY: &str = r#"{"max_attempts": 2, "initial_backoff_ms": 1, "backoff_cap_ms": 1}"#;

#[test]
fn a_host_without_the_script_profile_refuses_before_spawn_and_records_the_refusal() {
    let fixture = Fixture::new();
    fixture.package_script();
    let job = resolved_script_job(&fixture, Some(serde_json::from_str(FAST_RETRY).unwrap()));

    #[derive(Default)]
    struct NullHost;
    impl orbit_engine::test_support::NullRuntimeHost for NullHost {}

    let (audit, store, _audit_root) = audited_writer("jrun-script-refused");
    let error = execute_job_with_resume(
        &job,
        json!({"label": "v1"}),
        "jrun-script-refused",
        audit,
        &NullHost,
        None,
    )
    .expect_err("the script step refuses at dispatch");
    assert!(
        matches!(
            error,
            orbit_engine::DispatchError::ScriptRefused { code, ref script, .. }
                if code == script_refusal::SANDBOX_UNAVAILABLE && script == "package"
        ),
        "dispatch refuses with the typed script_sandbox_unavailable error: {error:?}"
    );
    assert_eq!(error.failure_code(), script_refusal::SANDBOX_UNAVAILABLE);
    assert!(error.is_non_retryable());
    assert!(
        !fixture.workspace().join("spawned-marker").exists(),
        "nothing was spawned: the entry's marker file must not exist"
    );
    let executions = script_executions(&store);
    assert_eq!(
        executions.len(),
        1,
        "one refused attempt, no retry: {executions:?}"
    );
    assert_eq!(executions[0]["outcome"], "refused");
    assert_eq!(
        executions[0]["refusal_code"],
        script_refusal::SANDBOX_UNAVAILABLE
    );
    assert_eq!(executions[0]["step_id"], "package");
}

/// The `Ok(success = false)` leaf ORB-00374 removed with its audit-message
/// coverage: a nonzero exit is retried per the step's `retry`, and the
/// failure message names the exit and the log tail.
#[test]
fn a_nonzero_exit_is_a_failed_attempt_the_step_retry_repeats() {
    let fixture = Fixture::new();
    fixture.package_script();
    let job = resolved_script_job(&fixture, Some(serde_json::from_str(FAST_RETRY).unwrap()));
    let host = ScriptedRunHost::new(ScriptStepEnd::Exited(3), "", "packing\nboom: disk full\n");
    let (audit, store, _audit_root) = audited_writer("jrun-script-exit");
    let outcome = execute_job_with_resume(
        &job,
        json!({"label": "v1"}),
        "jrun-script-exit",
        audit,
        &host,
        None,
    )
    .expect("a nonzero exit is an outcome, not a dispatch error");
    assert!(!outcome.success, "{outcome:?}");
    assert_eq!(host.attempts(), 2, "the step's retry repeats the attempt");
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("exited 3") && message.contains("boom: disk full"),
        "the failure names the exit and the log tail: {message}"
    );
    let executions = script_executions(&store);
    assert_eq!(executions.len(), 2, "one entry per attempt: {executions:?}");
    for execution in &executions {
        assert_eq!(execution["exit_code"], 3);
        assert_eq!(execution["grant_id"], "grant-1");
        assert!(execution["log_blob_ref"].is_string(), "{execution}");
    }
}

#[test]
fn exit_zero_json_becomes_the_step_output() {
    let fixture = Fixture::new();
    fixture.package_script();
    let job = resolved_script_job(&fixture, None);
    let host = ScriptedRunHost::new(
        ScriptStepEnd::Exited(0),
        "{\"archive\": \"site.tgz\"}\n",
        "",
    );
    let (audit, store, _audit_root) = audited_writer("jrun-script-ok");
    let outcome = execute_job_with_resume(
        &job,
        json!({"label": "v1"}),
        "jrun-script-ok",
        audit,
        &host,
        None,
    )
    .expect("the script step succeeds");
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.pipeline["package"]["archive"], "site.tgz",
        "{}",
        outcome.pipeline
    );
    assert_eq!(
        host.requests.lock().unwrap()[0].input,
        json!({"label": "v1"})
    );
    assert_eq!(script_executions(&store)[0]["exit_code"], 0);
}

#[test]
fn exit_zero_with_invalid_output_fails_without_retry() {
    let fixture = Fixture::new();
    fixture.package_script();
    let job = resolved_script_job(&fixture, Some(serde_json::from_str(FAST_RETRY).unwrap()));
    let host = ScriptedRunHost::new(ScriptStepEnd::Exited(0), "packed ok\n", "");
    let (audit, store, _audit_root) = audited_writer("jrun-script-invalid");
    let error = execute_job_with_resume(
        &job,
        json!({"label": "v1"}),
        "jrun-script-invalid",
        audit,
        &host,
        None,
    )
    .expect_err("invalid output fails the step");
    assert_eq!(error.failure_code(), script_failure::OUTPUT_INVALID);
    assert!(error.is_non_retryable());
    assert_eq!(host.attempts(), 1, "invalid output is not retried");
    assert_eq!(
        script_executions(&store)[0]["failure_code"],
        script_failure::OUTPUT_INVALID
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
