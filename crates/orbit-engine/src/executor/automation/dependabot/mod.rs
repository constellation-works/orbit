//! Host-owned Dependabot alert discovery.
//!
//! All `gh` processes run here with `NoSandbox` on the engine-private
//! deterministic boundary. Only the bounded, redacted snapshot leaves this
//! module; no credential or follow-up query capability enters an agent lane.

mod collect;
mod query;

use orbit_common::OrbitError;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::github_host::{OUTCOME_CAPABILITY_UNAVAILABLE, query_root};

const OUTCOME_NO_OPEN_ALERTS: &str = "no_open_alerts";
const OUTCOME_OPEN_ALERTS: &str = "open_alerts";

pub(super) fn collect_dependabot_alerts<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let queries = query::HostDependabotQueries::new(&query_root(host, input)?);
    let snapshot = collect::collect(&queries, input)?;
    Ok(serde_json::json!({
        "phase": "collect_dependabot_alerts",
        "dependabot_snapshot": snapshot,
    }))
}
