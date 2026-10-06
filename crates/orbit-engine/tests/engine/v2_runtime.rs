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

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ResolvedCliExecutor, ReviewerInvocationRequest, RuntimeHost,
    V2AuditWriter, V2DispatchInput, V2SqliteSink, dispatch_v2_activity, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::ReviewerInvocationEvent;
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, DeterministicSpec, V2AuditEvent, V2AuditEventKind,
};
use serde_json::{Value, json};

#[test]
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
#[test]
fn a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args() {
    use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
    use orbit_tools::{ActivityBinding, ToolContext};
    use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec};

    #[derive(Default)]
    struct BindingHost {
        seen: Mutex<Option<ToolContext>>,
    }

    impl RuntimeHost for BindingHost {
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

impl RuntimeHost for EchoHost {
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

fn build_writer_and_sinks(
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

fn workspace_root() -> PathBuf {
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
#[test]
fn parallel_join_policy_decides_the_block_from_its_branches() {
    let cases = [
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
        }]));
        let run = run_graph_job(&job, Value::Null);

        assert_eq!(run.succeeded(), expect_success, "{case}: {:?}", run.result);
        let joined = run
            .events
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::StepJoin {
                    branch_outcomes, ..
                } => Some(
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
    }
}

/// `fan_out:` runs one worker per item under the `max_workers` cap and
/// collects outputs in item order (not completion order) under both the step
/// id and the `fan_in.collect` alias.
#[test]
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
#[test]
fn fan_in_join_policy_decides_the_block_from_its_workers() {
    let cases = [
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
        let job = job_asset(json!([fan_out_step(4, join)]));
        let run = run_graph_job(&job, json!({ "items": items }));

        assert_eq!(run.succeeded(), expect_success, "{case}: {:?}", run.result);
        let failed = workers.chars().filter(|kind| *kind != '+').count() as u32;
        assert!(
            run.events.iter().any(|event| matches!(
                &event.kind,
                V2AuditEventKind::FaninJoined { collected, failed: joined_failed, .. }
                    if *collected == 3 - failed && *joined_failed == failed
            )),
            "{case}: fanin.joined counts collected and failed workers"
        );
        if let Ok(outcome) = &run.result {
            let collected = outcome.pipeline["scatter"].as_array().expect("collected");
            for (kind, output) in workers.chars().zip(collected) {
                assert_eq!(output.is_null(), kind != '+', "{case}: {collected:?}");
            }
        }
    }
}

/// `loop:` iterates its items in order, stops at the first `break_when`
/// match, reports a loop that exhausted its budget without converging, stops
/// at the first failed body, and refuses more items than `max_iterations`
/// before running any of them.
#[test]
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
#[test]
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
fn job_asset(steps: Value) -> orbit_types::workflow::JobV2 {
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
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let (writer, _envelope, _inner) = build_writer_and_sinks(audit_root.path(), "graph-run");
    let host = GraphHost::default();
    let result = execute_job_with_resume(job, input, "graph-run", writer.clone(), &host, None);
    let events = writer.events_snapshot().expect("persisted audit events");
    GraphRun {
        host,
        result,
        events,
    }
}

/// Runs the single `probe` action every graph fixture uses. Its input says
/// what to do: sleep `sleep_ms`, then panic, fail or echo the input back.
#[derive(Default)]
struct GraphHost {
    calls: Mutex<Vec<Value>>,
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

impl RuntimeHost for GraphHost {
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
#[test]
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
            assert!(matches!(
                pair[0].event,
                ReviewerInvocationEvent::Started { timeout_seconds } if timeout_seconds > 0
            ));
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

/// Run the shipped completion/re-review steps as one job graph. The
/// deterministic completion stub reports that the published, reviewed head
/// needs rebasing; the fake reviewer then fixes and certifies the new head,
/// owner validation reruns on the reviewer commit [ORB-13989], and the
/// pipeline republishes and completes it. This catches broken step wiring or
/// template references that action-level tests cannot see.
#[test]
fn shipped_completion_rebases_re_reviews_and_completes_the_new_head() {
    let audit_root = tempfile::tempdir().expect("audit tempdir");
    let (writer, _envelope, _inner) =
        build_writer_and_sinks(audit_root.path(), "complete-review-run");
    let host = CompletionReviewHost::default();
    let result = execute_job_with_resume(
        &shipped_completion_review_job(),
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "completion": "done",
        }),
        "complete-review-run",
        writer,
        &host,
        None,
    );

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let calls = host.calls();
    let actions = calls
        .iter()
        .map(|(action, _)| action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            "test_stub_worktree",
            "test_stub_commit",
            "test_stub_prepare_branch",
            "test_stub_sync_base",
            "test_stub_review_gate_admit",
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_push",
            "test_stub_pr_open",
            "test_stub_promote_tasks",
            "test_stub_pr_complete",
            "test_stub_review_gate_admit",
            "test_stub_agent_review_repair",
            "test_stub_review_gate_settle",
            "test_stub_candidate_validate",
            "test_stub_git_push",
            "test_stub_pr_complete",
        ],
        "a completion conflict must enter the shipped re-review route before the second merge"
    );

    let admissions = calls
        .iter()
        .filter(|(action, _)| action == "test_stub_review_gate_admit")
        .map(|(_, input)| input.clone())
        .collect::<Vec<_>>();
    assert_eq!(admissions.len(), 2);
    assert!(admissions[0].get("re_review_after").is_none());
    assert_eq!(admissions[1]["re_review_after"], "complete_pr");

    let reviewer_inputs = calls
        .iter()
        .filter(|(action, _)| action == "test_stub_agent_review_repair")
        .map(|(_, input)| input.clone())
        .collect::<Vec<_>>();
    assert_eq!(reviewer_inputs.len(), 2);
    assert_eq!(reviewer_inputs[0]["attempt_id"], "rvw-first");
    assert_eq!(reviewer_inputs[1]["attempt_id"], "rvw-re-review");

    let revalidations = calls
        .iter()
        .filter(|(action, _)| action == "test_stub_candidate_validate")
        .map(|(_, input)| input.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        revalidations.len(),
        1,
        "only the re-review's reviewer commit is revalidated"
    );
    assert_eq!(revalidations[0]["base_sha"], "rebased-base");
    assert_eq!(
        revalidations[0]["ownership_base_sha"],
        "rebased-implementation"
    );

    let completion_inputs = calls
        .iter()
        .filter(|(action, _)| action == "test_stub_pr_complete")
        .map(|(_, input)| input.clone())
        .collect::<Vec<_>>();
    assert_eq!(completion_inputs.len(), 2);
    assert_eq!(completion_inputs[0]["reviewed_head_sha"], "candidate");
    assert_eq!(completion_inputs[1]["reviewed_head_sha"], "rebased-head");
    assert_eq!(completion_inputs[1]["published_head_sha"], "rebased-head");

    let invocations = host.invocations();
    assert_eq!(
        invocations.len(),
        4,
        "both reviewer runs record their bounds"
    );
    assert_eq!(invocations[0].attempt_id, "rvw-first");
    assert_eq!(invocations[2].attempt_id, "rvw-re-review");
    assert!(matches!(
        invocations[0].event,
        ReviewerInvocationEvent::Started { .. }
    ));
    assert!(matches!(
        invocations[1].event,
        ReviewerInvocationEvent::Finished { .. }
    ));
    assert!(matches!(
        invocations[2].event,
        ReviewerInvocationEvent::Started { .. }
    ));
    assert!(matches!(
        invocations[3].event,
        ReviewerInvocationEvent::Finished { .. }
    ));
}

const REVIEWER: &str = "agent_review_repair";
const RECOVERY: &str = "step_failure_recovery";

/// Both dispatch errors and unsuccessful CLI outcomes bypass recovery when
/// the provider is unusable; the CLI-outcome case lives in v2_cli_agent.
#[test]
fn provider_unavailable_dispatch_errors_do_not_attempt_recovery() {
    struct UnavailableHost;
    impl RuntimeHost for UnavailableHost {
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
#[test]
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
            V2AuditEventKind::StepFinished { step_id, outcome, error_message }
                if step_id == "implement_one"
                    && outcome == "failed"
                    && error_message.as_deref().is_some_and(|message| {
                        message.contains(marker) && message.contains("kind=environment")
                    })
        )),
        "implement_one itself ends with the blocker: {events:?}"
    );
}

/// A blocker shape that is not `{kind, evidence}` stays ordinary success, and
/// a resolved `agent_implement` target honors a well-formed blocker even when
/// the step id is not `implement_one`.
#[test]
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

impl RuntimeHost for ScriptedHost {
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
    let outcome = execute_job_with_resume(&job, input, "blocker-run", writer.clone(), host, None)
        .expect("the blocker job runs to an outcome");
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

/// The shipped pipeline slice that reaches completion and can enter its
/// re-review branch, with all external activities resolved to the test host.
fn shipped_completion_review_job() -> orbit_types::workflow::JobV2 {
    let shipped = std::fs::read_to_string(
        workspace_root().join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml"),
    )
    .expect("read the shipped PR pipeline");
    let shipped = load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec;
    let find_step = |id: &str| {
        shipped
            .steps
            .iter()
            .find(|step| step.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("the shipped PR pipeline has `{id}`"))
    };
    let stub = |id: &str| {
        json!({
            "id": id,
            "spec": { "type": "deterministic", "action": format!("test_stub_{id}"), "config": {} },
        })
    };
    let stubs = |ids: &[&str]| ids.iter().map(|id| stub(id)).collect::<Vec<_>>();
    // Keep the graph boundary under test while replacing unrelated VCS and
    // task-store effects with deterministic stub activities.
    let prefix = stubs(&["worktree", "commit", "prepare_branch", "sync_base"]);
    let mut job = job_asset(json!(prefix));
    for id in [
        "review_gate_admit",
        "review",
        "review_gate_settle",
        "review_validate",
    ] {
        job.steps.push(find_step(id));
    }
    let middle = stubs(&["push", "pr_open", "promote_tasks"]);
    job.steps.extend(job_asset(json!(middle)).steps);
    job.steps.push(find_step("complete_pr"));
    for id in [
        "re_review_gate_admit",
        "re_review",
        "re_review_gate_settle",
        "re_review_validate",
        "re_push",
        "complete_reviewed_pr",
    ] {
        job.steps.push(find_step(id));
    }

    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "worktree",
        "commit",
        "prepare_branch",
        "sync_base",
        "review_gate_admit",
        REVIEWER,
        "review_gate_settle",
        "candidate_validate",
        "push",
        "git_push",
        "pr_open",
        "promote_tasks",
        "pr_complete",
        "pr_conflict_recovery",
        RECOVERY,
    ] {
        catalog.insert(
            name,
            ActivityV2 {
                description: format!("scripted `{name}`"),
                input_schema_json: Value::Null,
                output_schema_json: Value::Null,
                fs_profile: None,
                spec: ActivityV2Spec::Deterministic(DeterministicSpec {
                    action: format!("test_stub_{name}"),
                    config: Value::Null,
                }),
            },
        );
    }
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("resolve the shipped completion and review steps");
    job
}

#[derive(Default)]
struct CompletionReviewHost {
    calls: Mutex<Vec<(String, Value)>>,
    invocations: Mutex<Vec<ReviewerInvocationRequest>>,
}

impl CompletionReviewHost {
    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().expect("call log").clone()
    }

    fn invocations(&self) -> Vec<ReviewerInvocationRequest> {
        self.invocations.lock().expect("invocations").clone()
    }
}

impl RuntimeHost for CompletionReviewHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        let output = match action {
            "test_stub_worktree" => json!({
                "job_run_id": "complete-review-run",
                "workspace_path": "/worktrees/complete-review-run",
            }),
            "test_stub_commit" => json!({ "skipped_no_diff_expected": false }),
            "test_stub_prepare_branch" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": "candidate",
                "commits_behind": 0,
                "sync_required": false,
            }),
            "test_stub_sync_base" => json!({
                "head": "candidate-branch",
                "head_sha": "candidate",
                "base": "main",
                "base_ref": "origin/main",
                "base_sha": "base-sha",
                "remote_sha": "candidate",
                "commits_behind": 0,
                "sync_required": false,
                "rewritten": false,
            }),
            "test_stub_review_gate_admit" => {
                let re_review = input.get("re_review_after").is_some();
                json!({
                    "applies": true,
                    "first_task_id": "T-1",
                    "attempt_id": if re_review { "rvw-re-review" } else { "rvw-first" },
                    "lineage_key": "lineage-1",
                    "manifest_artifact": "review-manifest.json",
                    "report_artifact": "review-report.json",
                    "reviewer": { "crew": "reviewers" },
                })
            }
            "test_stub_agent_review_repair" => {
                json!({ "summary": "reviewed", "verdict": "accept" })
            }
            "test_stub_review_gate_settle" => {
                let attempt = input
                    .pointer("/admission/attempt_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let re_review = attempt == "rvw-re-review";
                json!({
                    "gate": "passed",
                    "reviewed_head_sha": if re_review { "rebased-head" } else { "candidate" },
                    "reviewed_base_sha": if re_review { "rebased-base" } else { "base-sha" },
                    "implementation_head_sha": if re_review { "rebased-implementation" } else { "candidate" },
                    "reviewer_fixed": re_review,
                    "review_fixes": "",
                })
            }
            "test_stub_push" => json!({ "local_sha": "candidate" }),
            "test_stub_git_push" => json!({
                "local_sha": if input.get("branch").and_then(Value::as_str) == Some("rebased-branch") {
                    "rebased-head"
                } else {
                    "candidate"
                },
            }),
            "test_stub_pr_open" => {
                json!({ "pr_number": "41", "pr_url": "https://example.invalid/41" })
            }
            "test_stub_promote_tasks" => json!({ "promoted": true }),
            "test_stub_candidate_validate" => json!({ "decision": "passed" }),
            "test_stub_pr_complete"
                if input.get("reviewed_head_sha").and_then(Value::as_str) == Some("candidate") =>
            {
                json!({
                    "re_review_required": true,
                    "rebased": {
                        "head": "rebased-branch",
                        "head_sha": "rebased-head",
                        "base": "main",
                        "base_ref": "origin/main",
                        "base_sha": "rebased-base",
                        "remote_sha_before": "candidate",
                        "head_sha_before": "candidate",
                        "rewritten": true,
                    },
                    "completed_task_ids": [],
                    "skipped_task_ids": [],
                })
            }
            "test_stub_pr_complete" => json!({
                "re_review_required": false,
                "merge": { "merged": true },
                "completed_task_ids": ["T-1"],
                "skipped_task_ids": [],
            }),
            other => panic!("unexpected action `{other}`"),
        };
        Ok(output)
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

impl RuntimeHost for ReviewerHost {
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

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some("system".to_string())
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
#[test]
fn exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation() {
    struct ExhaustedHost {
        calls: Mutex<Vec<String>>,
    }
    impl RuntimeHost for ExhaustedHost {
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
