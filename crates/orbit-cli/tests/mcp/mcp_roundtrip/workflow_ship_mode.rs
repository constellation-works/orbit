//! Ship's own `mode` must not trip the auto-drain workspace echo.

use super::*;

/// A bound `orbit.workflow` ship call that passes `mode` and no workspace
/// reaches the same gate as the call that omits `mode`. An `auto` call that
/// carries `mode` still requires an explicit selector.
#[test]
fn ship_mode_without_a_workspace_selector_matches_the_omitted_mode_gate() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--toolset", "dispatch"]);
    let task_ids = json!(["ORB-00001"]);

    let omitted = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "ship", "task_ids": task_ids, "model": "codex"}),
    );
    assert_eq!(omitted["code"], "capability_denied", "{omitted}");
    assert!(
        !message_contains(&omitted, "requires an explicit workspace selector"),
        "{omitted}"
    );

    for mode in ["local", "pr"] {
        let with_mode = client.call_tool_err(
            "orbit_workflow",
            json!({"action": "ship", "task_ids": task_ids, "mode": mode, "model": "codex"}),
        );
        assert_eq!(
            with_mode["code"], omitted["code"],
            "ship mode {mode} must reach the same gate as the call that omits mode: {with_mode}"
        );
        assert!(
            !message_contains(&with_mode, "requires an explicit workspace selector"),
            "ship mode {mode} must not be refused for a missing workspace selector: {with_mode}"
        );
    }

    let auto = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "mode": "status", "model": "codex"}),
    );
    assert_eq!(auto["code"], "invalid_input", "{auto}");
    assert!(
        message_contains(&auto, "requires an explicit workspace selector"),
        "{auto}"
    );
}

/// The selector refusal for an `auto` call that carries `mode` must not
/// mention `view`, which the caller never set. An explicit selector clears
/// the gate, and `auto` without `mode` keeps using the bound workspace.
#[test]
fn auto_mode_selector_refusal_does_not_advise_omitting_view() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(&["--toolset", "dispatch"]);

    let refused = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "mode": "status", "model": "codex"}),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    assert!(
        message_contains(&refused, "requires an explicit workspace selector"),
        "{refused}"
    );
    assert!(
        !message_contains(&refused, "`view`"),
        "the caller never set `view`, so the refusal must not name it: {refused}"
    );

    let selected = client.call_tool_err(
        "orbit_workflow",
        json!({
            "action": "auto",
            "mode": "status",
            "workspace": "ws_mcp-roundtrip",
            "model": "codex"
        }),
    );
    assert_eq!(selected["code"], "capability_denied", "{selected}");
    assert!(
        !message_contains(&selected, "requires an explicit workspace selector"),
        "an explicit selector must pass the selector gate: {selected}"
    );

    let bound = client.call_tool_err(
        "orbit_workflow",
        json!({"action": "auto", "model": "codex"}),
    );
    assert_eq!(bound["code"], "capability_denied", "{bound}");
    assert!(
        !message_contains(&bound, "requires an explicit workspace selector"),
        "auto without mode must use the bound workspace: {bound}"
    );
}

/// Ship lost its captured review on MCP and its timestamp, structured
/// validation warning and review on the native CLI.
#[test]
fn ship_receipts_preserve_submission_metadata_across_cli_and_mcp() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::workflow_ship_mode::ship_receipts_preserve_submission_metadata_across_cli_and_mcp",
    ) {
        return;
    }
    let workspace = McpWorkspace::init();
    workspace.run_ok(&["config", "set", "review.timing", "off"]);
    workspace.run_ok(&["config", "set", "workflow.purpose_crews.review", "sol"]);
    workspace.run_ok(&[
        "config",
        "set",
        "workflow.required_validation_commands",
        r#"["true"]"#,
    ]);
    crate::git_repo::run(
        &workspace.work,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/receipt-fixture.git",
        ],
    );
    for profile in [".bash_profile", ".profile", ".zprofile"] {
        std::fs::write(workspace.home.join(profile), "exit 42\n").unwrap();
    }
    std::fs::create_dir_all(workspace.home.join(".config/fish")).unwrap();
    std::fs::write(workspace.home.join(".config/fish/config.fish"), "exit 42\n").unwrap();
    // Exercise real submission without dispatching any task implementation.
    let jobs = workspace.home.join(".orbit/resources/jobs");
    std::fs::write(
        jobs.join("task_auto_pipeline.yaml"),
        json!({
            "schemaVersion":2,"kind":"Job","metadata":{"name":"task_auto_pipeline"},
            "spec":{"state":"enabled","kind":"workflow","steps":[{
                "id":"receipt","default_input":{"seconds":0},
                "spec":{"type":"deterministic","action":"sleep","config":{}}
            }]}
        })
        .to_string(),
    )
    .unwrap();
    let installed = workspace.home.join(".orbit/bin/orbit");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let mut receipts = Vec::new();
    for surface in ["mcp", "cli"] {
        let added = workspace.run_ok(&[
            "task",
            "add",
            "--title",
            surface,
            "--status",
            "backlog",
            "--crew",
            "sol",
            "--complexity",
            "low",
            "--context",
            "file:.gitignore",
            "--json",
        ]);
        let task: Value = stdout_json(&added);
        let id = task["id"].as_str().unwrap();
        let receipt = if surface == "mcp" {
            client.call_tool_ok(
                "orbit_workflow",
                json!({
                    "action":"ship","task_ids":[id],"mode":"pr","review":"before-landing"
                }),
            )
        } else {
            stdout_json(&workspace.run_ok(&[
                "run",
                "ship",
                id,
                "--mode",
                "pr",
                "--review",
                "before-landing",
                "--json",
            ]))
        };
        let run_id = receipt["run_id"].as_str().unwrap();
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let detail = loop {
            let detail: Value = stdout_json(&workspace.run_ok(&[
                "run",
                "show",
                run_id,
                "--no-reconcile",
                "--json",
            ]));
            if matches!(
                detail["run"]["state"].as_str(),
                Some("success" | "failed" | "timeout")
            ) {
                break detail;
            }
            assert!(Instant::now() < deadline, "worker did not finish: {detail}");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(detail["run"]["state"], "success", "{detail}");
        receipts.push((surface, receipt));
    }
    // Gather both boundary results before asserting so the failure shows all
    // missing fields, and both short-lived workers have finished.
    assert!(
        receipts.iter().all(|(_, receipt)| {
            receipt["submitted_at"]
                .as_str()
                .is_some_and(|time| chrono::DateTime::parse_from_rfc3339(time).is_ok())
                && receipt["validation_env_warning"]
                    .as_str()
                    .is_some_and(|warning| {
                        warning.contains("could not resolve the login-shell environment")
                    })
                && receipt["review"] == json!({"timing":"before-landing","source":"run-option"})
        }),
        "ship receipt metadata must survive both boundaries: {receipts:#?}"
    );
    for (surface, receipt) in receipts {
        assert!(
            receipt["warning"]
                .as_str()
                .unwrap()
                .lines()
                .any(|line| Some(line) == receipt["validation_env_warning"].as_str())
        );
        for (key, expected) in [
            ("wait_timeout", json!(false)),
            ("attempt", json!(1)),
            ("error_code", Value::Null),
            ("error_message", Value::Null),
        ] {
            if surface == "cli" {
                assert_eq!(receipt.get(key), Some(&expected), "{receipt}");
            } else {
                assert!(receipt.get(key).is_none(), "{receipt}");
            }
        }
    }
}
