#![allow(missing_docs, clippy::expect_used)]
#![cfg(unix)]

//! Operator opt-out regression through persisted resources and real dispatch.
//! This tests absence of confinement; it makes no claim about sandbox enforcement.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_core::application::workspace_sync::reconcile_workspace_managed_artifacts;
use orbit_core::bootstrap::init::{InitOptions, init_workspace_at_root};
use orbit_engine::{RuntimeHost, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, AgentLoopSpec, OnDenial, Provider, V2AuditEventKind,
};

fn seed_global(global: &Path) {
    init_workspace_at_root(
        global,
        InitOptions {
            global_only: true,
            // Match CLI `orbit init`, which always sets refresh_defaults.
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("CLI-equivalent global seeding");
}

#[test]
fn explicit_off_survives_opens_and_sync_then_spawns_bare_without_inner_sandbox() {
    let dir = tempfile::tempdir().expect("isolated runtime roots");
    let global = dir.path().join("global");
    let workspace = dir.path().join("repo/.orbit");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    seed_global(&global);

    let program = fake_codex(dir.path());
    let argv_path = dir.path().join("argv.txt");
    let parent_path = dir.path().join("parent.txt");

    // Use the documented operator path, including the literal wire choice.
    let executor_path = global.join("resources/executors/codex.yaml");
    let mut resource: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&executor_path).expect("seeded executor"))
            .expect("executor YAML");
    resource["spec"]["sandbox"] = serde_yaml::Value::String("off".to_string());
    resource["spec"]["command"] = serde_yaml::Value::String(program.display().to_string());
    std::fs::write(
        &executor_path,
        serde_yaml::to_string(&resource).expect("encode operator choice"),
    )
    .expect("persist operator choice");
    let configured = std::fs::read(&executor_path).expect("configured bytes");

    for _ in 0..2 {
        seed_global(&global);
        let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("fresh runtime open");
        assert_eq!(
            runtime
                .get_executor_def("codex")
                .expect("load")
                .expect("codex")
                .sandbox
                .map(|kind| kind.as_str()),
            Some("off")
        );
        reconcile_workspace_managed_artifacts(&global, &workspace, None, None, false)
            .expect("normal managed resource sync");
        assert_eq!(
            std::fs::read(&executor_path).expect("after sync"),
            configured
        );
    }

    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("dispatch runtime");
    let resolved = runtime
        .resolve_executor_sandbox("codex", Some("implementer"), None)
        .expect("resolve off")
        .expect("explicit descriptor");
    assert_eq!(resolved.kind.as_str(), "off");
    assert!(resolved.fs_profile.modify.is_empty());
    assert!(!resolved.allow_fallback);

    let audit = dispatch_codex(&runtime, dir.path());
    assert_eq!(
        std::fs::read_to_string(parent_path)
            .expect("actual parent")
            .trim(),
        std::process::id().to_string(),
        "provider must be a direct child, not a Bubblewrap child"
    );
    let argv = std::fs::read_to_string(argv_path).expect("actual provider arguments");
    let args: Vec<_> = argv.lines().collect();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--sandbox", "danger-full-access"])
    );
    assert!(!args.contains(&"workspace-write"));

    assert_effective_off(&audit, &program);

    // The file store's normal serializer preserves the explicit choice too.
    let def = runtime
        .get_executor_def("codex")
        .expect("load")
        .expect("codex");
    runtime.upsert_executor_def(&def).expect("store round trip");
    let persisted: ExecutorResource =
        serde_yaml::from_str(&std::fs::read_to_string(executor_path).expect("serialized resource"))
            .expect("resource round trip");
    assert_eq!(
        persisted.spec.sandbox.map(|kind| kind.as_str()),
        Some("off")
    );
}

fn fake_codex(dir: &Path) -> PathBuf {
    let program = dir.join("codex");
    let argv_path = dir.join("argv.txt");
    let parent_path = dir.join("parent.txt");
    std::fs::write(
        &program,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > '{}'
printf '%s\n' "$PPID" > '{}'
cat > /dev/null
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"bare":true}},"error":null}}'
"#,
            argv_path.display(),
            parent_path.display()
        ),
    )
    .expect("write fake provider");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("executable provider");

    program
}

fn dispatch_codex(runtime: &OrbitRuntime, dir: &Path) -> Arc<V2AuditWriter> {
    let audit = V2AuditWriter::with_disk_sinks(
        &dir.join("audit"),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "sandbox-off",
        "codex:test".to_string(),
        None,
    )
    .expect("audit writer");
    let spec = ActivityV2Spec::AgentLoop(AgentLoopSpec {
        instruction: "Return the requested response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Codex,
        wall_clock_timeout_seconds: 15,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    });
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "explicit_sandbox_off",
        spec: &spec,
        fs_profile: Some("implementer"),
        input: serde_json::json!({"prompt": "Return success."}),
        audit: audit.clone(),
        run_id: "sandbox-off",
        host: Some(runtime),
    })
    .expect("dispatch explicit off without a wrapper probe");
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["bare"], true);
    audit
}

fn assert_effective_off(audit: &V2AuditWriter, program: &Path) {
    let events = audit
        .events_snapshot()
        .expect("effective invocation introspection");
    let started = events
        .iter()
        .find(|event| matches!(event.kind, V2AuditEventKind::CliInvocationStarted { .. }))
        .expect("invocation started");
    let V2AuditEventKind::CliInvocationStarted {
        argv_redacted,
        sandbox_backend,
        sandbox_trusted_wrapper,
        sandbox_probe_outcome,
        sandbox_write_enforcement,
        sandbox_read_enforcement,
        ..
    } = &started.kind
    else {
        unreachable!("matched invocation event")
    };
    assert_eq!(argv_redacted.first().map(String::as_str), program.to_str());
    assert_eq!(sandbox_backend.as_deref(), Some("off"));
    assert_eq!(sandbox_trusted_wrapper, &None);
    assert_eq!(sandbox_probe_outcome, &None);
    assert_eq!(
        sandbox_write_enforcement.as_deref(),
        Some("write_unrestricted")
    );
    assert_eq!(
        sandbox_read_enforcement.as_deref(),
        Some("read_unrestricted")
    );
}
