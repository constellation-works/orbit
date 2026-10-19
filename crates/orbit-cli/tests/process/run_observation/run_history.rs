//! History filters reach SQL before the page cap; task identity and timing
//! are visible through the built CLI without scraping persisted input.
use super::*;
use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use serde_json::json;

#[test]
fn history_filters_before_limit_and_shows_tasks_and_duration() {
    if !isolated_run_observation(
        "run_observation::run_history::history_filters_before_limit_and_shows_tasks_and_duration",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "History delivery fixture".into(),
            description: "Identify the task in its delivery run.".into(),
            ..Default::default()
        })
        .unwrap();
    let task_id = task.id.to_string();
    let now = Utc::now();
    let db = fixture.db();
    for (id, state, hours, input, duration) in [
        (
            "jrun-history-old",
            "failed",
            30,
            json!({"task_ids":[task_id]}),
            Some(1200),
        ),
        (
            "jrun-history-failed",
            "failed",
            3,
            json!({"task_ids":[task_id, task_id]}),
            Some(13191607),
        ),
        (
            "jrun-history-held",
            "held",
            2,
            json!({"task_ids":[task_id]}),
            Some(250),
        ),
        (
            "jrun-history-success",
            "success",
            1,
            json!({"task_ids":[task_id]}),
            Some(2000),
        ),
        ("jrun-history-unrelated", "failed", 0, json!({}), None),
    ] {
        let at = (now - Duration::hours(hours)).to_rfc3339();
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                scheduled_at, started_at, finished_at, duration_ms, created_at, input_json)
             VALUES (?1, ?2, 'task_pr_pipeline', 1, ?3, ?4, ?4, ?4, ?5, ?4, ?6)",
            params![
                id,
                fixture.workspace_id(),
                state,
                at,
                duration,
                input.to_string()
            ],
        )
        .unwrap();
    }
    let history = |filters: &[&str]| {
        let mut args = vec!["run", "history", "--no-reconcile", "--json", "--limit", "1"];
        args.extend_from_slice(filters);
        fixture.json(&args)["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["run_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    for (filters, expected) in [
        (vec!["--task", &task_id], "jrun-history-success"),
        (vec!["--state", "failed,held"], "jrun-history-unrelated"),
        (vec!["--since", "24h"], "jrun-history-unrelated"),
        (
            vec![
                "--task",
                &task_id,
                "--state",
                "failed,held",
                "--since",
                "24h",
            ],
            "jrun-history-held",
        ),
        (
            vec![
                "--task",
                &task_id,
                "--state",
                "failed",
                "--since",
                "24h",
                "--job",
                "task_pr_pipeline",
            ],
            "jrun-history-failed",
        ),
    ] {
        assert_eq!(history(&filters), [expected], "{filters:?}");
    }
    assert_eq!(history(&["--state", "held"]), ["jrun-history-held"]);
    let future = (now + Duration::hours(1)).to_rfc3339();
    assert!(history(&["--since", &future]).is_empty());
    let filtered = fixture.json(&[
        "run",
        "history",
        "--no-reconcile",
        "--json",
        "--task",
        &task_id,
        "--state",
        "failed,held",
        "--since",
        "24h",
    ]);
    assert_eq!(filtered["runs"].as_array().unwrap().len(), 2);
    for run in filtered["runs"].as_array().unwrap() {
        assert_eq!(run["task_ids"], json!([task_id]));
    }
    assert_eq!(history(&["--task", "missing-task"]), Vec::<String>::new());
    for filters in [
        vec!["--state", "unknown"],
        vec!["--state", "failed,"],
        vec!["--since", "never"],
        vec!["--limit", "0"],
    ] {
        let mut command = fixture.orbit();
        command
            .args(["run", "history", "--no-reconcile"])
            .args(filters)
            .assert()
            .failure();
    }
    let output = fixture
        .orbit()
        .args(["run", "history", "--no-reconcile", "--format", "table"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let header = text.lines().next().unwrap();
    assert!(
        header.contains("TASK") && header.contains("DURATION"),
        "{text}"
    );
    assert!(
        !header.contains("ERROR_CODE") && !header.contains("ERROR_MESSAGE"),
        "{text}"
    );
    let row = text
        .lines()
        .find(|line| line.starts_with("jrun-history-failed "))
        .unwrap();
    assert!(
        row.contains(&task_id) && row.ends_with("3h 39m 51s"),
        "{text}"
    );
    let task_start = header.find("TASK").unwrap();
    let task_end = header.find("ATTEMPT").unwrap();
    let no_task = text
        .lines()
        .find(|line| line.starts_with("jrun-history-unrelated "))
        .unwrap();
    assert!(
        no_task.get(task_start..task_end).unwrap().trim().is_empty(),
        "{text}"
    );
    assert!(
        no_task
            .get(header.find("DURATION").unwrap()..)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{text}"
    );
    // Plain output labels its timestamp and duration columns only on request,
    // with the same columns the table shows.
    let plain = |extra: &[&str]| {
        let output = fixture
            .orbit()
            .args(["run", "history", "--no-reconcile", "--format", "plain"])
            .args(extra)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let bare = plain(&[]);
    let headed = plain(&["--header"]);
    let (plain_header, rows) = headed.split_once('\n').unwrap();
    assert_eq!(
        plain_header.split('\t').collect::<Vec<_>>(),
        header.split_whitespace().collect::<Vec<_>>(),
        "{headed}"
    );
    assert_eq!(rows, bare, "--header only prepends the header row");
    assert!(!bare.starts_with("RUN_ID"), "{bare}");
    let no_task_json =
        fixture.json(&["run", "history", "--no-reconcile", "--json", "--limit", "1"]);
    assert!(no_task_json["runs"][0]["task_ids"].is_null());
    let show = fixture
        .orbit()
        .args(["run", "show", "jrun-history-failed", "--no-reconcile"])
        .output()
        .unwrap();
    assert!(show.status.success(), "{show:?}");
    let text = String::from_utf8(show.stdout).unwrap();
    assert!(
        text.contains(&format!("Task: {task_id} — {}", task.title)),
        "{text}"
    );
    assert_eq!(
        fixture.json(&[
            "run",
            "show",
            "jrun-history-failed",
            "--no-reconcile",
            "--json"
        ])["run"]["task_ids"],
        json!([task_id])
    );
}

#[test]
fn history_error_column_names_failed_runs_at_the_end_capped() {
    if !isolated_run_observation(
        "run_observation::run_history::history_error_column_names_failed_runs_at_the_end_capped",
    ) {
        return;
    }
    use orbit_types::workflow::{PipelineState, RunRootCause};

    let fixture = Fixture::init();
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    let at = (Utc::now() - Duration::hours(1)).to_rfc3339();
    // A child run that failed on its own, a parent that echoes it, and a held
    // run with nothing recorded, next to a success.
    for (id, state) in [
        ("jrun-error-leaf", "failed"),
        ("jrun-error-echo", "failed"),
        ("jrun-error-held", "held"),
        ("jrun-error-success", "success"),
    ] {
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                scheduled_at, started_at, finished_at, duration_ms, created_at)
             VALUES (?1, ?2, 'task_pr_pipeline', 1, ?3, ?4, ?4, ?4, 1200, ?4)",
            params![id, workspace_id, state, at],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, target_id,
            state, started_at, finished_at, duration_ms, exit_code, error_code, error_message)
         VALUES (?1, 'jrun-error-leaf', 0, 'activity', 'implement', 'failed', ?2, ?2, 1200, 1,
            'provider_limit',
            'provider quota exhausted\nthe claimed leaf stopped before delivery and returns to its owner')",
        params![workspace_id, at],
    )
    .unwrap();
    let echo = PipelineState {
        root_cause: Some(RunRootCause {
            leaf_run_id: "jrun-error-leaf".into(),
            task_id: None,
            step: Some("implement".into()),
            code: Some("provider_limit".into()),
        }),
        ..PipelineState::new(
            "jrun-error-echo".into(),
            "task_pr_pipeline".into(),
            json!({}),
        )
    };
    db.execute(
        "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json)
         VALUES (?1, 'jrun-error-echo', ?2)",
        params![workspace_id, serde_json::to_string(&echo).unwrap()],
    )
    .unwrap();

    let output = fixture
        .orbit()
        .args(["run", "history", "--no-reconcile", "--format", "table"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let header = text.lines().next().unwrap();
    assert!(header.trim_end().ends_with("DURATION  ERROR"), "{text}");
    let error_start = header.find("ERROR").unwrap();
    let row = |id: &str| {
        text.lines()
            .find(|line| line.starts_with(id))
            .unwrap_or_else(|| panic!("no row for {id} in {text}"))
    };
    assert_eq!(
        row("jrun-error-leaf").get(error_start..).unwrap().trim(),
        "provider_limit: provider quota exhausted the claimed leaf..."
    );
    assert_eq!(
        row("jrun-error-echo").get(error_start..).unwrap().trim(),
        "root cause jrun-error-leaf at implement (provider_limit)"
    );
    for id in ["jrun-error-held", "jrun-error-success"] {
        assert!(
            row(id)
                .get(error_start..)
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "{id} has no recorded error: {text}"
        );
    }

    // Success-only listings keep the original columns.
    let successes = fixture
        .orbit()
        .args([
            "run",
            "history",
            "--no-reconcile",
            "--format",
            "table",
            "--state",
            "success",
        ])
        .output()
        .unwrap();
    assert!(successes.status.success(), "{successes:?}");
    let text = String::from_utf8(successes.stdout).unwrap();
    assert!(!text.lines().next().unwrap().contains("ERROR"), "{text}");
}

/// Three echoing runs of one task are one `--failures` row, named by the
/// root-cause run, with that run's first error line uncut. A second task's
/// own failure stays a second row. The root run itself is outside `--since`,
/// so the row has to load it rather than repeat the echo.
#[test]
fn history_failures_reports_one_row_per_task_and_root_cause() {
    if !isolated_run_observation(
        "run_observation::run_history::history_failures_reports_one_row_per_task_and_root_cause",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_types::workflow::{PipelineState, RunRootCause};

    let fixture = Fixture::terminal();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let task = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "History failure fixture.".into(),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let echoed = task("echoed failure");
    let other = task("separate failure");
    let workspace_id = fixture.workspace_id();
    let db = fixture.db();
    let at = |hours: i64| (Utc::now() - Duration::hours(hours)).to_rfc3339();
    let insert = |id: &str, state: &str, hours: i64, task_id: &str, crew: Option<&str>| {
        db.execute(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state,
                scheduled_at, started_at, finished_at, created_at, input_json, resolved_crew)
             VALUES (?1, ?2, 'task_pr_pipeline', 1, ?3, ?4, ?4, ?4, ?4, ?5, ?6)",
            params![
                id,
                workspace_id,
                state,
                at(hours),
                json!({"task_ids":[task_id]}).to_string(),
                crew,
            ],
        )
        .unwrap();
    };
    const ROOT: &str = "jrun-fail-root";
    const OTHER: &str = "jrun-fail-other";
    // Outside the 24h window, so a row that only echoes it must load this run.
    insert(ROOT, "failed", 48, &echoed, Some("grok"));
    insert(OTHER, "failed", 1, &other, Some("sol"));
    insert("jrun-fail-echo-c4", "failed", 2, &echoed, None);
    insert("jrun-fail-echo-held", "held", 3, &echoed, None);
    insert("jrun-fail-echo-c11", "failed", 4, &echoed, None);
    insert("jrun-fail-success", "success", 5, &echoed, None);
    let root_message = "provider quota exhausted before the claimed leaf could finish its delivery\nsecond line must not appear in the failures row";
    let other_message = "validation failed because the candidate build exceeded the elapsed budget by too many seconds\ndo not join this second line";
    for (id, target, code, message) in [
        (ROOT, "implement", "provider_limit", root_message),
        (OTHER, "validate", "validation", other_message),
    ] {
        db.execute(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type, target_id,
                state, started_at, finished_at, error_code, error_message)
             VALUES (?1, ?2, 0, 'activity', ?3, 'failed', ?4, ?4, ?5, ?6)",
            params![workspace_id, id, target, at(1), code, message],
        )
        .unwrap();
    }
    for id in [
        "jrun-fail-echo-c4",
        "jrun-fail-echo-held",
        "jrun-fail-echo-c11",
    ] {
        let state = PipelineState {
            root_cause: Some(RunRootCause {
                leaf_run_id: ROOT.into(),
                task_id: Some(echoed.clone()),
                step: Some("implement".into()),
                code: Some("provider_limit".into()),
            }),
            ..PipelineState::new(id.into(), "task_pr_pipeline".into(), json!({}))
        };
        db.execute(
            "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json)
             VALUES (?1, ?2, ?3)",
            params![workspace_id, id, serde_json::to_string(&state).unwrap()],
        )
        .unwrap();
    }

    let root_cause = format!("provider_limit: {}", root_message.lines().next().unwrap());
    let other_cause = format!("validation: {}", other_message.lines().next().unwrap());
    assert!(
        root_cause.chars().count() > 60 && other_cause.chars().count() > 60,
        "the fixture cause must exceed the ordinary ERROR cap"
    );

    let doc = fixture.json(&[
        "run",
        "history",
        "--no-reconcile",
        "--json",
        "--since",
        "24h",
        "--failures",
    ]);
    let rows = doc["failures"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{doc}");
    assert_eq!(
        rows[0],
        json!({
            "task_id": other,
            "run_id": OTHER,
            "step": "validate",
            "crew": "sol",
            "cause": other_cause,
            "show": format!("orbit run show {OTHER}"),
        }),
        "{doc}"
    );
    assert_eq!(
        rows[1],
        json!({
            "task_id": echoed,
            "run_id": ROOT,
            "step": "implement",
            "crew": "grok",
            "cause": root_cause,
            "show": format!("orbit run show {ROOT}"),
        }),
        "{doc}"
    );

    let output = fixture
        .orbit()
        .args([
            "run",
            "history",
            "--no-reconcile",
            "--format",
            "table",
            "--since",
            "24h",
            "--failures",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let header = text.lines().next().unwrap();
    assert_eq!(
        header.split_whitespace().collect::<Vec<_>>(),
        ["TASK", "RUN_ID", "STEP", "CREW", "CAUSE", "SHOW"],
        "{text}"
    );
    assert!(text.contains(&root_cause), "{text}");
    assert!(text.contains(&other_cause), "{text}");
    assert!(!text.contains("second line"), "{text}");
    assert!(!text.contains("do not join"), "{text}");
    assert!(!text.contains("..."), "{text}");
    for echo in [
        "jrun-fail-echo-c4",
        "jrun-fail-echo-held",
        "jrun-fail-echo-c11",
    ] {
        assert!(!text.contains(echo), "{text}");
    }
    assert!(text.contains(ROOT) && text.contains(OTHER), "{text}");

    let empty = fixture.json(&[
        "run",
        "history",
        "--no-reconcile",
        "--json",
        "--since",
        &(Utc::now() + Duration::hours(1)).to_rfc3339(),
        "--failures",
    ]);
    assert_eq!(empty["failures"], json!([]), "{empty}");
    fixture
        .orbit()
        .args(["run", "history", "--failures", "--incidents"])
        .assert()
        .failure();
}
