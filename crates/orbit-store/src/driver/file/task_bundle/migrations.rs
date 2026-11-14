//! Migration plans for v2 task-bundle YAML artifacts.
//!
//! Empty chains today: every bundle on disk is already at
//! [`TASK_ARTIFACT_SCHEMA_VERSION`]. Future schema bumps add a single
//! `add_step(prev, fn)` call here per artifact; the bundle read path already
//! calls `envelope_plan`.
//!
//! See `docs/design/task-artifacts/4_decisions.md` ADR-008 for the framework
//! design constraints (forward-only, untyped `Value -> Value`, read-time only,
//! no lossy cross-layout projections).

use std::sync::OnceLock;

use orbit_common::migration::Plan;
use orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION;

/// Migration plan for the `task.yaml` envelope (the top-level metadata
/// document in each bundle directory).
pub(crate) fn envelope_plan() -> &'static Plan {
    static PLAN: OnceLock<Plan> = OnceLock::new();
    PLAN.get_or_init(|| Plan::new("task-bundle:envelope", TASK_ARTIFACT_SCHEMA_VERSION))
}
