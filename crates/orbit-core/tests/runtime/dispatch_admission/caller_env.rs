use std::fs;
use std::path::Path;

use orbit_common::Env;
use orbit_core::adapter::command::{
    GlobalAuditStoreCache, ToolEntryPoint, execute_global_plugin_tool,
};
use orbit_core::application::plugin::{
    PluginAddOptions, PluginEnableOptions, enable_plugin, install_plugin,
};
use orbit_core::application::workspace_preset::WorkspacePreset;
use orbit_store::Store;
use orbit_store::contracts::{AuditEventFilter, AuditEventInsertParams, AuditInvocationFields};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use super::*;

fn install_exec_plugin(
    global_root: &Path,
    workspace_root: &Path,
    source_root: &Path,
    name: &str,
    with_schema: bool,
    mutating: bool,
) {
    let root = source_root.join(name).join(".orbit-plugin");
    fs::create_dir_all(root.join("bin")).expect("plugin bin dir");
    let backend = root.join("bin/backend.sh");
    fs::write(
        &backend,
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"output\":{\"pong\":true}}\\n'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).expect("chmod backend");
    }
    if with_schema {
        fs::write(
            root.join("config.schema.json"),
            "{\"type\":\"object\",\"properties\":{\"mode\":{\"type\":\"string\",\"enum\":[\"good\"]}},\"additionalProperties\":false}\n",
        )
        .expect("write schema");
    }
    let config_block = if with_schema {
        "  config:\n    schema: config.schema.json\n    defaults:\n      mode: good\n"
    } else {
        ""
    };
    let tool_policy = if mutating {
        "execution_kind: mutating\n      agent_callable: true"
    } else {
        "execution_kind: read_only"
    };
    fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: 1.0.0\n  \
             description: Caller env regression fixture plugin.\nspec:\n  backend:\n    type: \
             exec\n    command: bin/backend.sh\n  tools:\n    - name: ping\n      description: \
             Answer.\n      {tool_policy}\n      mcp_scope: global\n      input_schema:\n        \
             type: object\n{config_block}"
        ),
    )
    .expect("write plugin manifest");

    let runtime = OrbitRuntime::from_roots(global_root, workspace_root).expect("runtime");
    install_plugin(
        &runtime,
        root.to_str().expect("utf8 source"),
        &PluginAddOptions::default(),
    )
    .expect("install plugin");
    let enable_options = if mutating {
        PluginEnableOptions {
            grants: vec!["agent_tools".to_string()],
            ..Default::default()
        }
    } else {
        PluginEnableOptions::default()
    };
    enable_plugin(&runtime, name, &enable_options).expect("enable plugin");
}

#[test]
fn execute_global_plugin_tool_honors_composed_env_over_ambient() {
    if !isolated(
        "dispatch_admission::caller_env::execute_global_plugin_tool_honors_composed_env_over_ambient",
    ) {
        return;
    }

    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    let sources = root.path().join("sources");
    for dir in [&global_root, &workspace_root, &sources] {
        fs::create_dir_all(dir).expect("create fixture dir");
    }
    install_exec_plugin(
        &global_root,
        &workspace_root,
        &sources,
        "globalprobe",
        false,
        true,
    );

    // Ambient process env carries a retired override, no caller identity (so
    // mutating tool authorization would deny), and a restrictive activity
    // allowlist. The composed Env carries no retired override, an agent
    // identity, and admits the tool.
    let path_var = std::env::var("PATH").unwrap_or_default();
    let composed_env = Env::from_vars([
        ("PATH", path_var.as_str()),
        ("ORBIT_AGENT_NAME", "codex"),
        ("ORBIT_ACTIVITY_TOOLS", "globalprobe.ping"),
    ]);
    let ambient = orbit_common::test_env::scoped([
        ("ORBIT_BACKEND", Some("http")),
        ("ORBIT_AGENT_NAME", None),
        ("ORBIT_ACTIVITY_TOOLS", Some("other.tool")),
    ]);

    let result = execute_global_plugin_tool(
        &composed_env,
        orbit_core::composition::store_opener(),
        &global_root,
        "globalprobe.ping",
        json!({}),
        ToolEntryPoint::Cli,
        ToolSessionContext::default(),
    )
    .expect("composed env must govern global plugin config, allowlist, and authorization");
    assert_eq!(result["pong"], json!(true));

    let store = Store::open(&global_root.join("orbit.db")).expect("open audit store");
    let events = store
        .read()
        .and_then(|snapshot| {
            snapshot.audit_event().list(&AuditEventFilter {
                tool_name: Some("globalprobe.ping".to_string()),
                limit: 10,
                ..Default::default()
            })
        })
        .expect("list audit events");
    assert_eq!(
        events.len(),
        1,
        "expected one audit row for globalprobe.ping"
    );
    assert_eq!(events[0].role, "codex");

    drop(ambient);

    // Reverse check: when the composed Env carries the retired override while
    // the ambient process environment is clean, global_only_config must reject it.
    let _clean_ambient = orbit_common::test_env::scoped([("ORBIT_BACKEND", None)]);
    let rejected_env = Env::from_vars([("PATH", path_var.as_str()), ("ORBIT_BACKEND", "http")]);
    let error = execute_global_plugin_tool(
        &rejected_env,
        orbit_core::composition::store_opener(),
        &global_root,
        "globalprobe.ping",
        json!({}),
        ToolEntryPoint::Cli,
        ToolSessionContext::default(),
    )
    .expect_err("retired ORBIT_BACKEND in composed env must be rejected");
    assert!(
        error.to_string().contains("ORBIT_BACKEND"),
        "expected ORBIT_BACKEND rejection, got: {error}"
    );
}

#[test]
fn global_audit_store_cache_honors_composed_env_over_ambient() {
    if !isolated(
        "dispatch_admission::caller_env::global_audit_store_cache_honors_composed_env_over_ambient",
    ) {
        return;
    }

    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global_root = root.path().join("global");
    fs::create_dir_all(&global_root).expect("create global root");
    let store = Store::open(&global_root.join("orbit.db")).expect("init store");
    drop(store);

    let params = AuditEventInsertParams {
        execution_id: "exec-global-audit-cache-1".to_string(),
        command: "tool".to_string(),
        subcommand: Some("run-mcp".to_string()),
        tool_name: Some("orbit.workspace.list".to_string()),
        target_type: None,
        target_id: None,
        role: "agent".to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 1,
        working_directory: ".".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: None,
        pid: std::process::id(),
        session_id: None,
        workspace_id: None,
        caller_machine_id: None,
        caller_machine_name: None,
        process_machine_id: None,
        process_machine_name: None,
        transport: None,
        effective_capabilities: BTreeSet::new(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    };

    let ambient = orbit_common::test_env::scoped([("ORBIT_BACKEND", Some("http"))]);
    let cache = GlobalAuditStoreCache::new(orbit_core::composition::store_opener());
    cache
        .insert_audit_event_record(
            &Env::default(),
            &global_root,
            &params,
            AuditInvocationFields::default(),
        )
        .expect("clean composed env must succeed despite ambient ORBIT_BACKEND");
    drop(ambient);

    let _clean_ambient = orbit_common::test_env::scoped([("ORBIT_BACKEND", None)]);
    let rejected_env = Env::from_vars([("ORBIT_BACKEND", "http")]);
    let fresh_cache = GlobalAuditStoreCache::new(orbit_core::composition::store_opener());
    let error = fresh_cache
        .insert_audit_event_record(
            &rejected_env,
            &global_root,
            &params,
            AuditInvocationFields::default(),
        )
        .expect_err("retired ORBIT_BACKEND in composed env must be rejected");
    assert!(
        error.to_string().contains("ORBIT_BACKEND"),
        "expected ORBIT_BACKEND rejection, got: {error}"
    );
}

#[test]
fn workspace_preset_validate_for_workspace_honors_composed_env_over_ambient() {
    if !isolated(
        "dispatch_admission::caller_env::workspace_preset_validate_for_workspace_honors_composed_env_over_ambient",
    ) {
        return;
    }

    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    let sources = root.path().join("sources");
    let custom_jobs_dir = root.path().join("custom-jobs");
    let ambient_jobs_dir = root.path().join("ambient-jobs");
    for dir in [
        &global_root,
        &workspace_root,
        &sources,
        &custom_jobs_dir,
        &ambient_jobs_dir,
    ] {
        fs::create_dir_all(dir).expect("create fixture dir");
    }
    install_exec_plugin(
        &global_root,
        &workspace_root,
        &sources,
        "schemaprobe",
        true,
        false,
    );

    fs::write(
        custom_jobs_dir.join("custom_job.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: custom_job\nspec:\n  state: enabled\n  steps:\n    - id: noop\n      target: activity:sleep\n",
    )
    .expect("write custom job");
    let routines_dir = workspace_root.join("routines");
    fs::create_dir_all(&routines_dir).expect("create routines dir");
    fs::write(
        routines_dir.join(".orbit-managed-plugin-assets.json"),
        "{\n  \"schemaVersion\": 1,\n  \"assets\": {\n    \"probe-routine.yaml\": { \"plugin\": \"schemaprobe\", \"version\": \"1.0.0\", \"digest\": \"sha256:abc\" }\n  }\n}\n",
    )
    .expect("write plugin asset manifest");
    fs::write(
        routines_dir.join("probe-routine.yaml"),
        "schemaVersion: 1\nname: probe-routine\ndescription: Probe routine.\nenabled: false\ntrigger:\n  cron: \"0 3 * * *\"\ntarget: job:custom_job\n",
    )
    .expect("write routine");

    let config_preset_path = root.path().join("config-preset.toml");
    fs::write(
        &config_preset_path,
        "name = \"config-and-plugin\"\n\n[enable]\nroutines = [\"probe-routine\"]\n\n[config]\n\"review.minutes\" = 15\n\"plugins.schemaprobe.mode\" = \"good\"\n",
    )
    .expect("write config preset");
    let config_preset = WorkspacePreset::load(&config_preset_path).expect("load config preset");

    let enable_only_preset_path = root.path().join("enable-only-preset.toml");
    fs::write(
        &enable_only_preset_path,
        "name = \"enable-only\"\n\n[enable]\nroutines = [\"probe-routine\"]\n",
    )
    .expect("write enable-only preset");
    let enable_only_preset =
        WorkspacePreset::load(&enable_only_preset_path).expect("load enable-only preset");

    let custom_jobs_str = custom_jobs_dir.to_str().expect("utf8 custom_jobs_dir");
    let ambient_jobs_str = ambient_jobs_dir.to_str().expect("utf8 ambient_jobs_dir");
    let composed_env = Env::from_vars([("ORBIT_JOB_DIR", custom_jobs_str)]);

    let ambient = orbit_common::test_env::scoped([
        ("ORBIT_BACKEND", Some("http")),
        ("ORBIT_JOB_DIR", Some(ambient_jobs_str)),
    ]);

    config_preset
        .validate_for_workspace(
            &composed_env,
            orbit_core::composition::store_opener(),
            &global_root,
            &workspace_root,
        )
        .expect("config + plugin schema preset must honor composed env over ambient");
    enable_only_preset
        .validate_for_workspace(
            &composed_env,
            orbit_core::composition::store_opener(),
            &global_root,
            &workspace_root,
        )
        .expect("enable-only preset must honor composed env over ambient");

    drop(ambient);

    let _clean_ambient =
        orbit_common::test_env::scoped([("ORBIT_BACKEND", None), ("ORBIT_JOB_DIR", None)]);
    let rejected_env = Env::from_vars([
        ("ORBIT_BACKEND", "http"),
        ("ORBIT_JOB_DIR", custom_jobs_str),
    ]);
    for (label, preset) in [
        ("config-and-plugin", &config_preset),
        ("enable-only", &enable_only_preset),
    ] {
        let error = preset
            .validate_for_workspace(
                &rejected_env,
                orbit_core::composition::store_opener(),
                &global_root,
                &workspace_root,
            )
            .expect_err("retired ORBIT_BACKEND in composed env must be rejected");
        assert!(
            error.to_string().contains("ORBIT_BACKEND"),
            "{label}: expected ORBIT_BACKEND rejection, got: {error}"
        );
    }
}
