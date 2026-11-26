//! The apps: one module per subject a user can name, each with a `Command`,
//! an `Outcome`, an `Error` and one `exec`.
//!
//! A surface builds a `Command`, passes it with its [`Core`](crate::Core) to
//! that app's `exec`, and renders the `Outcome`. Every `exec` runs through
//! the one chokepoint in `exec.rs`, which authorizes and audits the command.
//! `friction`, `plugin`, `run`, `task` and `workspace` are apps; the other subjects still live in
//! `application/` and are reached through `OrbitRuntime`.

mod exec;
pub use exec::dashboard_authorize;
pub mod friction;
pub mod plugin;
pub mod run;
pub mod task;
pub(crate) mod tool_fields;
pub mod workspace;
