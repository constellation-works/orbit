#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]
//! v2 runtime integration coverage: deterministic binding, `parallel:`/`fan_out:`/`loop:` join
//! semantics through `execute_job_with_resume`, and shipped `review` step retry/recovery.
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^v2_runtime::/)'`.
mod gate_deadline;
use orbit_agent::loop_engine::InMemorySink;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    DispatchError, JobOutcome, ReviewerInvocationRequest, V2AuditWriter, V2DispatchInput,
    V2SqliteSink, dispatch_v2_activity, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, DeterministicSpec, V2AuditEvent, V2AuditEventKind,
};
use orbit_types::workflow::{
    ClaimFailureClass, JobRunState, PipelineState, ReviewerInvocationEvent,
    is_provider_unavailable, is_task_blocked_by_agent, is_upgrade_pending,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
/// [ORB-13115] A deterministic step's binding comes from the dispatch, not its tool arguments.
fn a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args() {
    use orbit_agent::loop_engine::audit::{AuditSink, NullSink};
    use orbit_tools::{ActivityBinding, ToolContext};
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
    let spec = scripted_activity("plugin.tool_call").spec;
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
        let mut seen = host.seen.lock().expect("seen");
        let context = seen.take().expect("a context was recorded");
        context
            .activity_binding
            .expect("an activity context carries its binding")
    };
    let host = BindingHost::default();
    let binding = dispatch(
        &host,
        json!({"task_id": "ORB-7", "tool": "pulsar.publish", "input": {"task_id": "ORB-999", "job_run_id": "jrun-forged"}}),
    );
    let bound = (binding.job_run_id.as_str(), binding.task_id.as_deref());
    let expected_bound = ("jrun-host", Some("ORB-7"));
    assert_eq!(bound, expected_bound, "arguments must not name the binding");
    let binding = dispatch(&host, json!({ "tool": "pulsar.publish" }));
    assert_eq!(binding.job_run_id, "jrun-host");
    assert_eq!(binding.task_id, None, "a step serving no task names none");
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
pub(crate) fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}
/// `parallel:` decides the block from its `join` policy, reports branches in order, and turns a panic into a failed branch.
fn parallel_join_policy_decides_the_block_from_its_branches() {
    let cases = [
        ("all, no branches", join("all", None), "", true),
        ("any, no branches", join("any", None), "", false),
        ("all, every branch succeeds", join("all", None), "+++", true),
        ("all, one branch fails", join("all", None), "+-+", false),
        ("any, one branch succeeds", join("any", None), "-+-", true),
        ("any, every branch fails", join("any", None), "---", false),
        ("quorum 2 of 3 met", join("quorum", Some(2)), "+-+", true),
        ("quorum 2 missed", join("quorum", Some(2)), "-+-", false),
        ("all, one branch panics", join("all", None), "+!", false),
    ];
    for (case, policy, branches, expect_success) in cases {
        let mut branch_steps = Vec::new();
        for (index, kind) in branches.chars().enumerate() {
            branch_steps.push(probe_step(&format!("br_{index}"), probe_input(kind, index)));
        }
        let fan = json!({"id": "fan", "parallel": {"join": policy, "branches": branch_steps}});
        let job = job_asset(json!([fan, probe_step("after", json!({"label": "after"}))]));
        let run = run_graph_job(&job, Value::Null);
        assert_join_gate(&run, case, expect_success);
        let joined = run
            .step_join_outcomes("fan", policy["mode"].as_str().unwrap())
            .unwrap_or_else(|| panic!("{case}: no step.join event"));
        let mut expected = Vec::new();
        for (index, kind) in branches.chars().enumerate() {
            let outcome = if kind == '+' { "success" } else { "error" };
            expected.push((format!("br_{index}"), outcome.to_string()));
        }
        assert_eq!(joined, expected, "{case}: branch order");
        if branches.is_empty() {
            assert_empty_block(&run, case, "fan", expect_success);
            assert_empty_join_resume(&job, Value::Null, &run, "fan", expect_success);
        }
    }
}
/// `fan_out:` collects outputs in item order under the worker cap and the `fan_in.collect` alias.
fn fan_out_collects_outputs_in_item_order_within_the_worker_cap() {
    let mut items = Vec::new();
    for (index, sleep_ms) in [120, 0, 40, 40, 40, 40].iter().enumerate() {
        items.push(json!({"label": format!("item-{index}"), "fail": false, "panic": false, "sleep_ms": sleep_ms}));
    }
    let job = job_asset(json!([fan_out_step(2, join("all", None))]));
    let run = run_graph_job(&job, json!({ "items": items }));
    let outcome = run.outcome();
    assert!(outcome.success);
    let collected = outcome.pipeline["scatter"]
        .as_array()
        .expect("collected array");
    let mut labels = Vec::new();
    for output in collected {
        labels.push(output["label"].as_str().expect("worker echoes its label"));
    }
    let expected_labels = ["item-0", "item-1", "item-2", "item-3", "item-4", "item-5"];
    assert_eq!(
        labels, expected_labels,
        "slowest item still collected first"
    );
    assert_eq!(outcome.pipeline["results"], outcome.pipeline["scatter"]);
    assert_eq!(run.host.calls().len(), 6);
    let peak = run.host.peak_in_flight();
    assert_eq!(peak, 2, "workers overlap within the cap");
}
/// `fan_in.join` judges workers; failed or panicked workers collect as `null` and count on `fanin.joined`.
fn fan_in_join_policy_decides_the_block_from_its_workers() {
    let cases = [
        ("all, no workers", join("all", None), "", true),
        ("any, no workers", join("any", None), "", false),
        ("quorum 1, no workers", join("quorum", Some(1)), "", false),
        ("all, every worker succeeds", join("all", None), "+++", true),
        ("all, one worker fails", join("all", None), "+-+", false),
        ("any, one worker succeeds", join("any", None), "-+-", true),
        ("any, every worker fails", join("any", None), "---", false),
        ("quorum 1 succeeds", join("quorum", Some(1)), "+", true),
        ("quorum 1 fails", join("quorum", Some(1)), "-", false),
        ("quorum exceeds count", join("quorum", Some(2)), "+", false),
        ("quorum 2 met", join("quorum", Some(2)), "+-+", true),
        ("quorum 3 missed", join("quorum", Some(3)), "+-+", false),
        ("all, one worker panics", join("all", None), "+!+", false),
    ];
    for (case, policy, workers, expect_success) in cases {
        let mut items = Vec::new();
        for (index, kind) in workers.chars().enumerate() {
            items.push(probe_input(kind, index));
        }
        let mut steps = vec![fan_out_step(4, policy)];
        steps.push(probe_step("after", json!({"label": "after"})));
        let job = job_asset(json!(steps));
        let input = json!({ "items": items });
        let run = run_graph_job(&job, input.clone());
        assert_join_gate(&run, case, expect_success);
        let worker_count = workers.len() as u32;
        let dispatched = run.fanout_dispatched("scatter", worker_count);
        assert!(dispatched, "{case}: fanout.dispatched count");
        let failed = workers.chars().filter(|kind| *kind != '+').count() as u32;
        let joined_ok = run.fanin_joined(worker_count - failed, failed);
        assert!(joined_ok, "{case}: fanin.joined counts");
        if let Ok(outcome) = &run.result {
            let collected = outcome.pipeline["scatter"].as_array().expect("collected");
            assert_eq!(collected.len(), workers.len(), "{case}");
            let results = &outcome.pipeline["results"];
            let scatter = &outcome.pipeline["scatter"];
            assert_eq!(results, scatter, "{case}");
            for (kind, output) in workers.chars().zip(collected) {
                assert_eq!(output.is_null(), kind != '+', "{case}: {collected:?}");
            }
        }
        if workers.is_empty() {
            assert_empty_block(&run, case, "scatter", expect_success);
            let worker_seen = run.has_worker_state();
            assert!(!worker_seen, "{case}: no worker events when empty");
            assert_empty_join_resume(&job, input, &run, "scatter", expect_success);
        }
    }
}
/// `loop:` stops at `break_when`, failure, or budget, and refuses over-budget lists before running.
fn loop_iterates_until_break_failure_or_budget() {
    let make = |stops: &str| -> Vec<Value> {
        let mut items = Vec::new();
        for (index, stop) in stops.chars().enumerate() {
            items.push(json!({"label": format!("item-{index}"), "stop": stop == 't', "fail": false, "panic": false, "sleep_ms": 0}));
        }
        items
    };
    // (case, max, stops t/f, fail_at, ran, success, ends, not_converged)
    let cases = [
        (
            "breaks",
            5,
            "ftf",
            None,
            2,
            true,
            &[(1, false), (2, true)][..],
            Some(false),
        ),
        ("exhausted", 3, "fff", None, 3, true, &[][..], Some(true)),
        ("body fails", 5, "fff", Some(1), 2, false, &[][..], None),
        ("over", 2, "fff", None, 0, false, &[][..], None),
    ];
    for (case, max, stops, fail_at, ran, success, ends, not_converged) in cases {
        let mut items = make(stops);
        if let Some(index) = fail_at {
            items[index]["fail"] = json!(true);
        }
        let run = run_graph_job(&loop_job(max), json!({ "items": items }));
        assert_eq!(run.succeeded(), success, "{case}: {:?}", run.result);
        let labels: Vec<String> = (0..ran).map(|index| format!("item-{index}")).collect();
        assert_eq!(run.host.labels(), labels, "{case}");
        if !ends.is_empty() {
            let ends_seen = run.loop_iteration_ends();
            assert_eq!(ends_seen.as_slice(), ends, "{case}: break iteration ends");
        }
        if let Some(not_converged) = not_converged {
            assert_eq!(run.did_not_converge(), not_converged, "{case}");
        }
        if case == "over" {
            let no_calls = run.host.calls().is_empty();
            assert!(no_calls, "{case}: refused before iterating");
        }
    }
}
/// Non-array JSON items fail naming `fan_out.items`/`loop.items`; arrays and bare `A, B` still parse.
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
            let Err(DispatchError::JobExecution(message)) = &run.result else {
                panic!("{construct} {case}: not JobExecution: {:?}", run.result)
            };
            let names_field = message.contains(field);
            assert!(names_field, "{construct} {case}: names {field}: {message}");
            let no_calls = run.host.calls().is_empty();
            assert!(no_calls, "{construct} {case}: no worker runs");
        }
    }
    let accepted = [
        ("bare list", json!("A, B"), vec![json!("A"), json!("B")]),
        ("json", json!(["A", "B"]), vec![json!("A"), json!("B")]),
        (
            "mixed",
            json!(["A", {"k": 1}]),
            vec![json!("A"), json!({"k": 1})],
        ),
        ("empty array", json!([]), Vec::new()),
    ];
    for (case, list, items) in accepted {
        let fan_out = run_graph_job(&echo_fan_out_job(), json!({ "list": list.clone() }));
        assert!(fan_out.succeeded(), "fan_out {case}: {:?}", fan_out.result);
        let pipeline = &fan_out.outcome().pipeline;
        let collected = pipeline["scatter"].as_array().expect("collected array");
        let mut values = Vec::new();
        for output in collected {
            values.push(output["value"].clone());
        }
        assert_eq!(values, items, "fan_out {case}: workers run in item order");
        let calls = fan_out.host.calls().len();
        assert_eq!(calls, items.len(), "fan_out {case}: one call per item");
        let loop_run = run_graph_job(&echo_loop_job(), json!({ "list": list }));
        assert!(loop_run.succeeded(), "loop {case}: {:?}", loop_run.result);
        let mut loop_values = Vec::new();
        for input in loop_run.host.calls() {
            loop_values.push(input["value"].clone());
        }
        assert_eq!(loop_values, items, "loop {case}: item order");
    }
}
/// A false `when:` guard skips at INFO; other skips (such as resumed steps) stay at WARN.
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
                        let step_id = step.0.expect("step_skipped names its step");
                        self.0
                            .lock()
                            .unwrap()
                            .push((step_id, *event.metadata().level()));
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
            let capture_run = || run_graph_job_with_resume(&job, input, Some(&resume));
            let run = tracing::subscriber::with_default(subscriber, capture_run);
            assert!(run.succeeded(), "{:?}", run.result);
            assert!(run.host.calls().is_empty(), "neither step dispatches");
            let expected_skips = [
                ("done".to_string(), tracing::Level::WARN),
                ("guarded".to_string(), tracing::Level::INFO),
            ];
            assert_eq!(
                *skips.lock().unwrap(),
                expected_skips,
                "resume warns; guard does not"
            );
        },
    );
}
fn join(mode: &str, n: Option<u32>) -> Value {
    match n {
        Some(n) => json!({"mode": mode, "n": n}),
        None => json!({"mode": mode}),
    }
}
/// One kind character per branch, worker or iteration: `+` succeeds, `-`
/// fails, `!` panics.
fn probe_input(kind: char, index: usize) -> Value {
    json!({"label": format!("item-{index}"), "fail": kind == '-', "panic": kind == '!', "sleep_ms": 0})
}
fn probe_step(id: &str, default_input: Value) -> Value {
    json!({"id": id, "default_input": default_input, "spec": {"type": "deterministic", "action": "probe", "config": {}}})
}
fn echo_item_step(id: &str) -> Value {
    probe_step(id, json!({ "value": "{{ item }}" }))
}
fn echo_fan_out_job() -> orbit_types::workflow::JobV2 {
    job_asset(
        json!([{"id": "scatter", "fan_in": {"join": join("all", None)},
        "fan_out": {"items": "{{ input.list }}", "max_workers": 2, "worker": echo_item_step("worker")}}]),
    )
}
fn echo_loop_job() -> orbit_types::workflow::JobV2 {
    job_asset(
        json!([{"id": "spin", "loop": {"items": "{{ input.list }}", "max_iterations": 8, "steps": [echo_item_step("body")]}}]),
    )
}
fn fan_out_step(max_workers: u32, join: Value) -> Value {
    json!({"id": "scatter", "fan_in": {"join": join, "collect": "results"},
        "fan_out": {"items": "{{ input.items }}", "max_workers": max_workers, "worker": probe_step("worker", item_fields())}})
}
fn loop_job(max_iterations: u32) -> orbit_types::workflow::JobV2 {
    let mut body = item_fields();
    body["stop"] = json!("{{ item.stop }}");
    job_asset(
        json!([{"id": "spin", "loop": {"items": "{{ input.items }}", "max_iterations": max_iterations,
        "break_when": "{{ steps.body.output.stop }} == true", "steps": [probe_step("body", body)]}}]),
    )
}
fn item_fields() -> Value {
    json!({"label": "{{ item.label }}", "fail": "{{ item.fail }}", "panic": "{{ item.panic }}", "sleep_ms": "{{ item.sleep_ms }}"})
}
/// Load steps as a job asset (JSON is valid YAML, so the loader reads it unchanged).
pub(crate) fn job_asset(steps: Value) -> orbit_types::workflow::JobV2 {
    let asset = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "graph_fixture"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": steps}});
    load_job_asset(&asset.to_string())
        .expect("fixture job asset loads")
        .spec
}
/// A catalog mapping each name to a scripted deterministic action (`name -> action`).
fn catalog_with(entries: &[(&str, &str)]) -> V2ActivityCatalog {
    let mut catalog = V2ActivityCatalog::new();
    for (name, action) in entries {
        catalog.insert(name.to_string(), scripted_activity(action));
    }
    catalog
}
fn shipped_pipeline() -> orbit_types::workflow::JobV2 {
    let path = workspace_root().join("crates/orbit-core/assets/jobs/task_pr_pipeline.yaml");
    let shipped = std::fs::read_to_string(path).expect("read the shipped PR pipeline");
    load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec
}
/// Resume state carrying every checkpointed step of `run` as succeeded.
fn checkpoint_resume(run: &GraphRun, input: &Value) -> PipelineState {
    let mut resume = PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
    resume.step_outputs = run.host.checkpoints.lock().expect("checkpoints").clone();
    let compounds = run.host.compound_outputs.lock().expect("compound outputs");
    resume.compound_outputs = compounds.clone();
    for index in resume.step_outputs.keys() {
        resume.step_states.insert(*index, JobRunState::Success);
    }
    resume
}
fn resumed_graph(job: &orbit_types::workflow::JobV2, input: &Value, run: &GraphRun) -> GraphRun {
    run_graph_job_with_resume(job, input.clone(), Some(&checkpoint_resume(run, input)))
}
/// A join gate: the block outcome and whether the following step ran.
fn assert_join_gate(run: &GraphRun, case: &str, expect_success: bool) {
    assert_eq!(run.succeeded(), expect_success, "{case}: {:?}", run.result);
    let after_ran = run.host.labels().iter().any(|label| label == "after");
    assert_eq!(after_ran, expect_success, "{case}: next step on success");
}
fn has_event(events: &[V2AuditEvent], pred: impl Fn(&V2AuditEventKind) -> bool) -> bool {
    events.iter().any(|event| pred(&event.kind))
}

fn det_job(ids: &[&str]) -> orbit_types::workflow::JobV2 {
    job_asset(json!(ids
        .iter()
        .map(|id| json!({"id": id, "spec": {"type": "deterministic", "action": id, "config": {}}}))
        .collect::<Vec<_>>()))
}
fn assert_empty_block(run: &GraphRun, case: &str, step_id: &str, success: bool) {
    assert_eq!(run.outcome().pipeline[step_id], json!([]), "{case}");
    assert_eq!(run.host.calls().len(), usize::from(success), "{case}");
}
fn assert_no_retry_or_recovery(events: &[V2AuditEvent], case: &str) {
    assert!(
        !has_event(events, |kind| matches!(
            kind,
            V2AuditEventKind::StepRetry { .. } | V2AuditEventKind::StepRecoveryAttempted { .. }
        )),
        "{case}: no retry or step recovery: {events:?}"
    );
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
        let mut ends = Vec::new();
        for event in &self.events {
            if let V2AuditEventKind::LoopIterationEnd {
                iteration, broke, ..
            } = &event.kind
            {
                ends.push((*iteration, *broke));
            }
        }
        ends
    }
    fn did_not_converge(&self) -> bool {
        has_event(&self.events, |kind| {
            matches!(kind, V2AuditEventKind::LoopDidNotConverge { .. })
        })
    }
    fn step_join_outcomes(&self, step_id: &str, mode: &str) -> Option<Vec<(String, String)>> {
        for event in &self.events {
            if let V2AuditEventKind::StepJoin {
                step_id: id,
                mode: m,
                branch_outcomes,
            } = &event.kind
                && id == step_id
                && m == mode
            {
                let mut outcomes = Vec::new();
                for branch in branch_outcomes {
                    outcomes.push((branch.branch_id.clone(), branch.outcome.clone()));
                }
                return Some(outcomes);
            }
        }
        None
    }
    fn fanout_dispatched(&self, step_id: &str, count: u32) -> bool {
        has_event(&self.events, |kind| {
            matches!(kind, V2AuditEventKind::FanoutDispatched { step_id: id, worker_count }
                if id == step_id && *worker_count == count)
        })
    }
    fn fanin_joined(&self, collected: u32, failed: u32) -> bool {
        has_event(&self.events, |kind| {
            matches!(kind, V2AuditEventKind::FaninJoined { collected: c, failed: f, .. }
                if *c == collected && *f == failed)
        })
    }
    fn step_skipped(&self, step_id: &str) -> bool {
        has_event(
            &self.events,
            |kind| matches!(kind, V2AuditEventKind::StepSkipped { step_id: id, .. } if id == step_id),
        )
    }
    fn has_worker_state(&self) -> bool {
        has_event(&self.events, |kind| {
            matches!(kind, V2AuditEventKind::WorkerState { .. })
        })
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
    let host = GraphHost::default();
    let (result, events) = run_job_resume(job, input, "graph-run", &host, resume);
    GraphRun {
        host,
        result,
        events,
    }
}

/// Empty successful joins checkpoint their outputs; unsuccessful ones are re-evaluated on resume.
fn assert_empty_join_resume(
    job: &orbit_types::workflow::JobV2,
    input: Value,
    run: &GraphRun,
    step_id: &str,
    success: bool,
) {
    let checkpoints = run.host.checkpoints.lock().expect("checkpoints");
    let checkpointed = checkpoints.contains_key(&0);
    assert_eq!(checkpointed, success, "{step_id}: join checkpoint");
    let mut resume = PipelineState::new("graph-run".into(), "graph_fixture".into(), input.clone());
    if let Some(output) = checkpoints.get(&0) {
        resume.step_outputs.insert(0, output.clone());
        resume.step_states.insert(0, JobRunState::Success);
    }
    drop(checkpoints);
    let resumed = run_graph_job_with_resume(job, input, Some(&resume));
    assert_eq!(resumed.succeeded(), success, "{step_id}: resumed outcome");
    assert_eq!(resumed.outcome().pipeline[step_id], json!([]));
    let alias_ok = step_id != "scatter" || resumed.outcome().pipeline["results"] == json!([]);
    assert!(alias_ok, "scatter resume keeps an empty results alias");
    assert_eq!(resumed.host.calls().len(), usize::from(success));
    let skipped = resumed.step_skipped(step_id);
    assert_eq!(skipped, success, "{step_id}: resume skip");
}
/// Ordinary resume keeps the seeded conflict; step recovery replaces it with the newer one.
fn incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint() {
    use orbit_common::{ConflictedFile, RecoverableVcsConflict};
    use orbit_types::workflow::activity_job::JobV2StepBody;
    struct DiagnosticHost {
        state: Mutex<PipelineState>,
        conflict: Mutex<Option<RecoverableVcsConflict>>,
        calls: Mutex<Vec<String>>,
    }
    impl DiagnosticHost {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
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
                "probe_rebase" => match self.conflict.lock().unwrap().take() {
                    Some(conflict) => {
                        Err(DispatchError::RecoverableVcsConflict(Box::new(conflict)))
                    }
                    None => Ok(json!({"rebased": true})),
                },
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
            let mut state = self.state.lock().unwrap();
            state.record_pipeline_output(step_id, output.clone());
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
        let run_id = "diagnostic-resume";
        // A custom action reaches the stub host; the built-in git_rebase
        // dispatches the real VCS implementation instead.
        let mut job = job_asset(
            json!([{"id": "sync_base", "spec": {"type": "deterministic", "action": "probe_rebase", "config": {}}} ]),
        );
        let initial_state = PipelineState::new(run_id.into(), "graph_fixture".into(), json!({}));
        let host = DiagnosticHost {
            state: Mutex::new(initial_state),
            conflict: Mutex::new(Some(conflict("old.rs", false))),
            calls: Mutex::new(Vec::new()),
        };
        let (stopped, _) = run_job_resume(&job, json!({}), run_id, &host, None);
        let stopped_conflict = matches!(stopped, Err(DispatchError::RecoverableVcsConflict(_)));
        assert!(stopped_conflict, "conflict checkpoints: {stopped:?}");
        let mut resume = host.state.lock().unwrap().clone();
        let empty = resume.step_states.is_empty() && resume.step_outputs.is_empty();
        assert!(empty, "diagnostics neither certify nor checkpoint");
        let paths = &resume.pipeline["sync_base"]["conflicting_paths"];
        assert_eq!(paths, &json!(["old.rs"]));
        let files = &resume.pipeline["sync_base"]["conflicting_files"];
        let expected_files = json!([{"path": "old.rs", "deterministically_resolvable": false}]);
        assert_eq!(files, &expected_files);
        // A persisted failed output may contain other fields; none may
        // become a usable checkpoint or survive into the success output.
        resume.pipeline["sync_base"]["failed_only"] = json!("discard me");
        *host.state.lock().unwrap() = resume.clone();
        host.calls.lock().unwrap().clear();
        let mut reads = job.clone();
        let JobV2StepBody::Target(target) = &mut reads.steps[0].body else {
            panic!("target step");
        };
        target.default_input = Some(json!({"prior": "{{ steps.sync_base.output }}"}));
        let error = run_job_resume(&reads, json!({}), run_id, &host, Some(&resume))
            .0
            .unwrap_err();
        let names_missing = matches!(error, DispatchError::JobExecution(ref message) if message.contains("no data recorded for step 'sync_base'"));
        assert!(names_missing, "outside the template pipeline: {error}");
        assert!(host.calls().is_empty());
        assert!(host.state.lock().unwrap().step_states.is_empty());
        if new_conflict {
            *host.conflict.lock().unwrap() = Some(conflict("new.rs", true));
            job.steps[0].recovery_activity = Some("resolve_conflict".into());
            job.steps[0].resolved_recovery_activity = Some(scripted_activity("resolve_conflict"));
        }
        let (resumed, events) = run_job_resume(&job, json!({}), run_id, &host, Some(&resume));
        let outcome = resumed.unwrap();
        assert!(outcome.success);
        let mut expected_calls = vec!["probe_rebase"];
        if new_conflict {
            expected_calls.extend(["resolve_conflict", "probe_rebase"]);
        }
        assert_eq!(host.calls(), expected_calls, "sync_base reruns; recovers");
        let expected = conflict(if new_conflict { "new.rs" } else { "old.rs" }, new_conflict);
        let expected_output = json!({"rebased": true, "conflicting_paths": expected.conflicting_paths, "conflicting_files": expected.conflicting_files});
        assert_eq!(outcome.pipeline["sync_base"], expected_output);
        let state = host.state.lock().unwrap();
        let recorded = state.pipeline_step_output("sync_base");
        assert_eq!(recorded, Some(&expected_output));
        assert_eq!(state.step_outputs.get(&0), Some(&expected_output));
        assert_eq!(state.step_states.get(&0), Some(&JobRunState::Success));
        assert!(has_event(&events, |kind| matches!(
            kind, V2AuditEventKind::StepStarted { step_id } if step_id == "sync_base"
        )));
        assert!(!has_event(&events, |kind| matches!(
            kind, V2AuditEventKind::StepSkipped { step_id, .. } if step_id == "sync_base"
        )));
    }
}
/// Runs the `probe` action: sleep `sleep_ms`, then panic, fail or echo the input back.
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
        let mut labels = Vec::new();
        for input in self.calls() {
            labels.push(input["label"].as_str().unwrap_or_default().to_string());
        }
        labels
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
        let mut compounds = self.compound_outputs.lock().unwrap();
        compounds.insert(step_index, compound_outputs.clone());
        let mut checkpoints = self.checkpoints.lock().expect("checkpoints");
        checkpoints.insert(step_index, output.clone());
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
        assert!(!flag("panic"), "probe panicked for {}", input["label"]);
        if flag("fail") {
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("probe failed for {}", input["label"]),
            });
        }
        Ok(input.clone())
    }
}
/// The shipped `review` step retries, then recovers, a fault-injected reviewer; only reviewer dispatches report invocation start/end.
fn the_shipped_review_step_retries_then_recovers_a_failing_reviewer() {
    for (failures, succeeds, expected_calls) in [
        (1, true, vec![REVIEWER, REVIEWER]),
        (
            usize::MAX,
            false,
            vec![REVIEWER, REVIEWER, RECOVERY, REVIEWER],
        ),
    ] {
        let host = ReviewerHost::failing(failures);
        let (result, events) = run_job_resume(
            &shipped_review_step_job(),
            json!({ "task_ids": ["T-1"] }),
            "review-run",
            &host,
            None,
        );
        let succeeded = matches!(&result, Ok(outcome) if outcome.success);
        assert_eq!(succeeded, succeeds, "{failures} failure(s): {result:?}");
        let calls = host.calls();
        assert_eq!(calls, expected_calls, "{failures} failure(s)");
        let reviewer_calls = calls.iter().filter(|call| **call == REVIEWER).count();
        let retried = has_event(&events, |kind| {
            matches!(
                kind,
                V2AuditEventKind::StepRetry { step_id, .. } if step_id == "review"
            )
        });
        assert!(retried, "the reviewer step retried after its first failure");
        let recovered = has_event(&events, |kind| {
            matches!(
                kind,
                V2AuditEventKind::StepRecoveryAttempted { step_id, recovery_activity, .. }
                    if step_id == "review" && recovery_activity == RECOVERY
            )
        });
        assert_eq!(recovered, !succeeds, "recovery runs only once retries fail");
        let invocations = host.invocations();
        assert_eq!(invocations.len(), reviewer_calls * 2);
        for pair in invocations.chunks(2) {
            assert!(matches!(pair[0].event, ReviewerInvocationEvent::Started));
            let finished = matches!(pair[1].event, ReviewerInvocationEvent::Finished { .. });
            assert!(finished);
            for request in pair {
                assert_eq!(request.run_id, "review-run");
                assert_eq!(request.lineage_key, "lineage-1");
                assert_eq!(request.attempt_id, "rvw-1");
            }
        }
    }
}
pub(crate) const REVIEWER: &str = "agent_review_repair";
pub(crate) const RECOVERY: &str = "step_failure_recovery";
/// Provider-unavailable dispatch errors bypass recovery (the CLI-outcome case lives in v2_cli_agent).
fn provider_unavailable_dispatch_errors_do_not_attempt_recovery() {
    let host = StubHost {
        calls: Mutex::new(Vec::new()),
        respond: |action: &str, _: &Value| {
            assert_eq!(action, "unavailable", "recovery must never dispatch");
            Err(DispatchError::CliInvocationFailed(
                "[provider_unavailable] provider authentication failed".into(),
            ))
        },
    };
    let mut job = job_asset(
        json!([{"id": "implement_one", "recovery_activity": "auth_recovery", "spec": {"type": "deterministic", "action": "unavailable", "config": {}}} ]),
    );
    let catalog = catalog_with(&[("auth_recovery", "unexpected_recovery")]);
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let (result, events) = run_job_resume(&job, json!({}), "auth-error", &host, None);
    let error = result.unwrap_err();
    let unavailable = is_provider_unavailable(None, Some(&error.to_string()));
    assert!(unavailable, "provider-unavailable: {error}");
    let recovered = has_event(&events, |kind| {
        matches!(kind, V2AuditEventKind::StepRecoveryAttempted { .. })
    });
    assert!(!recovered, "provider-unavailable consumes no recovery");
}
/// A typed blocker on `implement_one` ends the bundle with no retry, recovery, commit or final recovery; the failure activity gets `task_blocked_by_agent` and the kind.
fn an_implementer_blocker_ends_implement_one_without_recovery() {
    let marker = orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER;
    let (outcome, events, host) = run_blocker_case(
        "declared blocker",
        json!({"summary": "stopped", "blocker": {"kind": "environment", "evidence": "the toolchain the task needs is not installed"}}),
    );
    let message = outcome.message.expect("the job records the blocker");
    assert!(
        message.contains(marker) && message.contains("kind=environment"),
        "the job outcome carries the marker and kind: {message}"
    );
    let preserve = host.preserve_call();
    assert_eq!(preserve["error_code"], "task_blocked_by_agent");
    let error_message = preserve["error_message"].as_str().expect("error message");
    assert!(
        error_message.contains(marker) && error_message.contains("kind=environment"),
        "the failure activity receives the kind: {error_message}"
    );
    let finished_blocked = has_event(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepFinished { step_id, outcome, error_message, .. }
                if step_id == "implement_one"
                    && outcome == "failed"
                    && error_message.as_deref().is_some_and(|message| {
                        message.contains(marker) && message.contains("kind=environment")
                    })
        )
    });
    assert!(
        finished_blocked,
        "implement_one ends with the blocker: {events:?}"
    );
}
/// Every spelling of an upgrade refusal ends the step like a blocker, but under `upgrade_pending` (transient for claimed leaves).
fn an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending() {
    for kind in [
        "upgrade_admission_refused",
        "orbit_upgrade_admission_refused",
        "orbit.generation_switch_pending",
        "upgrade_pending",
    ] {
        let (outcome, _events, host) = run_blocker_case(
            kind,
            json!({"blocker": {"kind": kind, "evidence": "upgrade admission refused: cannot record the pending generation switch: Operation not permitted (os error 1)"}}),
        );
        let message = outcome.message.expect("the job records the refusal");
        let kind_tag = format!("kind={kind}");
        let upgrade = is_upgrade_pending(None, Some(&message))
            && !is_task_blocked_by_agent(None, Some(&message))
            && message.contains(&kind_tag);
        assert!(
            upgrade,
            "{kind}: upgrade refusal, not a task blocker: {message}"
        );
        let class = ClaimFailureClass::of_step_failure(None, Some(&message));
        let transient = Some(ClaimFailureClass::Transient);
        assert_eq!(class, transient, "{kind}: claimed leaf is transient");
        let preserve = host.preserve_call();
        assert_eq!(preserve["error_code"], "upgrade_pending", "{kind}");
    }
}
/// An upgrade refusal ends any agent step; a task blocker only the implementer's.
fn an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers() {
    for (kind, stops) in [("conflict", false), ("upgrade_admission_refused", true)] {
        let host = ScriptedHost::new(json!({
            "blocker": { "kind": kind, "evidence": "every orbit command was refused" },
        }));
        let outcome = run_retry_step(&host, "review", "review-refused", None);
        assert_eq!(outcome.success, !stops, "{kind}: {outcome:?}");
        host.expect_actions(&["implement"], kind);
        if stops {
            let pending = outcome
                .message
                .as_deref()
                .is_some_and(|message| is_upgrade_pending(None, Some(message)));
            assert!(pending, "{kind}: {outcome:?}");
        }
    }
}
/// A malformed blocker stays ordinary success; a resolved `agent_implement` target honors a well-formed one under any step id.
fn a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is() {
    let malformed = ScriptedHost::new(json!({
        "blocker": { "kind": "", "evidence": "missing kind" },
    }));
    let (outcome, _) = run_blocker_job(&malformed, json!({ "tasks": ["one", "two"] }));
    assert!(
        outcome.success,
        "malformed blocker stays success: {outcome:?}"
    );
    malformed.expect_actions(&["implement", "implement", "commit"], "malformed blocker");
    let named = ScriptedHost::new(json!({
        "blocker": { "kind": "conflict", "evidence": "the requirements contradict" },
    }));
    let outcome = run_retry_step(&named, "work", "named-blocker", Some("agent_implement"));
    assert!(!outcome.success, "agent_implement honors the blocker");
    named.expect_actions(&["implement"], "named blocker");
    let has_kind = outcome
        .message
        .as_deref()
        .is_some_and(|message| message.contains("kind=conflict"));
    assert!(has_kind, "the kind is on the outcome: {outcome:?}");
}
/// Malformed `unfiled_findings` fail at `implement_one` before delivery [ORB-14927]; well-formed or absent findings still deliver.
fn malformed_unfiled_findings_fail_implement_one_before_delivery() {
    let malformed = [
        json!(["The owner refused this. It needs a follow-up."]),
        json!("not an array"),
        json!([{ "title": "No description" }]),
        json!([{ "title": "  ", "description": "blank title" }]),
        json!([42]),
    ];
    for findings in malformed {
        let case = findings.to_string();
        assert_malformed_field_fails(
            &case,
            json!({ "summary": "done", "unfiled_findings": findings }),
            "unfiled_findings",
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
        assert_valid_output_delivers(output);
    }
}
/// Malformed claimed artifact arrays fail at `implement_one` before delivery; valid, absent and null arrays still deliver.
fn malformed_claimed_artifacts_fail_implement_one_before_delivery() {
    for field in ["task_artifacts", "no_diff_artifacts"] {
        for artifacts in [
            json!([{ "path": "report.json", "name": ".orbit/tmp/report.json" }]),
            json!([{ "path": "report.json" }]),
            json!([{ "path": "report.json", "source_path": ".orbit/tmp/report.json", "extra": true }]),
            json!([{ "path": 42, "source_path": ".orbit/tmp/report.json" }]),
            json!([{ "path": "report.json", "source_path": 42 }]),
            json!({ "path": "report.json", "source_path": ".orbit/tmp/report.json" }),
        ] {
            let case = artifacts.to_string();
            let failure = assert_malformed_field_fails(&case, json!({ field: artifacts }), field);
            assert!(
                failure.contains(orbit_types::workflow::CONTRACT_REFUSAL_MARKER),
                "{case}: refusal carries the contract marker: {failure}"
            );
            assert!(
                failure.contains("[{\"path\": ..., \"source_path\": ...}]"),
                "{case}: refusal names the expected shape: {failure}"
            );
        }
    }

    for output in [
        json!({
            "task_artifacts": [{ "path": "report.json", "source_path": ".orbit/tmp/report.json" }],
            "no_diff_artifacts": [{ "path": "evidence.json", "source_path": ".orbit/tmp/evidence.json" }]
        }),
        json!({
            "task_artifacts": [{ "path": "report.json", "source_path": ".orbit/tmp/report.json" }]
        }),
        json!({
            "no_diff_artifacts": [{ "path": "evidence.json", "source_path": ".orbit/tmp/evidence.json" }]
        }),
        json!({ "task_artifacts": [], "no_diff_artifacts": [] }),
        json!({ "task_artifacts": null, "no_diff_artifacts": null }),
        json!({}),
    ] {
        assert_valid_output_delivers(output);
    }
}
/// Malformed `regression_from` fails at `implement_one` before delivery, naming the omit-or-`null` empty case [ORB-17993]; absent, `null`, `[]` and valid IDs still deliver.
fn malformed_regression_from_fails_implement_one_before_delivery() {
    for regression_from in [
        json!("not filed; no captured task"),
        json!(["ORB-1", "not a task"]),
        json!(["ORB-1", 42]),
        json!([42]),
        json!({ "target": "ORB-1" }),
        json!(7),
    ] {
        let case = regression_from.to_string();
        let failure = assert_malformed_field_fails(
            &case,
            json!({ "summary": "done", "regression_from": regression_from }),
            "regression_from",
        );
        // The wording pin is warranted by F2026-10-567: the old refusal told
        // the implementer to return a culprit task ID even when the right
        // answer was to file no relation.
        let names_empty = failure.contains("omit") && failure.contains("null");
        assert!(
            names_empty,
            "{case}: refusal names the empty case [ORB-17993]"
        );
        let keeps_shape = failure.contains("<culprit task ID>");
        assert!(keeps_shape, "{case}: keeps culprit-list shape [ORB-17993]");
    }
    for output in [
        json!({ "summary": "no culprit" }),
        json!({ "summary": "done", "regression_from": Value::Null }),
        json!({ "summary": "done", "regression_from": [] }),
        json!({ "summary": "done", "regression_from": ["ORB-1"] }),
    ] {
        assert_valid_output_delivers(output);
    }
}
/// A host that records every action and answers with `respond`.
struct StubHost<F: Fn(&str, &Value) -> Result<Value, DispatchError> + Send + Sync> {
    calls: Mutex<Vec<String>>,
    respond: F,
}
impl<F: Fn(&str, &Value) -> Result<Value, DispatchError> + Send + Sync>
    orbit_engine::test_support::NullRuntimeHost for StubHost<F>
{
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls.lock().unwrap().push(action.to_string());
        (self.respond)(action, input)
    }
}
struct ScriptedHost {
    implement_output: Value,
    calls: Mutex<Vec<(String, Value)>>,
}
impl ScriptedHost {
    fn new(implement_output: Value) -> Self {
        Self {
            implement_output,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn preserve_call(&self) -> Value {
        for (action, input) in self.calls.lock().expect("call log").iter() {
            if action == "preserve_candidate" {
                return input.clone();
            }
        }
        panic!("failure activity ran")
    }
    fn expect_actions(&self, expected: &[&str], case: &str) {
        let actions = self.actions();
        let refs: Vec<&str> = actions.iter().map(String::as_str).collect();
        assert_eq!(refs, expected, "{case}: dispatch sequence");
    }
    fn actions(&self) -> Vec<String> {
        let mut actions = Vec::new();
        for (action, _) in self.calls.lock().expect("call log").iter() {
            actions.push(action.clone());
        }
        actions
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
/// One retrying, recovering `implement` step, optionally resolved under a
/// catalog activity name (as the shipped resolver sets for
/// `target: activity:agent_implement`).
fn run_retry_step(
    host: &ScriptedHost,
    id: &str,
    run_id: &str,
    activity: Option<&str>,
) -> JobOutcome {
    let mut job = job_asset(json!([{"id": id, "recovery_activity": "step_recovery",
        "retry": {"max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1},
        "spec": {"type": "deterministic", "action": "implement", "config": {}}} ]));
    resolve_job_catalog_refs_for_execution(&mut job, &blocker_catalog()).expect("resolve recovery");
    if let Some(name) = activity
        && let orbit_types::workflow::activity_job::JobV2StepBody::Target(target) =
            &mut job.steps[0].body
    {
        target.activity_name = Some(name.to_string());
    }
    run_job_resume(&job, json!({}), run_id, host, None)
        .0
        .expect("the step runs to an outcome")
}
fn blocker_catalog() -> V2ActivityCatalog {
    catalog_with(&[
        ("step_recovery", "step_recovery"),
        ("preserve_candidate", "preserve_candidate"),
        ("final_look", "final_look"),
    ])
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
/// Run a job on a fresh audit writer; return its outcome and persisted events.
fn run_job_resume(
    job: &orbit_types::workflow::JobV2,
    input: Value,
    run_id: &str,
    host: &impl orbit_engine::test_support::NullRuntimeHost,
    resume: Option<&PipelineState>,
) -> (Result<JobOutcome, DispatchError>, Vec<V2AuditEvent>) {
    let audit = tempfile::tempdir().expect("audit tempdir");
    let (writer, _, _) = build_writer_and_sinks(audit.path(), run_id);
    let outcome = execute_job_with_resume(job, input, run_id, writer.clone(), host, resume);
    let events = writer.events_snapshot().expect("persisted audit events");
    (outcome, events)
}
/// A blocker output ends the bundle: one implementer dispatch, then the
/// failure activity, with no retry, recovery, commit, or final look.
fn run_blocker_case(case: &str, output: Value) -> (JobOutcome, Vec<V2AuditEvent>, ScriptedHost) {
    let host = ScriptedHost::new(output);
    let (outcome, events) = run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
    assert!(!outcome.success, "{case}: {outcome:?}");
    host.expect_actions(&["implement", "preserve_candidate"], case);
    assert_no_retry_or_recovery(&events, case);
    (outcome, events, host)
}
fn run_blocker_job(host: &ScriptedHost, input: Value) -> (JobOutcome, Vec<V2AuditEvent>) {
    let (outcome, events) = try_run_blocker_job(host, input);
    (outcome.expect("the blocker job runs to an outcome"), events)
}
fn try_run_blocker_job(
    host: &ScriptedHost,
    input: Value,
) -> (Result<JobOutcome, DispatchError>, Vec<V2AuditEvent>) {
    let asset = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "blocker_fixture"},
        "spec": {"state": "enabled", "kind": "workflow", "failure_activity": "preserve_candidate",
            "final_recovery_activity": "final_look", "steps": [
                {"id": "implement_bundle", "loop": {"items": "{{ input.tasks }}", "max_iterations": 4, "steps": [
                    {"id": "implement_one", "recovery_activity": "step_recovery",
                     "retry": {"max_attempts": 3, "initial_backoff_ms": 1, "backoff_cap_ms": 1},
                     "spec": {"type": "deterministic", "action": "implement", "config": {}}}]}},
                {"id": "commit", "spec": {"type": "deterministic", "action": "commit", "config": {}}}]}});
    let mut job = load_job_asset(&asset.to_string())
        .expect("blocker fixture loads")
        .spec;
    resolve_job_catalog_refs_for_execution(&mut job, &blocker_catalog()).expect("resolve hooks");
    run_job_resume(&job, input, "blocker-run", host, None)
}
/// A malformed implementer field fails at `implement_one` before delivery;
/// returns the failure text for field-specific wording checks. The step
/// fails, retries and recovers like any failed step; the job ends in error
/// once those are spent.
fn assert_malformed_field_fails(case: &str, output: Value, field: &str) -> String {
    let host = ScriptedHost::new(output);
    let (outcome, events) = try_run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
    let failure = match outcome {
        Ok(outcome) => {
            let delivered = outcome.success;
            assert!(!delivered, "{case}: malformed {field} does not deliver");
            outcome.message.unwrap_or_default()
        }
        Err(error) => error.to_string(),
    };
    let names_field = failure.contains(field);
    assert!(names_field, "{case}: failure names {field}: {failure}");
    let committed = host.actions().iter().any(|action| action == "commit");
    assert!(
        !committed,
        "{case}: no delivery step runs: {:?}",
        host.actions()
    );
    let implement_failed = has_event(&events, |kind| {
        matches!(
            kind,
            V2AuditEventKind::StepFinished { step_id, outcome, .. }
                if step_id == "implement_one" && outcome == "error"
        )
    });
    assert!(
        implement_failed,
        "{case}: implement_one itself ends in error"
    );
    failure
}
fn assert_valid_output_delivers(output: Value) {
    let host = ScriptedHost::new(output.clone());
    let (outcome, _) = run_blocker_job(&host, json!({ "tasks": ["one", "two"] }));
    assert!(
        outcome.success,
        "{output}: a valid output delivers: {outcome:?}"
    );
    assert!(host.actions().iter().any(|action| action == "commit"));
}
/// Stub worktree and admission steps followed by the shipped `review` step,
/// whose reviewer and recovery activities resolve to scripted actions. Only
/// the backoff sleep is shortened; attempts and recovery stay as shipped.
fn shipped_review_step_job() -> orbit_types::workflow::JobV2 {
    let shipped = shipped_pipeline();
    let found = shipped.steps.iter().find(|step| step.id == "review");
    let mut review = found
        .cloned()
        .expect("the shipped PR pipeline has a review step");
    let retry_option = review.retry.as_mut();
    let retry = retry_option.expect("the review step declares retry");
    retry.initial_backoff_ms = 1;
    retry.backoff_cap_ms = 1;
    let mut job = det_job(&["worktree", "review_gate_admit"]);
    job.steps.push(review);
    let catalog = catalog_with(&[(REVIEWER, REVIEWER), (RECOVERY, RECOVERY)]);
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
            "worktree" => {
                Ok(json!({"job_run_id": "review-run", "workspace_path": "/worktrees/review-run"}))
            }
            "review_gate_admit" => Ok(
                json!({"applies": true, "decision": "admitted", "first_task_id": "T-1", "attempt_id": "rvw-1", "lineage_key": "lineage-1", "manifest_artifact": "review-manifest.json", "report_artifact": "review-report.json", "reviewer": {"crew": "reviewers"}}),
            ),
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
/// An admission refusal in the shipped prefix stops the graph before `implement_one`.
fn exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation() {
    let shipped = shipped_pipeline();
    let mut job = job_asset(json!([]));
    let prefix = shipped
        .steps
        .into_iter()
        .take_while(|step| step.id != "commit");
    job.steps.extend(prefix);
    let catalog = catalog_with(&[
        ("worktree_setup", "test_worktree_setup"),
        ("review_gate_admit", "test_review_gate_admit"),
        ("candidate_resume", "test_candidate_resume"),
        ("agent_implement", "test_agent_implement"),
        (RECOVERY, "test_step_failure_recovery"),
    ]);
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    let host = StubHost {
        calls: Mutex::new(Vec::new()),
        respond: |action: &str, input: &Value| {
            if action == "test_review_gate_admit" {
                assert_eq!(input["preflight"], true);
                return Err(DispatchError::DeterministicActionRefused {
                    action: action.into(),
                    message: "review_budget_exhausted".into(),
                });
            }
            Ok(json!({ "job_run_id": "preflight-run", "workspace_path": "/worktree" }))
        },
    };
    let (result, _) = run_job_resume(
        &job,
        json!({"task_ids": ["fixture-task"], "base_branch": "main", "base_sync": "local"}),
        "preflight-run",
        &host,
        None,
    );
    assert!(!matches!(&result, Ok(outcome) if outcome.success));
    let calls = host.calls.lock().unwrap();
    let expected = ["test_worktree_setup", "test_review_gate_admit"];
    assert_eq!(*calls, expected, "exhausted: no implementer or recovery");
}
fn fan_in_retains_position_aligned_errors_through_successful_join_and_resume() {
    let mut fan = fan_out_step(3, join("any", None));
    fan["fan_in"]["errors"] = json!("worker_errors");
    let job = job_asset(json!([fan, probe_step("after", json!({"label": "after"}))]));
    let input = json!({"items": [probe_input('+', 0), probe_input('-', 1), probe_input('!', 2)]});
    let run = run_graph_job(&job, input.clone());
    assert!(run.succeeded(), "{:?}", run.result);
    let errors = &run.outcome().pipeline["worker_errors"];
    assert_eq!(errors[0], Value::Null);
    let second_error = errors[1].as_str().unwrap();
    assert!(second_error.contains("probe failed"), "{errors}");
    assert!(errors[2].as_str().unwrap().contains("panicked"), "{errors}");
    assert!(run.outcome().pipeline["scatter"][1].is_null());
    assert!(run.outcome().pipeline["scatter"][2].is_null());
    let resumed = resumed_graph(&job, &input, &run);
    assert!(resumed.succeeded());
    assert_eq!(resumed.outcome().pipeline["worker_errors"], *errors);
    let no_calls = resumed.host.calls().is_empty();
    assert!(no_calls, "resume retains errors, no redispatch");
}
fn a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume() {
    let mut empty = fan_out_step(1, join("all", None));
    empty["id"] = json!("empty");
    empty["fan_out"]["worker"]["id"] = json!("empty_worker");
    empty["when"] = json!("{{ input.empty }} == true");
    empty["fan_in"]["errors"] = json!("worker_errors");
    let mut nonempty = fan_out_step(1, join("any", None));
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
        let resumed = resumed_graph(&job, &input, &run);
        assert!(resumed.succeeded());
        for alias in ["results", "worker_errors"] {
            let resumed_alias = &resumed.outcome().pipeline[alias];
            let run_alias = &run.outcome().pipeline[alias];
            assert_eq!(
                resumed_alias, run_alias,
                "skipped fan-out keeps {alias}: empty={is_empty}"
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
    /// Hold `held.lock`, then prove a second acquire times out after 25 ms.
    fn file_wait_times_out(root: &std::path::Path) {
        let path = root.join("held.lock");
        let _held =
            acquire_exclusive_file_lock(&path, "fixture holder", FileLockOptions::default())
                .unwrap();
        let opts = FileLockOptions {
            timeout: Duration::from_millis(25),
            ..FileLockOptions::default()
        };
        let refusal = acquire_exclusive_file_lock(&path, "fixture waiter", opts).unwrap_err();
        assert_eq!(refusal.kind(), std::io::ErrorKind::TimedOut);
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
                    file_wait_times_out(&self.root);
                    if action == "failed_wait" {
                        return Err(DispatchError::DeterministicActionFailed {
                            action: action.into(),
                            message: "injected refusal".into(),
                        });
                    }
                }
                "store_wait" => {
                    let connection = self.store.connection();
                    let barrier = Arc::new(std::sync::Barrier::new(2));
                    let held = barrier.clone();
                    let holder = std::thread::spawn(move || {
                        let _held = connection.lock().unwrap();
                        held.wait();
                        std::thread::sleep(Duration::from_millis(25));
                    });
                    barrier.wait();
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
        let store = Store::open_in_memory().unwrap();
        let host = WaitHost {
            root: root.path().into(),
            store,
        };
        let mut steps = vec!["file_wait", "store_wait", "quiet"];
        steps.extend(fail.then_some("failed_wait"));
        let job = det_job(&steps);
        let (outcome, events) = run_job_resume(&job, json!({}), "lock-wait-fixture", &host, None);
        if fail {
            assert!(outcome.is_err());
        } else {
            assert!(outcome.unwrap().success);
        }
        let mut finished = BTreeMap::new();
        for event in events {
            if let V2AuditEventKind::StepFinished {
                step_id,
                lock_wait_ms,
                ..
            } = event.kind
            {
                finished.insert(step_id, lock_wait_ms);
            }
        }
        for id in ["file_wait", "store_wait"] {
            assert!(finished[id].unwrap() > 0, "{id}: {finished:?}");
        }
        let quiet_wait = finished["quiet"];
        assert_eq!(quiet_wait, Some(0), "waits must not leak between leaves");
        if fail {
            assert!(finished["failed_wait"].unwrap() >= 25);
        }
    }
}
/// A fan-out nested under another (directly or through a parallel branch) is refused before any worker runs.
#[test]
fn nested_fan_out_is_rejected_before_any_worker_runs() {
    let inner = json!({"id": "inner", "fan_in": {"join": join("all", None)},
        "fan_out": {"items": "{{ input.list }}", "max_workers": 2, "worker": echo_item_step("leaf")}});
    for worker in [
        inner.clone(),
        json!({"id": "fork", "parallel": {"branches": [inner], "join": join("all", None)}}),
    ] {
        let job = job_asset(
            json!([{"id": "outer", "fan_in": {"join": join("all", None)},
            "fan_out": {"items": "{{ input.list }}", "max_workers": 2, "worker": worker}}]),
        );
        let run = run_graph_job(&job, json!({"list": ["A", "B"]}));
        let Err(DispatchError::JobValidation(message)) = &run.result else {
            panic!("expected JobValidation, got {:?}", run.result)
        };
        assert!(message.contains("nested fan-outs"), "{message}");
        assert!(run.host.calls().is_empty(), "no worker runs");
    }
}
macro_rules! runtime_contracts {
    ($name:ident: $($row:ident),+ $(,)?) => {
        #[test]
        fn $name() {
            crate::git_fixture::isolated(module_path!(), stringify!($name), || {
                crate::git_fixture::run_scenarios(&[$((stringify!($row), $row as fn())),+]);
            });
        }
    };
}
runtime_contracts!(runtime_contracts_1: a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args, parallel_join_policy_decides_the_block_from_its_branches, fan_out_collects_outputs_in_item_order_within_the_worker_cap);
runtime_contracts!(runtime_contracts_2: fan_in_join_policy_decides_the_block_from_its_workers, loop_iterates_until_break_failure_or_budget, items_expression_rejects_non_array_json, when_false_step_skips_log_below_warn_and_other_skips_keep_warn);
runtime_contracts!(runtime_contracts_3: incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint, the_shipped_review_step_retries_then_recovers_a_failing_reviewer, provider_unavailable_dispatch_errors_do_not_attempt_recovery, an_implementer_blocker_ends_implement_one_without_recovery);
runtime_contracts!(runtime_contracts_4: an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending, an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers, a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is, malformed_unfiled_findings_fail_implement_one_before_delivery, malformed_claimed_artifacts_fail_implement_one_before_delivery, malformed_regression_from_fails_implement_one_before_delivery);
runtime_contracts!(runtime_contracts_5: exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation, fan_in_retains_position_aligned_errors_through_successful_join_and_resume, a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume, leaf_completion_records_file_and_store_waits_without_leaking_between_steps);
