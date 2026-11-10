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
    runtime_at_config(root, CONFIG)
}

fn runtime_at_config(root: &Path, config: &str) -> OrbitRuntime {
    let global = root.join("home/.orbit");
    let orbit = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), config).unwrap();
    OrbitRuntime::from_roots(&global, &orbit).unwrap()
}

/// The resolved tools of one shipped activity, through its own declared
/// disallow list.
fn activity_resolved_tools(
    runtime: &OrbitRuntime,
    activity: &str,
) -> orbit_engine::ResolvedActivityTools {
    let yaml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("assets/activities/{activity}.yaml")),
    )
    .unwrap();
    let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
    let orbit_types::workflow::ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("agent activity")
    };
    let denied = spec.tool_disallow_list.unwrap_or_default();
    runtime
        .resolve_activity_tool_denials(&[], activity, &denied)
        .unwrap()
}

#[cfg(unix)]
/// The deny list the child of one shipped activity receives from the real CLI
/// dispatcher. A fixture provider records the `ORBIT_ACTIVITY_TOOLS_DENY` it
/// was launched with, so the claimed-mode additions, granted-tool lifts and
/// host suppressions are exactly what production stamps.
fn dispatched_deny_list(runtime: &OrbitRuntime, root: &Path, activity: &str) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt as _;

    let recorded = root.join(format!("{activity}.deny"));
    let bin = root.join(format!("{activity}-provider"));
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncat > /dev/null\nprintf '%s' \"$ORBIT_ACTIVITY_TOOLS_DENY\" > '{}'\nprintf '%s\\n' '{{\"type\":\"result\",\"is_error\":false,\"result\":\"{{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{{}},\\\"error\\\":null}}\"}}'\n",
            recorded.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut executor: orbit_types::resource::ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/claude.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(bin.display().to_string());
    executor.spec.sandbox = Some(orbit_types::workflow::ExecutorSandboxKind::Off);
    runtime
        .upsert_executor_def(&orbit_types::workflow::ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    let yaml = std::fs::read_to_string(assets.join(format!("activities/{activity}.yaml"))).unwrap();
    let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
    let orbit_types::workflow::ActivityV2Spec::AgentLoop(mut spec) = asset.spec.spec else {
        panic!("agent activity")
    };
    spec.provider = orbit_types::workflow::activity_job::Provider::Claude;
    let run = runtime
        .insert_job_run("notice_policy_fixture", 1, chrono::Utc::now(), None, None)
        .unwrap();
    let audit = orbit_engine::V2AuditWriter::with_disk_sinks(
        &root.join("audit"),
        std::sync::Arc::new(runtime.sqlite_store().unwrap()),
        runtime.workspace_id().unwrap(),
        &run.run_id,
        "fixture",
        None,
    )
    .unwrap();
    orbit_engine::activity_job::cli_runner::run_cli_backend(
        runtime,
        &spec,
        activity,
        &run.run_id,
        audit,
        &json!({"workspace_path": root.join("repo")}),
        None,
    )
    .unwrap();
    std::fs::read_to_string(&recorded)
        .unwrap_or_else(|error| panic!("{activity} provider never ran: {error}"))
        .split(',')
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(unix)]
/// Whether the child `run_cli_backend` launches for `activity` may call `tool`.
fn propagated_policy_allows(
    runtime: &OrbitRuntime,
    root: &Path,
    activity: &str,
    tool: &str,
) -> bool {
    let context = orbit_tools::ToolContext {
        tool_deny_policy: Some(orbit_types::workflow::ActivityToolDenyPolicy {
            activity: activity.to_string(),
            disallow_list: dispatched_deny_list(runtime, root, activity),
        }),
        ..Default::default()
    };
    orbit_runtime::tool_exec::check_activity_tool_policy(tool, &context).is_ok()
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

fn updated_task_links_control_notice_closure() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    let workspace = root.path().join("repo").to_string_lossy().into_owned();
    let add = |title: &str| {
        runtime
            .run_tool(
                "orbit.task.add",
                json!({"title": title, "description": "Fixture fix.",
            "acceptance_criteria": ["Fixed."], "complexity": "low", "workspace": workspace,
            "type": "bug", "model": "codex"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let done = |task: &str| {
        for input in [
            json!({"id": task, "plan": "Fix it.", "model": "codex"}),
            json!({"id": task, "status": "backlog", "model": "codex"}),
            json!({"id": task, "status": "in_progress", "model": "codex"}),
            json!({"id": task, "status": "review", "execution_summary": "Fixed.", "model": "codex"}),
        ] {
            runtime.run_tool("orbit.task.update", input).unwrap();
        }
        runtime
            .apply_task_automation_update(
                task,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(orbit_types::task::TaskStatus::Done),
                    status_note: Some("completion authorized by run jrun-notice".into()),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    let post = |key: &str, link: Option<&str>, close: bool| {
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "info", "title": key,
            "dedupe_key": key, "linked_task": link, "close_on_linked_done": close}),
            &operator,
        )
        .unwrap()
    };
    let show =
        |id: &str| call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    let a = add("Original task");
    let b = add("Replacement task");
    let already_done = add("Already completed task");
    done(&already_done);

    let effective = post("effective", None, false)["notice"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        post("effective", Some(&already_done), true)["outcome"],
        "updated"
    );
    assert_eq!(show(&effective)["notice"]["status"], "closed");
    let closed = call(
        &runtime,
        "orbit.notice.list",
        json!({"status": "closed"}),
        &operator,
    )
    .unwrap();
    assert!(
        closed["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice["id"] == effective)
    );

    let linked = post("linked", None, false)["notice"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(post("linked", Some(&b), true)["outcome"], "updated");
    let relinked = post("relinked", Some(&a), true)["notice"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(post("relinked", Some(&b), true)["outcome"], "updated");
    let opted_out = post("opted-out", Some(&a), true)["notice"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(post("opted-out", Some(&b), false)["outcome"], "updated");
    done(&a);
    assert_eq!(show(&relinked)["notice"]["status"], "open");
    assert_eq!(show(&opted_out)["notice"]["status"], "open");
    done(&b);
    for id in [&linked, &relinked] {
        let shown = show(id);
        assert_eq!(shown["notice"]["status"], "closed");
        assert_eq!(
            shown["history"].as_array().unwrap().last().unwrap()["action"],
            "linked_task_done"
        );
    }
    assert_eq!(show(&opted_out)["notice"]["status"], "open");
    let open = call(&runtime, "orbit.notice.list", json!({}), &operator).unwrap();
    assert!(
        open["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|notice| notice["id"] == opted_out)
    );
}

// Freeze the owner clock rather than sleeping across the five-minute boundary.
fn lost_reply_retries_preserve_editor_and_corroborator_state() {
    use chrono::{Duration, Utc};
    use orbit_core::test_support;

    for editor in [true, false] {
        for relative in [true, false] {
            let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
            let runtime = runtime_at(root.path());
            let task = add_task(&runtime, "Retry report");
            let agent = agent_session(&runtime, &task);
            let operator = session(McpCapability::Operator);
            let now = Utc::now();
            let mut request = json!({"kind": "incident", "title": "Retry report",
                "linked_task": task});
            if relative {
                request["expires_in_hours"] = json!(1);
            } else {
                request["expires_at"] = json!(now + Duration::minutes(6));
            }
            if !editor {
                let mut original = request.clone();
                original["body"] = json!("Operator content");
                test_support::with_notice_post_time(now, || {
                    call(&runtime, "orbit.notice.post", original, &operator).unwrap()
                });
            }
            let posted = test_support::with_notice_post_time(now, || {
                call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap()
            });
            let id = posted["notice"]["id"].as_str().unwrap();
            let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
            let audits = notice_audit(&runtime, AuditEventStatus::Success);
            let later = now + Duration::minutes(if relative { 40 } else { 2 });
            for _ in 0..6 {
                let retry = test_support::with_notice_post_time(later, || {
                    call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap()
                });
                assert_eq!(
                    retry["outcome"],
                    if editor { "updated" } else { "corroborated" }
                );
                assert_eq!(
                    retry["notice"], posted["notice"],
                    "a retry must move no stored field"
                );
            }
            // Explicit defaults and omitted defaults identify the same request.
            let mut defaults = request.clone();
            defaults["scope"] = json!("workspace");
            defaults["dedupe_key"] = json!(format!("task:{}", task.to_lowercase()));
            defaults["close_on_linked_done"] = json!(true);
            defaults["body"] = json!("");
            let retry = test_support::with_notice_post_time(later, || {
                call(&runtime, "orbit.notice.post", defaults, &agent).unwrap()
            });
            assert_eq!(retry["notice"], posted["notice"]);
            assert_eq!(notice_audit(&runtime, AuditEventStatus::Success), audits);
            assert_eq!(
                call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap(),
                shown
            );

            let mut invalid = request.clone();
            invalid["body"] = json!("A distinct report still validates expiry");
            if relative {
                invalid["expires_in_hours"] = json!(0);
            }
            refused(
                test_support::with_notice_post_time(later, || {
                    call(&runtime, "orbit.notice.post", invalid, &agent)
                }),
                "notice_expiry_invalid",
            );

            // An intervening write, even at the same timestamp, invalidates the
            // previous request's retry identity. Never search older fingerprints.
            let other = agent_session(&runtime, &add_task(&runtime, "Other reporter"));
            let mut intervening = request.clone();
            intervening.as_object_mut().unwrap().remove("expires_at");
            intervening
                .as_object_mut()
                .unwrap()
                .remove("expires_in_hours");
            test_support::with_notice_post_time(now, || {
                call(&runtime, "orbit.notice.post", intervening, &other).unwrap()
            });
            let result = test_support::with_notice_post_time(later, || {
                call(&runtime, "orbit.notice.post", request, &agent)
            });
            if relative {
                let result = result.unwrap();
                assert_eq!(
                    result["notice"]["reports"]["count"],
                    posted["notice"]["reports"]["count"].as_u64().unwrap() + 2
                );
            } else {
                refused(result, "notice_expiry_invalid");
            }
        }
    }
}

fn lost_reply_retries_bypass_a_reached_post_rate_only_for_the_latest_request() {
    for editor in [true, false] {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let runtime = runtime_at(root.path());
        let task = add_task(&runtime, "Rate-limited reporter");
        let agent = agent_session(&runtime, &task);
        let operator = session(McpCapability::Operator);
        let mut request = json!({"kind": "info", "title": "Rate-limited reporter",
            "linked_task": task});
        if !editor {
            call(&runtime, "orbit.notice.post", request.clone(), &operator).unwrap();
        }
        // Reach the fixed cap with distinct writes of the same open subject.
        let mut last = Value::Null;
        for report in 0..orbit_core::application::notice::limits::AGENT_POSTS_PER_TASK_PER_DAY {
            request["body"] = json!(format!("Report {report}"));
            last = call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap();
        }
        let id = last["notice"]["id"].as_str().unwrap();
        let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
        let audits = notice_audit(&runtime, AuditEventStatus::Success);
        let retry = call(&runtime, "orbit.notice.post", request.clone(), &agent).unwrap();
        assert_eq!(
            retry["outcome"],
            if editor { "updated" } else { "corroborated" }
        );
        assert_eq!(retry["notice"], last["notice"]);
        assert_eq!(notice_audit(&runtime, AuditEventStatus::Success), audits);
        assert_eq!(
            call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap(),
            shown
        );
        // The resolved author includes run/model identity, not just the bound task.
        let mut other_run = agent.clone();
        other_run.worker_invocation.as_mut().unwrap().bound_run_id = "jrun-another-run".into();
        refused(
            call(&runtime, "orbit.notice.post", request.clone(), &other_run),
            "notice_limit_exceeded",
        );
        request["model"] = json!("codex");
        refused(
            call(&runtime, "orbit.notice.post", request.clone(), &agent),
            "notice_limit_exceeded",
        );
        request.as_object_mut().unwrap().remove("model");
        request["body"] = json!("A genuinely new report");
        refused(
            call(&runtime, "orbit.notice.post", request, &agent),
            "notice_limit_exceeded",
        );
    }
}

fn assert_close_kept_corroboration(closed: &Value, corroborated: &Value, revision: &Value) {
    assert_eq!(
        corroborated["notice"]["revision"], *revision,
        "the overlapping post must leave content and revision unchanged"
    );
    assert!(
        corroborated["notice"]["reports"]["count"].as_u64().unwrap() > 1,
        "the corroboration must commit a higher report count before close writes"
    );
    assert_eq!(
        closed["notice"]["reports"]["count"],
        corroborated["notice"]["reports"]["count"]
    );
    assert_eq!(
        closed["notice"]["reports"]["last_at"],
        corroborated["notice"]["reports"]["last_at"]
    );
    assert_eq!(closed["notice"]["revision"], *revision);
}

/// Commit one corroboration of the open notice `dedupe_key` at `at`.
///
/// This is the store write a poster's transaction makes after it has already
/// decided the notice is open. A later tool post would not, once a linked
/// task is done.
fn commit_open_corroboration(
    runtime: &OrbitRuntime,
    dedupe_key: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Value {
    use orbit_store::contracts::{
        NoticeEventRow, NoticePostPlan, NoticePostQuery, NoticeStoreBackend,
    };

    let store = runtime.sqlite_store().unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let written = store
        .post_notice(
            &NoticePostQuery {
                workspace_id: &workspace_id,
                poster_task_id: None,
                poster_crew: None,
                events_since: at,
                purge_before: None,
            },
            &|snapshot| {
                let matched = snapshot
                    .unclosed
                    .iter()
                    .find(|row| row.dedupe_key == dedupe_key)
                    .expect("the notice is still stored open");
                let mut document: Value =
                    serde_json::from_str(&matched.record_json).expect("notice document");
                let count = document["reports"]["count"].as_u64().expect("report count") + 1;
                document["reports"]["count"] = json!(count);
                document["reports"]["last_at"] = json!(at);
                let mut row = matched.clone();
                row.record_json = serde_json::to_string(&document).expect("notice document");
                let expected_revision = matched.revision;
                let event = NoticeEventRow {
                    workspace_id: row.workspace_id.clone(),
                    notice_id: row.id.clone(),
                    at,
                    action: "corroborated".into(),
                    poster_class: "orchestrator".into(),
                    poster_crew: Some("sol".into()),
                    poster_task_id: None,
                    detail_json: json!({ "revision": expected_revision }).to_string(),
                };
                Ok(NoticePostPlan::Update {
                    row,
                    expected_revision,
                    event,
                })
            },
        )
        .expect("corroboration commits");
    let document: Value = serde_json::from_str(&written.record_json).expect("written notice");
    json!({
        "outcome": "corroborated",
        "notice": {
            "revision": written.revision,
            "reports": document["reports"],
        }
    })
}

/// Close reads the notice, a corroboration commits at that same revision,
/// then the close writes. The stored reports and the close result keep the
/// corroboration. No timing or extra load: the hook runs in that window.
fn close_preserves_corroboration_committed_after_close_read() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use chrono::{Duration, Utc};
    use orbit_core::test_support;

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    let later = Utc::now() + Duration::hours(5);

    let posted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Disk shelf", "dedupe_key": "incident:disk-shelf"}),
        &operator,
    )
    .unwrap();
    let id = posted["notice"]["id"].as_str().unwrap().to_string();
    let revision = posted["notice"]["revision"].clone();
    let seen = Rc::new(RefCell::new(None));
    let seen_hook = Rc::clone(&seen);
    let hooked = runtime.clone();
    let manual_id = id.clone();
    let closed = test_support::with_notice_close_snapshot_hook(
        move || {
            let corroborated = test_support::with_notice_post_time(later, || {
                call(
                    &hooked,
                    "orbit.notice.post",
                    json!({"kind": "incident", "title": "Disk shelf",
                           "dedupe_key": "incident:disk-shelf", "body": "A second report",
                           "orchestrator": "sol"}),
                    &session(McpCapability::Agent),
                )
                .unwrap()
            });
            *seen_hook.borrow_mut() = Some(corroborated);
        },
        || {
            call(
                &runtime,
                "orbit.notice.close",
                json!({"id": manual_id, "reason": "shelf replaced"}),
                &operator,
            )
            .unwrap()
        },
    );
    let corroborated = seen.borrow().clone().unwrap();
    assert_close_kept_corroboration(&closed, &corroborated, &revision);
    assert_eq!(closed["notice"]["status"], "closed");
    assert_eq!(closed["notice"]["closed"]["cause"], "manual");
    let shown = call(&runtime, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    assert_close_kept_corroboration(&shown, &corroborated, &revision);

    let workspace = root.path().join("repo").to_string_lossy().into_owned();
    let task = runtime
        .run_tool(
            "orbit.task.add",
            json!({"title": "Replace the shelf", "description": "Fixture fix.",
                   "acceptance_criteria": ["Shelf replaced."], "complexity": "low",
                   "workspace": workspace, "type": "bug", "model": "codex"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let linked = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Shelf still dead",
               "dedupe_key": "incident:shelf-still-dead", "linked_task": task,
               "close_on_linked_done": true}),
        &operator,
    )
    .unwrap();
    let linked_id = linked["notice"]["id"].as_str().unwrap().to_string();
    let linked_revision = linked["notice"]["revision"].clone();
    for input in [
        json!({"id": task, "plan": "Replace it.", "model": "codex"}),
        json!({"id": task, "status": "backlog", "model": "codex"}),
        json!({"id": task, "status": "in_progress", "model": "codex"}),
        json!({"id": task, "status": "review", "execution_summary": "Replaced.", "model": "codex"}),
    ] {
        runtime.run_tool("orbit.task.update", input).unwrap();
    }
    let seen = Rc::new(RefCell::new(None));
    let seen_hook = Rc::clone(&seen);
    let hooked = runtime.clone();
    test_support::with_notice_close_snapshot_hook(
        move || {
            // The task is already done, so a tool post no longer matches this
            // notice. Commit the corroboration through the store, which is the
            // write that can still land in this window.
            *seen_hook.borrow_mut() = Some(commit_open_corroboration(
                &hooked,
                "incident:shelf-still-dead",
                later,
            ));
        },
        || {
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
        },
    );
    let corroborated = seen.borrow().clone().unwrap();
    let shown = call(
        &runtime,
        "orbit.notice.show",
        json!({"id": linked_id}),
        &operator,
    )
    .unwrap();
    assert_close_kept_corroboration(&shown, &corroborated, &linked_revision);
    assert_eq!(shown["notice"]["status"], "closed");
    assert_eq!(shown["notice"]["closed"]["cause"], "linked_task_done");
}

fn agent_posting_baseline_governs_managed_runs() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let task = add_task(&runtime, "Posting agent's bound task");
    let agent = agent_session(&runtime, &task);

    // The three agent activities carry exactly the notice write tools in
    // their baselines; no other notice tool is granted by this slice.
    for activity in ["agent_implement", "agent_review_repair", "final_recovery"] {
        let resolved = activity_resolved_tools(&runtime, activity);
        assert!(
            !resolved
                .suppressed_tools
                .iter()
                .any(|tool| tool.starts_with("orbit.notice.")),
            "{activity} must suppress no notice tool when agent posting is on: {:?}",
            resolved.suppressed_tools
        );
        for tool in [
            "orbit.notice.post",
            "orbit.notice.show",
            "orbit.notice.close",
            "orbit.notice.list",
        ] {
            assert!(
                resolved.effective_tools.iter().any(|name| name == tool),
                "{activity} baseline must carry {tool}"
            );
            #[cfg(unix)]
            assert!(
                propagated_policy_allows(&runtime, root.path(), activity, tool),
                "{activity} child policy must allow {tool} when agent posting is on"
            );
        }
    }

    // A managed run posts through its own task, and a later prompt build's
    // brief (what a second run sees) quotes the stored agent notice.
    let posted = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Every agent hits the red shard",
               "linked_task": task}),
        &agent,
    )
    .unwrap();
    assert_eq!(posted["notice"]["author"]["class"], "agent");
    assert_eq!(posted["notice"]["author"]["task_id"], task.as_str());
    assert_eq!(posted["notice"]["author"]["run_id"], "jrun-notice-1");
    let id = posted["notice"]["id"].as_str().unwrap().to_string();
    let brief = runtime
        .notices_for_agent_input("jrun-second", "agent_implement", &json!({"task_id": task}))
        .expect("a second run's brief carries the open notice");
    assert_eq!(brief.block["trust"], "untrusted_data");
    assert!(
        brief.notices.iter().any(|entry| entry.starts_with(&id)),
        "the brief names the agent's notice: {:?}",
        brief.notices
    );
}

/// A link must resolve on the home owner: a task id whose prefix this
/// host never issued belongs to another host and is refused, as is one
/// no workspace here holds.
fn linked_task_this_owner_does_not_hold_is_refused() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = runtime_at(root.path());
    let operator = session(McpCapability::Operator);
    // Touch the local allocator so its prefix is the known one.
    add_task(&runtime, "Local fixture");

    let foreign = call(
        &runtime,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Foreign link", "linked_task": "XYZ-123"}),
        &operator,
    );
    let error = foreign.expect_err("a foreign task must not link");
    assert!(error.starts_with("notice_invalid_field"), "{error}");
    assert!(error.contains("another host"), "{error}");

    refused(
        call(
            &runtime,
            "orbit.notice.post",
            json!({"kind": "incident", "title": "Missing link", "linked_task": "ORB-999999"}),
            &operator,
        ),
        "notice_invalid_field",
    );
}

fn runtime_at_roots(global: &Path, orbit: &Path) -> OrbitRuntime {
    runtime_at_roots_config(global, orbit, CONFIG)
}

fn runtime_at_roots_config(global: &Path, orbit: &Path, config: &str) -> OrbitRuntime {
    std::fs::create_dir_all(global).unwrap();
    std::fs::create_dir_all(orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), config).unwrap();
    OrbitRuntime::from_roots(global, orbit).unwrap()
}

fn attention_notice_config() -> String {
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n"
        .to_string()
}

fn attention_pages_home_notices() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let home_root = root.path().join("home-ws/.orbit");
    let other_root = root.path().join("other-ws/.orbit");
    let config = attention_notice_config();
    let home = runtime_at_roots_config(&global, &home_root, &config);
    let other = runtime_at_roots_config(&global, &other_root, &config);
    let operator = session(McpCapability::Operator);
    let home_id = home.workspace_id().unwrap().to_string();
    assert_ne!(home_id, other.workspace_id().unwrap().as_str());

    // Within the fixed open-notice limit of 20 per workspace.
    let mut home_notice_ids = Vec::new();
    for index in 0..11 {
        let posted = call(
            &home,
            "orbit.notice.post",
            json!({"kind": "info", "title": format!("Home notice {index:02}")}),
            &operator,
        )
        .unwrap();
        home_notice_ids.push(posted["notice"]["id"].as_str().unwrap().to_string());
    }

    let query = |runtime: &OrbitRuntime, input: Value| {
        call(runtime, "orbit.attention", input, &operator).unwrap()
    };
    let first = query(&home, json!({"kind": "notice_open", "limit": 10}));
    assert_eq!(first["counts"]["matching"], 11);
    assert_eq!(first["counts"]["returned"], 10);
    assert_eq!(first["counts"]["by_kind"]["notice_open"], 10);
    assert_eq!(first["counts"]["truncated"], true);
    assert_eq!(first["items"].as_array().map(Vec::len), Some(10));
    assert!(first["degraded"].as_array().unwrap().is_empty());
    for item in first["items"].as_array().unwrap() {
        assert_eq!(item["workspace"]["id"], home_id);
        assert!(
            home_notice_ids
                .iter()
                .any(|id| item["refs"]["notice_id"].as_str() == Some(id.as_str()))
        );
    }

    let first_cursor = first["cursor"]["next"].as_str().unwrap();
    let second = query(
        &home,
        json!({"kind": "notice_open", "limit": 10, "since": first_cursor}),
    );
    assert_eq!(second["counts"]["matching"], 1);
    assert_eq!(second["counts"]["returned"], 1);
    assert_eq!(second["counts"]["unchanged"], 10);
    assert_eq!(second["cursor"]["status"], "accepted");
    assert!(second["degraded"].as_array().unwrap().is_empty());
    let second_cursor = second["cursor"]["next"].as_str().unwrap();
    let settled = query(
        &home,
        json!({"kind": "notice_open", "limit": 10, "since": second_cursor}),
    );
    assert_eq!(settled["counts"]["matching"], 0);
    assert_eq!(settled["counts"]["returned"], 0);
    assert_eq!(settled["counts"]["unchanged"], 11);
    assert!(settled["degraded"].as_array().unwrap().is_empty());

    let filtered = query(
        &home,
        json!({"kind": "notice_open", "severity": "medium", "limit": 100}),
    );
    assert_eq!(filtered["counts"]["matching"], 0);
    assert!(filtered["items"].as_array().unwrap().is_empty());
    assert!(filtered["degraded"].as_array().unwrap().is_empty());
}

/// A notice's home is the runtime's store partition, which a registered
/// checkout can hold under an id other than its registry selector
/// (`ws_orbit` against `orbit-5c61b3`). Attention must still list the
/// workspace's own notices, and a global notice homed in another workspace
/// on the same owner, once each.
fn attention_lists_notices_under_a_registry_id_that_differs_from_the_store_id() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let bind = |name: &str, logical: &str| {
        let orbit = root.path().join(format!("{name}/.orbit"));
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
        OrbitRuntime::from_roots_with_binding(
            &global,
            &orbit,
            orbit_core::WorkspaceRuntimeBinding {
                logical_workspace_id: logical.into(),
                task_partition_id: format!("{}-5c61b3", logical.trim_start_matches("ws_")),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: root.path().join(name),
                ship_mode: orbit_core::ShipMode::Local,
                base_branch: Some("main".into()),
            },
        )
        .unwrap()
    };
    let home = bind("home-ws", "ws_home");
    let other = bind("other-ws", "ws_other");
    let operator = session(McpCapability::Operator);
    let home_store_id = home.workspace_id().unwrap();
    assert_ne!(
        home_store_id, "ws_home",
        "the fixture needs a store id that differs from the registry selector"
    );

    let post = |runtime: &OrbitRuntime, input: Value| {
        call(runtime, "orbit.notice.post", input, &operator).unwrap()["notice"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let scoped = post(&home, json!({"kind": "info", "title": "Home only"}));
    let maintenance = post(
        &home,
        json!({"kind": "maintenance", "title": "Binary update", "scope": "global"}),
    );
    let closed = post(&home, json!({"kind": "info", "title": "Closed one"}));
    call(
        &home,
        "orbit.notice.close",
        json!({"id": closed, "reason": "done"}),
        &operator,
    )
    .unwrap();

    let ids_of = |runtime: &OrbitRuntime| -> Vec<(String, String, String)> {
        let page = call(
            runtime,
            "orbit.attention",
            json!({"kind": "notice_open", "severity": "low"}),
            &operator,
        )
        .unwrap();
        assert!(page["degraded"].as_array().unwrap().is_empty(), "{page}");
        let mut found = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["refs"]["notice_id"].as_str().unwrap().to_string(),
                    item["severity"].as_str().unwrap().to_string(),
                    item["workspace"]["id"].as_str().unwrap().to_string(),
                )
            })
            .collect::<Vec<_>>();
        found.sort();
        found
    };

    // Home workspace: its own notice and the global one; severity follows
    // the kind (maintenance medium, info low); the closed notice is absent.
    let mut expected = vec![
        (scoped.clone(), "low".to_string(), "ws_home".to_string()),
        (
            maintenance.clone(),
            "medium".to_string(),
            "ws_home".to_string(),
        ),
    ];
    expected.sort();
    assert_eq!(ids_of(&home), expected);

    // A different workspace on the same owner sees the global notice, and
    // not the home workspace's scoped one.
    assert_eq!(
        ids_of(&other),
        vec![(
            maintenance.clone(),
            "medium".to_string(),
            "ws_other".to_string()
        )]
    );
}

/// A notice in one workspace may link a task in another workspace the
/// same host owns: it reads closed once that task is done, and the
/// closure tick (the scheduler sweep's writer) stores the closure and
/// frees the dedupe key.
fn cross_workspace_linked_task_closes_on_read_and_in_the_closure_tick() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let home = runtime_at_roots(&global, &root.path().join("home-ws/.orbit"));
    let other = runtime_at_roots(&global, &root.path().join("other-ws/.orbit"));
    assert_ne!(
        home.workspace_id().unwrap(),
        other.workspace_id().unwrap(),
        "the fixture needs two workspace partitions on one host"
    );
    let operator = session(McpCapability::Operator);

    // Complete the linked task in the other workspace before the notice
    // exists, so no transition writer can materialize the closure.
    let other_root = root.path().join("other-ws").to_string_lossy().into_owned();
    let update = |input: Value| other.run_tool("orbit.task.update", input).unwrap();
    let task = other
        .run_tool(
            "orbit.task.add",
            json!({"title": "Fix in other workspace", "description": "Fixture fix.",
                   "acceptance_criteria": ["Fixed."], "complexity": "low",
                   "workspace": other_root, "type": "bug", "model": "codex"}),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    update(json!({"id": task, "plan": "1. Fix it.", "model": "codex"}));
    update(json!({"id": task, "status": "backlog", "model": "codex"}));
    update(json!({"id": task, "status": "in_progress", "model": "codex"}));
    update(
        json!({"id": task, "status": "review", "execution_summary": "Fixed.", "model": "codex"}),
    );
    other
        .apply_task_automation_update(
            &task,
            orbit_engine::TaskAutomationUpdate {
                status: Some(orbit_types::task::TaskStatus::Done),
                status_note: Some("completion authorized by run jrun-notice-xws".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let posted = call(
        &home,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Cross-workspace fix", "linked_task": task}),
        &operator,
    )
    .expect("a same-owner cross-workspace link is accepted");
    assert_eq!(
        posted["notice"]["linked_task"]["workspace_id"],
        other.workspace_id().unwrap().as_str()
    );
    let id = posted["notice"]["id"].as_str().unwrap().to_string();

    // Closed on read, because the linked task is done; not yet stored, so
    // the history has no linked_task_done event.
    let shown = call(&home, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    assert_eq!(shown["notice"]["status"], "closed");
    assert_eq!(shown["notice"]["closed"]["cause"], "linked_task_done");
    assert_eq!(shown["notice"]["linked_task"]["status"], "done");
    assert!(
        shown["history"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["action"] != "linked_task_done"),
        "{shown}"
    );

    home.run_notice_closure_tick().unwrap();
    let shown = call(&home, "orbit.notice.show", json!({"id": id}), &operator).unwrap();
    assert_eq!(
        shown["history"].as_array().unwrap().last().unwrap()["action"],
        "linked_task_done"
    );

    // The stored closure freed the dedupe key.
    let reposted = call(
        &home,
        "orbit.notice.post",
        json!({"kind": "incident", "title": "Cross-workspace fix", "linked_task": task}),
        &operator,
    )
    .unwrap();
    assert_eq!(reposted["outcome"], "created");
}

crate::scenario_table!(
    notice_authority,
    agent_posting_baseline_governs_managed_runs,
    operator_posts_updates_and_closes_any_notice,
    orchestrator_session_posts_fleet_wide_within_its_caps,
    run_bound_agent_reports_only_about_its_task,
    linked_task_done_closes_the_notice_and_frees_its_key,
    updated_task_links_control_notice_closure,
    lost_reply_retries_preserve_editor_and_corroborator_state,
    lost_reply_retries_bypass_a_reached_post_rate_only_for_the_latest_request,
    close_preserves_corroboration_committed_after_close_read,
    linked_task_this_owner_does_not_hold_is_refused,
    cross_workspace_linked_task_closes_on_read_and_in_the_closure_tick,
    attention_pages_home_notices,
    attention_lists_notices_under_a_registry_id_that_differs_from_the_store_id,
);
