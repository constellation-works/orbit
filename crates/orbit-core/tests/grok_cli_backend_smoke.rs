#![allow(missing_docs)]
// ORB-00045: this smoke deliberately invokes the installed Grok CLI and uses
// expect/assert for fixture setup and end-to-end validation.
#![allow(clippy::expect_used)]

use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

fn seed_grok_executor(runtime: &OrbitRuntime) {
    let resource: ExecutorResource =
        serde_yaml::from_str(include_str!("../assets/executors/grok.yaml"))
            .expect("parse embedded grok executor");
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name,
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    if !sandbox_exec_can_apply() {
        def.sandbox = None;
    }
    runtime
        .upsert_executor_def(&def)
        .expect("seed grok executor");
}

fn sandbox_exec_can_apply() -> bool {
    std::process::Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)\n(allow default)\n", "/usr/bin/true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
#[ignore = "requires installed, authenticated Grok CLI and network access"]
fn installed_grok_cli_backend_smoke_captures_stdout_artifact() {
    let version = std::process::Command::new("grok")
        .arg("--version")
        .output()
        .expect("grok CLI must be installed on PATH for this smoke");
    assert!(
        version.status.success(),
        "grok --version failed: {}",
        String::from_utf8_lossy(&version.stderr)
    );

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_grok_executor(&runtime);

    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "grok-installed-smoke",
        "grok:grok-4.6".to_string(),
        None,
    )
    .expect("build audit writer");
    let spec = AgentLoopSpec {
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some("grok-4.6".to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Grok,
        wall_clock_timeout_seconds: 120,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    };

    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "grok_installed_smoke",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": "Return exactly this JSON object and no other text: {\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"provider\":\"grok\",\"smoke\":\"ok\"},\"error\":null}"
        }),
        audit: audit.clone(),
        run_id: "grok-installed-smoke",
        host: Some(&runtime),
    })
    .expect("dispatch grok cli backend");

    assert!(
        outcome.success,
        "grok cli backend failed: {:?}; output={}",
        outcome.message, outcome.output
    );
    assert_eq!(outcome.output["provider"], "grok");
    assert!(
        outcome
            .output
            .get("stdout_text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.trim().is_empty()),
        "stdout preview should be non-empty"
    );
    assert!(
        outcome
            .output
            .get("stdout_blob_ref")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty()),
        "stdout blob ref should be captured"
    );
    let invocation = outcome
        .invocation
        .as_ref()
        .expect("well-formed grok stdout should parse invocation trace");
    assert_eq!(invocation.provider, "grok");
    assert_eq!(invocation.model.as_deref(), Some("grok-4.6"));
}
