//! Exact permalink `url` exposure on CLI and MCP single-task/single-run
//! results when `machine.web.public_url` is set, omission when unset or on
//! list/projected results, and parity across remote `executed_on` runs.

use super::*;
use orbit_types::task::ExecutionLocation;
use rusqlite::params;

fn seed_job_run(
    workspace: &McpWorkspace,
    workspace_id: &str,
    run_id: &str,
    executed_on: Option<&ExecutionLocation>,
) {
    let db = workspace.store();
    let executed_json = executed_on.map(|loc| serde_json::to_string(loc).unwrap());
    db.execute(
        "INSERT INTO job_runs (
            run_id, workspace_id, job_id, attempt, state, scheduled_at,
            started_at, finished_at, duration_ms, created_at, executed_on_json
        ) VALUES (?1, ?2, 'task_pr_pipeline', 1, 'success',
            '2026-11-26T00:00:00+00:00', '2026-11-26T00:00:01+00:00',
            '2026-11-26T00:00:02+00:00', 1000, '2026-11-26T00:00:00+00:00', ?3)",
        params![run_id, workspace_id, executed_json],
    )
    .expect("seed job run");
}

#[test]
fn single_task_and_run_urls_match_configured_origin_and_execution_location() {
    let test = concat!(
        module_path!(),
        "::single_task_and_run_urls_match_configured_origin_and_execution_location"
    )
    .split_once("::")
    .expect("test module")
    .1;
    if crate::isolated_test::run_in_child(test) {
        return;
    }

    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let task_id = author_task(&workspace, "Permalink URL parity task");

    let ws_show = stdout_json(&workspace.run_ok(&["workspace", "show", "--json"]));
    let workspace_id = ws_show["workspace"]["id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let runtime = orbit_core::runtime::OrbitRuntime::from_roots(
        &workspace.home.join(".orbit"),
        &workspace.work.join(".orbit"),
    )
    .expect("fixture runtime");
    let partition_workspace_id = runtime.workspace_id().expect("partition workspace id");
    let machine_id = orbit_registry::inspect_machine_identity(&workspace.home.join(".orbit"))
        .expect("inspect local machine identity")
        .id()
        .expect("local machine id")
        .to_string();

    let local_run_id = "jrun-20261126-0100";
    let remote_run_id = "jrun-20261126-0200";
    let remote_loc = ExecutionLocation {
        machine_id: "hm_remote_worker".to_string(),
        machine_name: Some("worker-box".to_string()),
    };
    seed_job_run(&workspace, &partition_workspace_id, local_run_id, None);
    seed_job_run(
        &workspace,
        &partition_workspace_id,
        remote_run_id,
        Some(&remote_loc),
    );

    // 1. Unset `machine.web.public_url`: single results omit `url`, and human text omits `URL:`.
    let mut client = workspace.serve_with_args(&["--operator", "--toolset", "runs"]);
    let unset_mcp_task = client.call_tool_ok("orbit_task_show", json!({"id": task_id}));
    assert!(unset_mcp_task.get("url").is_none(), "{unset_mcp_task}");
    let unset_mcp_snapshot = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace": selector, "id": task_id, "snapshot": true}),
    );
    assert!(
        unset_mcp_snapshot.get("url").is_none(),
        "{unset_mcp_snapshot}"
    );
    let unset_mcp_bounded = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace": selector, "id": task_id, "view": "bounded"}),
    );
    assert!(
        unset_mcp_bounded.get("url").is_none(),
        "{unset_mcp_bounded}"
    );
    let unset_mcp_run =
        client.call_tool_ok("orbit_run", json!({"action": "show", "id": local_run_id}));
    assert!(unset_mcp_run.get("url").is_none(), "{unset_mcp_run}");
    let unset_mcp_run_bounded = client.call_tool_ok(
        "orbit_run",
        json!({"workspace": selector, "action": "show", "id": local_run_id, "view": "bounded"}),
    );
    assert!(
        unset_mcp_run_bounded["run"].get("url").is_none(),
        "{unset_mcp_run_bounded}"
    );

    let unset_cli_task = stdout_json(&workspace.run_ok(&["task", "show", &task_id, "--json"]));
    assert!(unset_cli_task.get("url").is_none(), "{unset_cli_task}");
    let unset_cli_task_text =
        String::from_utf8_lossy(&workspace.run_ok(&["task", "show", &task_id]).stdout).into_owned();
    assert!(
        !unset_cli_task_text.contains("URL:"),
        "{unset_cli_task_text}"
    );

    let unset_cli_run =
        stdout_json(&workspace.run_ok(&["run", "show", local_run_id, "--no-reconcile", "--json"]));
    assert!(unset_cli_run["run"].get("url").is_none(), "{unset_cli_run}");
    let unset_cli_run_text = String::from_utf8_lossy(
        &workspace
            .run_ok(&["run", "show", local_run_id, "--no-reconcile"])
            .stdout,
    )
    .into_owned();
    assert!(!unset_cli_run_text.contains("URL:"), "{unset_cli_run_text}");
    drop(client);

    // 2. Configure `machine.web.public_url` (with trailing slash to confirm normalized origin).
    workspace.run_ok(&[
        "config",
        "set",
        "--global",
        "machine.web.public_url",
        "https://orbit.tail123.ts.net/",
    ]);
    let expected_task_url = format!("https://orbit.tail123.ts.net/t/{task_id}");
    let expected_local_run_url = format!(
        "https://orbit.tail123.ts.net/r/{local_run_id}?machine={machine_id}&workspace={workspace_id}"
    );
    let expected_remote_run_url = format!(
        "https://orbit.tail123.ts.net/r/{remote_run_id}?machine=hm_remote_worker&workspace={workspace_id}"
    );

    let mut client = workspace.serve_with_args(&["--operator", "--toolset", "runs"]);

    // CLI and MCP single-task reads share exact permalink URL.
    let mcp_task = client.call_tool_ok("orbit_task_show", json!({"id": task_id}));
    assert_eq!(mcp_task["url"], expected_task_url, "{mcp_task}");
    let mcp_task_bounded = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace": selector, "id": task_id, "view": "bounded"}),
    );
    assert_eq!(
        mcp_task_bounded["url"], expected_task_url,
        "{mcp_task_bounded}"
    );
    let mcp_task_snapshot = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace": selector, "id": task_id, "snapshot": true}),
    );
    assert_eq!(
        mcp_task_snapshot["url"], expected_task_url,
        "{mcp_task_snapshot}"
    );

    let tool_task = stdout_json(&workspace.tool_ok("orbit.task.show", json!({"id": task_id})));
    assert_eq!(tool_task["url"], expected_task_url, "{tool_task}");

    let cli_task = stdout_json(&workspace.run_ok(&["task", "show", &task_id, "--json"]));
    assert_eq!(cli_task["url"], expected_task_url, "{cli_task}");
    let cli_task_text =
        String::from_utf8_lossy(&workspace.run_ok(&["task", "show", &task_id]).stdout).into_owned();
    assert!(
        cli_task_text.contains(&format!("URL: {expected_task_url}")),
        "{cli_task_text}"
    );

    // Single-task writes expose exact URL, while bare `orbit task add` keeps bare ID stdout.
    let created_cli_json = stdout_json(&workspace.run_ok(&[
        "task",
        "add",
        "--title",
        "CLI JSON task URL",
        "--description",
        "Created via CLI JSON",
        "--complexity",
        "low",
        "--json",
    ]));
    let created_cli_id = created_cli_json["id"].as_str().unwrap();
    assert_eq!(
        created_cli_json["url"],
        format!("https://orbit.tail123.ts.net/t/{created_cli_id}")
    );
    let created_bare = String::from_utf8_lossy(
        &workspace
            .run_ok(&[
                "task",
                "add",
                "--title",
                "CLI bare stdout",
                "--description",
                "Created bare",
                "--complexity",
                "low",
            ])
            .stdout,
    )
    .into_owned();
    let bare_id = created_bare.trim();
    assert!(
        bare_id.starts_with("TST-") && !created_bare.contains("https://"),
        "human `orbit task add` must keep bare ID stdout: {created_bare:?}"
    );
    let mcp_created = client.call_tool_ok(
        "orbit_task_add",
        json!({
            "workspace": selector,
            "model": "codex",
            "request_id": "permalink-desktop-create",
            "title": "MCP desktop URL",
            "description": "Created via desktop add",
            "acceptance_criteria": ["Exact permalink"],
        }),
    );
    let mcp_created_id = mcp_created["snapshot"]["task"]["id"].as_str().unwrap();
    assert_eq!(
        mcp_created["url"],
        format!("https://orbit.tail123.ts.net/t/{mcp_created_id}"),
        "{mcp_created}"
    );

    // CLI and MCP single-run reads expose exact qualified URL (local fallback and remote
    // executed_on).
    for (run_id, expected_url) in [
        (local_run_id, &expected_local_run_url),
        (remote_run_id, &expected_remote_run_url),
    ] {
        let mcp_run = client.call_tool_ok("orbit_run", json!({"action": "show", "id": run_id}));
        assert_eq!(mcp_run["url"], *expected_url, "{mcp_run}");

        let mcp_run_bounded = client.call_tool_ok(
            "orbit_run",
            json!({"workspace": selector, "action": "show", "id": run_id, "view": "bounded"}),
        );
        assert_eq!(
            mcp_run_bounded["run"]["url"], *expected_url,
            "{mcp_run_bounded}"
        );

        let tool_run = stdout_json(&orbit_ok(
            McpWorkspace::orbit_command(&workspace.work, &workspace.home)
                .env("ORBIT_OPERATOR", "1")
                .args([
                    "tool",
                    "run",
                    "orbit.run",
                    "--input",
                    &json!({"action": "show", "id": run_id}).to_string(),
                ]),
        ));
        assert_eq!(tool_run["url"], *expected_url, "{tool_run}");

        let cli_run =
            stdout_json(&workspace.run_ok(&["run", "show", run_id, "--no-reconcile", "--json"]));
        assert_eq!(cli_run["run"]["url"], *expected_url, "{cli_run}");

        let cli_run_text = String::from_utf8_lossy(
            &workspace
                .run_ok(&["run", "show", run_id, "--no-reconcile"])
                .stdout,
        )
        .into_owned();
        assert!(
            cli_run_text.contains(&format!("URL: {expected_url}")),
            "{cli_run_text}"
        );
    }

    // Lists do NOT acquire a URL per row.
    let cli_task_list = stdout_json(&workspace.run_ok(&["task", "list", "--json"]));
    let cli_tasks = cli_task_list.as_array().expect("cli task list array");
    assert!(!cli_tasks.is_empty());
    assert!(
        cli_tasks.iter().all(|row| row.get("url").is_none()),
        "{cli_task_list}"
    );

    let mcp_task_list = client.call_tool_ok(
        "orbit_task_list",
        json!({"workspace": selector, "view": "bounded"}),
    );
    assert!(
        mcp_task_list["items"]
            .as_array()
            .expect("mcp task list items")
            .iter()
            .all(|row| row.get("url").is_none()),
        "{mcp_task_list}"
    );

    let tool_task_list = stdout_json(&workspace.tool_ok("orbit.task.list", json!({})));
    assert!(
        tool_task_list["tasks"]
            .as_array()
            .expect("tool task list")
            .iter()
            .all(|row| row.get("url").is_none()),
        "{tool_task_list}"
    );

    let cli_run_history = stdout_json(&workspace.run_ok(&["run", "history", "--json"]));
    let cli_runs = cli_run_history["runs"]
        .as_array()
        .expect("cli run history array");
    assert!(!cli_runs.is_empty());
    assert!(
        cli_runs.iter().all(|row| row.get("url").is_none()),
        "{cli_run_history}"
    );

    let mcp_run_list = client.call_tool_ok("orbit_run", json!({"action": "list"}));
    let mcp_run_items = mcp_run_list["items"].as_array().expect("mcp run list");
    assert!(!mcp_run_items.is_empty());
    assert!(
        mcp_run_items.iter().all(|row| row.get("url").is_none()),
        "{mcp_run_list}"
    );

    let mcp_run_list_bounded = client.call_tool_ok(
        "orbit_run",
        json!({"workspace": selector, "action": "list", "view": "bounded"}),
    );
    let mcp_run_bounded_items = mcp_run_list_bounded["items"]
        .as_array()
        .expect("mcp bounded run list");
    assert!(!mcp_run_bounded_items.is_empty());
    assert!(
        mcp_run_bounded_items
            .iter()
            .all(|row| row.get("url").is_none()),
        "{mcp_run_list_bounded}"
    );

    // Retained projection contracts keep their behavior (`fields` / `field` omit `url`).
    let projected_fields = stdout_json(&workspace.tool_ok(
        "orbit.task.show",
        json!({"id": task_id, "fields": ["id", "title"]}),
    ));
    assert!(projected_fields.get("url").is_none(), "{projected_fields}");
    assert_eq!(projected_fields["id"], task_id);

    let projected_ref = stdout_json(
        &workspace.tool_ok("orbit.task.show", json!({"id": task_id, "field": "title"})),
    );
    assert!(projected_ref.get("url").is_none(), "{projected_ref}");
    assert_eq!(projected_ref, json!("Permalink URL parity task"));

    let snapshot_projected = client.call_tool_ok(
        "orbit_task_show",
        json!({"workspace": selector, "id": task_id, "snapshot": true, "fields": ["revision"]}),
    );
    assert!(
        snapshot_projected.get("url").is_none(),
        "{snapshot_projected}"
    );
    assert_eq!(snapshot_projected.as_object().unwrap().len(), 2);

    let unknown_field =
        client.call_tool_err("orbit_task_show", json!({"id": task_id, "fields": ["url"]}));
    assert_eq!(unknown_field["code"], "invalid_input", "{unknown_field}");
}
