//! `orbit-web` — HTTP API, dashboard UI, and remote web connection.
//!
//! This crate isolates the axum-based dashboard (HTML/JS assets + `/api/*`
//! handlers) from orbit-cli so that CLI changes do not force rebuilds of the
//! large dependency subtree (axum, etc). Behavior is identical to the prior
//! in-tree implementation.
//!
//! Public surface is deliberately tiny: `ServeArgs` (clap) plus two entry
//! points — `serve()` for a caller-supplied runtime (single-workspace mode)
//! and `serve_from_env()`, the entry point for `orbit web serve`, which
//! always serves every registered workspace (global mode; see ORB-10029).
//! All routes, content types, defaults, and graceful shutdown are preserved.

mod api;
mod assets;
mod connect;
mod health;
mod log_format;
mod parse;
mod projections;
mod runtime_memo;
mod serve;
mod ssh_tunnel;
mod state;

#[cfg(test)]
mod tests;

pub use connect::{ConnectArgs, connect};

#[cfg(test)]
pub(crate) use assets::{DASHBOARD_CSP, DASHBOARD_FILES, serve_dashboard_file};
pub(crate) use serve::{DEFAULT_DASHBOARD_PORT, default_workspace_selection, open_browser};
pub use serve::{ServeArgs, serve, serve_from_env};
#[cfg(test)]
pub(crate) use serve::{
    build_state, check_bindable_host, default_workspace_for_cwd, drain_with_grace_period,
    health_router,
};

/// The dashboard stylesheet, served as one file from per-screen sources.
/// Order is cascade order: at equal specificity a later file wins, so shared
/// layers come first and a file that refines another comes after it.
pub(crate) const DASHBOARD_CSS: &str = concat!(
    include_str!("../assets/dashboard/css/base.css"),
    include_str!("../assets/dashboard/css/shell.css"),
    include_str!("../assets/dashboard/css/components.css"),
    include_str!("../assets/dashboard/css/tasks.css"),
    include_str!("../assets/dashboard/css/task-detail.css"),
    include_str!("../assets/dashboard/css/markdown.css"),
    include_str!("../assets/dashboard/css/runs.css"),
    include_str!("../assets/dashboard/css/run-detail.css"),
    include_str!("../assets/dashboard/css/dock.css"),
    include_str!("../assets/dashboard/css/log-tail.css"),
    include_str!("../assets/dashboard/css/audit.css"),
    include_str!("../assets/dashboard/css/health.css"),
    include_str!("../assets/dashboard/css/scoreboard.css"),
    include_str!("../assets/dashboard/css/knowledge.css"),
    include_str!("../assets/dashboard/css/automation.css"),
    include_str!("../assets/dashboard/css/drain.css"),
    include_str!("../assets/dashboard/css/settings.css"),
    include_str!("../assets/dashboard/css/plugins.css"),
);
