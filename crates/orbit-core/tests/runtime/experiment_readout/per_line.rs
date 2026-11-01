//! Per-line metrics are a ratio of sums, with zero-line and unavailable
//! rows counted apart, and a lines_changed breach withholds the winner.

use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_store::contracts::{
    DiffClassCounts, ExperimentAssignment, ExperimentStoreBackend, LandedDiffSource,
    LandedDiffStats, LandedDiffStoreBackend, ReviewableLines,
};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use serde_json::json;

fn open_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[experiments]\nenabled = true\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    (root, runtime)
}

fn actor() -> ExperimentActor {
    ExperimentActor {
        actor: "per-line-test".into(),
        reason: None,
    }
}

fn arms() -> serde_json::Value {
    json!([
        { "name": "control", "control": true, "weight": 1 },
        { "name": "treatment", "weight": 1 }
    ])
}

fn enroll(runtime: &OrbitRuntime, experiment_id: &str, arm_name: &str, done: bool) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("per-line {arm_name}"),
            complexity: TaskComplexity::Low,
            task_type: Some(TaskType::Chore),
            status: Some(if done {
                TaskStatus::Done
            } else {
                TaskStatus::Backlog
            }),
            ..TaskAddParams::default()
        })
        .unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let store = runtime.sqlite_store().unwrap();
    store
        .assign_experiments(&workspace_id, &task.id, true, true, &|records, _, _| {
            let record = records
                .iter()
                .find(|record| record.experiment.id == experiment_id)
                .unwrap();
            let arm = record.arms.iter().find(|arm| arm.name == arm_name).unwrap();
            Ok(vec![ExperimentAssignment {
                experiment: experiment_id.into(),
                task_id: task.id.to_string(),
                arm: Some(arm_name.into()),
                segment: Some(1),
                digest: Some(arm.overlay_digest.clone()),
                assigned_at: Utc::now(),
                forced: false,
                exclusion_reason: None,
            }])
        })
        .unwrap();
    // A done task with no implementation run is interactive and left out of
    // every readout. A pipeline run that names the task keeps it managed.
    if done {
        runtime
            .stores()
            .jobs()
            .insert_job_run(
                "task_local_pipeline",
                1,
                Utc::now(),
                Some(json!({ "task_ids": [task.id.to_string()] })),
                None,
            )
            .unwrap();
    }
    task.id.to_string()
}

fn done_at(runtime: &OrbitRuntime, task_id: &str) -> DateTime<Utc> {
    runtime
        .get_task_history(task_id)
        .unwrap()
        .iter()
        .rev()
        .find(|entry: &&TaskHistoryEntry| entry.to_status == Some(TaskStatus::Done))
        .unwrap_or_else(|| panic!("{task_id} has no done history"))
        .at
}

fn backdate_created_at(global: &Path, task_id: &str, done_at: DateTime<Utc>, hours: i64) {
    let created = done_at - Duration::hours(hours);
    let stamp = created.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let mut stack = vec![global.join("tasks/workspaces")];
    let mut yaml = None;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) == Some(task_id) {
                let candidate = path.join("task.yaml");
                if candidate.is_file() {
                    yaml = Some(candidate);
                }
            }
            stack.push(path);
        }
    }
    let yaml = yaml.unwrap_or_else(|| panic!("no bundle for {task_id}"));
    let text = std::fs::read_to_string(&yaml).unwrap();
    let mut replaced = false;
    let next = text
        .lines()
        .map(|line| {
            if !replaced && line.trim_start().starts_with("created_at:") {
                replaced = true;
                let indent = line.len() - line.trim_start().len();
                format!("{:width$}created_at: {stamp}", "", width = indent)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(replaced, "created_at missing from {yaml:?}: {text}");
    std::fs::write(yaml, next).unwrap();
}

fn cost(runtime: &OrbitRuntime, task_id: &str, usd: f64) {
    runtime
        .insert_invocation_trace_record(&orbit_store::contracts::InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: format!("fixture-{task_id}"),
            account: None,
            activity_id: "agent_implement".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![task_id.to_string()],
            trace: InvocationTrace {
                usage: TokenUsage::default(),
                provider_cost_usd: Some(usd),
                ..InvocationTrace::default()
            },
        })
        .unwrap();
}

fn lines(
    runtime: &OrbitRuntime,
    task_id: &str,
    seq: u64,
    class_added: u64,
    reviewable: Option<ReviewableLines>,
) {
    runtime
        .sqlite_store()
        .unwrap()
        .record_landed_diff_stats(&LandedDiffStats {
            workspace_id: runtime.workspace_id().unwrap(),
            task_id: task_id.to_string(),
            base_commit: format!("{:040x}", seq * 2),
            landed_commit: format!("{:040x}", seq * 2 + 1),
            landed_at: Utc::now(),
            recorded_at: Utc::now(),
            job_run_id: None,
            source: LandedDiffSource::Landing,
            crew: None,
            model: None,
            complexity: None,
            task_type: None,
            code: DiffClassCounts {
                added: class_added,
                deleted: 0,
                files: 1,
            },
            tests: DiffClassCounts::default(),
            other: DiffClassCounts::default(),
            reviewable,
        })
        .unwrap();
}

struct Measured {
    cost: f64,
    lines: u64,
    lead_hours: i64,
}

fn measured(
    runtime: &OrbitRuntime,
    global: &Path,
    experiment: &str,
    arm: &str,
    sample: Measured,
    seq: u64,
) -> String {
    let task_id = enroll(runtime, experiment, arm, true);
    let landed = done_at(runtime, &task_id);
    backdate_created_at(global, &task_id, landed, sample.lead_hours);
    cost(runtime, &task_id, sample.cost);
    lines(
        runtime,
        &task_id,
        seq,
        sample.lines,
        Some(ReviewableLines {
            added: sample.lines,
            deleted: 0,
        }),
    );
    task_id
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-9
}

/// Mixed task sizes, one zero-line run and one unavailable historical row.
/// The arm value is the hand-computed ratio of sums, not the mean of ratios.
#[test]
fn ratio_of_sums_excludes_zero_and_unavailable_lines() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::per_line::ratio_of_sums_excludes_zero_and_unavailable_lines",
    ) {
        return;
    }
    let (root, runtime) = open_runtime();
    let global = root.path().join("home/.orbit");
    let experiment = runtime
        .create_experiment(
            "per-line-ratio",
            ExperimentDefinitionInput {
                arms: Some(arms()),
                primary_metric: Some("cost_per_line".into()),
                guardrails: Some(json!({"lines_changed": "+0"})),
                min_sample: Some(2),
                ..ExperimentDefinitionInput::default()
            },
            actor(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor())
        .unwrap();

    // Control: 10/100 and 30/50. Ratio of sums is 40/150, mean of ratios is 0.35.
    measured(
        &runtime,
        &global,
        &experiment.id,
        "control",
        Measured {
            cost: 10.0,
            lines: 100,
            lead_hours: 10,
        },
        1,
    );
    measured(
        &runtime,
        &global,
        &experiment.id,
        "control",
        Measured {
            cost: 30.0,
            lines: 50,
            lead_hours: 20,
        },
        2,
    );
    // Treatment: 2/10 and 9/30. Ratio of sums is 11/40, mean of ratios is 0.25.
    measured(
        &runtime,
        &global,
        &experiment.id,
        "treatment",
        Measured {
            cost: 2.0,
            lines: 10,
            lead_hours: 4,
        },
        3,
    );
    measured(
        &runtime,
        &global,
        &experiment.id,
        "treatment",
        Measured {
            cost: 9.0,
            lines: 30,
            lead_hours: 6,
        },
        4,
    );
    let zero = enroll(&runtime, &experiment.id, "control", true);
    lines(
        &runtime,
        &zero,
        5,
        0,
        Some(ReviewableLines {
            added: 0,
            deleted: 0,
        }),
    );
    let historical = enroll(&runtime, &experiment.id, "control", true);
    lines(&runtime, &historical, 6, 5, None);
    let _open = enroll(&runtime, &experiment.id, "control", false);

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let metric = |name: &str| {
        readout
            .metrics
            .iter()
            .find(|metric| metric.metric == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let arm = |name: &str, metric_name: &str| {
        metric(metric_name)
            .arms
            .iter()
            .find(|arm| arm.arm == name)
            .unwrap()
    };

    let control_cost = arm("control", "cost_per_line");
    let treatment_cost = arm("treatment", "cost_per_line");
    assert_eq!(metric("cost_per_line").kind, "ratio");
    assert_eq!(control_cost.estimate.n, 2);
    assert_eq!(treatment_cost.estimate.n, 2);
    assert!(close(control_cost.estimate.mean.unwrap(), 40.0 / 150.0));
    assert!(close(treatment_cost.estimate.mean.unwrap(), 11.0 / 40.0));
    assert!(
        (control_cost.estimate.mean.unwrap() - 0.35).abs() > 1e-6,
        "equal line counts would hide a mean of ratios"
    );
    let control_lead = arm("control", "lead_time_per_line");
    let treatment_lead = arm("treatment", "lead_time_per_line");
    assert!(close(control_lead.estimate.mean.unwrap(), 30.0 / 150.0));
    assert!(close(treatment_lead.estimate.mean.unwrap(), 10.0 / 40.0));
    assert!((control_lead.estimate.mean.unwrap() - 0.25).abs() > 1e-6);
    for estimate in [
        &control_cost.estimate,
        &treatment_cost.estimate,
        &control_lead.estimate,
        &treatment_lead.estimate,
    ] {
        let [low, high] = estimate.mean_interval.expect("paired bootstrap");
        let mean = estimate.mean.unwrap();
        assert!(low <= mean && mean <= high, "{low} {mean} {high}");
        assert!(estimate.median.is_none());
    }
    for metric_name in ["cost_per_line", "lead_time_per_line"] {
        let control = arm("control", metric_name);
        let treatment = arm("treatment", metric_name);
        assert_eq!(control.zero_lines_excluded, Some(1));
        assert_eq!(control.unavailable_lines_excluded, Some(1));
        assert_eq!(control.open, 1);
        assert_eq!(control.unmeasured, 0);
        assert_eq!(treatment.zero_lines_excluded, Some(0));
        assert_eq!(treatment.unavailable_lines_excluded, Some(0));
    }
    let cost_usd = arm("control", "cost_usd");
    let payload = serde_json::to_value(cost_usd).unwrap();
    assert!(payload.get("zero_lines_excluded").is_none());
    assert!(payload.get("unavailable_lines_excluded").is_none());
}

/// Improving cost per line while adding lines does not win.
#[test]
fn per_line_primary_without_guardrail_is_refused_and_a_breach_is_not_a_winner() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::per_line::per_line_primary_without_guardrail_is_refused_and_a_breach_is_not_a_winner",
    ) {
        return;
    }
    let (_root, runtime) = open_runtime();
    let refused = runtime
        .create_experiment(
            "per-line-unguarded",
            ExperimentDefinitionInput {
                arms: Some(arms()),
                primary_metric: Some("cost_per_line".into()),
                min_sample: Some(2),
                ..ExperimentDefinitionInput::default()
            },
            actor(),
        )
        .expect_err("a per-line primary needs lines_changed");
    assert!(refused.to_string().contains("lines_changed"), "{refused}");

    let (root, runtime) = open_runtime();
    let global = root.path().join("home/.orbit");
    let experiment = runtime
        .create_experiment(
            "per-line-guarded",
            ExperimentDefinitionInput {
                arms: Some(arms()),
                primary_metric: Some("cost_per_line".into()),
                guardrails: Some(json!({"lines_changed": "+0"})),
                min_sample: Some(2),
                ..ExperimentDefinitionInput::default()
            },
            actor(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor())
        .unwrap();
    // Identical pairs so the ratio interval is the point estimate and excludes zero.
    // Control cost ratio 1, mean lines 10. Treatment cost ratio 0.2, mean lines 100.
    for seq in [1_u64, 2] {
        measured(
            &runtime,
            &global,
            &experiment.id,
            "control",
            Measured {
                cost: 10.0,
                lines: 10,
                lead_hours: 1,
            },
            seq,
        );
    }
    for seq in [3_u64, 4] {
        let task_id = enroll(&runtime, &experiment.id, "treatment", true);
        let landed = done_at(&runtime, &task_id);
        backdate_created_at(&global, &task_id, landed, 1);
        cost(&runtime, &task_id, 2.0);
        lines(
            &runtime,
            &task_id,
            seq,
            100,
            Some(ReviewableLines {
                added: 10,
                deleted: 0,
            }),
        );
    }

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let metric = readout
        .metrics
        .iter()
        .find(|metric| metric.metric == "cost_per_line")
        .unwrap();
    let difference = metric
        .differences
        .iter()
        .find(|difference| difference.arm == "treatment")
        .unwrap();
    assert!(close(difference.overall.estimate.unwrap(), -0.8));
    let [_, high] = difference.overall.interval.unwrap();
    assert!(
        high < 0.0,
        "the per-line improvement excludes zero: {difference:?}"
    );
    assert_ne!(readout.verdict.state, "winner");
    assert!(
        readout
            .verdict
            .reasons
            .iter()
            .any(|reason| reason.contains("lines_changed") && reason.contains("guardrail")),
        "{:?}",
        readout.verdict
    );
}
