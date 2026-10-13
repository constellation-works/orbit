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

#[test]
fn run_show_prints_human_durations_and_one_based_steps_in_every_human_mode() {
    if !isolated_run_observation(
        "run_observation::run_show_display::run_show_prints_human_durations_and_one_based_steps_in_every_human_mode",
    ) {
        return;
    }
    let fixture = Fixture::init();
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
}

#[test]
fn run_show_hides_shipped_catalog_layers_unless_verbose() {
    if !isolated_run_observation(
        "run_observation::run_show_display::run_show_hides_shipped_catalog_layers_unless_verbose",
    ) {
        return;
    }
    let fixture = Fixture::init();
    seed_interrupted_run(&fixture);
    let json = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
    let layers = json["catalog_layers"].as_array().unwrap();
    assert!(
        !layers.is_empty() && layers.iter().all(|layer| layer["layer"] == "shipped"),
        "the fixture must resolve only shipped references: {layers:?}"
    );

    let quiet = shown(&fixture, &[]);
    assert!(!quiet.contains("Catalog:"), "{quiet}");
    let verbose = shown(&fixture, &["--verbose"]);
    assert_eq!(
        verbose.matches("Catalog:").count(),
        layers.len(),
        "{verbose}"
    );
}

#[test]
fn run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record() {
    if !isolated_run_observation(
        "run_observation::run_show_display::run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record",
    ) {
        return;
    }
    let fixture = Fixture::init();
    seed_interrupted_run(&fixture);

    let json = fixture.json(&["run", "show", INTERRUPTED, "--no-reconcile", "--json"]);
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

    let text = shown(&fixture, &[]);
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
#[test]
fn every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted() {
    if !isolated_run_observation(
        "run_observation::run_show_display::every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted",
    ) {
        return;
    }
    let fixture = Fixture::init();
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

#[test]
fn run_show_lists_a_final_recovery_resume_with_its_rationale() {
    let fixture = Fixture::init();
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
