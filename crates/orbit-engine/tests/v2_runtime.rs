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
//! events, and job assets with `parallel:`, `fan_out:` and `loop:` blocks
//! keep their join semantics when run through `execute_job_with_resume`.
//!
//! Runs under `cargo nextest run -p orbit-engine --test v2_runtime`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_agent::loop_engine::InMemorySink;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ResolvedCliExecutor, RuntimeHost, V2AuditWriter, V2DispatchInput,
    V2SqliteSink, dispatch_v2_activity, execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{ActivityV2, V2AuditEvent, V2AuditEventKind};
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
