//! Task delivery selection through the composed runtime, with a bounded,
//! opt-in Linux measurement of the same public operation.

use chrono::{Duration, TimeZone, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    task: String,
    other_task: String,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let jobs = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        let mut git = std::process::Command::new("git");
        orbit_common::test_env::clear_inherited_authority(|key| {
            git.env_remove(key);
        });
        assert!(
            git.args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        for (name, delivery) in [
            ("fixture_delivery", "  task_delivery: {}\n"),
            ("fixture_check", ""),
        ] {
            std::fs::write(jobs.join(format!("{name}.yaml")), format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {name}\nspec:\n  state: enabled\n{delivery}  steps: []\n"
            )).unwrap();
        }
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let task = |title: &str| {
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "Delivery lookup fixture.".into(),
                    acceptance_criteria: vec!["Observe the matching delivery run.".into()],
                    plan: "Read delivery history.".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string()
        };
        let selected = task("Selected task");
        let other_task = task("Other task");
        Self {
            _root: root,
            runtime,
            task: selected,
            other_task,
        }
    }

    fn run(&self, name: &str, job: &str, time: i64, input: Option<Value>) -> JobRun {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::seconds(time);
        let mut run = self
            .runtime
            .insert_job_run(job, 1, at, input, None)
            .unwrap();
        let original_id = run.run_id.clone();
        // Explicit creation times make ordering deterministic without sleeps.
        run.run_id = name.into();
        run.created_at = at;
        let store = self.runtime.sqlite_store().unwrap();
        let workspace = self.runtime.workspace_id().unwrap();
        // The inserted template is not part of the fixture's history.
        store
            .delete_job_run_for_workspace(&workspace, &original_id)
            .unwrap();
        store
            .upsert_job_run_for_workspace(&workspace, &run, None)
            .unwrap();
        run
    }

    fn selected(&self) -> String {
        self.runtime
            .observe_task_delivery(&self.task, None)
            .unwrap()
            .run_id
    }
}

#[test]
fn newest_delivery_uses_submitted_task_bindings() {
    if !super::dispatch_admission::isolated(
        "task_delivery::newest_delivery_uses_submitted_task_bindings",
    ) {
        return;
    }
    let fixture = Fixture::new();
    assert!(matches!(
        fixture.runtime.observe_task_delivery(&fixture.task, None),
        Err(OrbitError::InvalidInput(_))
    ));
    fixture.run(
        "older",
        "fixture_delivery",
        1,
        Some(json!({"task_ids": [fixture.task]})),
    );
    // Multi-task submissions and duplicate bindings must behave like membership.
    fixture.run(
        "newest",
        "fixture_delivery",
        2,
        Some(json!({"task_ids": [fixture.other_task, fixture.task, fixture.task, 7]})),
    );
    fixture.run(
        "other",
        "fixture_delivery",
        3,
        Some(json!({"task_ids": [fixture.other_task]})),
    );
    fixture.run(
        "check",
        "fixture_check",
        4,
        Some(json!({"task_ids": [fixture.task]})),
    );
    fixture.run(
        "missing-job",
        "missing_job",
        5,
        Some(json!({"task_ids": [fixture.task]})),
    );
    // Creation time, not completion activity, defines "newest". Equal times
    // keep the store's deterministic run-id ordering.
    let mut tie = fixture.run(
        "z-tied",
        "fixture_delivery",
        2,
        Some(json!({"task_ids": [fixture.task]})),
    );
    tie.finished_at = Some(tie.created_at + Duration::days(1));
    let store = fixture.runtime.sqlite_store().unwrap();
    store
        .upsert_job_run_for_workspace(&fixture.runtime.workspace_id().unwrap(), &tie, None)
        .unwrap();
    tie.run_id = "foreign".into();
    tie.created_at += Duration::days(2);
    store
        .upsert_job_run_for_workspace("foreign-workspace", &tie, None)
        .unwrap();
    // None of these shapes is a submitted task_ids array.
    for (index, input) in [
        None,
        Some(json!({})),
        Some(json!({"task_ids": fixture.task})),
        Some(json!({"task_ids": {"key": fixture.task}})),
        Some(json!({"nested": {"task_ids": [fixture.task]}})),
        Some(json!({"task_ids": [format!("{}-suffix", fixture.task)]})),
    ]
    .into_iter()
    .enumerate()
    {
        fixture.run(
            &format!("unbound-{index}"),
            "fixture_delivery",
            6 + index as i64,
            input,
        );
    }
    assert_eq!(fixture.selected(), "newest");
    assert_eq!(
        fixture
            .runtime
            .observe_task_delivery(&fixture.other_task, None)
            .unwrap()
            .run_id,
        "other"
    );
    assert_eq!(
        fixture
            .runtime
            .observe_task_delivery(&fixture.task, Some("older"))
            .unwrap()
            .run_id,
        "older"
    );
    assert!(
        fixture
            .runtime
            .observe_task_delivery(&fixture.task, Some("other"))
            .is_err()
    );

    // An unrelated row that cannot be hydrated proves selection does not
    // deserialize every workspace run before applying task membership.
    let store = fixture.runtime.sqlite_store().unwrap();
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET knowledge_metrics_json = '{' WHERE run_id = 'other'",
                    [],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(fixture.selected(), "newest");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "bounded Linux measurement; run explicitly with --ignored --nocapture"]
fn measure_delivery_lookup() {
    if !super::dispatch_admission::isolated_ignored("task_delivery::measure_delivery_lookup") {
        return;
    }
    let fixture = Fixture::new();
    // Keep this fixture and profile identical between baseline and candidate.
    const RUNS: usize = 4096;
    const SAMPLES: usize = 31;
    let payload = "x".repeat(384);
    let store = fixture.runtime.sqlite_store().unwrap();
    let workspace = fixture.runtime.workspace_id().unwrap();
    let mut template = fixture
        .runtime
        .insert_job_run("fixture_delivery", 1, Utc::now(), None, None)
        .unwrap();
    store
        .delete_job_run_for_workspace(&workspace, &template.run_id)
        .unwrap();
    let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    for index in 0..RUNS {
        template.run_id = format!("fixture-{index:05}");
        template.created_at = start + Duration::seconds(index as i64);
        template.scheduled_at = template.created_at;
        template.job_id = if index == 2 {
            "fixture_check"
        } else {
            "fixture_delivery"
        }
        .into();
        template.input = Some(json!({
            "task_ids": [if index < 3 { &fixture.task } else { &fixture.other_task }],
            "payload": payload,
        }));
        store
            .upsert_job_run_for_workspace(&workspace, &template, None)
            .unwrap();
    }
    // Measure the serialized bytes actually persisted in this fixture.
    let (all_runs, all_bytes, matching_runs, matching_bytes): (usize, usize, usize, usize) = store.with_read_connection(|conn| {
        let all = conn.query_row(
            "SELECT COUNT(*), SUM(length(CAST(input_json AS BLOB))) FROM job_runs WHERE workspace_id = ?1",
            [&workspace], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        let matching = conn.query_row(
            "SELECT COUNT(*), SUM(length(CAST(input_json AS BLOB))) FROM job_runs \
             WHERE workspace_id = ?1 AND json_type(input_json, '$.task_ids') = 'array' \
             AND EXISTS (SELECT 1 FROM json_each(input_json, '$.task_ids') AS binding \
                         WHERE binding.type = 'text' AND binding.value = ?2)",
            [&workspace, &fixture.task], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        Ok((all.0, all.1, matching.0, matching.1))
    }).unwrap();
    assert_eq!(all_runs, RUNS);
    assert_eq!(matching_runs, 3);
    for _ in 0..5 {
        assert_eq!(fixture.selected(), "fixture-00001");
    }
    let mut latency_us = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        assert_eq!(fixture.selected(), "fixture-00001");
        latency_us.push(started.elapsed().as_micros());
    }
    latency_us.sort_unstable();
    // This is serialized input volume, not allocator/RSS usage, with hydration
    // independently guarded by newest_delivery_uses_submitted_task_bindings.
    let report = json!({
        "arm": option_env!("ORBIT_DELIVERY_MEASUREMENT_ARM").unwrap_or("candidate"),
        "source_root": env!("CARGO_MANIFEST_DIR"),
        "profile": "Cargo test (dev defaults)", "fixture_runs": RUNS,
        "matching_runs": matching_runs, "all_input_json_bytes": all_bytes,
        "matching_input_json_bytes": matching_bytes,
        "samples": SAMPLES, "warmups": 5, "latency_us": latency_us,
        "median_us": latency_us[SAMPLES / 2], "p95_us": latency_us[SAMPLES * 95 / 100],
        "selected_run": "fixture-00001",
    });
    use std::io::Write;
    writeln!(std::io::stdout(), "DELIVERY_MEASUREMENT {report}").unwrap();
}
