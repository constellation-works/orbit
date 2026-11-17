//! Global (cross-workspace) endpoints (ORB-00030).
//!
//! These handlers take the whole [`DashboardState`](crate::state::DashboardState) rather than a single
//! workspace runtime (via the [`Ws`](crate::state::Ws) extractor), because they
//! describe or aggregate across every servable workspace. In single mode they
//! degrade gracefully: `/api/workspaces` reports the one synthetic entry and
//! `/api/tasks/all` returns that workspace's tasks.

mod all_job_runs;
mod all_tasks;
mod fan_out;
mod fleet_drains;
mod list;

pub(super) use all_job_runs::list_all_job_runs;
pub(super) use all_tasks::list_all_tasks;
pub(super) use fleet_drains::list_fleet_drains;
pub(crate) use list::entry_is_owned;
pub(super) use list::list_workspaces;
