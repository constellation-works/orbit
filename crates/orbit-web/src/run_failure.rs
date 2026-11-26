//! The diagnostic cause line used by the dashboard's task detail.

use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::app::run::{Command, exec};
use orbit_core::{JobRun, OrbitRuntime};

pub(crate) fn failure_root_line(runtime: &OrbitRuntime, run: &JobRun) -> Option<String> {
    let core = RegisteredRuntimeFactory::core_for_service(runtime.clone());
    let cause = exec(&core, Command::failure_cause(run)).ok()?.value?;
    format_cause_line(
        &cause.run_id,
        &cause.state.to_string(),
        cause.step.as_deref(),
        cause.message.as_deref(),
    )
}

fn format_cause_line(
    run_id: &str,
    state: &str,
    step: Option<&str>,
    message: Option<&str>,
) -> Option<String> {
    let step = step.map(str::trim).filter(|text| !text.is_empty());
    let message = message.map(str::trim).filter(|text| !text.is_empty());
    if step.is_none() && message.is_none() {
        return None;
    }
    let mut line = format!("{run_id} {state}");
    if let Some(step) = step {
        line.push_str(" at ");
        line.push_str(step);
    }
    if let Some(message) = message {
        line.push_str(" - ");
        line.push_str(message);
    }
    Some(line)
}
