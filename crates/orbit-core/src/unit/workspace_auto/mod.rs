//! The workspace auto-drain's deterministic actions: the per-iteration
//! admission classifier, the `--approve-proposed` selection, pilot wait and
//! report, the drain window, the read-only readiness projection and limiter
//! shares, plus the live drain and leaf-run reads they share.

mod adopt;

mod approvals;

mod automation_port;

mod classify;

mod drains;

mod idle_with_work;

mod job_port;

mod limiter;

mod live_agents;

mod mode;

mod pilot_coordination;

mod readiness;

mod task_port;

mod wave;

mod window;

pub(crate) use adopt::adopt_orphaned_deliveries;
pub(crate) use approvals::{
    await_approval_pilot, record_proposed_approvals, select_proposed_approvals,
};
pub(crate) use automation_port::AutomationPort;
pub(crate) use classify::DrainLeafPort;
pub(crate) use classify::classify_workspace_auto_tasks;
#[cfg(feature = "test-support")]
pub use classify::release_failure_hook::with_reservation_release_failure;
pub(crate) use drains::{drain_auto_concurrency_mode, drain_concurrency_ceiling};
pub(crate) use idle_with_work::drain_idle_with_work;
pub use idle_with_work::{IDLE_WITH_WORK_DAYS, IdleWithWork};
pub(crate) use job_port::JobPort;
#[cfg_attr(not(feature = "test-support"), allow(unused_imports))]
pub use limiter::forecast_accuracy;
pub use limiter::forecast_accuracy_selected;
pub use mode::run_auto_concurrency;
pub use readiness::{
    FleetReadinessFacts, fleet_readiness_facts, workspace_auto_readiness, workspace_fleet_readiness,
};
pub(crate) use readiness::{
    executor_readiness_change_keys, workspace_auto_readiness_with_detail, workspace_drain_phase,
};
pub(crate) use task_port::TaskPort;
pub(crate) use window::drain_window;
