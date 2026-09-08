//! Sibling tests for `identity.rs` (per docs/design-patterns/test_layout.md).
//!
//! Agent/model identity resolution used to reach through the v1
//! `ActivityExecutorRegistry`. [ORB-10395] deleted that registry, so the lookup
//! now reads the executor def store directly. These tests pin that wiring: a
//! `model_pair_override` seeded into the store must be observable through the
//! public `RuntimeHost::resolved_agent_model_pair` surface.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::derive_cost_usd;
use orbit_engine::RuntimeHost;
use orbit_store::InvocationQuery;
use orbit_types::identity::AgentModelPair;
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workflow::{ExecutorDef, ExecutorType, ModelPairOverride};
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber, span};

use crate::OrbitRuntime;

fn executor_def(name: &str, model_pair_override: Option<ModelPairOverride>) -> ExecutorDef {
    let now = Utc::now();
    ExecutorDef {
        name: name.to_string(),
        executor_type: ExecutorType::DirectAgent,
        command: Some(name.to_string()),
        args: Vec::new(),
        stdout_format: None,
        model_pair_override,
        model_flag: None,
        timeout_seconds: None,
        env: HashMap::new(),
        sandbox: None,
        allow_fallback: false,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

#[test]
fn resolved_agent_model_pair_reads_the_executor_def_store() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    runtime
        .upsert_executor_def(&executor_def(
            "claude",
            Some(ModelPairOverride {
                strong: "claude-orchestrator".to_string(),
                weak: "claude-helper".to_string(),
            }),
        ))
        .expect("seed executor def");

    assert_eq!(
        RuntimeHost::resolved_agent_model_pair(&runtime, "claude"),
        Some(AgentModelPair::new("claude-orchestrator", "claude-helper"))
    );
}

#[test]
fn resolved_agent_model_pair_is_none_without_an_override() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    runtime
        .upsert_executor_def(&executor_def("codex", None))
        .expect("seed executor def");

    assert_eq!(
        RuntimeHost::resolved_agent_model_pair(&runtime, "codex"),
        None
    );
}

#[test]
fn resolved_agent_model_pair_is_none_for_an_unregistered_executor() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    assert_eq!(
        RuntimeHost::resolved_agent_model_pair(&runtime, "not-registered"),
        None
    );
}

const GROK_LEDGER_MISMATCH_WARNING: &str = "provider-reported model differs from requested model";

fn seed_running_job_run(runtime: &OrbitRuntime, job_id: &str) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now(), None, None)
        .expect("insert job run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    run.run_id
}

fn grok_usage() -> TokenUsage {
    TokenUsage {
        input: 1_000,
        output: 250,
        ..TokenUsage::default()
    }
}

fn capture_invocation_warnings<F, T>(f: F) -> (T, Vec<CapturedWarning>)
where
    F: FnOnce() -> T,
{
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = WarningCaptureSubscriber {
        events: Arc::clone(&events),
        next_span_id: AtomicU64::new(1),
    };
    let result = tracing::subscriber::with_default(subscriber, f);
    let events = events.lock().expect("events lock").clone();
    (result, events)
}

#[derive(Debug, Clone)]
struct CapturedWarning {
    target: String,
    message: String,
}

struct WarningCaptureSubscriber {
    events: Arc<Mutex<Vec<CapturedWarning>>>,
    next_span_id: AtomicU64,
}

impl Subscriber for WarningCaptureSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "orbit.core.invocation"
    }

    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visitor = MessageCapture::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .expect("events lock")
            .push(CapturedWarning {
                target: event.metadata().target().to_string(),
                message: visitor.message,
            });
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}
}

#[derive(Default)]
struct MessageCapture {
    message: String,
}

impl Visit for MessageCapture {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_empty() {
            self.message = format!("{value:?}");
        }
    }
}

fn has_provider_model_mismatch_warning(events: &[CapturedWarning]) -> bool {
    events.iter().any(|event| {
        event.target == "orbit.core.invocation"
            && event.message.contains(GROK_LEDGER_MISMATCH_WARNING)
    })
}

#[test]
fn grok_build_ledger_model_is_stored_as_the_requested_public_id() {
    // Live Grok Build JSON (`grok` 1.0.5+) carries modelUsage key
    // `grok-4.6-build` for `--model grok-4.6`. Parser extraction keeps the
    // ledger key; ingest identity must persist the priced public menu id.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = seed_running_job_run(&runtime, "grok_ledger_job");
    let usage = grok_usage();
    let trace = InvocationTrace {
        usage: usage.clone(),
        provider_model: Some("grok-4.6-build".to_string()),
        provider_cost_usd: Some(0.0123),
        ..InvocationTrace::default()
    };

    let ((), warnings) = capture_invocation_warnings(|| {
        RuntimeHost::persist_invocation_trace(
            &runtime,
            &run_id,
            "implement_one",
            "grok",
            Some("grok-4.6"),
            &serde_json::json!({ "task_id": "ORB-10970" }),
            &trace,
        )
        .expect("persist grok ledger ingest");
    });

    assert!(
        !has_provider_model_mismatch_warning(&warnings),
        "grok-4.6 vs grok-4.6-build must not warn; captured={warnings:?}"
    );

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run_id),
            limit: 1,
            ..InvocationQuery::default()
        })
        .expect("query invocation records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].model.as_deref(), Some("grok-4.6"));
    let derived = records[0].derived_cost_usd.expect("grok-4.6 is priced");
    let expected =
        derive_cost_usd("grok-4.6", records[0].ts, &usage).expect("shipped grok-4.6 row");
    assert!(
        (derived - expected).abs() < 1e-12,
        "derived={derived} expected={expected}"
    );
    assert_eq!(
        derive_cost_usd("grok-4.6-build", records[0].ts, &usage),
        None,
        "the usage-ledger id itself is not a priced row"
    );
}

#[test]
fn grok_build_ledger_alias_does_not_warn_for_matching_public_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for (requested, provider) in [
        ("grok-4.6", "grok-4.6-build"),
        ("grok-4.5", "grok-4.5-build"),
    ] {
        let (stored, warnings) = capture_invocation_warnings(|| {
            runtime
                .invocation_agent_model_identity(
                    "grok",
                    Some(requested),
                    Some(provider),
                    "jrun-grok-ledger",
                    "implement_one",
                )
                .1
        });
        assert_eq!(
            stored.as_deref(),
            Some(requested),
            "requested={requested} provider={provider}"
        );
        assert!(
            !has_provider_model_mismatch_warning(&warnings),
            "requested={requested} provider={provider} captured={warnings:?}"
        );
    }
}

#[test]
fn grok_build_ledger_mismatch_across_public_versions_still_warns() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let (stored, warnings) = capture_invocation_warnings(|| {
        runtime
            .invocation_agent_model_identity(
                "grok",
                Some("grok-4.6"),
                Some("grok-4.5-build"),
                "jrun-grok-drift",
                "implement_one",
            )
            .1
    });
    assert_eq!(stored.as_deref(), Some("grok-4.5-build"));
    assert!(
        has_provider_model_mismatch_warning(&warnings),
        "requested grok-4.6 vs ledger grok-4.5-build is real drift; captured={warnings:?}"
    );
}
