//! `orbit run show` renders durations, step numbers, catalog layers and an
//! interrupted run's steps for a human reader, while `--json` stays exact.

use super::*;

const INTERRUPTED: &str = "jrun-20261008-0026";
const STEP_IDS: [&str; 5] = ["worktree", "prepare", "implement_one", "validate", "commit"];

/// An interrupted shipped-pipeline run: 13191607 ms long, a record holding only
/// the reconciler's run-level step, and five steps in its audit trail.
fn seed_interrupted_run(fixture: &Fixture) {
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    db.execute(
        "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
             scheduled_at, started_at, finished_at, duration_ms, created_at)
         VALUES (?1, ?2, 'task_pr_pipeline', 1, 'interrupted',
             '2026-10-08T00:26:00+00:00', '2026-10-08T00:26:00+00:00',
             '2026-10-08T04:05:51+00:00', 13191607, '2026-10-08T00:26:00+00:00')",
        params![INTERRUPTED, workspace_id],
    )
    .expect("seed interrupted run");
    db.execute(
        "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type,
             target_id, state, started_at, finished_at, duration_ms, error_code,
             error_message)
         VALUES (?1, ?2, 0, 'job', 'task_pr_pipeline', 'interrupted',
             '2026-10-08T00:26:00+00:00', '2026-10-08T04:05:51+00:00', 13191607,
             'worker_terminated', 'job run marked interrupted')",
        params![workspace_id, INTERRUPTED],
    )
    .expect("seed run-level step");
    for (index, step_id) in STEP_IDS.iter().enumerate() {
        let finished = index + 1 < STEP_IDS.len();
        let mut events = vec![("step_started", index * 2)];
        if finished {
            events.push(("step_finished", index * 2 + 1));
        }
        for (body_kind, offset) in events {
            let ts = format!("2026-10-08T00:{:02}:00+00:00", 26 + offset);
            let event_id = format!("{INTERRUPTED}-{step_id}-{body_kind}");
            let payload = serde_json::json!({
                "event_id": event_id,
                "body_kind": body_kind,
                "step_id": step_id,
                "outcome": "success",
                "lock_wait_ms": if index == 0 { Some(42) } else { Some(0) },
                "ts": ts,
            });
            db.execute(
                "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version,
                     event_type, ts, run_id, agent_identity, payload_json)
                 VALUES (?1, ?2, 'v2_envelope', 1, 'activity.progress', ?3, ?4, 'test', ?5)",
                params![workspace_id, event_id, ts, INTERRUPTED, payload.to_string()],
            )
            .expect("seed audit event");
        }
    }
}

fn shown(fixture: &Fixture, extra: &[&str]) -> String {
    let output = fixture
        .orbit()
        .args(["run", "show", INTERRUPTED, "--no-reconcile"])
        .args(extra)
        .output()
        .expect("spawn orbit");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_show_prints_human_durations_and_one_based_steps_in_every_human_mode() {
    let fixture = Fixture::terminal();
    seed_interrupted_run(&fixture);
    // Piped output (`auto`) renders the plain form of the table.
    for format in ["table", "auto"] {
        let text = shown(&fixture, &["--format", format]);
        assert!(text.contains("Duration: 3h 39m 51s"), "{format}: {text}");
        assert!(!text.contains("13191607"), "{format}: {text}");
        let first_row = text
            .lines()
            .find(|line| line.contains("worktree"))
            .unwrap_or_else(|| panic!("{format}: no worktree row in {text}"));
        assert_eq!(
            first_row.split_whitespace().next(),
            Some("1"),
            "{format}: {text}"
        );
    }

    let json = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
    assert_eq!(json["run"]["duration_ms"], 13_191_607);
    assert_eq!(json["steps"][0]["step_index"], 0);
    assert_eq!(json["steps"][0]["lock_wait_ms"], 42);
    assert_eq!(json["steps"][1]["lock_wait_ms"], 0);
    let table = shown(&fixture, &["--format", "table"]);
    assert!(table.contains("LOCK WAIT (ms)"), "{table}");
    let detail = shown(&fixture, &["--step", "worktree"]);
    assert!(detail.contains("Lock wait: 42ms"), "{detail}");
    let quiet = shown(&fixture, &["--step", "prepare"]);
    assert!(!quiet.contains("Lock wait:"), "{quiet}");

    // These views read the same immutable interrupted run and audit trail.
    let layers = json["catalog_layers"].as_array().unwrap();
    assert!(
        !layers.is_empty() && layers.iter().all(|layer| layer["layer"] == "shipped"),
        "the fixture must resolve only shipped references: {layers:?}"
    );

    let default_human = shown(&fixture, &[]);
    assert!(!default_human.contains("Catalog:"), "{default_human}");
    let verbose = shown(&fixture, &["--verbose"]);
    assert_eq!(
        verbose.matches("Catalog:").count(),
        layers.len(),
        "{verbose}"
    );

    assert_eq!(json["steps_source"], "audit");
    let ids = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["target_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, STEP_IDS);
    // The record keeps the reconciler's step untouched.
    assert_eq!(json["run"]["steps"][0]["error_code"], "worker_terminated");

    let text = default_human;
    let rows = STEP_IDS
        .iter()
        .map(|id| {
            text.lines()
                .find(|line| line.contains(id))
                .unwrap_or_else(|| panic!("no {id} row in {text}"))
                .split_whitespace()
                .next()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(rows, ["1", "2", "3", "4", "5"], "{text}");
}

/// `orbit run show` and MCP `orbit.workflow.run.show`/`list` choose and
/// reconcile an ended run's steps by one rule: the audit trail answers for a
/// run-level-only record, and the step the run died in reads as the run's
/// terminal state with an `interrupted` outcome rather than `running`.
fn every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted() {
    let fixture = Fixture::terminal();
    seed_interrupted_run(&fixture);
    // The MCP run tools are an operator surface.
    let tool = |name: &str, input: serde_json::Value| {
        let output = fixture
            .orbit()
            .env("ORBIT_OPERATOR", "1")
            .args(["tool", "run", name, "--input", &input.to_string()])
            .output()
            .expect("spawn orbit");
        assert!(output.status.success(), "{name}: {output:?}");
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("tool JSON")
    };

    let cli = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
    let shown = tool(
        "orbit.workflow.run.show",
        serde_json::json!({ "id": INTERRUPTED }),
    );
    let listed = tool("orbit.workflow.run.list", serde_json::json!({}));
    let listed = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"] == INTERRUPTED)
        .unwrap_or_else(|| panic!("{INTERRUPTED} absent from {listed}"))
        .clone();
    for (surface, run, steps, source) in [
        ("cli", &cli["run"], &cli["steps"], &cli["steps_source"]),
        ("mcp show", &shown, &shown["steps"], &shown["steps_source"]),
        (
            "mcp list",
            &listed,
            &listed["steps"],
            &listed["steps_source"],
        ),
    ] {
        assert_eq!(source, "audit", "{surface}: {run}");
        let steps = steps.as_array().unwrap();
        let ids = steps
            .iter()
            .map(|step| step["target_id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, STEP_IDS, "{surface}");
        let died_in = steps.last().unwrap();
        assert_eq!(died_in["state"], "interrupted", "{surface}: {died_in}");
        assert_eq!(died_in["outcome"], "interrupted", "{surface}: {died_in}");
        assert_eq!(steps[0]["state"], "success", "{surface}");
        assert_eq!(steps[0]["outcome"], "success", "{surface}");
        assert_eq!(steps[0]["lock_wait_ms"], 42, "{surface}");
        // The run-level step explains the outcome on every surface.
        assert_eq!(run["error_code"], "worker_terminated", "{surface}: {run}");
        assert_eq!(
            run["error_message"], "job run marked interrupted",
            "{surface}: {run}"
        );
    }
    assert_eq!(
        shown["activity_provenance"],
        cli["run"]["activity_provenance"]
    );

    let step = fixture.json(&[
        "run",
        "show",
        INTERRUPTED,
        "--no-reconcile",
        "--step",
        "commit",
        "--json",
    ]);
    assert_eq!(step["step"]["state"], "interrupted", "{step}");
    assert_eq!(step["step"]["outcome"], "interrupted", "{step}");
}

fn run_show_lists_a_final_recovery_resume_with_its_rationale() {
    let fixture = Fixture::terminal();
    seed_interrupted_run(&fixture);
    let rationale = "Set probe.command before returning the deferral record";
    let payload = serde_json::json!({
        "event_id": "final-resume", "body_kind": "final_recovery_attempted",
        "step_id": "implement_bundle", "final_recovery_activity": "final_recovery",
        "outcome": "resume", "decision": "resume", "detail": rationale,
        "ts": "2026-10-08T04:05:00+00:00",
    });
    fixture
        .db()
        .execute(
            "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version,
            event_type, ts, run_id, agent_identity, payload_json)
         VALUES (?1, 'final-resume', 'v2_envelope', 1, 'job.final_recovery_attempted',
            '2026-10-08T04:05:00+00:00', ?2, 'test', ?3)",
            params![fixture.workspace_id(), INTERRUPTED, payload.to_string()],
        )
        .unwrap();
    let json = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
    let attempts = json["recovery_attempts"]["items"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["outcome"], "resume");
    assert_eq!(attempts[0]["recovery_activity"], "final_recovery");
    assert_eq!(attempts[0]["decision"]["verdict"], "resume");
    assert_eq!(attempts[0]["diagnostic"], rationale);
    assert!(shown(&fixture, &[]).contains(rationale));
}

/// [ORB-16330] `orbit run show --json` names the claimed worker the way MCP
/// does, and an unreadable pipeline checkpoint is a warning with
/// `pipeline_state` null rather than a failed command.
fn run_show_keeps_worker_identity_and_degrades_on_an_unreadable_checkpoint() {
    let fixture = Fixture::terminal();
    seed_interrupted_run(&fixture);
    let workspace_id = fixture.workspace_id();
    let input = r#"{"task_id":"ORB-1"}"#;
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET pid = 4242, pid_start_time = 'v1:4242', input_json = ?1
             WHERE run_id = ?2",
            params![input, INTERRUPTED],
        )
        .expect("record worker identity");
    let readable = serde_json::json!({
        "run_id": INTERRUPTED,
        "job_id": "task_pr_pipeline",
        "initial_input": {},
        "pipeline": {"marker": "readable"},
        "updated_at": "2026-10-08T00:26:00+00:00",
    });
    fixture
        .db()
        .execute(
            "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json)
             VALUES (?1, ?2, ?3)",
            params![workspace_id, INTERRUPTED, readable.to_string()],
        )
        .expect("seed readable checkpoint");

    let shown = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
    assert_eq!(shown["run"]["pid"], 4242, "{shown}");
    assert_eq!(shown["run"]["pid_start_time"], "v1:4242", "{shown}");
    assert_eq!(shown["run"]["input"]["task_id"], "ORB-1", "{shown}");
    assert_eq!(
        shown["pipeline_state"]["pipeline"]["marker"], "readable",
        "{shown}"
    );

    fixture
        .db()
        .execute(
            "UPDATE job_run_states SET pipeline_state_json = '{' WHERE run_id = ?1",
            params![INTERRUPTED],
        )
        .expect("corrupt checkpoint");
    let output = fixture
        .orbit()
        .env_remove("RUST_LOG")
        .args(["run", "show", INTERRUPTED, "--no-reconcile", "--json"])
        .output()
        .expect("spawn orbit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "an unreadable checkpoint must not fail run show\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("warning: pipeline checkpoint unreadable"),
        "run show must warn on stderr: {stderr}"
    );
    let degraded: serde_json::Value = serde_json::from_str(&stdout).expect("run show JSON");
    assert!(
        degraded["pipeline_state"].is_null(),
        "unreadable checkpoint reports pipeline_state null: {degraded}"
    );
    assert_eq!(degraded["run"]["pid"], 4242, "{degraded}");
    assert_eq!(degraded["run"]["pid_start_time"], "v1:4242", "{degraded}");
    assert_eq!(degraded["run"]["input"]["task_id"], "ORB-1", "{degraded}");
    assert_eq!(degraded["run"]["run_id"], INTERRUPTED, "{degraded}");
}

/// A skipped step's condition is not the failure a cascaded parent reports.
fn run_show_reports_the_failed_step_before_a_later_skip() {
    const CHILD: &str = "jrun-20261018-skipped-root-cause-child";
    const PARENT: &str = "jrun-20261018-skipped-root-cause-parent";
    let fixture = Fixture::terminal();
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    for (run_id, state) in [(CHILD, "failed"), (PARENT, "failed")] {
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                 input_json, scheduled_at, started_at, finished_at, created_at)
             VALUES (?1, ?2, 'fixture_job', 1, ?3, '{}',
                 '2026-10-18T00:00:00+00:00', '2026-10-18T00:00:01+00:00',
                 '2026-10-18T00:00:02+00:00', '2026-10-18T00:00:00+00:00')",
            params![run_id, workspace_id, state],
        )
        .expect("seed cascade runs");
    }
    for (event_id, step_id, body_kind, details, ts) in [
        (
            "commit-started",
            "commit_task",
            "step_started",
            serde_json::json!({ "step_id": "commit_task" }),
            "2026-10-18T00:00:01+00:00",
        ),
        (
            "commit-finished",
            "commit_task",
            "step_finished",
            serde_json::json!({
                "step_id": "commit_task",
                "outcome": "failed",
                "error_message": "task-commit lock timeout",
            }),
            "2026-10-18T00:00:02+00:00",
        ),
        (
            "ship-started",
            "ship_leaves",
            "step_started",
            serde_json::json!({ "step_id": "ship_leaves" }),
            "2026-10-18T00:00:03+00:00",
        ),
        (
            "ship-skipped",
            "ship_leaves",
            "step_skipped",
            serde_json::json!({
                "step_id": "ship_leaves",
                "reason": "ship_leaves => false",
            }),
            "2026-10-18T00:00:04+00:00",
        ),
    ] {
        let mut payload = serde_json::json!({
            "event_id": format!("{CHILD}-{event_id}"),
            "body_kind": body_kind,
            "ts": ts,
            "step_id": step_id,
        });
        if let Some(details) = details.as_object() {
            payload
                .as_object_mut()
                .expect("audit payload object")
                .extend(details.clone());
        }
        db.execute(
            "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version,
                 event_type, ts, run_id, agent_identity, payload_json)
             VALUES (?1, ?2, 'v2_envelope', 1, 'activity.progress', ?3, ?4, 'test', ?5)",
            params![
                workspace_id,
                format!("{CHILD}-{event_id}"),
                ts,
                CHILD,
                payload.to_string()
            ],
        )
        .expect("seed cascade audit event");
    }

    let runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.home.join(".orbit"),
        &fixture.work.join(".orbit"),
    )
    .expect("open runtime");
    runtime
        .write_run_state(
            PARENT,
            &orbit_types::workflow::PipelineState::new(
                PARENT.to_string(),
                "fixture_job".to_string(),
                serde_json::json!({}),
            ),
        )
        .expect("seed parent state");
    let failure = orbit_engine::RuntimeHost::run_deterministic(
        &runtime,
        "pipeline_success_guard",
        &serde_json::json!({}),
        &serde_json::json!({
            "run_id": PARENT,
            "context": "fixture child run",
            "results": [{
                "status": "failed",
                "run_id": CHILD,
                "error": "fixture child failed",
            }],
        }),
        orbit_tools::ToolContext::default(),
    )
    .expect_err("the failed child makes its parent fail");
    assert!(
        failure
            .to_string()
            .contains("fixture child run did not succeed")
    );

    let json = fixture.json(&["run", "show", PARENT, "--no-reconcile", "--json"]);
    assert_eq!(json["run"]["root_cause"]["leaf_run_id"], CHILD, "{json}");
    assert_eq!(json["run"]["root_cause"]["step"], "commit_task", "{json}");
}

#[test]
fn run_show_contracts() {
    if !isolated_run_observation("run_observation::run_show_display::run_show_contracts") {
        return;
    }
    for (name, check) in [
        (
            "run_show_prints_human_durations_and_one_based_steps_in_every_human_mode",
            run_show_prints_human_durations_and_one_based_steps_in_every_human_mode as fn(),
        ),
        (
            "every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted",
            every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted as fn(),
        ),
        (
            "run_show_lists_a_final_recovery_resume_with_its_rationale",
            run_show_lists_a_final_recovery_resume_with_its_rationale as fn(),
        ),
        (
            "run_show_keeps_worker_identity_and_degrades_on_an_unreadable_checkpoint",
            run_show_keeps_worker_identity_and_degrades_on_an_unreadable_checkpoint as fn(),
        ),
        (
            "run_show_reports_the_failed_step_before_a_later_skip",
            run_show_reports_the_failed_step_before_a_later_skip as fn(),
        ),
    ] {
        tracing::info!(case = name, "CLI contract row");
        check();
    }
}
