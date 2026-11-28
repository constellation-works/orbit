//! Desktop auto-drain translation. Authorization remains at the tool chokepoint;
//! scheduling, claims, stopping and settlement reuse the CLI/dashboard runtime.
use crate::OrbitRuntime;
use crate::app::tool_fields::optional_u32;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_string_list_alias,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};
pub(super) fn readiness(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: &Value,
    limit: usize,
) -> Result<Value, OrbitError> {
    let task_ids = optional_string_list_alias(input, &["task"])?.unwrap_or_default();
    let concurrency = optional_u32(input, "concurrency")?;
    let allowed_crews =
        optional_csv_or_string_list_alias(input, &["allow_crew"])?.unwrap_or_default();
    // This is a bounded observational read, not a reconciliation tick.
    let mut value = crate::unit::workspace_auto::workspace_auto_readiness(
        runtime,
        &task_ids,
        concurrency,
        limit,
        &allowed_crews,
    )?;
    value["controls_authorized"] = json!(
        crate::runtime::authorization::resolved_caller_capabilities(runtime.env(), session)
            .contains(&McpCapability::Operator)
    );
    Ok(value)
}
