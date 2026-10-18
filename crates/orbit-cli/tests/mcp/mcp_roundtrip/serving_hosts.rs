//! Host registration changes reach an already-open MCP workspace runtime.

use super::*;

#[test]
fn host_registry_edits_refresh_a_live_workspace_runtime() {
    let test = concat!(
        module_path!(),
        "::host_registry_edits_refresh_a_live_workspace_runtime"
    )
    .split_once("::")
    .expect("test module")
    .1;
    if crate::isolated_test::run_in_child(test) {
        return;
    }
    let workspace = McpWorkspace::init();
    let log = workspace._temp.path().join("serve-runtime-opens.log");
    let mut client = workspace.serve_logging_runtime_opens(&log);
    client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(runtime_opens(&log), 1);

    // Change only the host registry, leaving the workspace and config layers
    // untouched. An ordinary host mutation commits through the same API.
    let global = workspace.home.join(".orbit");
    let registry = orbit_registry::hosts::load_host_registry(&global).expect("host registry");
    registry
        .commit(vec![orbit_registry::hosts::HostEntry {
            name: "pool-follower".to_string(),
            machine_id: "hm_0123456789abcdef".to_string(),
            ssh: "pool-follower".to_string(),
            task_prefix: "RVW".to_string(),
            host_os: Some(orbit_types::task::HostOs::Linux),
        }])
        .expect("register host");
    client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(
        runtime_opens(&log),
        2,
        "adding a host must refresh the serving OS set in a live MCP session"
    );

    orbit_registry::hosts::load_host_registry(&global)
        .expect("host registry")
        .commit(Vec::new())
        .expect("remove host");
    client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(
        runtime_opens(&log),
        3,
        "removing a host must stop the cached runtime from counting its OS"
    );
    client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(runtime_opens(&log), 3, "unchanged hosts reuse the runtime");
}
