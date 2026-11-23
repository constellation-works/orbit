//! The `workspace` field projects the registry identity the runtime's open
//! recorded, a miss included, without reading `workspaces.json` again.

use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;

fn test_runtime() -> (TempDir, OrbitRuntime) {
    let root =
        TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("repo").join(".orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build test runtime");
    (root, runtime)
}

fn project(runtime: &OrbitRuntime, tool: &str, mut input: Value) -> Value {
    input["workspace"] = json!(runtime.paths().repo_root.to_string_lossy());
    runtime.run_tool(tool, input).expect("project workspace")
}

#[test]
fn workspace_projection_uses_the_identity_recorded_at_open() {
    if !super::dispatch_admission::isolated(
        "task_workspace_projection::workspace_projection_uses_the_identity_recorded_at_open",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let registry = orbit_registry::workspace_registry::registry_path_for(&runtime.global_root());
    assert!(!registry.exists(), "no workspaces.json exists to reload");
    let identity = json!({"id": "ws_opened", "name": "opened"});
    let opened = runtime
        .clone()
        .with_opened_registered_workspace(Some(("ws_opened".into(), "opened".into())));
    let added = project(
        &opened,
        "orbit.task.add",
        json!({
            "title": "Workspace projection",
            "description": "Identity comes from the open.",
            "complexity": "low",
            "fields": ["workspace", "id"],
            "model": "codex"
        }),
    );
    assert_eq!(added["workspace"], identity);
    let id = added["id"].as_str().expect("task id").to_string();
    let shown = project(
        &opened,
        "orbit.task.show",
        json!({"id": id, "fields": ["workspace"], "model": "codex"}),
    );
    assert_eq!(shown["workspace"], identity);
    let updated = project(
        &opened,
        "orbit.task.update",
        json!({"id": id, "priority": "medium", "fields": ["workspace"], "model": "codex"}),
    );
    assert_eq!(updated["workspace"], identity);

    let miss = runtime.with_opened_registered_workspace(None);
    let shown = project(
        &miss,
        "orbit.task.show",
        json!({"id": id, "fields": ["workspace"], "model": "codex"}),
    );
    assert_eq!(shown["workspace"], Value::Null);
}
