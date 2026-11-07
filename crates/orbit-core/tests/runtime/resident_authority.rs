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

fn stopped_mandate_refuses_new_widening_and_allows_narrowing() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "add": ["dispatch"]}),
        &operator(),
    )
    .unwrap();
    call(
        &runtime,
        "orbit.resident.control",
        json!({"task_id": id, "action": "stop", "reason": "Finished"}),
        &operator(),
    )
    .unwrap();
    let before = show(&runtime, &id)["authority"].clone();
    for session in [&operator(), &orchestrator()] {
        let error = call(
            &runtime,
            "orbit.resident.grant",
            json!({"task_id": id, "add": ["merge"], "quote": QUOTE}),
            session,
        )
        .expect_err("stopped mandates cannot be widened or request widening");
        assert!(error.contains("ended"), "{error}");
    }
    assert_eq!(show(&runtime, &id)["authority"], before);
    let narrowed = call(
        &runtime,
        "orbit.resident.grant",
        json!({"task_id": id, "remove": ["dispatch"], "model": "claude"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(narrowed["outcome"], "narrowed");
    assert_eq!(narrowed["authority"]["rights"], json!([]));
}

fn stopped_mandate_refuses_pending_grant_but_allows_rejection() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let raise = |right| {
        call(
            &runtime,
            "orbit.resident.grant",
            json!({"task_id": id, "add": [right], "quote": QUOTE, "model": "claude"}),
            &orchestrator(),
        )
        .unwrap()["request"]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let grant = raise("dispatch");
    let reject = raise("merge");
    call(
        &runtime,
        "orbit.resident.control",
        json!({"task_id": id, "action": "stop", "reason": "Finished"}),
        &operator(),
    )
    .unwrap();
    let before = show(&runtime, &id)["authority"].clone();
    let error = runtime
        .answer_decision_request_as_human(&grant, "grant", None, "human:daniel".into(), None)
        .expect_err("a pending answer cannot widen a stopped mandate");
    assert!(error.to_string().contains("ended"), "{error}");
    assert_eq!(show(&runtime, &id)["authority"], before);
    runtime
        .answer_decision_request_as_human(&reject, "reject", None, "human:daniel".into(), None)
        .unwrap();
    assert_eq!(show(&runtime, &id)["authority"], before);
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

fn resident_stale_comment_is_refused_without_writes() {
    use chrono::{Duration, Utc};
    use orbit_types::workflow::ResidentLiveCycle;

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let mut record = json!({"kind": "resident.checkpoint", "schema": 1, "mandate": id,
        "cycle": "rcy-dead", "run_id": "jrun-dead", "crew": "sol", "recovery_of": null,
        "wake": {"reasons": ["heartbeat"], "digest": format!("sha256:{}", "0".repeat(64))},
        "authority": {"rights": [], "observe_only": true, "narrowed_by": null},
        "actions": [], "decisions": [], "requests": [], "costs": [], "notes": "",
        "next_wake": {"reason": "heartbeat", "not_before": "2030-01-01T00:00:00Z"},
        "continuity": "off", "outcome": "idle", "probe": false});
    // The old public comment boundary accepted this dead cycle verbatim.
    // Compare all durable task state, not just the checkpoint read model.
    let snapshot = || {
        (
            runtime.get_task(&id).unwrap(),
            runtime.get_task_comments(&id).unwrap(),
            runtime.get_task_history(&id).unwrap(),
        )
    };
    let refuse = |tool: &str, payload: Value, reason: &str| {
        let before = snapshot();
        let error = call(&runtime, tool, payload, &orchestrator()).expect_err(reason);
        assert!(error.contains(reason), "{error}");
        assert_eq!(snapshot(), before, "refusal must write nothing");
    };
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": record.to_string(), "model": "codex"}),
        "resident_cycle_stale",
    );
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        "resident_cycle_stale",
    );
    assert_eq!(show(&runtime, &id)["latest_checkpoint"], Value::Null);

    let dead = ResidentLiveCycle {
        cycle: "rcy-dead".into(),
        run_id: "jrun-dead".into(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    runtime
        .set_resident_live_cycle(&id, None, Some(&dead))
        .unwrap();
    let live = ResidentLiveCycle {
        cycle: "rcy-live".into(),
        run_id: "jrun-live".into(),
        ..dead
    };
    runtime
        .set_resident_live_cycle(&id, Some("rcy-dead"), Some(&live))
        .unwrap();
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        "resident_cycle_stale",
    );
    let before = snapshot();
    assert!(
        runtime
            .set_resident_live_cycle(&id, Some("rcy-dead"), None)
            .is_err()
    );
    assert_eq!(
        snapshot(),
        before,
        "a stale finalizer cannot clear the live cycle"
    );
    record["cycle"] = json!("rcy-live");
    // Correct cycle, wrong run must also fail without writes.
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        "resident_cycle_stale",
    );
    record["run_id"] = json!("jrun-live");
    let mut malformed = record.clone();
    malformed["schema"] = json!(2);
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": malformed, "model": "codex"}),
        "resident_record_invalid",
    );
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": malformed.to_string(), "model": "codex"}),
        "resident_record_invalid",
    );
    malformed = record.clone();
    malformed["authority"]["workspaces"] = json!(["other"]);
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": malformed, "model": "codex"}),
        "resident_record_invalid",
    );

    let request = json!({"kind": "resident.request", "schema": 1, "mandate": id,
        "cycle": "rcy-live", "id": "q1", "decision_request": "DR-example", "task": id,
        "class": "architecture", "question": "Which schema?", "recommended": "a", "blocking": false, "blocks": []});
    call(
        &runtime,
        "orbit.task.update",
        json!({"id": id, "comment": request.to_string(), "model": "codex"}),
        &orchestrator(),
    )
    .unwrap();
    let mut malformed_request = request.clone();
    malformed_request["class"] = json!("unknown");
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": malformed_request.to_string(), "model": "codex"}),
        "resident_record_invalid",
    );
    malformed_request = request.clone();
    malformed_request["cycle"] = json!("rcy-dead");
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": malformed_request.to_string(), "model": "codex"}),
        "resident_cycle_stale",
    );

    let answer = json!({"kind": "resident.answer", "schema": 1, "mandate": id,
        "request_id": "q1", "reverses": null, "quote": "  Keep the schema.\nWait until Monday.  ",
        "source": {"human": "daniel", "via": "manager-session", "session": "session-1", "said_at": "2026-11-02T09:58:00Z"},
        "relayed_by": {"crew": "sol", "model": "gpt-6.1-sol"}});
    let mut malformed_answer = answer.clone();
    malformed_answer["rights"] = json!(["merge"]);
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": malformed_answer.to_string(), "model": "codex"}),
        "resident_record_invalid",
    );
    malformed_answer = answer.clone();
    malformed_answer["relayed_by"] = Value::Null;
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": malformed_answer.to_string(), "model": "codex"}),
        "requires `relayed_by`",
    );
    call(
        &runtime,
        "orbit.task.update",
        json!({"id": id, "comment": answer.to_string(), "model": "codex"}),
        &orchestrator(),
    )
    .unwrap();
    let comments = runtime.get_task_comments(&id).unwrap();
    let stored_answer: Value = serde_json::from_str(&comments.last().unwrap().message).unwrap();
    assert_eq!(
        stored_answer, answer,
        "relay quote and provenance stay verbatim"
    );

    record["notes"] = json!("Recheck the schema next cycle");
    record["actions"] = json!([{"key": format!("resident:{id}:rcy-live:1"), "kind": "dispatch", "subject": id, "result": "ok", "ref": "jrun-leaf"}]);
    record["decisions"] = json!([{"id": "d1", "summary": "Wait for the schema", "class": "reversible", "reversible": true, "p_wrong": "low", "rework_units": 1, "precedent": null}]);
    record["requests"] =
        json!([{"id": "q1", "decision_request": "DR-example", "task": id, "blocking": false}]);
    record["costs"] = json!([{"kind": "manual_step", "what": "Checked the schema", "minutes": 1, "retries": 0, "slot_minutes": 0, "wished": "Automatic schema check", "signature": "manual-schema-check"}]);
    record["costs"].as_array_mut().unwrap().push(
        json!({"kind":"wait", "what":"Waited on CI", "minutes":2, "retries":0, "slot_minutes":0, "wished":null, "signature":null}),
    );
    let written = call(
        &runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(written["checkpoint"], record);
    let ledger = call(&runtime, "orbit.evolution.list", json!({}), &operator()).unwrap();
    assert_eq!(
        ledger["count"], 1,
        "checkpoint costs must reach the capability-gap ledger"
    );
    assert_eq!(ledger["records"][0]["minutes"], 1);
    use orbit_store::contracts::EvolutionStoreBackend;
    assert_eq!(
        runtime
            .sqlite_store()
            .unwrap()
            .evolution_costs(&runtime.workspace_id().unwrap())
            .unwrap()
            .len(),
        2,
        "every checkpoint cost, including unsigned costs, is retained"
    );
    runtime.reconcile_evolution_checkpoint(&id).unwrap();
    let ledger_again = call(&runtime, "orbit.evolution.list", json!({}), &operator()).unwrap();
    assert_eq!(
        ledger_again, ledger,
        "checkpoint projection replay must not duplicate costs"
    );
    evolution_corroboration_and_suppression(&runtime, &id, &ledger["records"][0]);
    assert_eq!(show(&runtime, &id)["latest_checkpoint"], record);
    assert_eq!(
        show(&runtime, &id)["authority"]["revision"],
        0,
        "checkpoint authority is only a report"
    );
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        "resident_checkpoint_exists",
    );
    refuse(
        "orbit.task.update",
        json!({"id": id, "comment": record.to_string(), "model": "codex"}),
        "resident_checkpoint_exists",
    );
    runtime
        .set_resident_live_cycle(&id, Some("rcy-live"), None)
        .unwrap();
    refuse(
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        "resident_cycle_stale",
    );
    assert_eq!(show(&runtime, &id)["latest_checkpoint"], record);
}

fn resident_sensitive_checkpoint_is_refused_without_writes() {
    use chrono::{Duration, Utc};
    use orbit_types::workflow::ResidentLiveCycle;

    let _env = orbit_common::test_env::scoped([(
        "RESIDENT_FIXTURE_API_TOKEN",
        Some("resident-fixture-sensitive-value"),
    )]);
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let live = ResidentLiveCycle {
        cycle: "rcy-live".into(),
        run_id: "jrun-live".into(),
        expires_at: Utc::now() + Duration::hours(1),
    };
    runtime
        .set_resident_live_cycle(&id, None, Some(&live))
        .unwrap();
    let mut record = json!({"kind": "resident.checkpoint", "schema": 1, "mandate": id,
        "cycle": live.cycle, "run_id": live.run_id, "crew": "sol", "recovery_of": null,
        "wake": {"reasons": ["heartbeat"], "digest": format!("sha256:{}", "0".repeat(64))},
        "authority": {"rights": [], "observe_only": true, "narrowed_by": null},
        "actions": [], "decisions": [], "requests": [], "costs": [], "notes": "",
        "next_wake": {"reason": "heartbeat", "not_before": "2030-01-01T00:00:00Z"},
        "continuity": "off", "outcome": "idle", "probe": false});
    let snapshot = || {
        (
            runtime.get_task(&id).unwrap(),
            runtime.get_task_comments(&id).unwrap(),
            runtime.get_task_history(&id).unwrap(),
        )
    };
    let home = std::env::var("HOME").unwrap();
    for notes in [
        format!("Read {home}/private/checkpoint"),
        home,
        format!("credential glpat-{}", "c".repeat(20)),
        "resident-fixture-sensitive-value".into(),
    ] {
        record["notes"] = json!(notes);
        for surface in ["checkpoint", "comment", "core-comment", "core-checkpoint"] {
            let before = snapshot();
            let result = match surface {
                "checkpoint" => call(
                    &runtime,
                    "orbit.resident.checkpoint",
                    json!({"task_id": id, "record": record, "model": "codex"}),
                    &orchestrator(),
                )
                .map(|_| ()),
                "comment" => call(
                    &runtime,
                    "orbit.task.update",
                    json!({"id": id, "comment": record.to_string(), "model": "codex"}),
                    &orchestrator(),
                )
                .map(|_| ()),
                "core-comment" => runtime
                    .update_task_with_identity(
                        &id,
                        TaskUpdateParams {
                            comment: Some(record.to_string()),
                            ..Default::default()
                        },
                        None,
                        Some("codex".into()),
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                "core-checkpoint" => runtime
                    .append_resident_checkpoint(&id, record.clone(), None, Some("codex".into()))
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                _ => unreachable!(),
            };
            let error = result.expect_err("sensitive resident record must be refused");
            assert!(
                error.contains(
                    "resident records must be supplied without sensitive text requiring redaction"
                ),
                "{surface}: {error}"
            );
            assert_eq!(snapshot(), before, "{surface}: refusal must write nothing");
        }
    }
    record["notes"] = json!("  Recheck the schema.\nWait until Monday.  ");
    let written = call(
        &runtime,
        "orbit.resident.checkpoint",
        json!({"task_id": id, "record": record, "model": "codex"}),
        &orchestrator(),
    )
    .unwrap();
    assert_eq!(written["checkpoint"], record);
    let comments = runtime.get_task_comments(&id).unwrap();
    let stored: Value = serde_json::from_str(&comments.last().unwrap().message).unwrap();
    assert_eq!(stored, record, "safe checkpoint text stays verbatim");
}

fn relay_returns_comment_and_answer_matching_the_advertised_output() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let (id, _) = mandate(&runtime, "2030-01-01T00:00:00Z");
    let quote = "Ship CSV first.";
    let relayed = call(
        &runtime,
        "orbit.resident.relay",
        json!({"task_id": id, "quote": quote,
               "source": {"human": "daniel", "via": "chat", "said_at": "2026-11-02T10:00:00Z"},
               "orchestrator": "sol", "model": "claude"}),
        &orchestrator(),
    )
    .unwrap();

    let mut keys: Vec<&str> = relayed
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["answer", "comment", "task_id"]);
    assert_eq!(relayed["task_id"], id);
    let comments = runtime.get_task_comments(&id).unwrap();
    assert_eq!(relayed["comment"]["index"], comments.len());
    assert!(relayed["comment"]["at"].is_string());
    assert!(relayed["comment"]["by"].is_string());
    assert_eq!(relayed["answer"]["kind"], "resident.answer");
    assert_eq!(relayed["answer"]["mandate"], id);
    assert_eq!(relayed["answer"]["quote"], quote);
    assert_eq!(relayed["answer"]["source"]["human"], "daniel");
    assert!(relayed["answer"]["relayed_by"].is_object());
    let stored: Value = serde_json::from_str(&comments.last().unwrap().message).unwrap();
    assert_eq!(stored, relayed["answer"], "answer is the record as written");
}

crate::scenario_table!(
    resident_authority,
    relay_returns_comment_and_answer_matching_the_advertised_output,
    stopped_mandate_refuses_pending_grant_but_allows_rejection,
    stopped_mandate_refuses_new_widening_and_allows_narrowing,
    resident_sensitive_checkpoint_is_refused_without_writes,
    resident_stale_comment_is_refused_without_writes,
    agent_widening_is_a_request_the_human_answers,
    rejected_or_superseded_requests_change_nothing,
    any_task_writer_narrows_and_is_recorded,
    losing_the_resident_tag_clears_effective_authority_but_keeps_history,
);

fn evolution_corroboration_and_suppression(
    runtime: &OrbitRuntime,
    mandate_id: &str,
    original: &Value,
) {
    let id = original["id"].as_str().unwrap();
    let proposal = runtime
        .add_task(TaskAddParams {
            title: "Automatic schema checking".into(),
            ..Default::default()
        })
        .unwrap();
    let add = |minutes: u64, slot: u64, mandate: &str| {
        call(runtime, "orbit.evolution.add", json!({"event": {"kind":"manual_step", "what":"Checked the schema", "minutes":minutes, "slot_minutes":slot, "retries":1, "wished":"Automatic schema check", "signature":"manual-schema-check"}, "mandate":mandate}), &orchestrator()).unwrap()
    };
    let second = add(9, 0, &proposal.id);
    assert_eq!(second["record"]["id"], id);
    assert_eq!(second["record"]["occurrence_count"], 2);
    assert_eq!(second["record"]["mandate_count"], 2);
    assert_eq!(second["record"]["minutes"], 10);
    let frictions = runtime.run_tool("orbit.friction.list", json!({})).unwrap();
    assert!(frictions.as_array().is_some_and(|rows| rows.is_empty()));
    assert!(
        runtime
            .run_tool("orbit.friction.resolve", json!({"id":id}))
            .is_err(),
        "evolution cannot be resolved as friction"
    );
    assert!(
        call(
            runtime,
            "orbit.task.update",
            json!({"id":proposal.id,"relations":[{"type":"resolves","target":id}]}),
            &orchestrator()
        )
        .is_err(),
        "friction resolves relations cannot target evolution"
    );
    let denied = call(
        runtime,
        "orbit.evolution.decline",
        json!({"id":id}),
        &orchestrator(),
    );
    assert!(denied.is_err(), "only the operator may suppress proposals");
    runtime.decline_evolution_record(id, 30).unwrap();
    let in_state = |state: &str| {
        call(
            runtime,
            "orbit.evolution.list",
            json!({ "state": state }),
            &operator(),
        )
        .unwrap()["count"]
            .clone()
    };
    assert_eq!(in_state("declined"), 1);
    add(9, 0, mandate_id);
    assert_eq!(
        in_state("declined"),
        1,
        "19 minutes must not double a 10 minute baseline"
    );
    add(0, 1, mandate_id);
    assert_eq!(
        in_state("open"),
        1,
        "minutes plus slot minutes reaching twice the decline baseline reopens the record"
    );
    let all = call(
        runtime,
        "orbit.evolution.list",
        json!({"state":"open"}),
        &operator(),
    )
    .unwrap();
    assert_eq!(all["count"], 1);
    assert_eq!(all["records"][0]["occurrence_count"], 4);
    assert_eq!(all["records"][0]["minutes"], 19);
    assert_eq!(all["records"][0]["slot_minutes"], 1);
    assert_eq!(
        call(
            runtime,
            "orbit.evolution.list",
            json!({"since":"2099-01-01T00:00:00Z"}),
            &operator()
        )
        .unwrap()["count"],
        0
    );
    let unsigned = call(runtime, "orbit.evolution.add", json!({"event":{"kind":"wait", "what":"Waited on CI", "minutes":3, "retries":0, "slot_minutes":0}}), &orchestrator()).unwrap();
    assert!(unsigned["record"].is_null());
    assert_eq!(
        call(runtime, "orbit.evolution.list", json!({}), &operator()).unwrap()["count"],
        1
    );
    let invalid = call(
        runtime,
        "orbit.evolution.add",
        json!({"event":{"kind":"manual_step", "what":"Bad event", "minutes":1, "retries":0, "slot_minutes":0, "signature":"Wrong_Key"}}),
        &orchestrator(),
    );
    assert!(invalid.is_err());
}
