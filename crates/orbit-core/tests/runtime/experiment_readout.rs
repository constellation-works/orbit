//! Public readouts preserve sample ratios and distinguish missing evidence
//! from a measured comparison after the experiment's horizon.

use chrono::{Duration, Utc};
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_store::contracts::{
    ExperimentAssignment, ExperimentEventRow, ExperimentStoreBackend, ExperimentWrite,
    ExperimentWriteOutcome, InvocationInsertParams,
};
use orbit_types::telemetry::{InvocationTrace, TokenUsage, ToolCallTrace};
use serde_json::json;

#[test]
fn large_valid_arm_weights_produce_the_expected_sample_ratio() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::large_valid_arm_weights_produce_the_expected_sample_ratio",
    ) {
        return;
    }

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
    let actor = ExperimentActor {
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "sample-ratio-overflow",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true, "weight": 3_000_000_000_u64 },
                    { "name": "treatment", "weight": 3_000_000_000_u64 }
                ])),
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    let experiment_id = experiment.id;
    let started = runtime
        .change_experiment(&experiment_id, ExperimentChange::Start, actor)
        .unwrap();
    assert_eq!(started.segment, 1);

    let workspace_id = runtime.workspace_id().unwrap();
    let store = runtime.sqlite_store().unwrap();
    for index in 0..200 {
        let task = runtime
            .add_task(TaskAddParams {
                title: format!("sample ratio task {index}"),
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..TaskAddParams::default()
            })
            .unwrap();
        let task_id = task.id.to_string();
        let arm_name = if index < 100 { "control" } else { "treatment" };
        let assignments = store
            .assign_experiments(&workspace_id, &task_id, true, true, &|records, _, _| {
                let record = records
                    .iter()
                    .find(|record| record.experiment.id == experiment_id)
                    .expect("started experiment is in the store");
                let arm = record
                    .arms
                    .iter()
                    .find(|arm| arm.name == arm_name)
                    .expect("assigned arm is in the experiment");
                Ok(vec![ExperimentAssignment {
                    experiment: experiment_id.clone(),
                    task_id: task_id.clone(),
                    arm: Some(arm_name.into()),
                    segment: Some(1),
                    digest: Some(arm.overlay_digest.clone()),
                    assigned_at: Utc::now(),
                    forced: false,
                    exclusion_reason: None,
                }])
            })
            .unwrap();
        assert_eq!(assignments.len(), 1);
    }

    let readout = runtime.experiment_readout(&experiment_id, None).unwrap();
    assert_eq!(readout.enrollment.analysed, 200);
    let sample_ratio = &readout.sample_ratio;
    let segment = sample_ratio.segments.first().unwrap();
    assert_eq!(segment.observed["control"], 100);
    assert_eq!(segment.observed["treatment"], 100);
    assert_eq!(segment.expected["control"], 100.0);
    assert_eq!(segment.expected["treatment"], 100.0);
    assert_eq!(sample_ratio.chi_square, Some(0.0));
    assert_eq!(sample_ratio.p_value, Some(1.0));
    assert!(!sample_ratio.mismatch);
}

type PrimaryCell = (TaskComplexity, &'static [u64], &'static [u64]);

#[test]
fn expired_readout_requires_a_primary_comparison_interval() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::expired_readout_requires_a_primary_comparison_interval",
    ) {
        return;
    }

    struct Case {
        name: &'static str,
        cells: &'static [PrimaryCell],
        estimate: Option<f64>,
        verdict: &'static str,
    }
    let cases = [
        Case {
            name: "no-settled-outcomes",
            cells: &[],
            estimate: None,
            verdict: "inconclusive",
        },
        Case {
            name: "one-outcome-per-arm",
            cells: &[(TaskComplexity::Low, &[200], &[100])],
            estimate: Some(-100.0),
            verdict: "inconclusive",
        },
        Case {
            name: "one-outcome-in-control",
            cells: &[(TaskComplexity::Low, &[200], &[100, 100])],
            estimate: Some(-100.0),
            verdict: "inconclusive",
        },
        Case {
            name: "one-outcome-in-treatment-cell",
            cells: &[
                (TaskComplexity::Low, &[200, 200], &[100]),
                (TaskComplexity::Medium, &[200, 200], &[100, 100, 100]),
            ],
            estimate: Some(-100.0),
            verdict: "inconclusive",
        },
        Case {
            name: "no-common-complexity-cell",
            cells: &[
                (TaskComplexity::Low, &[200, 200], &[]),
                (TaskComplexity::Medium, &[], &[100, 100]),
            ],
            estimate: None,
            verdict: "inconclusive",
        },
        Case {
            name: "measured-equal-arms",
            cells: &[(TaskComplexity::Low, &[200, 200], &[200, 200])],
            estimate: Some(0.0),
            verdict: "no_difference",
        },
        Case {
            name: "measured-improvement",
            cells: &[(TaskComplexity::Low, &[200, 200], &[100, 100])],
            estimate: Some(-100.0),
            verdict: "winner",
        },
    ];
    let mut failures = Vec::new();
    for case in cases {
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
        let actor = ExperimentActor {
            actor: "readout-test".into(),
            reason: None,
        };
        let experiment = runtime
            .create_experiment(
                case.name,
                ExperimentDefinitionInput {
                    arms: Some(json!([
                        { "name": "control", "control": true },
                        { "name": "treatment" }
                    ])),
                    primary_metric: Some("implement_tokens".into()),
                    min_sample: Some(100),
                    max_days: Some(1),
                    ..ExperimentDefinitionInput::default()
                },
                actor.clone(),
            )
            .unwrap();
        let workspace_id = runtime.workspace_id().unwrap();
        let store = runtime.sqlite_store().unwrap();
        let mut record = store
            .experiment_record(&workspace_id, &experiment.id)
            .unwrap()
            .unwrap();
        let revision = record.experiment.revision;
        let started_at = Utc::now() - Duration::days(2);
        record.experiment.status = "running".into();
        record.experiment.segment = 1;
        record.experiment.revision += 1;
        // Seed a historical start through the append-only store contract,
        // exercising expiry without sleeping or replacing the readout's clock.
        assert_eq!(
            store
                .write_experiment(&ExperimentWrite {
                    event: ExperimentEventRow {
                        seq: record.experiment.revision,
                        kind: "started".into(),
                        from_status: Some("draft".into()),
                        to_status: Some("running".into()),
                        segment: 1,
                        actor: actor.actor,
                        at: started_at,
                        reason: None,
                        detail_json:
                            json!({"segment": 1, "weights": {"control": 1, "treatment": 1}})
                                .to_string(),
                    },
                    experiment: record.experiment,
                    arms: None,
                    expected_revision: Some(revision),
                    running_limit: None,
                })
                .unwrap(),
            ExperimentWriteOutcome::Written
        );

        // No settled data must also cover enrolled tasks still in flight.
        if case.cells.is_empty() {
            for arm in ["control", "treatment"] {
                enroll_primary_sample(&runtime, &experiment.id, arm, TaskComplexity::Low, None);
            }
        }
        for (complexity, control, treatment) in case.cells {
            for (arm, samples) in [("control", *control), ("treatment", *treatment)] {
                for tokens in samples {
                    enroll_primary_sample(
                        &runtime,
                        &experiment.id,
                        arm,
                        *complexity,
                        Some(*tokens),
                    );
                }
            }
        }
        let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
        assert!(readout.verdict.horizon_reached, "{}", case.name);
        assert!(!readout.sample_ratio.mismatch, "{}", case.name);
        let primary = readout
            .metrics
            .iter()
            .find(|m| m.metric == "implement_tokens")
            .unwrap();
        assert!(primary.measured);
        let difference = &primary.differences[0].overall;
        assert_eq!(difference.estimate, case.estimate, "{}", case.name);
        if case.verdict == "inconclusive" {
            assert!(difference.interval.is_none(), "{}", case.name);
            assert_eq!(readout.verdict.arm, None, "{}", case.name);
            assert!(
                readout
                    .verdict
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("treatment")
                        && reason.contains("no interval")
                        && reason.contains("implement_tokens")),
                "{}: {:?}",
                case.name,
                readout.verdict.reasons
            );
        } else {
            assert!(difference.interval.is_some(), "{}", case.name);
            assert_eq!(
                readout.verdict.arm.as_deref(),
                (case.verdict == "winner").then_some("treatment"),
                "{}",
                case.name
            );
        }
        if readout.verdict.state != case.verdict
            || (case.verdict == "inconclusive" && readout.verdict.label != "inconclusive")
        {
            failures.push(format!(
                "{}: expected {}, got {:?}",
                case.name, case.verdict, readout.verdict
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Creates a backlog task of `complexity` and enrolls it in `arm_name`.
fn enroll_arm_task(
    runtime: &OrbitRuntime,
    experiment_id: &str,
    arm_name: &str,
    complexity: TaskComplexity,
) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("experiment sample for {arm_name}"),
            complexity,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let store = runtime.sqlite_store().unwrap();
    store
        .assign_experiments(&workspace_id, &task.id, true, true, &|records, _, _| {
            let record = records
                .iter()
                .find(|r| r.experiment.id == experiment_id)
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
    task.id.to_string()
}

fn enroll_primary_sample(
    runtime: &OrbitRuntime,
    experiment_id: &str,
    arm_name: &str,
    complexity: TaskComplexity,
    tokens: Option<u64>,
) -> String {
    let task_id = enroll_arm_task(runtime, experiment_id, arm_name, complexity);
    if let Some(tokens) = tokens {
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: format!("fixture-{task_id}"),
                account: None,
                activity_id: "agent_implement".into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![task_id.clone()],
                trace: InvocationTrace {
                    usage: TokenUsage {
                        input: tokens,
                        ..TokenUsage::default()
                    },
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
        runtime.archive_task(&task_id).unwrap();
    }
    task_id
}

/// Enrolls a task in `arm_name` whose first implement invocation made
/// `calls`, each `(tool name, result bytes)` in order. `None` records no
/// implement invocation. Archives the task when `terminal`, so its outcome
/// settles.
fn enroll_first_edit_sample(
    runtime: &OrbitRuntime,
    experiment_id: &str,
    arm_name: &str,
    calls: Option<&[(&str, u64)]>,
    terminal: bool,
) -> String {
    let task_id = enroll_arm_task(runtime, experiment_id, arm_name, TaskComplexity::Low);
    if let Some(calls) = calls {
        let tool_calls: Vec<ToolCallTrace> = calls
            .iter()
            .enumerate()
            .map(|(seq, &(tool_name, result_bytes))| ToolCallTrace {
                seq: seq as u32,
                tool_name: tool_name.into(),
                result_bytes,
                ..ToolCallTrace::default()
            })
            .collect();
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: format!("fixture-{task_id}"),
                account: None,
                activity_id: "agent_implement".into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![task_id.clone()],
                trace: InvocationTrace {
                    tool_calls,
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
    }
    if terminal {
        runtime.archive_task(&task_id).unwrap();
    }
    task_id
}

/// What a first-edit case's treatment arm settles to.
#[derive(Clone, Copy)]
enum FirstEdit {
    /// Settled and measured, with `(turns, result bytes)` before the edit.
    Measured(f64, f64),
    /// Settled, with no value in the records.
    Unmeasured,
    /// Still in flight.
    Open,
}

#[test]
fn first_edit_readout_scores_the_first_implement_invocation() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::first_edit_readout_scores_the_first_implement_invocation",
    ) {
        return;
    }

    struct Case {
        name: &'static str,
        /// The first implement invocation's tool calls, or `None` for no
        /// implement invocation.
        calls: Option<&'static [(&'static str, u64)]>,
        terminal: bool,
        expected: FirstEdit,
    }
    let cases = [
        Case {
            name: "two-reads-then-editor",
            calls: Some(&[("read", 10), ("read", 20), ("edit", 5)]),
            terminal: true,
            expected: FirstEdit::Measured(2.0, 30.0),
        },
        Case {
            name: "mcp-editor-tail-after-reads",
            calls: Some(&[("read", 10), ("mcp__fs__write_file", 0)]),
            terminal: true,
            expected: FirstEdit::Measured(1.0, 10.0),
        },
        Case {
            name: "two-reads-then-native-file-change",
            calls: Some(&[("read", 10), ("read", 20), ("file_change", 0)]),
            terminal: true,
            expected: FirstEdit::Measured(2.0, 30.0),
        },
        Case {
            name: "edit-at-first-call",
            calls: Some(&[("edit", 40)]),
            terminal: true,
            expected: FirstEdit::Measured(0.0, 0.0),
        },
        Case {
            name: "no-implement-invocation",
            calls: None,
            terminal: true,
            expected: FirstEdit::Unmeasured,
        },
        Case {
            name: "missing-trace",
            calls: Some(&[]),
            terminal: true,
            expected: FirstEdit::Unmeasured,
        },
        Case {
            name: "no-detectable-editor",
            calls: Some(&[("command_execution", 12), ("shell", 3)]),
            terminal: true,
            expected: FirstEdit::Unmeasured,
        },
        Case {
            name: "in-flight-with-edit",
            calls: Some(&[("read", 10), ("edit", 5)]),
            terminal: false,
            expected: FirstEdit::Open,
        },
        Case {
            name: "in-flight-without-invocation",
            calls: None,
            terminal: false,
            expected: FirstEdit::Open,
        },
    ];
    for case in cases {
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
        let actor = ExperimentActor {
            actor: "readout-test".into(),
            reason: None,
        };
        let experiment = runtime
            .create_experiment(
                case.name,
                ExperimentDefinitionInput {
                    arms: Some(json!([
                        { "name": "control", "control": true },
                        { "name": "treatment" }
                    ])),
                    primary_metric: Some("turns_before_first_edit".into()),
                    ..ExperimentDefinitionInput::default()
                },
                actor.clone(),
            )
            .unwrap();
        runtime
            .change_experiment(&experiment.id, ExperimentChange::Start, actor)
            .unwrap();
        enroll_first_edit_sample(
            &runtime,
            &experiment.id,
            "treatment",
            case.calls,
            case.terminal,
        );

        let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
        // (n, mean, unmeasured, open) of the treatment arm for one metric.
        let treatment = |metric_name: &str| {
            let metric = readout
                .metrics
                .iter()
                .find(|metric| metric.metric == metric_name)
                .unwrap();
            assert!(metric.measured, "{}: {metric_name}", case.name);
            let arm = metric
                .arms
                .iter()
                .find(|arm| arm.arm == "treatment")
                .unwrap();
            (arm.estimate.n, arm.estimate.mean, arm.unmeasured, arm.open)
        };
        let (want_n, want_turns, want_bytes, want_unmeasured, want_open) = match case.expected {
            FirstEdit::Measured(turns, bytes) => (1, Some(turns), Some(bytes), 0, 0),
            FirstEdit::Unmeasured => (0, None, None, 1, 0),
            FirstEdit::Open => (0, None, None, 0, 1),
        };
        let turns = treatment("turns_before_first_edit");
        assert_eq!(
            turns,
            (want_n, want_turns, want_unmeasured, want_open),
            "{}: turns_before_first_edit",
            case.name
        );
        let bytes = treatment("result_bytes_before_first_edit");
        assert_eq!(
            bytes,
            (want_n, want_bytes, want_unmeasured, want_open),
            "{}: result_bytes_before_first_edit",
            case.name
        );
        assert_eq!(
            readout.primary_observations.len(),
            want_n,
            "{}: primary observations",
            case.name
        );
    }
}

#[test]
fn first_edit_readout_excludes_a_retry_when_same_timestamp_paging_stalls() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::first_edit_readout_excludes_a_retry_when_same_timestamp_paging_stalls",
    ) {
        return;
    }

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
    let actor = ExperimentActor {
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "first-edit-incomplete",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("turns_before_first_edit".into()),
                guardrails: Some(json!({ "result_bytes_before_first_edit": "+0" })),
                min_sample: Some(1),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    enroll_first_edit_sample(
        &runtime,
        &experiment.id,
        "control",
        Some(&[("read", 100), ("read", 100), ("edit", 0)]),
        true,
    );
    let task_id = enroll_first_edit_sample(
        &runtime,
        &experiment.id,
        "treatment",
        Some(&[("read", 100), ("edit", 0)]),
        true,
    );
    let older = Utc::now() - Duration::hours(1);
    let newer = older + Duration::minutes(1);
    let store = runtime.sqlite_store().unwrap();
    store
        .connection()
        .lock()
        .unwrap()
        .execute("UPDATE invocations SET ts = ?1", [older.to_rfc3339()])
        .unwrap();

    let complete = runtime.experiment_readout(&experiment.id, None).unwrap();
    for (name, means) in [
        ("turns_before_first_edit", [2.0, 1.0]),
        ("result_bytes_before_first_edit", [200.0, 100.0]),
    ] {
        let metric = complete.metrics.iter().find(|m| m.metric == name).unwrap();
        for (arm, mean) in ["control", "treatment"].into_iter().zip(means) {
            let estimate = metric.arms.iter().find(|a| a.arm == arm).unwrap();
            assert_eq!(estimate.estimate.n, 1, "complete {name}: {arm}");
            assert_eq!(estimate.estimate.mean, Some(mean));
        }
    }
    assert_eq!(complete.primary_observations.len(), 2);
    assert!(complete.verdict.horizon_reached);
    assert!(
        complete
            .warnings
            .iter()
            .all(|w| w.kind != "first_edit_incomplete")
    );

    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: "first-edit-retry".into(),
            account: None,
            activity_id: "agent_implement".into(),
            agent: "codex".into(),
            provider: None,
            model: None,
            task_ids: vec![task_id.clone()],
            trace: InvocationTrace {
                tool_calls: vec![ToolCallTrace {
                    tool_name: "edit".into(),
                    ..ToolCallTrace::default()
                }],
                ..InvocationTrace::default()
            },
        })
        .unwrap();
    {
        let connection = store.connection();
        let mut connection = connection.lock().unwrap();
        let tx = connection.transaction().unwrap();
        tx.execute(
            "UPDATE invocations SET ts = ?1 WHERE job_run_id = 'first-edit-retry'",
            [newer.to_rfc3339()],
        )
        .unwrap();
        // One bounded fixture batch fills the newest page at exactly one
        // timestamp. Inclusive timestamp pagination repeats it and cannot
        // reach the older originals; no timing or concurrent load is needed.
        tx.execute(
            "WITH RECURSIVE fixture(n) AS (
                VALUES(1) UNION ALL SELECT n + 1 FROM fixture WHERE n < 9999
             ) INSERT INTO invocations(ts, workspace_id, job_run_id, activity_id, agent)
             SELECT ?1, ?2, 'first-edit-page-' || n, 'agent_implement', 'codex' FROM fixture",
            rusqlite::params![newer.to_rfc3339(), runtime.workspace_id().unwrap()],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let collected = runtime.first_implement_edits(Some(older)).unwrap();
    assert!(
        collected.truncated,
        "fixture must hit the pagination boundary"
    );
    assert_eq!(collected.by_task[&task_id].score, Some((0, 0)));

    let incomplete = runtime.experiment_readout(&experiment.id, None).unwrap();
    for name in ["turns_before_first_edit", "result_bytes_before_first_edit"] {
        let metric = incomplete
            .metrics
            .iter()
            .find(|m| m.metric == name)
            .unwrap();
        for arm in &metric.arms {
            assert_eq!(arm.estimate.n, 0, "incomplete {name}: {}", arm.arm);
            assert_eq!(arm.estimate.mean, None);
            assert_eq!((arm.unmeasured, arm.open), (1, 0));
            assert!(arm.segments.iter().all(|s| s.estimate.n == 0));
        }
        for difference in &metric.differences {
            assert_eq!(difference.overall.estimate, None);
            assert_eq!(difference.overall.interval, None);
        }
    }
    assert!(incomplete.primary_observations.is_empty());
    assert_eq!(incomplete.verdict.state, "collecting");
    assert!(!incomplete.verdict.horizon_reached);
    assert!(incomplete.verdict.arm.is_none());
    assert_eq!(incomplete.guardrails[0].arms[0].status, "unknown");
    let warning = incomplete
        .warnings
        .iter()
        .find(|w| w.kind == "first_edit_incomplete")
        .expect("incomplete collection must carry coverage evidence");
    assert!(!warning.message.is_empty());
    let unaffected = incomplete
        .metrics
        .iter()
        .find(|m| m.metric == "delivery_done_rate")
        .unwrap();
    assert!(unaffected.arms.iter().all(|a| a.estimate.n == 1));
}

#[test]
fn cost_readout_excludes_partial_sums_and_identifies_estimated_tasks() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::cost_readout_excludes_partial_sums_and_identifies_estimated_tasks",
    ) {
        return;
    }

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
    let actor = ExperimentActor {
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "cost-coverage",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("cost_usd".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    // Below the 272K prompt tier, so the totals alone fix the derived cost.
    let priced = (Some("gpt-6.1-sol"), None, 200_000, 0);
    let reported = (Some("gpt-6.1-sol"), Some(5.0), 1_000_000, 0);
    let zero = (None, Some(0.0), 0, 0);
    let unpriced = (Some("unpriced-test-model"), None, 1_000_000, 0);
    let invalid_basis = (Some("gpt-6.1-sol"), None, 100, 200);
    let cases = [
        (vec![reported], true),
        (vec![zero], true),
        (vec![priced], true),
        (vec![reported, priced], true),
        (vec![reported, unpriced], true),
        (vec![unpriced], true),
        (vec![reported, invalid_basis], true),
        (vec![], true),
        (vec![reported], false),
    ];
    for arm in ["control", "treatment"] {
        for (case_index, (invocations, terminal)) in cases.iter().enumerate() {
            let task_id =
                enroll_primary_sample(&runtime, &experiment.id, arm, TaskComplexity::Low, None);
            for (index, (model, cost, input, cache_read)) in invocations.iter().enumerate() {
                let run_id = format!("fixture-{task_id}-{index}");
                let activity = if index == 0 {
                    "agent_implement"
                } else {
                    "review"
                };
                let execution = (case_index % 4 != 3).then(|| {
                    runtime
                        .sqlite_store()
                        .unwrap()
                        .begin_agent_execution(
                            runtime.stores().jobs().workspace_id(),
                            &run_id,
                            activity,
                            "codex",
                            *model,
                            &orbit_types::telemetry::AgentExecutionIdentity {
                                crew: Some("fixture".into()),
                                reasoning_effort: match case_index % 4 {
                                    0 => Some(orbit_types::identity::ReasoningEffort::High),
                                    1 => Some(orbit_types::identity::ReasoningEffort::Xhigh),
                                    _ => None,
                                },
                            },
                        )
                        .unwrap()
                });
                runtime
                    .insert_invocation_trace_record(&InvocationInsertParams {
                        agent_execution_id: execution,
                        job_run_id: format!("fixture-{task_id}-{index}"),
                        account: None,
                        activity_id: if index == 0 {
                            "agent_implement"
                        } else {
                            "review"
                        }
                        .into(),
                        agent: "codex".into(),
                        provider: None,
                        model: model.map(str::to_string),
                        task_ids: vec![task_id.clone()],
                        trace: InvocationTrace {
                            usage: TokenUsage {
                                input: *input,
                                cache_read: *cache_read,
                                ..TokenUsage::default()
                            },
                            provider_cost_usd: *cost,
                            ..InvocationTrace::default()
                        },
                    })
                    .unwrap();
            }
            if *terminal {
                runtime.archive_task(&task_id).unwrap();
            }
        }
    }

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    for arm in &readout.arms {
        let efforts: std::collections::BTreeSet<_> = arm
            .effort_breakdown
            .iter()
            .map(|row| serde_json::to_string(&row.reasoning_effort).unwrap())
            .collect();
        for expected in ["\"high\"", "\"xhigh\"", "null", "\"unknown\""] {
            assert!(efforts.contains(expected), "{:?}", arm.effort_breakdown);
        }
    }
    let metric = readout
        .metrics
        .iter()
        .find(|metric| metric.metric == "cost_usd")
        .unwrap();
    let derived = orbit_common::derive_cost_usd(
        "gpt-6.1-sol",
        Utc::now(),
        &TokenUsage {
            input: 200_000,
            ..TokenUsage::default()
        },
        None,
    )
    .unwrap();
    assert!(derived > 0.0);
    for arm in &metric.arms {
        assert_eq!(
            arm.estimate.n, 4,
            "partial provider sums must never be measured"
        );
        assert_eq!(arm.unmeasured, 4);
        assert_eq!(arm.open, 1);
        assert_eq!(arm.estimate.mean, Some((10.0 + 2.0 * derived) / 4.0));
        let payload = serde_json::to_value(arm).unwrap();
        assert_eq!(
            payload["cost_coverage"],
            json!({"reported": 2, "estimated": 2, "unmeasured": 4})
        );
    }
    assert_eq!(readout.primary_observations.len(), 8);
    assert!(
        readout
            .metrics
            .iter()
            .filter(|metric| metric.metric != "cost_usd")
            .all(|metric| serde_json::to_value(&metric.arms)
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .all(|arm| arm.get("cost_coverage").is_none()))
    );
}

mod visual;
