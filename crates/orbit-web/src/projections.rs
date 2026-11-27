//! Dashboard-local projections: the audit event the API serves, built on the
//! base the CLI shares (`orbit_types::telemetry::audit_event_base_json`), and
//! the cached [`TaskRiskinessModel`] task reads lend the task app.

use std::sync::Arc;

use orbit_core::Core;
use orbit_core::app::codebase::TaskRiskinessModel;
use orbit_core::app::run::Command;
use orbit_types::telemetry::AuditEvent;
use orbit_types::telemetry::audit_event_base_json;
use serde_json::{Value, json};

use crate::runtime_memo::{TASK_RISKINESS_STALE_LIMIT, TASK_RISKINESS_TTL};
use crate::state::{DashboardState, run_read};

pub(crate) fn audit_event_to_json(event: &AuditEvent) -> Value {
    let actor = event.actor();
    let mut value = audit_event_base_json(event);
    // ORB-10888: the canonical actor beside the raw label. `role` stays
    // byte-for-byte what was recorded; these are derived.
    value["actor"] = json!(actor.id);
    value["actor_kind"] = json!(actor.kind.to_string());
    value["actor_vendor"] = json!(actor.vendor);
    value["actor_family"] = json!(actor.family);
    value["actor_model"] = json!(actor.model);
    value
}

/// The riskiness model cached for `runtime`. The first read for a runtime
/// waits for one scan; later reads are served from the cache, stale while one
/// background refresh replaces it. A failed scan is logged and leaves the
/// estimate unavailable rather than failing the task read.
pub(crate) async fn cached_riskiness_model(
    state: &DashboardState,
    runtime: &Arc<Core>,
) -> Option<Arc<TaskRiskinessModel>> {
    let scan_runtime = Arc::clone(runtime);
    match state
        .riskiness_memo()
        .get_or_refresh(
            runtime,
            (),
            TASK_RISKINESS_TTL,
            TASK_RISKINESS_STALE_LIMIT,
            move || run_read(&scan_runtime, Command::task_riskiness_model()),
        )
        .await
    {
        Ok(served) => Some(served.body),
        Err(error) => {
            tracing::warn!(%error, "task riskiness model unavailable");
            None
        }
    }
}
