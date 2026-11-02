//! Resident authority at the tool boundary: only the human widens. A
//! widening from an agent or orchestrator session changes nothing and raises
//! one `resident_authority` decision request quoting the relay verbatim; the
//! human's `grant` applies it with the request as provenance, `reject` or an
//! intervening change leaves it unapplied, and any task writer narrows.
use std::path::Path;

use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskArtifact;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";
const QUOTE: &str = "you may dispatch in this workspace, not merge";

fn runtime_at(root: &Path) -> OrbitRuntime {
    let global = root.join("home/.orbit");
    let orbit = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    OrbitRuntime::from_roots(&global, &orbit).unwrap()
}

fn operator() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    }
}

fn orchestrator() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Agent].into(),
        origin_session_id: Some("mcp-session-1".into()),
        ..Default::default()
    }
}

fn call(
    runtime: &OrbitRuntime,
    tool: &str,
    input: Value,
    session: &ToolSessionContext,
) -> Result<Value, String> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            tool,
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session.clone(),
        )
        .map(|outcome| outcome.value)
        .map_err(|error| error.to_string())
}

/// A tagged assignment with a mandate covering this workspace.
fn mandate(runtime: &OrbitRuntime, deadline: &str) -> (String, String) {
    let workspace = runtime.workspace_id().unwrap();
    let id = runtime
        .add_task(TaskAddParams {
            title: "[orchestration]: scribe v2 - opus".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Deliver the remaining scribe v2 slices",
        "exit_criteria": ["Every scribe-v2 task is done or rejected"],
        "deadline": deadline,
        "workspaces": [workspace],
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    (id, workspace)
}

fn show(runtime: &OrbitRuntime, id: &str) -> Value {
    call(
        runtime,
        "orbit.resident.show",
        json!({"task_id": id}),
        &operator(),
    )
    .unwrap()
}

fn agent_widening_is_a_request_the_human_answers() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, workspace) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let input = json!({"task_id": id, "add": ["dispatch"], "workspaces": [workspace],
                       "quote": QUOTE, "orchestrator": "sol", "model": "claude"});

    // Without the human's words the relay is refused outright.
    let error = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["dispatch"], "model": "claude"}),
        &orchestrator(),
    )
    .expect_err("an agent widening without a quote is refused");
    assert!(error.contains("only the operator widens"), "{error}");

    let raised = call(
        &runtime,
        "orbit.resident.grant",
        input.clone(),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(raised["outcome"], "requested");
    assert_eq!(raised["authority"]["revision"], 0);
    assert_eq!(raised["request"]["gate"], "resident_authority");
    assert_eq!(raised["request"]["resident_grant"]["quote"], QUOTE);
    assert_eq!(raised["request"]["resident_grant"]["relay"]["crew"], "sol");
    let request_id = raised["request"]["id"].as_str().unwrap().to_string();

    // The same addition reuses the open request.
    let again = call(&runtime, "orbit.resident.grant", input, &orchestrator()).unwrap();
    assert_eq!(again["deduplicated"], true);
    assert_eq!(again["request"]["id"], request_id);

    // An operator-capable MCP session is still a relay, not the human.
    let mcp_operator = ToolSessionContext {
        origin_session_id: Some("mcp-session-2".into()),
        ..operator()
    };
    let relayed = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["promote"], "quote": "promote too"}),
        &mcp_operator,
    )
    .unwrap();
    assert_eq!(relayed["outcome"], "requested");

    let shown = show(&runtime, &id);
    assert_eq!(shown["state"], "active");
    assert_eq!(shown["effective_authority"]["rights"], json!([]));
    assert_eq!(shown["authority"]["revision"], 0);
    assert_eq!(shown["open_requests"].as_array().unwrap().len(), 2);

    let answered = runtime
        .answer_decision_request_as_human(
            &request_id,
            "grant",
            None,
            "human:daniel".into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(answered.state.as_str(), "answered");
    let shown = show(&runtime, &id);
    assert_eq!(shown["effective_authority"]["rights"], json!(["dispatch"]));
    assert_eq!(
        shown["effective_authority"]["workspaces"],
        json!([workspace])
    );
    let granted = &shown["authority"]["granted"][0];
    assert_eq!(granted["via"], format!("decision:{request_id}"));
    assert_eq!(granted["by"], "human:daniel");
    assert_eq!(granted["quote"], QUOTE);
    // The grant changed the authority, so the other request lapsed.
    assert_eq!(shown["open_requests"], json!([]));
}

fn rejected_or_superseded_requests_change_nothing() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let raise = |right: &str| {
        call(
            &runtime,
            "orbit.resident.grant",
            json!({"task_id": id, "add": [right], "quote": QUOTE, "model": "claude"}),
            &orchestrator(),
        )
        .unwrap()["request"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let rejected = raise("merge");
    runtime
        .answer_decision_request_as_human(&rejected, "reject", None, "human:daniel".into(), None)
        .unwrap();
    assert_eq!(show(&runtime, &id)["authority"]["revision"], 0);

    let stale = raise("promote");
    let direct = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["dispatch"]}),
        &operator(),
    )
    .unwrap();
    assert_eq!(direct["outcome"], "granted");
    assert_eq!(direct["authority"]["granted"][0]["via"], "operator");
    let error = runtime
        .answer_decision_request_as_human(&stale, "grant", None, "human:daniel".into(), None)
        .expect_err("a request the authority moved past is superseded");
    assert!(error.to_string().contains("superseded"), "{error}");
    assert_eq!(
        show(&runtime, &id)["authority"]["rights"],
        json!(["dispatch"])
    );
}

fn losing_the_resident_tag_clears_effective_authority_but_keeps_history() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, workspace) = mandate(&runtime, "2030-01-01T00:00:00Z");
    call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["dispatch"], "workspaces": [workspace]}),
        &operator(),
    )
    .unwrap();
    let granted = show(&runtime, &id);
    assert_eq!(granted["state"], "active");
    assert_eq!(
        granted["effective_authority"]["rights"],
        json!(["dispatch"])
    );
    assert_eq!(
        granted["effective_authority"]["workspaces"],
        json!([workspace])
    );

    // Remove only the `resident` tag; mandate and authority records stay.
    runtime
        .update_task_as_human(
            &id,
            TaskUpdateParams {
                tags: Some(vec![]),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();

    let shown = show(&runtime, &id);
    assert_eq!(shown["state"], "invalid");
    assert!(
        shown["state_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("not tagged"),
        "unexpected state_reason: {shown}"
    );
    assert_eq!(shown["effective_authority"]["rights"], json!([]));
    assert_eq!(shown["effective_authority"]["workspaces"], json!([]));
    assert_eq!(shown["authority"]["rights"], json!(["dispatch"]));
    assert_eq!(shown["authority"]["revision"], 1);
    assert_eq!(shown["authority"]["granted"].as_array().unwrap().len(), 1);
}

fn any_task_writer_narrows_and_is_recorded() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, workspace) = mandate(&runtime, "2030-01-01T00:00:00Z");
    call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["dispatch", "merge"], "workspaces": [workspace]}),
        &operator(),
    )
    .unwrap();

    let narrowed = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "remove": ["merge"], "model": "claude"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(narrowed["outcome"], "narrowed");
    assert_eq!(narrowed["authority"]["rights"], json!(["dispatch"]));
    assert_eq!(narrowed["authority"]["narrowed"][0]["by"], "claude");
    assert_eq!(
        narrowed["authority"]["narrowed"][0]["removed"],
        json!(["merge"])
    );

    let shortened = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "deadline": "2029-06-01T00:00:00Z", "model": "claude"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(shortened["outcome"], "narrowed");
    let shown = show(&runtime, &id);
    assert_eq!(shown["mandate"]["deadline"], "2029-06-01T00:00:00Z");
    assert_eq!(
        shown["authority"]["narrowed"][1]["deadline"],
        "2029-06-01T00:00:00Z"
    );
    let error = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "deadline": "2031-01-01T00:00:00Z", "model": "claude"}),
        &orchestrator(),
    )
    .expect_err("a later deadline is not narrowing");
    assert!(error.contains("only shortens"), "{error}");

    let dropped = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "drop_workspaces": [workspace], "model": "claude"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(dropped["authority"]["workspaces"], json!([]));
    assert_eq!(dropped["authority"]["revision"], 4);
    let listed = call(&runtime, "orbit.resident.list", json!({}), &operator()).unwrap();
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["mandates"][0]["authority_revision"], 4);
}

crate::scenario_table!(
    resident_authority,
    agent_widening_is_a_request_the_human_answers,
    rejected_or_superseded_requests_change_nothing,
    any_task_writer_narrows_and_is_recorded,
    losing_the_resident_tag_clears_effective_authority_but_keeps_history,
);
