//! Auto-tasks [ORB-10149]: dynamically-defined recurring task templates + one
//! generic scheduler.
//!
//! Every periodic need in orbit used to be bespoke code (qa-sweep, ship-sweep,
//! …). Auto-tasks replace that pattern with a primitive: a definition is a
//! YAML record ([`loader`]) under `.orbit/auto_tasks/` with a schedule, an
//! `enabled` toggle, a task template, and a dedupe policy. That directory is
//! per-user state, not a repository artifact. A single generic scheduler
//! ([`scheduler`]) fires the due, enabled definitions and mints tasks from
//! their templates — periodic work becomes data, not code. The host scheduler
//! tick runs the evaluator directly for each registered owner checkout.
//!
//! - [`loader`] — discover + parse definitions, fail-closed.
//! - [`schedule`] — due-math (cron reuses the routine machinery; interval is
//!   native), with catch-up collapse.
//! - [`state`] — host-local, workspace-scoped last-fired cursors.
//! - [`scheduler`] — the evaluator called by the host tick.
//! - `change_probe` — the `skip_if_unchanged` precondition's evidence.
//! - [`crud`] — the shared add/list/show/update/toggle/mint domain surface.

use std::borrow::Cow;

mod change_probe;
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
        "backlog-hygiene",
        include_str!("../../../assets/auto_tasks/backlog-hygiene.yaml"),
    ),
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
        "doc-duties",
        include_str!("../../../assets/auto_tasks/doc-duties.yaml"),
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
        "run-failure-patterns",
        include_str!("../../../assets/auto_tasks/run-failure-patterns.yaml"),
    ),
    (
        "security-review",
        include_str!("../../../assets/auto_tasks/security-review.yaml"),
    ),
];

/// Placeholder the shipped delivery definitions carry for the workspace's
/// integration branch. Seeding renders it to the registered base branch, so a
/// `main`-based workspace never inherits a literal `agent-main` that no tick
/// can baseline against.
pub(crate) const BASE_BRANCH_PLACEHOLDER: &str = "__ORBIT_BASE_BRANCH__";

/// Render one embedded default against the workspace's base branch. Defaults
/// without the placeholder are returned as shipped.
pub(crate) fn render_default_auto_task<'a>(content: &'a str, base_branch: &str) -> Cow<'a, str> {
    if content.contains(BASE_BRANCH_PLACEHOLDER) {
        Cow::Owned(content.replace(BASE_BRANCH_PLACEHOLDER, base_branch))
    } else {
        Cow::Borrowed(content)
    }
}

#[cfg(test)]
mod tests;
