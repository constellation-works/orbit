//! The apps: one module per subject a user can name, each with a `Command`,
//! an `Outcome`, an `Error` and one `exec`.
//!
//! A surface builds a `Command`, passes it with its [`Core`](crate::Core) to
//! that app's `exec`, and renders the `Outcome`. Every `exec` runs through
//! the one chokepoint in `exec.rs`, which authorizes and audits the command.
//! A command line also scopes each invocation with `invocation.rs`, which
//! settles that row once output rendering finishes and writes the row of an
//! invocation that never reached an `exec`.
//! `agent`, `attention`, `audit`, `auto_task`, `automation`, `codebase`,
//! `config`, `crew_memory`, `doctor`, `evolution`, `experiment`, `friction`, `gc`,
//! `host`, `notice`, `plugin`, `resident`, `review`, `routine`, `run`, `search`,
//! `skill`, `task`, `tool`, `update` and `workspace` are apps; `host` and `update`
//! open no workspace, so they have no `exec` and their surfaces call them directly.
//! The other subjects still live in `application/` and are reached through
//! `OrbitRuntime`.

pub mod agent;

pub mod attention;

pub mod audit;

pub mod auto_task;

pub mod automation;

pub mod codebase;

pub mod config;

pub mod crew_memory;

pub mod doctor;

pub mod evolution;

mod exec;

pub mod experiment;

pub mod friction;

pub mod gc;

pub mod host;

mod invocation;

pub mod notice;

pub mod plugin;

pub mod resident;

pub mod review;

pub mod routine;

pub mod run;

pub mod search;

pub mod skill;

pub mod task;

pub mod tool;

pub(crate) mod tool_call;

pub(crate) mod tool_fields;

pub mod update;

pub mod workspace;

pub use exec::{DashboardCaller, dashboard_authorize};
pub use invocation::{AuditWriteFailure, CommandMeta, InvocationAuditScope};
