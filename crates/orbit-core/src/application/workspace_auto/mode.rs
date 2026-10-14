//! Capture the controller mode once at the shared durable submission boundary.

use orbit_common::OrbitError;
use orbit_types::workflow::AutoConcurrencyMode;
use serde_json::{Value, json};

use crate::OrbitRuntime;

impl OrbitRuntime {
    pub(crate) fn capture_auto_concurrency(
        &self,
        job_name: &str,
        input: &mut Value,
        retain_snapshot: bool,
    ) -> Result<(), OrbitError> {
        if job_name != "workspace_auto_pipeline" {
            if job_name == "workspace_pull_pipeline" && input.get("auto_concurrency").is_some() {
                return Err(OrbitError::InvalidInput(
                    "auto_concurrency is not supported for pull drains; configure the executor host mode".into(),
                ));
            }
            return Ok(());
        }
        if !input.is_object() {
            return Err(OrbitError::InvalidInput(
                "pipeline run input must be a JSON object".into(),
            ));
        }
        let (mode, source) = match input.get("auto_concurrency") {
            Some(value) => (parse_mode(value)?, "explicit"),
            None => (configured_mode(self)?, "configured"),
        };
        // Resumes/restarts keep both the captured policy and its provenance.
        if !retain_snapshot || input.get("auto_concurrency_source").is_none() {
            input["auto_concurrency_source"] = json!(source);
        }
        input["auto_concurrency"] = json!(mode);
        Ok(())
    }

    /// Read the submitted effective controller policy, rather than current configuration.
    pub fn run_auto_concurrency(&self, run_id: &str) -> Result<Value, OrbitError> {
        let run = self.show_job_run(run_id)?;
        let input = run.input.unwrap_or_else(|| json!({}));
        self.auto_concurrency_view(Some(&input))
    }

    pub(crate) fn auto_concurrency_view(&self, input: Option<&Value>) -> Result<Value, OrbitError> {
        let configured = configured_mode(self)?;
        let mode = match input.and_then(|input| input.get("auto_concurrency")) {
            None | Some(Value::Null) => configured,
            Some(value) => parse_mode(value)?,
        };
        Ok(json!({
            "mode": mode,
            "source": input.and_then(|input| input.get("auto_concurrency_source"))
                .and_then(Value::as_str).unwrap_or("configured"),
            "configured_mode": configured,
        }))
    }
}

// Dashboard and MCP runtimes can outlive a configuration write. Resolve the
// global-only default at submission/readback, while existing runs continue
// using their captured input and admission keeps its existing host thresholds.
fn configured_mode(runtime: &OrbitRuntime) -> Result<AutoConcurrencyMode, OrbitError> {
    orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))?
    .auto_concurrency
    .mode
    .parse()
    .map_err(invalid_mode)
}

pub(super) fn effective_mode(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<AutoConcurrencyMode, OrbitError> {
    match input.get("auto_concurrency") {
        None | Some(Value::Null) => runtime
            .context
            .settings()
            .auto_concurrency()
            .mode
            .parse()
            .map_err(invalid_mode),
        Some(value) => parse_mode(value),
    }
}

fn parse_mode(value: &Value) -> Result<AutoConcurrencyMode, OrbitError> {
    serde_json::from_value(value.clone())
        .map_err(|_| invalid_mode("auto_concurrency must be off, shadow or enforce"))
}

fn invalid_mode(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.to_string())
}
