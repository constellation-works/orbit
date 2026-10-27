//! A routine consumer follows a real command's durable result through MCP and CLI.
use super::*;

#[test]
fn routine_command_results_survive_failure_and_resume_without_becoming_completed() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::routine_outputs::routine_command_results_survive_failure_and_resume_without_becoming_completed",
    ) {
        return;
    }
    let workspace = McpWorkspace::init();
    let selector = workspace.work.to_str().unwrap();
    let job = workspace
        .home
        .join(".orbit/resources/jobs/command-result.yaml");
    // Enough multibyte output to cross the display cap; the useful end must survive.
    let script = format!(
        "printf '%s' '{}'; printf '\\nAuthorization: Bearer fixture-secret-value\\nfailed-tail\\n' >&2; test -f ready",
        "é".repeat(3000)
    );
    std::fs::write(&job, json!({
        "schemaVersion":2,"kind":"Job","metadata":{"name":"command-result"},
        "spec":{"state":"enabled","kind":"workflow","steps":[
            {"id":"skipped","when":"false","spec":{"type":"deterministic","action":"local_shell","config":{"command":"/usr/bin/true"}}},
            {"id":"script","spec":{"type":"deterministic","action":"local_shell","config":{"shell":"/bin/sh","script":script}}}
        ]}
    }).to_string()).unwrap();
    let failed = workspace.run(&["run", "job", "command-result", "--wait", "--json"]);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["state"], "failed", "{failed}");
    let run_id = failed["run_id"].as_str().unwrap();
    let shown: Value = serde_json::from_slice(
        &workspace
            .run_ok(&["run", "show", run_id, "--no-reconcile", "--json"])
            .stdout,
    )
    .unwrap();
    assert_command_result(&shown["steps"], 1, "error");
    assert_ne!(
        shown["pipeline_state"]["step_states"]["1"], "success",
        "failed diagnostics must never become a successful resume checkpoint: {shown}"
    );

    let routine = workspace.work.join(".orbit/routines/command-result.yaml");
    std::fs::write(routine, "schemaVersion: 1\nname: command-result\nenabled: true\ntrigger:\n  cron: '0 9 * * *'\ntarget: job:command-result\n").unwrap();
    // Link the observed fire to the real run; no synthetic step output shape.
    workspace.store().execute(
        "INSERT INTO routine_fires (routine_name,slot,attempt,state,run_id,source_workspace,created_at,updated_at) VALUES ('command-result',?1,1,'dispatched',?2,?3,?1,?1)",
        rusqlite::params![chrono::Utc::now().to_rfc3339(),run_id,selector],
    ).unwrap();
    let mut client = workspace.serve_with_args(&["--operator"]);
    let listed = client.call_tool_ok(
        "orbit_routine_control",
        json!({"workspace":selector,"action":"list"}),
    );
    let row = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "command-result")
        .unwrap();
    assert_eq!(row["last_fire"]["run_id"], run_id, "{row}");
    let cli_list: Value =
        serde_json::from_slice(&workspace.run_ok(&["routine", "list", "--json"]).stdout).unwrap();
    let row = cli_list["routines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "command-result")
        .unwrap();
    assert_eq!(row["last_fire"]["run_id"], run_id, "{row}");

    for view in ["full", "bounded"] {
        let mut input = json!({"workspace":selector,"id":run_id});
        if view == "bounded" {
            input["view"] = json!(view);
        }
        let shown = client.call_tool_ok("orbit_workflow_run_show", input);
        let steps = if view == "bounded" {
            &shown["run"]["steps"]
        } else {
            &shown["steps"]
        };
        assert_command_result(steps, 1, "error");
    }
    // A real resume must rerun the failed command, replacing its diagnostic.
    std::fs::write(workspace.work.join("ready"), "").unwrap();
    let resumed: Value = serde_json::from_slice(
        &workspace
            .run_ok(&["job", "resume", run_id, "--wait", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(resumed["state"], "success", "{resumed}");
    let resumed_id = resumed["run_id"].as_str().unwrap();
    let shown = client.call_tool_ok(
        "orbit_workflow_run_show",
        json!({"workspace":selector,"id":resumed_id,"view":"bounded"}),
    );
    assert_command_result(&shown["run"]["steps"], 0, "success");

    // Historical successful commands stored the captured streams but no tail.
    // Start with this real command's checkpoint, rather than inventing output.
    let runtime = orbit_core::OrbitRuntime::from_roots(
        &workspace.home.join(".orbit"),
        &workspace.work.join(".orbit"),
    )
    .unwrap();
    let mut state = runtime.read_run_state(resumed_id).unwrap().unwrap();
    for output in state
        .step_outputs
        .values_mut()
        .chain(state.pipeline.as_object_mut().unwrap().values_mut())
    {
        if let Some(fields) = output.as_object_mut() {
            fields.remove("log_tail");
            fields.remove("log_tail_truncated");
        }
    }
    runtime.write_run_state(resumed_id, &state).unwrap();
    let historical = client.call_tool_ok(
        "orbit_workflow_run_show",
        json!({"workspace":selector,"id":resumed_id,"view":"bounded"}),
    );
    assert_command_result(&historical["run"]["steps"], 0, "success");
}

fn assert_command_result(steps: &Value, exit_code: i32, state: &str) {
    let step = steps
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["target_id"] == "script")
        .unwrap();
    assert_eq!(step["state"], state, "{step}");
    assert_eq!(step["exit_code"], exit_code, "{step}");
    let output = &step["output"];
    assert_eq!(output["exit_code"], exit_code, "{step}");
    let tail = output["log_tail"].as_str().unwrap();
    assert!(tail.len() <= 4096, "tail exceeds its byte budget");
    assert!(tail.ends_with("failed-tail\n"), "{tail}");
    assert!(
        !tail.contains("fixture-secret-value"),
        "credential reached the public result"
    );
    assert_eq!(output["log_tail_truncated"], true);
    assert!(
        output.get("stdout").is_none() && output.get("argv").is_none(),
        "only the bounded command summary is public: {output}"
    );
}
