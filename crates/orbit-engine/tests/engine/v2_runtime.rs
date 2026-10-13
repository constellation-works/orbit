#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! v2 runtime integration coverage: the deterministic reference activity
//! dispatches through a stub `RuntimeHost` and persists its §7 envelope
//! events, a deterministic step's tool binding comes from the dispatch rather
//! than its tool arguments, and job assets with `parallel:`, `fan_out:` and `loop:` blocks
//! keep their join semantics when run through `execute_job_with_resume`,
//! and the shipped before-PR `review` step retries and recovers a failing
//! reviewer while charging only reviewer invocations.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^v2_runtime::/)'`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ResolvedCliExecutor, ReviewerInvocationRequest, V2AuditWriter,
    V2DispatchInput, V2SqliteSink, dispatch_v2_activity, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, DeterministicSpec, V2AuditEvent, V2AuditEventKind,
};
use orbit_types::workflow::{JobRunState, PipelineState, ReviewerInvocationEvent};
use serde_json::{Value, json};

fn deterministic_reference_dispatches_and_persists_audit_events() -> Result<(), String> {
    let references_dir = workspace_root().join("crates/orbit-core/assets/activities/examples");
    let tmp_audit = tempfile::tempdir().map_err(|err| err.to_string())?;
    smoke_dispatch_deterministic(
        &references_dir.join("deterministic_reference.yaml"),
        tmp_audit.path(),
    )
}

/// [ORB-13115] A deterministic step's tools learn which task and run they
/// serve from the dispatcher, not from the tool arguments the step forwards.
/// The host binds the run; the dispatcher adds the task from the run's own
/// input, the value a CLI agent step would export as `ORBIT_TASK_ID`.
fn a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args() {
    use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
    use orbit_tools::{ActivityBinding, ToolContext};
    use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec};

    #[derive(Default)]
    struct BindingHost {
        seen: Mutex<Option<ToolContext>>,
    }

    impl orbit_engine::test_support::NullRuntimeHost for BindingHost {
        fn tool_context_for_activity(
            &self,
            run_id: Option<&str>,
            _: Option<&str>,
            _: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
            _: Option<&[String]>,
        ) -> ToolContext {
            ToolContext {
                activity_binding: run_id.map(|job_run_id| ActivityBinding {
                    job_run_id: job_run_id.to_string(),
                    task_id: None,
                }),
                ..ToolContext::default()
            }
        }

        fn run_deterministic(
            &self,
            _: &str,
            _: &Value,
            _: &Value,
            tool_context: ToolContext,
        ) -> Result<Value, DispatchError> {
            *self.seen.lock().expect("seen") = Some(tool_context);
            Ok(json!({}))
        }
    }

    let spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: "plugin.tool_call".to_string(),
        config: Value::Null,
    });
    let sink: Arc<dyn AuditSink> = Arc::new(NullSink);
    let dispatch = |host: &BindingHost, input: Value| {
        dispatch_v2_activity(V2DispatchInput {
            activity_name: "publish",
            spec: &spec,
            fs_profile: None,
            input,
            audit: Arc::new(V2AuditWriter::new("jrun-host", "test", sink.clone())),
            run_id: "jrun-host",
            host: Some(host),
        })
        .expect("dispatch");
        host.seen
            .lock()
            .expect("seen")
            .take()
            .and_then(|context| context.activity_binding)
            .expect("an activity context carries its binding")
    };

    let host = BindingHost::default();
    let binding = dispatch(
        &host,
        json!({
            "task_id": "ORB-7",
            "tool": "pulsar.publish",
            "input": { "task_id": "ORB-999", "job_run_id": "jrun-forged" },
        }),
    );
    assert_eq!(
        binding,
        ActivityBinding {
            job_run_id: "jrun-host".to_string(),
            task_id: Some("ORB-7".to_string()),
        },
        "the plugin's own arguments must not name the task or run it is bound to"
    );

    let binding = dispatch(&host, json!({ "tool": "pulsar.publish" }));
    assert_eq!(binding.job_run_id, "jrun-host");
    assert_eq!(binding.task_id, None, "a step serving no task names none");
}

fn smoke_dispatch_deterministic(
    path: &std::path::Path,
    audit_root: &std::path::Path,
) -> Result<(), String> {
    let yaml = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let asset = load_v2(&yaml)?;

    let run_id = "smoke-det-001";
    let (writer, envelope, _inner) = build_writer_and_sinks(audit_root, run_id);

    let host = EchoHost;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: &asset.name,
        spec: &asset.spec.spec,
        fs_profile: asset.spec.fs_profile.as_deref(),
        input: Value::Null,
        audit: writer.clone(),
        run_id,
        host: Some(&host),
    })
    .map_err(|e| format!("dispatch: {e}"))?;

    if !outcome.success {
        return Err(format!("deterministic returned non-success: {outcome:?}"));
    }
    assert_sqlite_nonempty(&envelope)?;
    Ok(())
}

struct EchoHost;

impl orbit_engine::test_support::NullRuntimeHost for EchoHost {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Ok(serde_json::json!({
            "action": action,
            "config": config,
            "input": input,
            "echo": "deterministic smoke stub"
        }))
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Err(DispatchError::CliInvocationFailed(
            "EchoHost has no CLI provider mapping".into(),
        ))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext::default()
    }
}

pub(crate) fn build_writer_and_sinks(
    audit_root: &std::path::Path,
    run_id: &str,
) -> (Arc<V2AuditWriter>, Arc<V2SqliteSink>, Arc<InMemorySink>) {
    let blob_dir = audit_root.join("blobs");
    let _ = std::fs::create_dir_all(&blob_dir);
    let inner = Arc::new(InMemorySink::new(blob_dir));
    let envelope = Arc::new(V2SqliteSink::for_audit_root(
        Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite sink")),
        "ws_smoke",
        run_id,
        "smoke-agent",
        None,
        audit_root,
    ));
    let writer = Arc::new(
        V2AuditWriter::new(run_id, "smoke-agent", inner.clone())
            .with_envelope_sink(envelope.clone()),
    );
    (writer, envelope, inner)
}

fn load_v2(yaml: &str) -> Result<V2ReferenceAsset, String> {
    match load_activity_asset(yaml) {
        Ok(a) => Ok(V2ReferenceAsset {
            name: a.name,
            spec: a.spec,
        }),
        Err(err) => Err(format!("load: {err}")),
    }
}

struct V2ReferenceAsset {
    name: String,
    spec: ActivityV2,
}

fn assert_sqlite_nonempty(sink: &V2SqliteSink) -> Result<(), String> {
    let count = sink
        .persisted_event_count()
        .map_err(|e| format!("read audit sqlite rows: {e}"))?;
    if count == 0 {
        return Err("audit sqlite rows are empty".to_string());
    }
    Ok(())
}

pub(crate) fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

// --------------------------------------------------------------------------
// Job-graph semantics: parallel, fan-out and loop blocks
// --------------------------------------------------------------------------

/// `parallel:` decides the block from its `join` policy, reports every branch
/// in declaration order on `step.join`, and turns a panicking branch into a
/// failed branch instead of unwinding through the job thread.
fn parallel_join_policy_decides_the_block_from_its_branches() {
    let cases = [
        ("all, no branches", json!({"mode": "all"}), "", true),
        ("any, no branches", json!({"mode": "any"}), "", false),
        (
            "all, every branch succeeds",
            json!({"mode": "all"}),
            "+++",
            true,
        ),
        (
            "all, one branch fails",
            json!({"mode": "all"}),
            "+-+",
            false,
        ),
        (
            "any, one branch succeeds",
            json!({"mode": "any"}),
            "-+-",
            true,
        ),
        (
            "any, every branch fails",
            json!({"mode": "any"}),
            "---",
            false,
        ),
        (
            "quorum 2 of 3 met",
            json!({"mode": "quorum", "n": 2}),
            "+-+",
            true,
        ),
        (
            "quorum 2 of 3 missed",
            json!({"mode": "quorum", "n": 2}),
            "-+-",
            false,
        ),
        (
            "all, one branch panics",
            json!({"mode": "all"}),
            "+!",
            false,
        ),
    ];
    for (case, join, branches, expect_success) in cases {
        let branch_steps: Vec<Value> = branches
            .chars()
            .enumerate()
            .map(|(index, kind)| probe_step(&format!("br_{index}"), probe_input(kind, index)))
            .collect();
        let job = job_asset(json!([{
            "id": "fan",
            "parallel": { "join": join, "branches": branch_steps },
        }, probe_step("after", json!({"label": "after"}))]));
        let run = run_graph_job(&job, Value::Null);

        assert_eq!(run.succeeded(), expect_success, "{case}: {:?}", run.result);
        assert_eq!(
            run.host.labels().iter().any(|label| label == "after"),
            expect_success,
            "{case}: only a successful join runs the next step"
        );
        let joined = run
            .events
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::StepJoin {
                    step_id,
                    mode,
                    branch_outcomes,
                } if step_id == "fan" && mode == join["mode"].as_str().unwrap() => Some(
                    branch_outcomes
                        .iter()
                        .map(|branch| (branch.branch_id.clone(), branch.outcome.clone()))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{case}: no step.join event"));
        let expected: Vec<(String, String)> = branches
            .chars()
            .enumerate()
            .map(|(index, kind)| {
                let outcome = if kind == '+' { "success" } else { "error" };
                (format!("br_{index}"), outcome.to_string())
            })
            .collect();
        assert_eq!(
            joined, expected,
            "{case}: branch outcomes in declaration order"
        );
        if branches.is_empty() {
            assert_eq!(run.outcome().pipeline["fan"], json!([]), "{case}");
            assert_eq!(
                run.host.calls().len(),
                usize::from(expect_success),
                "{case}"
            );
            assert_empty_join_resume(&job, Value::Null, &run, "fan", expect_success);
        }
    }
}

/// `fan_out:` runs one worker per item under the `max_workers` cap and
/// collects outputs in item order (not completion order) under both the step
/// id and the `fan_in.collect` alias.
fn fan_out_collects_outputs_in_item_order_within_the_worker_cap() {
    let items: Vec<Value> = [120, 0, 40, 40, 40, 40]
        .iter()
        .enumerate()
        .map(|(index, sleep_ms)| json!({"label": format!("item-{index}"), "fail": false, "panic": false, "sleep_ms": sleep_ms}))
        .collect();
    let job = job_asset(json!([fan_out_step(2, json!({"mode": "all"}))]));
    let run = run_graph_job(&job, json!({ "items": items }));

    let outcome = run.outcome();
    assert!(outcome.success);
    let labels: Vec<&str> = outcome.pipeline["scatter"]
        .as_array()
        .expect("collected array")
        .iter()
        .map(|output| output["label"].as_str().expect("worker echoes its label"))
        .collect();
    assert_eq!(
        labels,
        ["item-0", "item-1", "item-2", "item-3", "item-4", "item-5"],
        "item 0 finishes last but is still collected first"
    );
    assert_eq!(outcome.pipeline["results"], outcome.pipeline["scatter"]);
    assert_eq!(run.host.calls().len(), 6);
    assert_eq!(
        run.host.peak_in_flight(),
        2,
        "workers overlap, but never beyond max_workers"
    );
}

/// `fan_in.join` judges the collected workers; a failed or panicked worker is
/// collected as `null` and counted on `fanin.joined`.
fn fan_in_join_policy_decides_the_block_from_its_workers() {
    let cases = [
        ("all, no workers", json!({"mode": "all"}), "", true),
        ("any, no workers", json!({"mode": "any"}), "", false),
        (
            "quorum 1, no workers",
            json!({"mode": "quorum", "n": 1}),
            "",
            false,
        ),
        (
            "all, every worker succeeds",
            json!({"mode": "all"}),
            "+++",
            true,
        ),
        (
            "all, one worker fails",
            json!({"mode": "all"}),
            "+-+",
            false,
        ),
        (
            "any, one worker succeeds",
            json!({"mode": "any"}),
            "-+-",
            true,
        ),
        (
            "any, every worker fails",
            json!({"mode": "any"}),
            "---",
            false,
        ),
        (
            "quorum 1, one worker succeeds",
            json!({"mode": "quorum", "n": 1}),
            "+",
            true,
        ),
        (
            "quorum 1, one worker fails",
            json!({"mode": "quorum", "n": 1}),
            "-",
            false,
        ),
        (
            "quorum exceeds runtime count",
            json!({"mode": "quorum", "n": 2}),
            "+",
            false,
        ),
        (
            "quorum 2 met",
            json!({"mode": "quorum", "n": 2}),
            "+-+",
            true,
        ),
        (
            "quorum 3 missed",
            json!({"mode": "quorum", "n": 3}),
            "+-+",
            false,
        ),
        (
            "all, one worker panics",
            json!({"mode": "all"}),
            "+!+",
            false,
        ),
    ];
    for (case, join, workers, expect_success) in cases {
        let items: Vec<Value> = workers
            .chars()
            .enumerate()
            .map(|(index, kind)| probe_input(kind, index))
            .collect();
        let job = job_asset(json!([
            fan_out_step(4, join),
            probe_step("after", json!({"label": "after"})),
        ]));
        let input = json!({ "items": items });
        let run = run_graph_job(&job, input.clone());

        assert_eq!(run.succeeded(), expect_success, "{case}: {:?}", run.result);
        assert_eq!(
            run.host.labels().iter().any(|label| label == "after"),
            expect_success,
            "{case}: only a successful join runs the next step"
        );
        let worker_count = workers.len() as u32;
        assert!(
            run.events.iter().any(|event| matches!(
                &event.kind,
                V2AuditEventKind::FanoutDispatched { step_id, worker_count: dispatched }
                    if step_id == "scatter" && *dispatched == worker_count
            )),
            "{case}: fanout.dispatched records the runtime item count"
        );
        let failed = workers.chars().filter(|kind| *kind != '+').count() as u32;
        assert!(
            run.events.iter().any(|event| matches!(
                &event.kind,
                V2AuditEventKind::FaninJoined { collected, failed: joined_failed, .. }
                    if *collected == worker_count - failed && *joined_failed == failed
            )),
            "{case}: fanin.joined counts collected and failed workers"
        );
        if let Ok(outcome) = &run.result {
            let collected = outcome.pipeline["scatter"].as_array().expect("collected");
            assert_eq!(collected.len(), workers.len(), "{case}");
            assert_eq!(
                outcome.pipeline["results"], outcome.pipeline["scatter"],
                "{case}"
            );
            for (kind, output) in workers.chars().zip(collected) {
                assert_eq!(output.is_null(), kind != '+', "{case}: {collected:?}");
            }
        }
        if workers.is_empty() {
            assert_eq!(run.outcome().pipeline["scatter"], json!([]), "{case}");
            assert_eq!(
                run.host.calls().len(),
                usize::from(expect_success),
                "{case}"
            );
            assert!(
                !run.events
                    .iter()
                    .any(|event| matches!(event.kind, V2AuditEventKind::WorkerState { .. })),
                "{case}: no worker audit events for an empty fan-out"
            );
            assert_empty_join_resume(&job, input, &run, "scatter", expect_success);
        }
    }
}

/// `loop:` iterates its items in order, stops at the first `break_when`
/// match, reports a loop that exhausted its budget without converging, stops
/// at the first failed body, and refuses more items than `max_iterations`
/// before running any of them.
fn loop_iterates_until_break_failure_or_budget() {
    let stops = |values: &[bool]| -> Vec<Value> {
        values
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                json!({"label": format!("item-{index}"), "stop": stop, "fail": false, "panic": false, "sleep_ms": 0})
            })
            .collect()
    };

    let broke = run_graph_job(
        &loop_job(5),
        json!({ "items": stops(&[false, true, false]) }),
    );
    assert!(broke.outcome().success);
    assert_eq!(broke.host.labels(), ["item-0", "item-1"]);
    assert_eq!(
        broke.loop_iteration_ends(),
        [(1, false), (2, true)],
        "the loop ends on the iteration whose output matches break_when"
    );
    assert!(!broke.did_not_converge());

    let exhausted = run_graph_job(
        &loop_job(3),
        json!({ "items": stops(&[false, false, false]) }),
    );
    assert!(
        exhausted.outcome().success,
        "an exhausted loop is not a failure"
    );
    assert_eq!(exhausted.host.labels(), ["item-0", "item-1", "item-2"]);
    assert!(exhausted.did_not_converge());

    let mut failing = stops(&[false, false, false]);
    failing[1]["fail"] = json!(true);
    let failed = run_graph_job(&loop_job(5), json!({ "items": failing }));
    assert!(!failed.succeeded());
    assert_eq!(
        failed.host.labels(),
        ["item-0", "item-1"],
        "no iteration runs after the body fails"
    );

    let over_budget = run_graph_job(
        &loop_job(2),
        json!({ "items": stops(&[false, false, false]) }),
    );
    assert!(!over_budget.succeeded());
    assert!(
        over_budget.host.calls().is_empty(),
        "an over-budget item list is refused before the first iteration"
    );
}

/// An items expression that renders valid JSON must be an array. `null`, an
/// object, a number, a bool, or a JSON string fails the step with
/// `JobExecution` naming `fan_out.items` or `loop.items`, and no worker runs.
/// A JSON array still parses, including one that holds an object, and a bare
/// `A, B` string still splits on the comma.
fn items_expression_rejects_non_array_json() {
    let rejected = [
        ("null", Value::Null),
        ("object", json!({"a": 1, "b": 2})),
        ("number", json!(0)),
        ("bool", json!(false)),
        ("string", json!("\"quoted\"")),
    ];
    for (case, list) in rejected {
        for (construct, field, job) in [
            ("fan_out", "fan_out.items", echo_fan_out_job()),
            ("loop", "loop.items", echo_loop_job()),
        ] {
            let run = run_graph_job(&job, json!({ "list": list.clone() }));
            match &run.result {
                Err(DispatchError::JobExecution(message)) => {
                    assert!(
                        message.contains(field),
                        "{construct} {case}: error names {field}: {message}"
                    );
                }
                other => panic!("{construct} {case}: expected JobExecution, got {other:?}"),
            }
            assert!(
                run.host.calls().is_empty(),
                "{construct} {case}: no worker runs for non-array JSON"
            );
        }
    }

    let accepted = [
        ("bare list", json!("A, B"), vec![json!("A"), json!("B")]),
        (
            "json array",
            json!(["A", "B"]),
            vec![json!("A"), json!("B")],
        ),
        (
            "json array of mixed values",
            json!(["A", {"k": 1}]),
            vec![json!("A"), json!({"k": 1})],
        ),
        ("empty array", json!([]), Vec::new()),
    ];
    for (case, list, items) in accepted {
        let fan_out = run_graph_job(&echo_fan_out_job(), json!({ "list": list.clone() }));
        assert!(fan_out.succeeded(), "fan_out {case}: {:?}", fan_out.result);
        let collected = fan_out.outcome().pipeline["scatter"]
            .as_array()
            .expect("fan_out collects an array");
        let values: Vec<Value> = collected
            .iter()
            .map(|output| output["value"].clone())
            .collect();
        assert_eq!(values, items, "fan_out {case}: workers run in item order");
        assert_eq!(
            fan_out.host.calls().len(),
            items.len(),
            "fan_out {case}: one call per item"
        );

        let loop_run = run_graph_job(&echo_loop_job(), json!({ "list": list }));
        assert!(loop_run.succeeded(), "loop {case}: {:?}", loop_run.result);
        let loop_values: Vec<Value> = loop_run
            .host
            .calls()
            .iter()
            .map(|input| input["value"].clone())
            .collect();
        assert_eq!(
            loop_values, items,
            "loop {case}: iterations run in item order"
        );
    }
}

/// A step whose `when:` guard is false is ordinary control flow and logs its
/// skip at INFO; a skip that is not a guard decision, such as a resumed step,
/// stays at WARN so the operational feed still shows it.
fn when_false_step_skips_log_below_warn_and_other_skips_keep_warn() {
    // Tracing callsite interest is shared by parallel libtest bodies. Keep
    // this capture in its own process while the parent suite stays parallel.
    super::git_fixture::isolated(
        module_path!(),
        "when_false_step_skips_log_below_warn_and_other_skips_keep_warn",
        || {
            use tracing_subscriber::{Layer, layer::SubscriberExt};
            struct StepId(Option<String>);
            impl tracing::field::Visit for StepId {
                fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                    if field.name() == "step_id" {
                        self.0 = Some(value.to_string());
                    }
                }
                fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
            }
            struct Capture(Arc<Mutex<Vec<(String, tracing::Level)>>>);
            impl<S: tracing::Subscriber> Layer<S> for Capture {
                fn on_event(
                    &self,
                    event: &tracing::Event<'_>,
                    _: tracing_subscriber::layer::Context<'_, S>,
                ) {
                    if event.metadata().target() == "orbit.job.step_skipped" {
                        let mut step = StepId(None);
                        event.record(&mut step);
                        self.0.lock().unwrap().push((
                            step.0.expect("step_skipped names its step"),
                            *event.metadata().level(),
                        ));
                    }
                }
            }

            let mut guarded = probe_step("guarded", probe_input('+', 1));
            guarded["when"] = json!("{{ input.flag }} == true");
            let job = job_asset(json!([probe_step("done", probe_input('+', 0)), guarded]));
            let input = json!({ "flag": false });
            let mut resume =
                PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
            resume.step_outputs.insert(0, json!({}));
            resume.step_states.insert(0, JobRunState::Success);

            let skips = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(Capture(skips.clone()));
            let run = tracing::subscriber::with_default(subscriber, || {
                run_graph_job_with_resume(&job, input, Some(&resume))
            });
            assert!(run.succeeded(), "{:?}", run.result);
            assert!(run.host.calls().is_empty(), "neither step dispatches");
            assert_eq!(
                *skips.lock().unwrap(),
                [
                    ("done".to_string(), tracing::Level::WARN),
                    ("guarded".to_string(), tracing::Level::INFO),
                ],
                "a resume skip warns; a false when: guard does not"
            );
        },
    );
}

/// One kind character per branch, worker or iteration: `+` succeeds, `-`
/// fails, `!` panics.
fn probe_input(kind: char, index: usize) -> Value {
    json!({
        "label": format!("item-{index}"),
        "fail": kind == '-',
        "panic": kind == '!',
        "sleep_ms": 0,
    })
}

fn probe_step(id: &str, default_input: Value) -> Value {
    json!({
        "id": id,
        "default_input": default_input,
        "spec": { "type": "deterministic", "action": "probe", "config": {} },
    })
}

fn echo_item_step(id: &str) -> Value {
    probe_step(id, json!({ "value": "{{ item }}" }))
}

fn echo_fan_out_job() -> orbit_types::workflow::JobV2 {
    job_asset(json!([{
        "id": "scatter",
        "fan_out": {
            "items": "{{ input.list }}",
            "max_workers": 2,
            "worker": echo_item_step("worker"),
        },
        "fan_in": { "join": {"mode": "all"} },
    }]))
}

fn echo_loop_job() -> orbit_types::workflow::JobV2 {
    job_asset(json!([{
        "id": "spin",
        "loop": {
            "items": "{{ input.list }}",
            "max_iterations": 8,
            "steps": [echo_item_step("body")],
        },
    }]))
}

fn fan_out_step(max_workers: u32, join: Value) -> Value {
    json!({
        "id": "scatter",
        "fan_out": {
            "items": "{{ input.items }}",
            "max_workers": max_workers,
            "worker": probe_step("worker", item_fields()),
        },
        "fan_in": { "join": join, "collect": "results" },
    })
}

fn loop_job(max_iterations: u32) -> orbit_types::workflow::JobV2 {
    let mut body = item_fields();
    body["stop"] = json!("{{ item.stop }}");
    job_asset(json!([{
        "id": "spin",
        "loop": {
            "items": "{{ input.items }}",
            "max_iterations": max_iterations,
            "break_when": "{{ steps.body.output.stop }} == true",
            "steps": [probe_step("body", body)],
        },
    }]))
}

fn item_fields() -> Value {
    json!({
        "label": "{{ item.label }}",
        "fail": "{{ item.fail }}",
        "panic": "{{ item.panic }}",
        "sleep_ms": "{{ item.sleep_ms }}",
    })
}

/// Load the steps as a job asset, the form a catalog job reaches the engine
/// in. JSON is valid YAML, so the loader reads it unchanged.
pub(crate) fn job_asset(steps: Value) -> orbit_types::workflow::JobV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "graph_fixture" },
        "spec": { "state": "enabled", "kind": "workflow", "steps": steps },
    });
    load_job_asset(&asset.to_string())
        .expect("fixture job asset loads")
        .spec
}

struct GraphRun {
    host: GraphHost,
    result: Result<JobOutcome, DispatchError>,
    events: Vec<V2AuditEvent>,
}

impl GraphRun {
    fn succeeded(&self) -> bool {
        matches!(&self.result, Ok(outcome) if outcome.success)
    }

    fn outcome(&self) -> &JobOutcome {
        self.result.as_ref().expect("job ran to an outcome")
    }

    fn loop_iteration_ends(&self) -> Vec<(u32, bool)> {
        self.events
            .iter()
            .filter_map(|event| match &event.kind {
                V2AuditEventKind::LoopIterationEnd {
                    iteration, broke, ..
                } => Some((*iteration, *broke)),
                _ => None,
            })
            .collect()
    }

    fn did_not_converge(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::LoopDidNotConverge { .. }))
    }
}

fn run_graph_job(job: &orbit_types::workflow::JobV2, input: Value) -> GraphRun {
    run_graph_job_with_resume(job, input, None)
}

fn run_graph_job_with_resume(
    job: &orbit_types::workflow::JobV2,
    input: Value,
    resume: Option<&PipelineState>,
) -> GraphRun {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), "graph-run");
    let host = GraphHost::default();
    let result = execute_job_with_resume(job, input, "graph-run", writer.clone(), &host, resume);
    let events = writer.events_snapshot().expect("persisted audit events");
    GraphRun {
        host,
        result,
        events,
    }
}

/// Empty successful joins checkpoint their outputs; unsuccessful joins must
/// be evaluated again on resume rather than allowing later steps to run.
fn assert_empty_join_resume(
    job: &orbit_types::workflow::JobV2,
    input: Value,
    run: &GraphRun,
    step_id: &str,
    success: bool,
) {
    let checkpoints = run.host.checkpoints.lock().expect("checkpoints");
    assert_eq!(
        checkpoints.contains_key(&0),
        success,
        "{step_id}: checkpoint only successful joins"
    );
    let mut resume = PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
    if let Some(output) = checkpoints.get(&0) {
        resume.step_outputs.insert(0, output.clone());
        resume.step_states.insert(0, JobRunState::Success);
    }
    let resumed = run_graph_job_with_resume(job, input, Some(&resume));
    assert_eq!(
        resumed.succeeded(),
        success,
        "{step_id}: resumed join outcome"
    );
    assert_eq!(resumed.outcome().pipeline[step_id], json!([]));
    if step_id == "scatter" {
        assert_eq!(resumed.outcome().pipeline["results"], json!([]));
    }
    assert_eq!(resumed.host.calls().len(), usize::from(success));
    assert_eq!(
        resumed.events.iter().any(|event| matches!(
            &event.kind, V2AuditEventKind::StepSkipped { step_id: skipped, .. } if skipped == step_id
        )),
        success,
        "{step_id}: resume skips only a successful empty join"
    );
}

/// Ordinary resume keeps the seeded conflict, and step recovery replaces it
/// with the newer one. Final-recovery invalidation is covered by
/// `final_recovery_resume_checkpoints_the_latest_conflict_diagnostics`.
fn incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint() {
    use orbit_common::{ConflictedFile, RecoverableVcsConflict};
    use orbit_types::workflow::activity_job::JobV2StepBody;

    struct DiagnosticHost {
        state: Mutex<PipelineState>,
        conflict: Mutex<Option<RecoverableVcsConflict>>,
        calls: Mutex<Vec<String>>,
    }

    impl orbit_engine::test_support::NullRuntimeHost for DiagnosticHost {
        fn run_deterministic(
            &self,
            action: &str,
            _: &Value,
            _: &Value,
            _: orbit_tools::ToolContext,
        ) -> Result<Value, DispatchError> {
            self.calls.lock().unwrap().push(action.to_string());
            match action {
                "probe_rebase" => {
                    if let Some(conflict) = self.conflict.lock().unwrap().take() {
                        return Err(DispatchError::RecoverableVcsConflict(Box::new(conflict)));
                    }
                    Ok(json!({"rebased": true}))
                }
                "resolve_conflict" => Ok(json!({})),
                _ => panic!("unexpected action {action}"),
            }
        }

        fn checkpoint_step_diagnostic(
            &self,
            _: &str,
            step_id: &str,
            output: &Value,
        ) -> Result<(), DispatchError> {
            self.state
                .lock()
                .unwrap()
                .record_pipeline_output(step_id, output.clone());
            Ok(())
        }

        fn checkpoint_step(
            &self,
            _: &str,
            index: u32,
            step_id: &str,
            output: &Value,
            _: &BTreeMap<String, Value>,
        ) -> Result<(), DispatchError> {
            let mut state = self.state.lock().unwrap();
            state.record_step(index, JobRunState::Success, Some(output.clone()), None);
            state.record_pipeline_output(step_id, output.clone());
            Ok(())
        }
    }

    let conflict = |path: &str, resolvable: bool| RecoverableVcsConflict {
        operation: "git_rebase".into(),
        original_base_sha: "original".into(),
        target_base_sha: "target".into(),
        conflicting_paths: vec![path.into()],
        conflicting_files: vec![ConflictedFile {
            path: path.into(),
            deterministically_resolvable: resolvable,
        }],
        diagnostic: "stopped rebase".into(),
    };
    for new_conflict in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let run_id = "diagnostic-resume";
        let (writer, _, _) = build_writer_and_sinks(root.path(), run_id);
        let mut job = job_asset(json!([{
            "id": "sync_base",
            // A custom action reaches the stub host; the built-in
            // git_rebase dispatches the real VCS implementation instead.
            "spec": {"type": "deterministic", "action": "probe_rebase", "config": {}},
        }]));
        let host = DiagnosticHost {
            state: Mutex::new(PipelineState::new(
                run_id.into(),
                "graph_fixture".into(),
                json!({}),
            )),
            conflict: Mutex::new(Some(conflict("old.rs", false))),
            calls: Mutex::new(Vec::new()),
        };
        let stopped = execute_job_with_resume(&job, json!({}), run_id, writer.clone(), &host, None);
        assert!(
            matches!(stopped, Err(DispatchError::RecoverableVcsConflict(_))),
            "initial conflict must checkpoint diagnostics: {stopped:?}"
        );
        let mut resume = host.state.lock().unwrap().clone();
        assert!(
            resume.step_states.is_empty(),
            "diagnostics do not certify completion"
        );
        assert!(
            resume.step_outputs.is_empty(),
            "diagnostics are not usable checkpoints"
        );
        assert_eq!(
            resume.pipeline["sync_base"]["conflicting_paths"],
            json!(["old.rs"])
        );
        assert_eq!(
            resume.pipeline["sync_base"]["conflicting_files"],
            json!([{
                "path": "old.rs", "deterministically_resolvable": false,
            }])
        );
        // A persisted failed output may contain other fields; none may
        // become a usable checkpoint or survive into the success output.
        resume.pipeline["sync_base"]["failed_only"] = json!("discard me");
        *host.state.lock().unwrap() = resume.clone();
        host.calls.lock().unwrap().clear();

        let mut reads_incomplete = job.clone();
        let JobV2StepBody::Target(target) = &mut reads_incomplete.steps[0].body else {
            panic!("target step");
        };
        target.default_input = Some(json!({"prior": "{{ steps.sync_base.output }}"}));
        let error = execute_job_with_resume(
            &reads_incomplete,
            json!({}),
            run_id,
            writer.clone(),
            &host,
            Some(&resume),
        )
        .unwrap_err();
        assert!(
            matches!(error, DispatchError::JobExecution(ref message) if message.contains("no data recorded for step 'sync_base'")),
            "incomplete output stays outside the template pipeline: {error}"
        );
        assert!(host.calls.lock().unwrap().is_empty());
        assert!(host.state.lock().unwrap().step_states.is_empty());

        if new_conflict {
            *host.conflict.lock().unwrap() = Some(conflict("new.rs", true));
            job.steps[0].recovery_activity = Some("resolve_conflict".into());
            job.steps[0].resolved_recovery_activity = Some(ActivityV2 {
                description: String::new(),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: "resolve_conflict".into(),
                    config: json!({}),
                }),
            });
        }
        let (resumed_writer, _, _) = build_writer_and_sinks(root.path(), run_id);
        let outcome = execute_job_with_resume(
            &job,
            json!({}),
            run_id,
            resumed_writer.clone(),
            &host,
            Some(&resume),
        )
        .unwrap();
        assert!(outcome.success);
        assert_eq!(
            *host.calls.lock().unwrap(),
            if new_conflict {
                vec!["probe_rebase", "resolve_conflict", "probe_rebase"]
            } else {
                vec!["probe_rebase"]
            },
            "an incomplete sync_base must execute; a fresh conflict must recover and rerun"
        );
        let expected = conflict(if new_conflict { "new.rs" } else { "old.rs" }, new_conflict);
        let expected_output = json!({
            "rebased": true,
            "conflicting_paths": expected.conflicting_paths,
            "conflicting_files": expected.conflicting_files,
        });
        assert_eq!(outcome.pipeline["sync_base"], expected_output);
        let state = host.state.lock().unwrap();
        assert_eq!(
            state.pipeline_step_output("sync_base"),
            Some(&expected_output)
        );
        assert_eq!(state.step_outputs.get(&0), Some(&expected_output));
        assert_eq!(state.step_states.get(&0), Some(&JobRunState::Success));
        assert!(
            resumed_writer
                .events_snapshot()
                .unwrap()
                .iter()
                .any(|event| matches!(
                    &event.kind, V2AuditEventKind::StepStarted { step_id } if step_id == "sync_base"
                ))
        );
        assert!(!resumed_writer.events_snapshot().unwrap().iter().any(|event| matches!(
            &event.kind, V2AuditEventKind::StepSkipped { step_id, .. } if step_id == "sync_base"
        )));
    }
}

/// Runs the single `probe` action every graph fixture uses. Its input says
/// what to do: sleep `sleep_ms`, then panic, fail or echo the input back.
#[derive(Default)]
struct GraphHost {
    calls: Mutex<Vec<Value>>,
    checkpoints: Mutex<BTreeMap<u32, Value>>,
    compound_outputs: Mutex<BTreeMap<u32, BTreeMap<String, Value>>>,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

impl GraphHost {
    fn calls(&self) -> Vec<Value> {
        self.calls.lock().expect("call log").clone()
    }

    fn labels(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|input| input["label"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }
}

impl orbit_engine::test_support::NullRuntimeHost for GraphHost {
    fn checkpoint_step(
        &self,
        _run_id: &str,
        step_index: u32,
        _step_id: &str,
        output: &Value,
        compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        self.compound_outputs
            .lock()
            .unwrap()
            .insert(step_index, compound_outputs.clone());
        self.checkpoints
            .lock()
            .expect("checkpoints")
            .insert(step_index, output.clone());
        Ok(())
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        assert_eq!(action, "probe", "graph fixtures dispatch only `probe`");
        self.calls.lock().expect("call log").push(input.clone());
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        let flag = |name: &str| input[name] == json!(true) || input[name] == json!("true");
        let sleep_ms = input["sleep_ms"]
            .as_u64()
            .or_else(|| input["sleep_ms"].as_str().and_then(|ms| ms.parse().ok()))
            .unwrap_or(0);
        std::thread::sleep(Duration::from_millis(sleep_ms));
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if flag("panic") {
            panic!("probe panicked for {}", input["label"]);
        }
        if flag("fail") {
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("probe failed for {}", input["label"]),
            });
        }
        Ok(input.clone())
    }
}

// --------------------------------------------------------------------------
// Before-PR reviewer resilience [ORB-13890]
// --------------------------------------------------------------------------

/// The shipped `review` step's own `retry:` and `recovery_activity:`, driven
/// with a fault-injected reviewer. A transient failure is retried and the
/// step succeeds without recovery; a persistent one exhausts its attempts,
/// gets one `step_failure_recovery` and one re-attempt, and only then fails
/// the run. Each reviewer dispatch — and nothing else — reports its start
/// and end, so backoff and recovery are never charged as review minutes.
fn the_shipped_review_step_retries_then_recovers_a_failing_reviewer() {
    for (failures, succeeds, expected_calls) in [
        (1, true, vec![REVIEWER, REVIEWER]),
        (
            usize::MAX,
            false,
            vec![REVIEWER, REVIEWER, RECOVERY, REVIEWER],
        ),
    ] {
        let audit_root = tempfile::tempdir().expect("audit tempdir");
        let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), "review-run");
        let host = ReviewerHost::failing(failures);
        let result = execute_job_with_resume(
            &shipped_review_step_job(),
            json!({ "task_ids": ["T-1"] }),
            "review-run",
            writer.clone(),
            &host,
            None,
        );
        let events = writer.events_snapshot().expect("persisted audit events");

        assert_eq!(
            matches!(&result, Ok(outcome) if outcome.success),
            succeeds,
            "{failures} failure(s): {result:?}"
        );
        assert_eq!(host.calls(), expected_calls, "{failures} failure(s)");
        assert!(
            events.iter().any(|event| matches!(
                &event.kind,
                V2AuditEventKind::StepRetry { step_id, .. } if step_id == "review"
            )),
            "the reviewer step retried after its first failure"
        );
        let recovered = events.iter().any(|event| {
            matches!(
                &event.kind,
                V2AuditEventKind::StepRecoveryAttempted { step_id, recovery_activity, .. }
                    if step_id == "review" && recovery_activity == RECOVERY
            )
        });
        assert_eq!(recovered, !succeeds, "recovery runs only once retries fail");

        let reviewer_calls = expected_calls
            .iter()
            .filter(|call| **call == REVIEWER)
            .count();
        let invocations = host.invocations();
        assert_eq!(invocations.len(), reviewer_calls * 2);
        for pair in invocations.chunks(2) {
            assert!(matches!(pair[0].event, ReviewerInvocationEvent::Started));
            assert!(matches!(
                pair[1].event,
                ReviewerInvocationEvent::Finished { .. }
            ));
            for request in pair {
                assert_eq!(
                    (
                        request.run_id.as_str(),
                        request.lineage_key.as_str(),
                        request.attempt_id.as_str()
                    ),
                    ("review-run", "lineage-1", "rvw-1")
                );
            }
        }
    }
}

pub(crate) const REVIEWER: &str = "agent_review_repair";
pub(crate) const RECOVERY: &str = "step_failure_recovery";

/// Both dispatch errors and unsuccessful CLI outcomes bypass recovery when
/// the provider is unusable; the CLI-outcome case lives in v2_cli_agent.
fn provider_unavailable_dispatch_errors_do_not_attempt_recovery() {
    struct UnavailableHost;
    impl orbit_engine::test_support::NullRuntimeHost for UnavailableHost {
        fn run_deterministic(
            &self,
            action: &str,
            _: &Value,
            _: &Value,
            _: orbit_tools::ToolContext,
        ) -> Result<Value, DispatchError> {
            assert_eq!(action, "unavailable", "recovery must never dispatch");
            Err(DispatchError::CliInvocationFailed(
                "[provider_unavailable] provider authentication failed".into(),
            ))
        }
    }
    let mut job = job_asset(json!([{
        "id":"implement_one", "recovery_activity":"auth_recovery",
        "spec":{"type":"deterministic", "action":"unavailable", "config":{}}
    }]));
    let mut catalog = V2ActivityCatalog::new();
    catalog.insert(
        "auth_recovery",
        ActivityV2 {
            description: String::new(),
            input_schema_json: Value::Null,
            output_schema_json: Value::Null,
            fs_profile: None,
            spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                action: "unexpected_recovery".into(),
                config: Value::Null,
            }),
        },
    );
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let audit = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(audit.path(), "auth-error");
    let error = execute_job_with_resume(
        &job,
        json!({}),
        "auth-error",
        writer.clone(),
        &UnavailableHost,
        None,
    )
    .unwrap_err();
    assert!(orbit_types::workflow::is_provider_unavailable(
        None,
        Some(&error.to_string())
    ));
    assert!(
        !writer
            .events_snapshot()
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::StepRecoveryAttempted { .. })),
        "an unavailable provider does not consume recovery admission"
    );
}

/// A typed `{kind, evidence}` blocker on `implement_one` ends the bundle.
/// The nested step is configured to retry and recover, a later commit step
/// follows, and the job has final recovery plus a failure activity. None of
/// the retry, step recovery, commit, or final recovery dispatches run. The
/// failure activity is invoked once with `task_blocked_by_agent` and the kind.
fn an_implementer_blocker_ends_implement_one_without_recovery() {
    let marker = orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER;
    let host = ScriptedHost {
        implement_output: json!({
            "summary": "stopped",
            "blocker": {
                "kind": "environment",
                "evidence": "the toolchain the task needs is not installed",
            },
        }),
        calls: Mutex::new(Vec::new()),
    };
    let (outcome, events) = run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
    let actions = host.actions();

    assert!(
        !outcome.success,
        "a declared blocker is not a successful job: {outcome:?}"
    );
    let message = outcome.message.expect("the job records the blocker");
    assert!(
        message.contains(marker) && message.contains("kind=environment"),
        "the job outcome carries the marker and kind: {message}"
    );
    assert_eq!(
        actions,
        vec!["implement".to_string(), "preserve_candidate".to_string()],
        "one implementer dispatch, then the failure activity; no retry, recovery, commit, or final look: {actions:?}"
    );
    let preserve = host
        .calls
        .lock()
        .expect("call log")
        .iter()
        .find(|(action, _)| action == "preserve_candidate")
        .expect("failure activity ran")
        .1
        .clone();
    assert_eq!(preserve["error_code"], "task_blocked_by_agent");
    let error_message = preserve["error_message"].as_str().expect("error message");
    assert!(
        error_message.contains(marker) && error_message.contains("kind=environment"),
        "the failure activity receives the kind: {error_message}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event.kind,
            V2AuditEventKind::StepRetry { .. } | V2AuditEventKind::StepRecoveryAttempted { .. }
        )),
        "a blocker does not retry or enter step recovery: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            &event.kind,
            V2AuditEventKind::StepFinished { step_id, outcome, error_message, .. }
                if step_id == "implement_one"
                    && outcome == "failed"
                    && error_message.as_deref().is_some_and(|message| {
                        message.contains(marker) && message.contains("kind=environment")
                    })
        )),
        "implement_one itself ends with the blocker: {events:?}"
    );
}

/// An Orbit upgrade refused the agent's own `orbit` mid-step. Every spelling
/// agents gave that refusal ends the step like a blocker, with no retry, step
/// recovery or final recovery, but under `upgrade_pending` rather than
/// `task_blocked_by_agent`. The failure activity receives that code, so the
/// handoff keeps the candidate without blocking the task, and a claimed
/// leaf's settlement reads it as transient.
fn an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending() {
    for kind in [
        "upgrade_admission_refused",
        "orbit_upgrade_admission_refused",
        "orbit.generation_switch_pending",
        "upgrade_pending",
    ] {
        let host = ScriptedHost {
            implement_output: json!({
                "blocker": {
                    "kind": kind,
                    "evidence": "upgrade admission refused: cannot record the pending generation \
                                 switch: Operation not permitted (os error 1)",
                },
            }),
            calls: Mutex::new(Vec::new()),
        };
        let (outcome, events) = run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));

        assert!(!outcome.success, "{kind}: {outcome:?}");
        let message = outcome.message.expect("the job records the refusal");
        assert!(
            orbit_types::workflow::is_upgrade_pending(None, Some(&message))
                && !orbit_types::workflow::is_task_blocked_by_agent(None, Some(&message))
                && message.contains(&format!("kind={kind}")),
            "{kind}: the outcome is an upgrade refusal, not a task blocker: {message}"
        );
        assert_eq!(
            orbit_types::workflow::ClaimFailureClass::of_step_failure(None, Some(&message)),
            Some(orbit_types::workflow::ClaimFailureClass::Transient),
            "{kind}: a claimed leaf settles it as transient"
        );
        assert_eq!(
            host.actions(),
            vec!["implement".to_string(), "preserve_candidate".to_string()],
            "{kind}: no retry, recovery, commit or final look"
        );
        let preserve = host
            .calls
            .lock()
            .expect("call log")
            .iter()
            .find(|(action, _)| action == "preserve_candidate")
            .expect("failure activity ran")
            .1
            .clone();
        assert_eq!(preserve["error_code"], "upgrade_pending", "{kind}");
        assert!(
            !events.iter().any(|event| matches!(
                event.kind,
                V2AuditEventKind::StepRetry { .. } | V2AuditEventKind::StepRecoveryAttempted { .. }
            )),
            "{kind}: an upgrade refusal does not retry or enter step recovery: {events:?}"
        );
    }
}

/// A reviewer meets the same refusal as the implementer, so an upgrade
/// refusal ends any agent step. A task blocker stays the implementer's: on
/// another step it is not a stop.
fn an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers() {
    for (kind, stops) in [("conflict", false), ("upgrade_admission_refused", true)] {
        let host = ScriptedHost {
            implement_output: json!({
                "blocker": { "kind": kind, "evidence": "every orbit command was refused" },
            }),
            calls: Mutex::new(Vec::new()),
        };
        let mut job = job_asset(json!([{
            "id": "review",
            "recovery_activity": "step_recovery",
            "retry": { "max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1 },
            "spec": { "type": "deterministic", "action": "implement", "config": {} },
        }]));
        resolve_job_catalog_refs_for_execution(&mut job, &blocker_catalog())
            .expect("resolve recovery");
        let audit = tempfile::tempdir().expect("audit tempdir");
        let (writer, _, _) = build_writer_and_sinks(audit.path(), "review-refused");
        let outcome =
            execute_job_with_resume(&job, json!({}), "review-refused", writer, &host, None)
                .expect("the review step runs to an outcome");
        assert_eq!(outcome.success, !stops, "{kind}: {outcome:?}");
        assert_eq!(host.actions(), vec!["implement".to_string()], "{kind}");
        if stops {
            assert!(
                outcome.message.as_deref().is_some_and(|message| {
                    orbit_types::workflow::is_upgrade_pending(None, Some(message))
                }),
                "{kind}: {outcome:?}"
            );
        }
    }
}

/// A blocker shape that is not `{kind, evidence}` stays ordinary success, and
/// a resolved `agent_implement` target honors a well-formed blocker even when
/// the step id is not `implement_one`.
fn a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is() {
    let malformed = ScriptedHost {
        implement_output: json!({
            "blocker": { "kind": "", "evidence": "missing kind" },
        }),
        calls: Mutex::new(Vec::new()),
    };
    let (outcome, _) = run_blocker_job(&malformed, json!({ "tasks": ["one", "two"] }));
    assert!(
        outcome.success,
        "a malformed blocker does not fail the step: {outcome:?}"
    );
    assert_eq!(
        malformed.actions(),
        vec![
            "implement".to_string(),
            "implement".to_string(),
            "commit".to_string()
        ],
    );

    let named = ScriptedHost {
        implement_output: json!({
            "blocker": { "kind": "conflict", "evidence": "the requirements contradict" },
        }),
        calls: Mutex::new(Vec::new()),
    };
    let mut job = job_asset(json!([{
        "id": "work",
        "recovery_activity": "step_recovery",
        "retry": { "max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1 },
        "spec": { "type": "deterministic", "action": "implement", "config": {} },
    }]));
    resolve_job_catalog_refs_for_execution(&mut job, &blocker_catalog()).expect("resolve recovery");
    // Inline specs have no catalog name until something sets it. The shipped
    // resolver does that for `target: activity:agent_implement`.
    if let orbit_types::workflow::activity_job::JobV2StepBody::Target(target) =
        &mut job.steps[0].body
    {
        target.activity_name = Some("agent_implement".to_string());
    }
    let audit = tempfile::tempdir().expect("audit tempdir");
    let (writer, _, _) = build_writer_and_sinks(audit.path(), "named-blocker");
    let outcome = execute_job_with_resume(&job, json!({}), "named-blocker", writer, &named, None)
        .expect("the named activity runs to an outcome");
    assert!(!outcome.success, "agent_implement honors the blocker");
    assert_eq!(named.actions(), vec!["implement".to_string()]);
    assert!(
        outcome
            .message
            .as_deref()
            .is_some_and(|message| message.contains("kind=conflict")),
        "the kind is on the outcome: {outcome:?}"
    );
}

/// An implementer whose `unfiled_findings` are plain strings, not the declared
/// `{title, description}` objects, fails at `implement_one` [ORB-14927]. The
/// delivery steps after it never run, so nothing is committed, pushed or
/// opened for a handoff that cannot be accepted. Well-formed findings, and
/// output with no findings, still deliver.
fn malformed_unfiled_findings_fail_implement_one_before_delivery() {
    let malformed = [
        json!(["The owner refused this. It needs a follow-up."]),
        json!("not an array"),
        json!([{ "title": "No description" }]),
        json!([{ "title": "  ", "description": "blank title" }]),
        json!([42]),
    ];
    for findings in malformed {
        let host = ScriptedHost {
            implement_output: json!({ "summary": "done", "unfiled_findings": findings }),
            calls: Mutex::new(Vec::new()),
        };
        let (outcome, events) = try_run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
        let actions = host.actions();

        // The step fails, retries and recovers like any failed step; the job
        // ends in error once those are spent.
        let failure = match outcome {
            Ok(outcome) => {
                assert!(
                    !outcome.success,
                    "{findings}: malformed findings do not deliver: {outcome:?}"
                );
                outcome.message.unwrap_or_default()
            }
            Err(error) => error.to_string(),
        };
        assert!(
            failure.contains("unfiled_findings"),
            "{findings}: the failure names the field: {failure}"
        );
        assert!(
            !actions.iter().any(|action| action == "commit"),
            "{findings}: no delivery step runs for the attempt: {actions:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                &event.kind,
                V2AuditEventKind::StepFinished { step_id, outcome, .. }
                    if step_id == "implement_one" && outcome == "error"
            )),
            "{findings}: implement_one itself ends in error"
        );
    }

    for output in [
        json!({ "unfiled_findings": [{
            "title": "A finding",
            "description": "Its detail.",
            "relations": [{ "type": "spawned_from", "target": "ORB-1" }],
        }] }),
        json!({ "unfiled_findings": [] }),
        json!({ "unfiled_findings": null }),
        json!({ "summary": "no findings" }),
    ] {
        let host = ScriptedHost {
            implement_output: output.clone(),
            calls: Mutex::new(Vec::new()),
        };
        let (outcome, _) = run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
        assert!(
            outcome.success,
            "{output}: a valid output delivers: {outcome:?}"
        );
        assert!(host.actions().iter().any(|action| action == "commit"));
    }
}

struct ScriptedHost {
    implement_output: Value,
    calls: Mutex<Vec<(String, Value)>>,
}

impl ScriptedHost {
    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .map(|(action, _)| action.clone())
            .collect()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for ScriptedHost {
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        if action == "implement" {
            Ok(self.implement_output.clone())
        } else {
            Ok(json!({}))
        }
    }
}

fn blocker_catalog() -> V2ActivityCatalog {
    let mut catalog = V2ActivityCatalog::new();
    for name in ["step_recovery", "preserve_candidate", "final_look"] {
        catalog.insert(name.to_string(), scripted_activity(name));
    }
    catalog
}

fn scripted_activity(action: &str) -> ActivityV2 {
    ActivityV2 {
        description: String::new(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

fn run_blocker_job(host: &ScriptedHost, input: Value) -> (JobOutcome, Vec<V2AuditEvent>) {
    let (outcome, events) = try_run_blocker_job(host, input);
    (outcome.expect("the blocker job runs to an outcome"), events)
}

fn try_run_blocker_job(
    host: &ScriptedHost,
    input: Value,
) -> (Result<JobOutcome, DispatchError>, Vec<V2AuditEvent>) {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "blocker_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "failure_activity": "preserve_candidate",
            "final_recovery_activity": "final_look",
            "steps": [{
                "id": "implement_bundle",
                "loop": {
                    "items": "{{ input.tasks }}",
                    "max_iterations": 4,
                    "steps": [{
                        "id": "implement_one",
                        "recovery_activity": "step_recovery",
                        "retry": {
                            "max_attempts": 3,
                            "initial_backoff_ms": 1,
                            "backoff_cap_ms": 1
                        },
                        "spec": { "type": "deterministic", "action": "implement", "config": {} }
                    }]
                }
            }, {
                "id": "commit",
                "spec": { "type": "deterministic", "action": "commit", "config": {} }
            }]
        }
    });
    let mut job = load_job_asset(&asset.to_string())
        .expect("blocker fixture loads")
        .spec;
    resolve_job_catalog_refs_for_execution(&mut job, &blocker_catalog()).expect("resolve hooks");
    let audit = tempfile::tempdir().expect("audit tempdir");
    let (writer, _, _) = build_writer_and_sinks(audit.path(), "blocker-run");
    let outcome = execute_job_with_resume(&job, input, "blocker-run", writer.clone(), host, None);
    let events = writer.events_snapshot().expect("persisted audit events");
    (outcome, events)
}

/// Stub worktree and admission steps followed by the shipped `review` step,
/// whose reviewer and recovery activities resolve to scripted actions. Only
/// the backoff sleep is shortened; attempts and recovery stay as shipped.
fn shipped_review_step_job() -> orbit_types::workflow::JobV2 {
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml"),
    )
    .expect("read the shipped PR pipeline");
    let shipped = load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec;
    let mut review = shipped
        .steps
        .iter()
        .find(|step| step.id == "review")
        .cloned()
        .expect("the shipped PR pipeline has a review step");
    let retry = review
        .retry
        .as_mut()
        .expect("the review step declares retry");
    retry.initial_backoff_ms = 1;
    retry.backoff_cap_ms = 1;

    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": id, "config": {} },
        })
    };
    let mut job = job_asset(json!([stub("worktree"), stub("review_gate_admit")]));
    job.steps.push(review);
    let mut catalog = V2ActivityCatalog::new();
    for name in [REVIEWER, RECOVERY] {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: name.to_string(),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).expect("resolve the review step");
    job
}

/// Serves the stub gate steps, a reviewer that fails its first `failures`
/// dispatches like an unavailable provider, and a recovery that succeeds.
struct ReviewerHost {
    failures_left: Mutex<usize>,
    calls: Mutex<Vec<&'static str>>,
    invocations: Mutex<Vec<ReviewerInvocationRequest>>,
}

impl ReviewerHost {
    fn failing(failures: usize) -> Self {
        Self {
            failures_left: Mutex::new(failures),
            calls: Mutex::default(),
            invocations: Mutex::default(),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("call log").clone()
    }

    fn invocations(&self) -> Vec<ReviewerInvocationRequest> {
        self.invocations.lock().expect("invocations").clone()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for ReviewerHost {
    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "worktree" => Ok(json!({
                "job_run_id": "review-run",
                "workspace_path": "/worktrees/review-run",
            })),
            "review_gate_admit" => Ok(json!({
                "applies": true,
                "decision": "admitted",
                "first_task_id": "T-1",
                "attempt_id": "rvw-1",
                "lineage_key": "lineage-1",
                "manifest_artifact": "review-manifest.json",
                "report_artifact": "review-report.json",
                "reviewer": { "crew": "reviewers" },
            })),
            REVIEWER => {
                self.calls.lock().expect("call log").push(REVIEWER);
                let mut failures_left = self.failures_left.lock().expect("failures");
                if *failures_left == 0 {
                    return Ok(json!({ "summary": "reviewed", "verdict": "accept" }));
                }
                *failures_left = failures_left.saturating_sub(1);
                Err(DispatchError::CliInvocationFailed(
                    "provider overloaded".to_string(),
                ))
            }
            RECOVERY => {
                self.calls.lock().expect("call log").push(RECOVERY);
                Ok(json!({ "recovered": true }))
            }
            other => panic!("unexpected action `{other}`"),
        }
    }

    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        self.invocations
            .lock()
            .expect("invocations")
            .push(request.clone());
        Ok(None)
    }
}

/// Execute the shipped prefix, so an admission refusal must stop the graph
/// before `implement_one`, even when that step has failure recovery enabled.
fn exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation() {
    struct ExhaustedHost {
        calls: Mutex<Vec<String>>,
    }
    impl orbit_engine::test_support::NullRuntimeHost for ExhaustedHost {
        fn run_deterministic(
            &self,
            action: &str,
            _: &Value,
            input: &Value,
            _: orbit_tools::ToolContext,
        ) -> Result<Value, DispatchError> {
            self.calls.lock().unwrap().push(action.into());
            if action == "test_review_gate_admit" {
                assert_eq!(input["preflight"], true);
                return Err(DispatchError::DeterministicActionRefused {
                    action: action.into(),
                    message: "review_budget_exhausted".into(),
                });
            }
            Ok(json!({ "job_run_id": "preflight-run", "workspace_path": "/worktree" }))
        }
    }
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml"),
    )
    .unwrap();
    let shipped = load_job_asset(&shipped).unwrap().spec;
    let mut job = job_asset(json!([]));
    job.steps.extend(
        shipped
            .steps
            .into_iter()
            .take_while(|step| step.id != "commit"),
    );
    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "worktree_setup",
        "review_gate_admit",
        "candidate_resume",
        "agent_implement",
        RECOVERY,
    ] {
        catalog.insert(
            name,
            ActivityV2 {
                description: name.into(),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("test_{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (writer, _, _) = build_writer_and_sinks(root.path(), "preflight-run");
    let host = ExhaustedHost {
        calls: Mutex::new(Vec::new()),
    };
    let result = execute_job_with_resume(
        &job,
        json!({"task_ids": ["fixture-task"], "base_branch": "main", "base_sync": "local"}),
        "preflight-run",
        writer,
        &host,
        None,
    );
    assert!(!matches!(&result, Ok(outcome) if outcome.success));
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["test_worktree_setup", "test_review_gate_admit"],
        "an exhausted lineage consumes no implementer or recovery invocation: {result:?}"
    );
}

fn fan_in_retains_position_aligned_errors_through_successful_join_and_resume() {
    let mut fan = fan_out_step(3, json!({"mode": "any"}));
    fan["fan_in"]["errors"] = json!("worker_errors");
    let job = job_asset(json!([fan, probe_step("after", json!({"label": "after"}))]));
    let input = json!({"items": [probe_input('+', 0), probe_input('-', 1), probe_input('!', 2)]});
    let run = run_graph_job(&job, input.clone());
    assert!(run.succeeded(), "{:?}", run.result);
    let errors = &run.outcome().pipeline["worker_errors"];
    assert_eq!(errors[0], Value::Null);
    assert!(
        errors[1].as_str().unwrap().contains("probe failed"),
        "{errors}"
    );
    assert!(errors[2].as_str().unwrap().contains("panicked"), "{errors}");
    assert!(run.outcome().pipeline["scatter"][1].is_null());
    assert!(run.outcome().pipeline["scatter"][2].is_null());
    let mut resume = PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
    resume.step_outputs = run.host.checkpoints.lock().unwrap().clone();
    resume.compound_outputs = run.host.compound_outputs.lock().unwrap().clone();
    for index in resume.step_outputs.keys() {
        resume.step_states.insert(*index, JobRunState::Success);
    }
    let resumed = run_graph_job_with_resume(&job, input, Some(&resume));
    assert!(resumed.succeeded());
    assert_eq!(resumed.outcome().pipeline["worker_errors"], *errors);
    assert!(
        resumed.host.calls().is_empty(),
        "resume retains errors without redispatching workers"
    );
}

fn a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume() {
    let mut empty = fan_out_step(1, json!({"mode": "all"}));
    empty["id"] = json!("empty");
    empty["fan_out"]["worker"]["id"] = json!("empty_worker");
    empty["when"] = json!("{{ input.empty }} == true");
    empty["fan_in"]["errors"] = json!("worker_errors");
    let mut nonempty = fan_out_step(1, json!({"mode": "any"}));
    nonempty["when"] = json!("{{ input.empty }} == false");
    nonempty["fan_in"]["errors"] = json!("worker_errors");
    let job = job_asset(json!([empty, nonempty]));
    for is_empty in [true, false] {
        let items = if is_empty {
            json!([])
        } else {
            json!([probe_input('+', 0)])
        };
        let input = json!({"empty": is_empty, "items": items});
        let run = run_graph_job(&job, input.clone());
        assert!(run.succeeded());
        let mut resume =
            PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
        resume.step_outputs = run.host.checkpoints.lock().unwrap().clone();
        resume.compound_outputs = run.host.compound_outputs.lock().unwrap().clone();
        for index in resume.step_outputs.keys() {
            resume.step_states.insert(*index, JobRunState::Success);
        }
        let resumed = run_graph_job_with_resume(&job, input, Some(&resume));
        assert!(resumed.succeeded());
        for alias in ["results", "worker_errors"] {
            assert_eq!(
                resumed.outcome().pipeline[alias],
                run.outcome().pipeline[alias],
                "a skipped fan-out never publishes an alias: empty={is_empty}, alias={alias}"
            );
        }
    }
}

fn leaf_completion_records_file_and_store_waits_without_leaking_between_steps() {
    use orbit_common::fs::file_lock::{FileLockOptions, acquire_exclusive_file_lock};
    use orbit_store::Store;
    struct WaitHost {
        root: PathBuf,
        store: Store,
    }
    impl orbit_engine::test_support::NullRuntimeHost for WaitHost {
        fn run_deterministic(
            &self,
            action: &str,
            _: &Value,
            _: &Value,
            _: orbit_tools::ToolContext,
        ) -> Result<Value, DispatchError> {
            match action {
                "file_wait" | "failed_wait" => {
                    let path = self.root.join("held.lock");
                    let _held = acquire_exclusive_file_lock(
                        &path,
                        "fixture holder",
                        FileLockOptions::default(),
                    )
                    .unwrap();
                    let refusal = acquire_exclusive_file_lock(
                        &path,
                        "fixture waiter",
                        FileLockOptions {
                            timeout: Duration::from_millis(25),
                            ..FileLockOptions::default()
                        },
                    )
                    .unwrap_err();
                    assert_eq!(refusal.kind(), std::io::ErrorKind::TimedOut);
                    if action == "failed_wait" {
                        return Err(DispatchError::DeterministicActionFailed {
                            action: action.into(),
                            message: "injected refusal".into(),
                        });
                    }
                }
                "store_wait" => {
                    let connection = self.store.connection();
                    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
                    let holder = std::thread::spawn(move || {
                        let _held = connection.lock().unwrap();
                        ready_tx.send(()).unwrap();
                        waiting_rx.recv().unwrap();
                        std::thread::sleep(Duration::from_millis(25));
                    });
                    ready_rx.recv().unwrap();
                    waiting_tx.send(()).unwrap();
                    self.store.with_transaction(|_| Ok(())).unwrap();
                    holder.join().unwrap();
                }
                _ => {}
            }
            Ok(json!({}))
        }
    }
    for fail in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let host = WaitHost {
            root: root.path().into(),
            store: Store::open_in_memory().unwrap(),
        };
        let (writer, _, _) = build_writer_and_sinks(root.path(), "lock-wait-fixture");
        let mut steps = vec!["file_wait", "store_wait", "quiet"];
        if fail {
            steps.push("failed_wait");
        }
        let job = job_asset(json!(
            steps
                .iter()
                .map(|id| json!({
                    "id": id, "spec": {"type": "deterministic", "action": id, "config": {}}
                }))
                .collect::<Vec<_>>()
        ));
        let outcome = execute_job_with_resume(
            &job,
            json!({}),
            "lock-wait-fixture",
            writer.clone(),
            &host,
            None,
        );
        if fail {
            assert!(outcome.is_err());
        } else {
            assert!(outcome.unwrap().success);
        }
        let finished = writer
            .events_snapshot()
            .unwrap()
            .into_iter()
            .filter_map(|event| match event.kind {
                V2AuditEventKind::StepFinished {
                    step_id,
                    lock_wait_ms,
                    ..
                } => Some((step_id, lock_wait_ms)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        for id in ["file_wait", "store_wait"] {
            assert!(finished[id].unwrap() > 0, "{id}: {finished:?}");
        }
        assert_eq!(
            finished["quiet"],
            Some(0),
            "waits must not leak between leaves"
        );
        if fail {
            assert!(finished["failed_wait"].unwrap() >= 25);
        }
    }
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn runtime_contracts_1() {
    crate::git_fixture::isolated(module_path!(), "runtime_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            (
                "deterministic_reference_dispatches_and_persists_audit_events",
                || deterministic_reference_dispatches_and_persists_audit_events().unwrap(),
            ),
            (
                "a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args",
                a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args,
            ),
            (
                "parallel_join_policy_decides_the_block_from_its_branches",
                parallel_join_policy_decides_the_block_from_its_branches,
            ),
            (
                "fan_out_collects_outputs_in_item_order_within_the_worker_cap",
                fan_out_collects_outputs_in_item_order_within_the_worker_cap,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn runtime_contracts_2() {
    crate::git_fixture::isolated(module_path!(), "runtime_contracts_2", || {
        crate::git_fixture::run_scenarios(&[
            (
                "fan_in_join_policy_decides_the_block_from_its_workers",
                fan_in_join_policy_decides_the_block_from_its_workers,
            ),
            (
                "loop_iterates_until_break_failure_or_budget",
                loop_iterates_until_break_failure_or_budget,
            ),
            (
                "items_expression_rejects_non_array_json",
                items_expression_rejects_non_array_json,
            ),
            (
                "when_false_step_skips_log_below_warn_and_other_skips_keep_warn",
                when_false_step_skips_log_below_warn_and_other_skips_keep_warn,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn runtime_contracts_3() {
    crate::git_fixture::isolated(module_path!(), "runtime_contracts_3", || {
        crate::git_fixture::run_scenarios(&[
            (
                "incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint",
                incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint,
            ),
            (
                "the_shipped_review_step_retries_then_recovers_a_failing_reviewer",
                the_shipped_review_step_retries_then_recovers_a_failing_reviewer,
            ),
            (
                "provider_unavailable_dispatch_errors_do_not_attempt_recovery",
                provider_unavailable_dispatch_errors_do_not_attempt_recovery,
            ),
            (
                "an_implementer_blocker_ends_implement_one_without_recovery",
                an_implementer_blocker_ends_implement_one_without_recovery,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn runtime_contracts_4() {
    crate::git_fixture::isolated(module_path!(), "runtime_contracts_4", || {
        crate::git_fixture::run_scenarios(&[
            (
                "an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending",
                an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending,
            ),
            (
                "an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers",
                an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers,
            ),
            (
                "a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is",
                a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is,
            ),
            (
                "malformed_unfiled_findings_fail_implement_one_before_delivery",
                malformed_unfiled_findings_fail_implement_one_before_delivery,
            ),
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn runtime_contracts_5() {
    crate::git_fixture::isolated(module_path!(), "runtime_contracts_5", || {
        crate::git_fixture::run_scenarios(&[
            (
                "exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation",
                exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation,
            ),
            (
                "fan_in_retains_position_aligned_errors_through_successful_join_and_resume",
                fan_in_retains_position_aligned_errors_through_successful_join_and_resume,
            ),
            (
                "a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume",
                a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume,
            ),
            (
                "leaf_completion_records_file_and_store_waits_without_leaking_between_steps",
                leaf_completion_records_file_and_store_waits_without_leaking_between_steps,
            ),
        ]);
    });
}
