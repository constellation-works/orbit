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
