//! The apps: one module per subject a user can name, each with a `Command`,
//! an `Outcome`, an `Error` and one `exec`.
//!
//! A surface builds a `Command`, passes it with its [`Core`](crate::Core) to
//! that app's `exec`, and renders the `Outcome`. Every `exec` runs through
//! the one chokepoint in `exec.rs`, which authorizes and audits the command.
//! `audit`, `auto_task`, `automation`, `config`, `evolution`, `experiment`,
//! `friction`, `plugin`, `resident`, `routine`, `run`, `skill`, `task` and
//! `workspace` are apps; the other subjects still live in `application/` and
//! are reached through `OrbitRuntime`.

pub mod audit;
pub mod auto_task;
pub mod automation;
pub mod config;
pub mod evolution;
mod exec;
pub mod experiment;
pub mod friction;
pub mod plugin;
pub mod resident;
pub mod routine;
pub mod run;
pub mod skill;
pub mod task;
pub(crate) mod tool_call;
pub(crate) mod tool_fields;
pub mod workspace;
pub use exec::{DashboardCaller, InvocationAuditScope, dashboard_authorize};
