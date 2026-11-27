//! The `workspace` field projects the registry identity the runtime's open
//! recorded, a miss included, without reading `workspaces.json` again.

use chrono::Utc;
use orbit_common::governance::task::TaskVerb;
use orbit_core::app::task::{self, Command, Input, Shown};
use orbit_core::composition::core_for_command;
use orbit_core::runtime::OrbitRuntime;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};
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

fn full_cli_show(runtime: &OrbitRuntime, id: &str) -> Shown {
    let fields = json!({"id": id, "model": "codex"})
        .as_object()
        .expect("show fields")
        .clone();
    *task::exec(
        &core_for_command(runtime.clone()),
        Command::Operation(TaskVerb::Show, Input::new(fields)),
    )
    .expect("show task through the command-line task surface")
    .into_shown()
    .expect("full task show")
}

fn save_workspace_registry(runtime: &OrbitRuntime, id: &str, name: &str) {
    let now = Utc::now();
    let repo_root = runtime.paths().repo_root.clone();
    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: id.to_string(),
            name: name.to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        }],
        checkouts: vec![WorkspaceCheckout::owner(
            id.to_string(),
            repo_root.clone(),
            repo_root.join(".orbit"),
        )],
        ..WorkspaceRegistry::default()
    };
    orbit_registry::workspace_registry::save_registry_to(
        &registry,
        &orbit_registry::workspace_registry::registry_path_for(&runtime.global_root()),
    )
    .expect("save workspace registry fixture");
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

#[test]
fn full_cli_task_show_uses_the_identity_recorded_at_open() {
    if !super::dispatch_admission::isolated(
        "task_workspace_projection::full_cli_task_show_uses_the_identity_recorded_at_open",
    ) {
        return;
    }

    let (_root, runtime) = test_runtime();
    let added = project(
        &runtime,
        "orbit.task.add",
        json!({
            "title": "Full workspace show",
            "description": "The full command keeps its opened workspace identity.",
            "complexity": "low",
            "fields": ["id"],
            "model": "codex"
        }),
    );
    let id = added["id"].as_str().expect("task id").to_string();
    let registry_path =
        orbit_registry::workspace_registry::registry_path_for(&runtime.global_root());

    save_workspace_registry(&runtime, "ws_opened", "opened");
    let opened = runtime
        .clone()
        .with_opened_registered_workspace(Some(("ws_opened".into(), "opened".into())));

    save_workspace_registry(&runtime, "ws_opened", "renamed");
    assert_eq!(
        full_cli_show(&opened, &id).workspace,
        Some(("ws_opened".into(), "opened".into())),
        "a registry rename after open must not change the command's identity"
    );

    std::fs::remove_file(&registry_path).expect("remove registry after open");
    assert_eq!(
        full_cli_show(&opened, &id).workspace,
        Some(("ws_opened".into(), "opened".into())),
        "an unavailable registry after open must not erase the command's identity"
    );

    let recorded_miss = runtime.with_opened_registered_workspace(None);
    save_workspace_registry(&recorded_miss, "ws_later", "registered-later");
    assert_eq!(
        full_cli_show(&recorded_miss, &id).workspace,
        None,
        "a recorded miss must not be filled by a later registry load"
    );
}
