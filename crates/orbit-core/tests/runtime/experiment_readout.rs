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
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
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

fn enroll_primary_sample(
    runtime: &OrbitRuntime,
    experiment_id: &str,
    arm_name: &str,
    complexity: TaskComplexity,
    tokens: Option<u64>,
) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: format!("primary sample for {arm_name}"),
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
    if let Some(tokens) = tokens {
        runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                job_run_id: format!("fixture-{}", task.id),
                account: None,
                activity_id: "agent_implement".into(),
                agent: "codex".into(),
                provider: None,
                model: Some("gpt-6.1-sol".into()),
                task_ids: vec![task.id.to_string()],
                trace: InvocationTrace {
                    usage: TokenUsage {
                        input: tokens,
                        ..TokenUsage::default()
                    },
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
        runtime.archive_task(&task.id).unwrap();
    }
    task.id.to_string()
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
        for (invocations, terminal) in &cases {
            let task_id =
                enroll_primary_sample(&runtime, &experiment.id, arm, TaskComplexity::Low, None);
            for (index, (model, cost, input, cache_read)) in invocations.iter().enumerate() {
                runtime
                    .insert_invocation_trace_record(&InvocationInsertParams {
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
