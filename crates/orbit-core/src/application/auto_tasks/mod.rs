//! Auto-tasks [ORB-10149]: dynamically-defined recurring task templates + one
//! generic scheduler.
//!
//! Every periodic need in orbit used to be bespoke code (qa-sweep, ship-sweep,
//! …). Auto-tasks replace that pattern with a primitive: a definition is a
//! git-versioned YAML record ([`loader`]) with a schedule, an `enabled` toggle,
//! a task template, and a dedupe policy. A single generic scheduler
//! ([`scheduler`]) fires the due, enabled definitions and mints tasks from
//! their templates — periodic work becomes data, not code. The host scheduler
//! tick runs the evaluator directly for each registered owner checkout.
//!
//! - [`loader`] — discover + parse definitions, fail-closed.
//! - [`schedule`] — due-math (cron reuses the routine machinery; interval is
//!   native), with catch-up collapse.
//! - [`state`] — host-local, workspace-scoped last-fired cursors.
//! - [`scheduler`] — the evaluator called by the host tick.
//! - [`crud`] — the shared add/list/show/update/toggle/mint domain surface.

pub mod crud;
pub use orbit_automation::auto_tasks::loader;
pub use orbit_automation::auto_tasks::schedule;
pub mod scheduler;
pub mod state;

pub use crud::{AutoTaskAddParams, AutoTaskUpdateParams};
pub use loader::{
    AutoTaskCollection, AutoTaskLoadError, LoadedAutoTask, auto_tasks_dir, collect_auto_tasks,
    definition_path,
};
pub use schedule::{AutoTaskDueDecision, decide_due, validate_schedule};
pub use scheduler::{
    AutoTaskFireReport, AutoTaskSchedulerOutcome, SchedulerOptions, open_auto_task_instance,
    run_auto_task_scheduler_at,
};
pub use state::{AutoTaskCursor, AutoTaskCursorState, cursor_state_path, load_cursor_state};

/// Default definitions embedded in the Orbit binary and materialized into a
/// workspace on initialization. Defaults are deliberately inert: users must
/// explicitly mint one or enable it through the existing auto-task surface.
pub(crate) const DEFAULT_AUTO_TASK_FILES: &[(&str, &str)] = &[
    (
        "delivery-code-review",
        include_str!("../../../assets/auto_tasks/delivery-code-review.yaml"),
    ),
    (
        "delivery-qa",
        include_str!("../../../assets/auto_tasks/delivery-qa.yaml"),
    ),
    (
        "code-review",
        include_str!("../../../assets/auto_tasks/code-review.yaml"),
    ),
    (
        "friction-curation",
        include_str!("../../../assets/auto_tasks/friction-curation.yaml"),
    ),
    (
        "qa-sweep",
        include_str!("../../../assets/auto_tasks/qa-sweep.yaml"),
    ),
    (
        "security-review",
        include_str!("../../../assets/auto_tasks/security-review.yaml"),
    ),
];

#[cfg(test)]
mod tests;
