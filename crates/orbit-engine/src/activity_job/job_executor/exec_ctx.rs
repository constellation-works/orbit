// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use super::*;

/// Outputs of completed steps keyed by step id (or a fan-in `collect` alias),
/// stored in the `{ "output": raw }` shape the template engine reads. One
/// `Arc` is shared by every template context built from it, so evaluation
/// never rebuilds or deep-clones the map; a write after a snapshot was taken
/// copies on write, while a write with no live snapshot mutates in place.
pub(super) type PipelineSteps = Arc<HashMap<String, Value>>;

pub(super) struct ExecCtx<'a> {
    pub(super) run_id: String,
    pub(super) audit: Arc<V2AuditWriter>,
    pub(super) host: &'a dyn RuntimeHost,
    pub(super) input: Value,
    pub(super) pipeline: Arc<Mutex<PipelineSteps>>,
    pub(super) recovery_activity: Option<ResolvedRecoveryActivity>,
    pub(super) failure_activity: Option<ResolvedRecoveryActivity>,
    /// `Some(value)` inside a fan-out worker. Rendered into template context
    /// as `{{ item }}`.
    pub(super) item: Option<Value>,
    pub(super) iteration: Option<u32>,
}

impl ExecCtx<'_> {
    /// Resolved task id for the activity input, if any. Threaded onto every
    /// job-lifecycle tracing emission so subprocess and step events correlate.
    pub(super) fn task_id(&self) -> Option<&str> {
        super::super::cli_runner::task_id_from_input(&self.input)
    }

    /// Current step-output map as a shared snapshot. Cheap: bumps one
    /// refcount under the lock. Drop it before the next `record_pipeline`
    /// on the same context so that write can mutate in place.
    pub(super) fn pipeline_snapshot(&self) -> PipelineSteps {
        Arc::clone(&self.pipeline.lock().expect("pipeline poisoned"))
    }

    /// Raw step outputs (`step id → output`) as one JSON object, for the run
    /// result and the failure activity payload. Clones each output once.
    pub(super) fn pipeline_value(&self) -> Value {
        Value::Object(
            self.pipeline_snapshot()
                .iter()
                .map(|(k, v)| (k.clone(), unwrap_step_output(v)))
                .collect(),
        )
    }

    pub(super) fn template_ctx(&self) -> TemplateContext {
        let steps = self.pipeline_snapshot();
        let mut input = self.input.clone();
        if let Some(item) = &self.item {
            // Expose item under input.item for template resolution. v1's
            // template engine only splits paths under a named namespace; we
            // reuse the `input.*` namespace to keep the resolver unchanged.
            if let Value::Object(map) = &mut input {
                map.insert("item".to_string(), item.clone());
            }
        }
        if let Some(iteration) = self.iteration
            && let Value::Object(map) = &mut input
        {
            map.insert("iteration".to_string(), Value::from(iteration));
        }
        TemplateContext {
            input,
            env: Default::default(),
            workspace_path: None,
            item: self.item.clone(),
            iteration: self.iteration,
            steps,
        }
    }
}

/// The template engine reads `{{ steps.<id>.output.<field> }}`, so pipeline
/// outputs are stored under an `output` key — the same `.output.` prefix
/// callers would use against a v1 step. Moves `raw`; no copy.
pub(super) fn wrap_step_output(raw: Value) -> Value {
    let mut wrapped = serde_json::Map::with_capacity(1);
    wrapped.insert("output".to_string(), raw);
    Value::Object(wrapped)
}

/// Inverse of [`wrap_step_output`]: the raw output a step recorded.
fn unwrap_step_output(wrapped: &Value) -> Value {
    wrapped.get("output").cloned().unwrap_or(Value::Null)
}

/// Build the shared step map from raw outputs (resume seeding).
pub(super) fn pipeline_steps_from_raw(raw: HashMap<String, Value>) -> PipelineSteps {
    Arc::new(
        raw.into_iter()
            .map(|(k, v)| (k, wrap_step_output(v)))
            .collect(),
    )
}

/// Result of running a single step.
pub(super) struct StepOutcome {
    pub(super) success: bool,
    pub(super) output: Value,
    pub(super) message: Option<String>,
}

/// Record a step output. Mutates the map in place unless a snapshot handed
/// out by `pipeline_snapshot` (a fan-out worker's inherited map, a template
/// context still in scope) is alive, in which case the map is copied first.
pub(super) fn record_pipeline(ctx: &ExecCtx<'_>, key: &str, v: Value) {
    let mut steps = ctx.pipeline.lock().expect("pipeline poisoned");
    Arc::make_mut(&mut steps).insert(key.to_string(), wrap_step_output(v));
}

// ---------------------------------------------------------------------------
// Parallel
// ---------------------------------------------------------------------------
