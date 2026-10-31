//! Public readouts preserve sample ratios and distinguish missing evidence
//! from a measured comparison after the experiment's horizon.

use chrono::{Duration, Utc};
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput, ExperimentReadout,
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
/// No job applies an arm's overlay to a task worked in an interactive
/// session, so an assignment that reached review without a job run is not
/// treated: the readout leaves it out of every estimate and counts it, while
/// assignments not yet implemented stay analysed with open outcomes.
#[test]
fn readout_leaves_interactive_assignments_out_of_the_treated_population() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::readout_leaves_interactive_assignments_out_of_the_treated_population",
    ) {
        return;
    }
    use orbit_core::application::task::TaskUpdateParams;
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
            "interactive-excluded",
            ExperimentDefinitionInput {
                arms: Some(json!([{"name":"control","control":true},{"name":"treatment"}])),
                primary_metric: Some("first_pass_acceptance".into()),
                ..Default::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    for arm in ["control", "treatment"] {
        enroll_arm_task(&runtime, &experiment.id, arm, TaskComplexity::Low);
    }
    let interactive = enroll_arm_task(&runtime, &experiment.id, "treatment", TaskComplexity::Low);
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        runtime
            .force_update_task_with_identity(
                &interactive,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    assert_eq!(
        readout.enrollment.analysed, 2,
        "unrun assignments stay analysed"
    );
    assert_eq!(readout.enrollment.interactive_excluded, 1);
    assert_eq!(
        readout.enrollment.unreadable, 0,
        "an excluded task is not an unreadable one"
    );
}

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

/// Records one implement invocation for `task_id` whose tool calls are
/// `calls`, in order. `tool_calls_incomplete` marks a suffix that outgrew the
/// output capture. Inserts made in sequence get increasing `(ts, id)`, so the
/// first one is the task's earlier invocation.
fn insert_implement_calls(
    runtime: &OrbitRuntime,
    task_id: &str,
    job_run_id: &str,
    calls: &[&str],
    tool_calls_incomplete: bool,
) {
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: job_run_id.into(),
            account: None,
            activity_id: "agent_implement".into(),
            agent: "grok".into(),
            provider: None,
            model: None,
            task_ids: vec![task_id.into()],
            trace: InvocationTrace {
                tool_calls: calls
                    .iter()
                    .enumerate()
                    .map(|(index, tool_name)| ToolCallTrace {
                        seq: index as u32 + 1,
                        tool_name: (*tool_name).into(),
                        ..ToolCallTrace::default()
                    })
                    .collect(),
                tool_calls_incomplete,
                ..InvocationTrace::default()
            },
        })
        .unwrap();
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
            name: "read-then-hashline-edit",
            calls: Some(&[("read_file", 8), ("hashline_edit", 0)]),
            terminal: true,
            expected: FirstEdit::Measured(1.0, 8.0),
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

/// [ORB-20534] Seeded implement tool-call records: an arm counts tasks, not
/// calls, that recorded an explorer delegation under any stored name.
#[test]
fn readout_counts_tasks_with_a_recorded_explorer_call() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::readout_counts_tasks_with_a_recorded_explorer_call",
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
            "explorer-uptake",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    let seed = |arm: &str, activity: &str, calls: &[(&str, u64)], suffix: &str| {
        let task_id = enroll_arm_task(&runtime, &experiment.id, arm, TaskComplexity::Low);
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: format!("fixture-{task_id}-{suffix}"),
                account: None,
                activity_id: activity.into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![task_id.clone()],
                trace: InvocationTrace {
                    tool_calls: calls
                        .iter()
                        .enumerate()
                        .map(|(seq, &(tool_name, result_bytes))| ToolCallTrace {
                            seq: seq as u32,
                            tool_name: tool_name.into(),
                            result_bytes,
                            ..ToolCallTrace::default()
                        })
                        .collect(),
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
        runtime.archive_task(&task_id).unwrap();
    };
    seed(
        "treatment",
        "agent_implement",
        &[("orbit_task_show", 2), ("orbit.agent.invoke", 0)],
        "dotted",
    );
    seed(
        "treatment",
        "agent_implement",
        &[("orbit_agent_invoke", 4)],
        "stored",
    );
    seed(
        "treatment",
        "agent_implement",
        &[("mcp__orbit__orbit_agent_invoke", 0)],
        "mcp",
    );
    seed(
        "treatment",
        "agent_implement",
        &[("Orbit.Agent.Invoke", 0)],
        "case",
    );
    seed(
        "treatment",
        "agent_implement",
        &[("orbit_task_show", 3)],
        "other-tool",
    );
    seed(
        "treatment",
        "review",
        &[("orbit_agent_invoke", 1)],
        "not-implement",
    );
    let twice = enroll_arm_task(&runtime, &experiment.id, "treatment", TaskComplexity::Low);
    for turn in 0..2 {
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: format!("fixture-{twice}-{turn}"),
                account: None,
                activity_id: "agent_implement".into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![twice.clone()],
                trace: InvocationTrace {
                    tool_calls: vec![ToolCallTrace {
                        seq: 1,
                        tool_name: "orbit_agent_invoke".into(),
                        result_bytes: 0,
                        ..ToolCallTrace::default()
                    }],
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
    }
    runtime.archive_task(&twice).unwrap();
    seed(
        "treatment",
        "implement_one",
        &[("mcp__orbit__orbit.agent.invoke", 0)],
        "implement-one",
    );
    seed(
        "control",
        "agent_implement",
        &[("orbit_agent_invoke", 0)],
        "control-yes",
    );
    seed("control", "agent_implement", &[], "control-no");

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let arm = |name: &str| {
        readout
            .arms
            .iter()
            .find(|arm| arm.name == name)
            .unwrap_or_else(|| panic!("missing arm {name}"))
    };
    assert_eq!(readout.schema_version, 1);
    assert_eq!(readout.enrollment.analysed, 10);
    assert_eq!(readout.enrollment.interactive_excluded, 0);
    assert_eq!(
        arm("treatment").n,
        8,
        "every enrolled treatment task is analysed"
    );
    assert_eq!(
        arm("treatment").tasks_with_explorer_call,
        6,
        "a non-delegation tool and a non-implement activity do not count, and two calls are one task"
    );
    assert_eq!(arm("control").n, 2);
    assert_eq!(arm("control").tasks_with_explorer_call, 1);

    let json = serde_json::to_value(&readout).unwrap();
    assert_eq!(json["schema_version"], 1);
    for (name, tasks) in [("treatment", 6), ("control", 1)] {
        let row = json["arms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["tasks_with_explorer_call"], tasks, "{name}");
    }
}

/// [ORB-20572] An invocation whose output outgrew the capture bound keeps only
/// the suffix of its tool calls: its surviving editor is not a measured first
/// edit, and a missing explorer call is not a confirmed absence.
#[test]
fn readout_does_not_measure_or_rule_out_calls_from_a_truncated_tool_trace() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::readout_does_not_measure_or_rule_out_calls_from_a_truncated_tool_trace",
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
            "truncated-tool-trace",
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

    let seed = |arm: &str, calls: &[&str], tool_calls_incomplete: bool| {
        let task_id = enroll_arm_task(&runtime, &experiment.id, arm, TaskComplexity::Low);
        insert_implement_calls(
            &runtime,
            &task_id,
            &format!("fixture-{task_id}"),
            calls,
            tool_calls_incomplete,
        );
        runtime.archive_task(&task_id).unwrap();
    };
    // The early explorer and read were lost; only the late editor survives.
    seed("treatment", &["hashline_edit"], true);
    // An explorer call that survived is a call whatever else was lost.
    seed("treatment", &["orbit.agent.invoke", "hashline_edit"], true);
    seed("control", &["read_file", "hashline_edit"], false);

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let arm = |name: &str| readout.arms.iter().find(|arm| arm.name == name).unwrap();
    assert_eq!(arm("treatment").tasks_with_explorer_call, 1);
    assert_eq!(
        arm("treatment").tasks_explorer_unobserved,
        1,
        "the lost early calls may have held an explorer delegation"
    );
    assert_eq!(arm("control").tasks_with_explorer_call, 0);
    assert_eq!(arm("control").tasks_explorer_unobserved, 0);

    let first_edit = |arm_name: &str| {
        let metric = readout
            .metrics
            .iter()
            .find(|metric| metric.metric == "turns_before_first_edit")
            .unwrap();
        let arm = metric.arms.iter().find(|arm| arm.arm == arm_name).unwrap();
        (arm.estimate.n, arm.estimate.mean, arm.unmeasured)
    };
    assert_eq!(
        first_edit("treatment"),
        (0, None, 2),
        "a suffix's surviving editor is not a measured first edit"
    );
    assert_eq!(first_edit("control"), (1, Some(1.0), 0));
    assert!(
        readout
            .warnings
            .iter()
            .any(|warning| warning.kind == "tool_trace_incomplete"),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-20590] A truncated later invocation leaves a task's complete first
/// edit measured: only its explorer uptake is unconfirmed. A truncated first
/// invocation is the case whose first edit stays unmeasured, so it alone
/// raises `tool_trace_incomplete`.
#[test]
fn readout_keeps_a_complete_first_edit_when_a_later_invocation_is_truncated() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::readout_keeps_a_complete_first_edit_when_a_later_invocation_is_truncated",
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
            "complete-first-then-truncated-retry",
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

    let first_edit = |readout: &ExperimentReadout, arm_name: &str| {
        let metric = readout
            .metrics
            .iter()
            .find(|metric| metric.metric == "turns_before_first_edit")
            .unwrap();
        let arm = metric.arms.iter().find(|arm| arm.arm == arm_name).unwrap();
        (arm.estimate.n, arm.estimate.mean, arm.unmeasured)
    };

    // The complete invocation is recorded first, so it is the task's first
    // edit; the truncated retry only leaves its explorer delegation unproven.
    let retried = enroll_arm_task(&runtime, &experiment.id, "treatment", TaskComplexity::Low);
    insert_implement_calls(
        &runtime,
        &retried,
        "retry-first",
        &["read_file", "hashline_edit"],
        false,
    );
    insert_implement_calls(&runtime, &retried, "retry-later", &["hashline_edit"], true);
    runtime.archive_task(&retried).unwrap();
    let control = enroll_arm_task(&runtime, &experiment.id, "control", TaskComplexity::Low);
    insert_implement_calls(
        &runtime,
        &control,
        "control",
        &["read_file", "hashline_edit"],
        false,
    );
    runtime.archive_task(&control).unwrap();

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let arm = |name: &str| readout.arms.iter().find(|arm| arm.name == name).unwrap();
    assert_eq!(
        first_edit(&readout, "treatment"),
        (1, Some(1.0), 0),
        "the complete first invocation is the measured first edit"
    );
    assert_eq!(first_edit(&readout, "control"), (1, Some(1.0), 0));
    assert_eq!(arm("treatment").tasks_with_explorer_call, 0);
    assert_eq!(
        arm("treatment").tasks_explorer_unobserved,
        1,
        "the truncated retry may have held an explorer delegation"
    );
    assert!(
        readout
            .warnings
            .iter()
            .any(|warning| warning.kind == "explorer_trace_incomplete"),
        "{:?}",
        readout.warnings
    );
    assert!(
        readout
            .warnings
            .iter()
            .all(|warning| warning.kind != "tool_trace_incomplete"),
        "no first edit is unmeasured in this readout: {:?}",
        readout.warnings
    );

    // A truncated first invocation on a second task leaves that task's first
    // edit unmeasured, while the complete task's sample is still kept.
    let truncated = enroll_arm_task(&runtime, &experiment.id, "treatment", TaskComplexity::Low);
    insert_implement_calls(
        &runtime,
        &truncated,
        "truncated-first",
        &["hashline_edit"],
        true,
    );
    runtime.archive_task(&truncated).unwrap();
    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    assert_eq!(
        first_edit(&readout, "treatment"),
        (1, Some(1.0), 1),
        "a truncated first invocation is unmeasured; the complete one still counts"
    );
    assert!(
        readout
            .warnings
            .iter()
            .any(|warning| warning.kind == "tool_trace_incomplete"),
        "{:?}",
        readout.warnings
    );
}

/// [ORB-20573] Exactly 100,000 newer unrelated implement invocations must
/// not evict an enrolled task's recorded explorer call from the readout.
#[test]
fn readout_counts_explorer_call_beyond_workspace_invocation_scan_limit() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::readout_counts_explorer_call_beyond_workspace_invocation_scan_limit",
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
            "explorer-uptake-beyond-scan-limit",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    let seed = |activity: &str, tool_name: &str, suffix: &str| {
        let task_id = enroll_arm_task(&runtime, &experiment.id, "treatment", TaskComplexity::Low);
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: format!("explorer-cap-{task_id}-{suffix}"),
                account: None,
                activity_id: activity.into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![task_id.clone()],
                trace: InvocationTrace {
                    tool_calls: vec![ToolCallTrace {
                        seq: 0,
                        tool_name: tool_name.into(),
                        result_bytes: 0,
                        ..ToolCallTrace::default()
                    }],
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
        task_id
    };

    // Counted once, however many post-assignment implement calls it made.
    let counted = seed("agent_implement", "orbit.agent.invoke", "counted");
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: format!("explorer-cap-{counted}-counted-again"),
            account: None,
            activity_id: "agent_implement".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![counted.clone()],
            trace: InvocationTrace {
                tool_calls: vec![ToolCallTrace {
                    seq: 0,
                    tool_name: "mcp__orbit__orbit_agent_invoke".into(),
                    result_bytes: 0,
                    ..ToolCallTrace::default()
                }],
                ..InvocationTrace::default()
            },
        })
        .unwrap();
    runtime.archive_task(&counted).unwrap();

    // An explorer call recorded before assignment does not count.
    let before_assignment = seed("agent_implement", "orbit_agent_invoke", "before-assignment");
    let store = runtime.sqlite_store().unwrap();
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE invocations SET ts = ?1 WHERE job_run_id = ?2",
            rusqlite::params![
                (Utc::now() - Duration::hours(2)).to_rfc3339(),
                format!("explorer-cap-{before_assignment}-before-assignment")
            ],
        )
        .unwrap();
    runtime.archive_task(&before_assignment).unwrap();

    // An explorer call outside an implement activity does not count.
    let review_only = seed("review", "orbit_agent_invoke", "review-only");
    runtime.archive_task(&review_only).unwrap();

    // An implement invocation without an explorer call does not count.
    let no_explorer = seed("agent_implement", "orbit_task_show", "no-explorer");
    runtime.archive_task(&no_explorer).unwrap();

    // One bounded fixture batch: exactly the old scan limit of newer,
    // unrelated (unlinked) implement invocations, in a transaction.
    let newer = Utc::now() + Duration::days(1);
    {
        let connection = store.connection();
        let mut connection = connection.lock().unwrap();
        let tx = connection.transaction().unwrap();
        tx.execute(
            "WITH RECURSIVE fixture(n) AS (
                VALUES(1) UNION ALL SELECT n + 1 FROM fixture WHERE n < 100000
             ) INSERT INTO invocations(ts, workspace_id, job_run_id, activity_id, agent)
             SELECT ?1, ?2, 'explorer-cap-unrelated-' || n, 'agent_implement', 'codex'
             FROM fixture",
            rusqlite::params![newer.to_rfc3339(), runtime.workspace_id().unwrap()],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    assert_eq!(readout.enrollment.analysed, 4);
    let treatment = readout
        .arms
        .iter()
        .find(|arm| arm.name == "treatment")
        .unwrap();
    assert_eq!(treatment.n, 4);
    assert_eq!(
        treatment.tasks_with_explorer_call, 1,
        "100,000 newer unrelated invocations must not evict the enrolled \
         task's post-assignment implement explorer call, and it counts once"
    );
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
