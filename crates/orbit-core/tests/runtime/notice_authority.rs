//! Fleet notice authority at the tool boundary: the poster class comes from
//! the caller's session, never from input, and each class is held to its
//! rights, caps and limits, with an audit row for every write and refusal.
use std::path::Path;

use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::TaskAddParams;
use orbit_engine::RuntimeHost as _;
use orbit_types::task::ExecutionLocation;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext, WorkerInvocation};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn runtime_at(root: &Path) -> OrbitRuntime {
    let global = root.join("home/.orbit");
    let orbit = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    OrbitRuntime::from_roots(&global, &orbit).unwrap()
}

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [capability].into(),
        ..Default::default()
    }
}

fn agent_session(runtime: &OrbitRuntime, task_id: &str) -> ToolSessionContext {
    ToolSessionContext {
        worker_invocation: Some(WorkerInvocation {
            owner_machine_id: "owner-machine".into(),
            owner_workspace_id: runtime.workspace_id().unwrap(),
            owner_destination: "owner".into(),
            task_id: task_id.into(),
            claim_id: "claim-1".into(),
            execution: ExecutionLocation {
                machine_id: "leaf-machine".into(),
                machine_name: None,
            },
            bound_run_id: "jrun-notice-1".into(),
        }),
        ..session(McpCapability::Agent)
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

fn refused(result: Result<Value, String>, code: &str) {
    let error = result.expect_err(&format!("expected {code}"));
    assert!(error.starts_with(code), "expected {code}, got: {error}");
}

fn add_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

fn notice_audit(runtime: &OrbitRuntime, status: AuditEventStatus) -> usize {
    runtime
        .list_audit_events_with_kind(None, None, Some("notice".into()), Some(status), None, 500)
        .unwrap()
        .len()
}

fn operator_posts_updates_and_closes_any_notice() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    let orchestrator = session(McpCapability::Agent);

    let created = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Swap disks", "scope": "global",
               "hosts": ["dk-server-1"], "expires_in_hours": 720}),
        &operator,
    )
    .unwrap();
    assert_eq!(created["outcome"], "created");
    assert_eq!(created["notice"]["author"]["class"], "operator");
    assert_eq!(created["notice"]["dedupe_key"], "maintenance:swap-disks");
    let id = created["notice"]["id"].as_str().unwrap().to_string();
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "info", "title": "Too long", "expires_in_hours": 721}),
            &operator,
        ),
        "notice_expiry_exceeds_cap",
    );
    let updated = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Swap disks", "body": "Hold dispatch",
               "hosts": ["dk-server-1"]}),
        &operator,
    )
    .unwrap();
    assert_eq!(updated["outcome"], "updated");
    assert_eq!(updated["notice"]["id"], id);
    assert_eq!(updated["notice"]["revision"], 2);
    assert_eq!(updated["notice"]["scope"], "global");

    // An orchestrator re-posting the operator's key only corroborates it,
    // and may not close it.
    let corroborated = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Swap disks", "body": "changed",
               "orchestrator": "sol"}),
        &orchestrator,
    )
    .unwrap();
    assert_eq!(corroborated["outcome"], "corroborated");
    assert_eq!(corroborated["notice"]["body"], "Hold dispatch");
    assert_eq!(corroborated["notice"]["reports"]["count"], 3);
    refused(
        call(
            &runtime,
            "orbit.notice.close",
            json!({"id": id, "reason": "not mine", "orchestrator": "sol"}),
            &orchestrator,
        ),
        "notice_close_refused",
    );

    let closed = call(
        &runtime,
        "orbit.notice.close",
        json!({"id": id, "reason": "disks swapped"}),
        &operator,
    )
    .unwrap();
    assert_eq!(closed["notice"]["status"], "closed");
    let again = call(
        &runtime,
        "orbit.notice.close",
        json!({"id": id, "reason": "again"}),
        &operator,
    )
    .unwrap();
    assert_eq!(again["already_inactive"], true);
    let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    assert_eq!(shown["history"].as_array().map(Vec::len), Some(4));

    // created, updated, corroborated and two closes succeeded; the cap and
    // the orchestrator's close were refused.
    assert_eq!(notice_audit(&runtime, AuditEventStatus::Success), 4);
    assert_eq!(notice_audit(&runtime, AuditEventStatus::Denied), 2);
}

fn orchestrator_session_posts_fleet_wide_within_its_caps() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let orchestrator = session(McpCapability::Agent);

    let posted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "maintenance", "title": "Rotate keys", "scope": "global",
               "expires_in_hours": 72, "orchestrator": "sol", "model": "codex"}),
        &orchestrator,
    )
    .unwrap();
    assert_eq!(posted["notice"]["author"]["class"], "orchestrator");
    assert_eq!(posted["notice"]["author"]["crew"], "sol");
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "maintenance", "title": "Too long", "expires_in_hours": 73,
                   "orchestrator": "sol"}),
            &orchestrator,
        ),
        "notice_expiry_exceeds_cap",
    );
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "info", "title": "Unknown crew", "orchestrator": "nobody"}),
            &orchestrator,
        ),
        "notice_poster_unidentified",
    );
    // An agent session without orchestrator attribution or a run binding
    // has no poster class.
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "info", "title": "Who am I"}),
            &orchestrator,
        ),
        "notice_poster_unidentified",
    );
    let closed = call(
        &runtime,
        "orbit.notice.close",
        json!({"dedupe_key": "maintenance:rotate-keys", "reason": "rotated",
               "orchestrator": "sol"}),
        &orchestrator,
    )
    .unwrap();
    assert_eq!(closed["notice"]["closed"]["by"]["class"], "orchestrator");
    assert_eq!(notice_audit(&runtime, AuditEventStatus::Denied), 3);
}

fn run_bound_agent_reports_only_about_its_task() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let task = add_task(&runtime, "Agent's bound task");
    let other = add_task(&runtime, "Another agent's task");
    let agent = agent_session(&runtime, &task);
    let other_agent = agent_session(&runtime, &other);

    // Operator capability on a run-bound session does not make it the
    // operator, and the input cannot choose a class.
    let mut escalated = agent.clone();
    escalated
        .effective_capabilities
        .insert(McpCapability::Operator);
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "incident", "title": "Global", "scope": "global",
                   "linked_task": task, "orchestrator": "sol"}),
            &escalated,
        ),
        "notice_scope_refused",
    );
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "maintenance", "title": "Hold", "linked_task": task}),
            &agent,
        ),
        "notice_kind_refused",
    );
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "incident", "title": "No evidence"}),
            &agent,
        ),
        "notice_evidence_required",
    );
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "incident", "title": "Too long", "linked_task": task,
                   "expires_in_hours": 25}),
            &agent,
        ),
        "notice_expiry_exceeds_cap",
    );

    let posted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Shard 2 is red",
               "evidence": [{"type": "run", "value": "jrun-notice-1"}]}),
        &agent,
    )
    .unwrap();
    assert_eq!(posted["outcome"], "created");
    let author = &posted["notice"]["author"];
    assert_eq!(author["class"], "agent");
    assert_eq!(author["task_id"], task.as_str());
    assert_eq!(author["run_id"], "jrun-notice-1");
    let id = posted["notice"]["id"].as_str().unwrap().to_string();

    // Another task's agent corroborates and may not close it.
    let corroborated = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Shard 2 is red", "body": "rewrite",
               "evidence": [{"type": "run", "value": "jrun-other"}]}),
        &other_agent,
    )
    .unwrap();
    assert_eq!(corroborated["outcome"], "corroborated");
    assert_eq!(corroborated["notice"]["reports"]["count"], 2);
    refused(
        call(
            &runtime,
            "orbit.notice.close",
            json!({"id": id, "reason": "not mine"}),
            &other_agent,
        ),
        "notice_close_refused",
    );

    // Two open notices per bound task.
    call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "info", "title": "Second", "linked_task": task}),
        &agent,
    )
    .unwrap();
    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "info", "title": "Third", "linked_task": other}),
            &agent,
        ),
        "notice_limit_exceeded",
    );

    let closed = call(
        &runtime,
        "orbit.notice.close",
        json!({"id": id, "reason": "fixed"}),
        &agent,
    )
    .unwrap();
    assert_eq!(closed["notice"]["status"], "closed");
    // Six refusals: scope, kind, evidence, cap, close and the open limit.
    assert_eq!(notice_audit(&runtime, AuditEventStatus::Denied), 6);
}

fn linked_task_done_closes_the_notice_and_frees_its_key() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    let workspace = root.path().join("repo").to_string_lossy().into_owned();
    let update = |input: Value| runtime.run_tool("orbit.task.update", input).unwrap();
    let task = runtime
        .run_tool(
            "orbit.task.add",
            json!({"title": "Fix shard 2", "description": "Fixture fix.",
                   "acceptance_criteria": ["Shard 2 is green."], "complexity": "low",
                   "workspace": workspace, "type": "bug", "model": "codex"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let posted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Shard 2 is red", "linked_task": task}),
        &operator,
    )
    .unwrap();
    assert_eq!(
        posted["notice"]["dedupe_key"],
        format!("task:{}", task.to_lowercase())
    );
    assert_eq!(posted["notice"]["close_on_linked_done"], true);
    let id = posted["notice"]["id"].as_str().unwrap().to_string();

    update(json!({"id": task, "plan": "1. Fix it.", "model": "codex"}));
    update(json!({"id": task, "status": "backlog", "model": "codex"}));
    update(json!({"id": task, "status": "in_progress", "model": "codex"}));
    update(
        json!({"id": task, "status": "review", "execution_summary": "Fixed.",
                  "model": "codex"}),
    );
    runtime
        .apply_task_automation_update(
            &task,
            orbit_engine::TaskAutomationUpdate {
                status: Some(orbit_types::task::TaskStatus::Done),
                status_note: Some("completion authorized by run jrun-notice".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    assert_eq!(shown["notice"]["status"], "closed");
    assert_eq!(shown["notice"]["closed"]["cause"], "linked_task_done");
    assert_eq!(shown["notice"]["linked_task"]["status"], "done");
    let history = shown["history"].as_array().unwrap();
    assert_eq!(history.last().unwrap()["action"], "linked_task_done");
    let open = call(&runtime, "orbit.notice.list", json!({}), &operator).unwrap();
    assert_eq!(open["notices"].as_array().map(Vec::len), Some(0));
    let reposted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Shard 2 is red", "linked_task": task}),
        &operator,
    )
    .unwrap();
    assert_eq!(reposted["outcome"], "created");
    assert_ne!(reposted["notice"]["id"], id);
}

crate::scenario_table!(
    notice_authority,
    operator_posts_updates_and_closes_any_notice,
    orchestrator_session_posts_fleet_wide_within_its_caps,
    run_bound_agent_reports_only_about_its_task,
    linked_task_done_closes_the_notice_and_frees_its_key,
);
