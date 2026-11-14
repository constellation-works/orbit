//! The worker side of a pipeline run, split by who owns which half.
//!
//! - `command` / `log` — how a worker process is launched and where its stdio
//!   lands.
//! - `supervisor` — the parent-side [`PipelineWorkerSupervisor`] that spawns a
//!   worker, watches its startup, and settles a run whose worker died.
//! - `exit` — how a worker process's exit is observed and written, and a late
//!   exit amended into reconciliation's diagnostic. The persisted record's
//!   format and its readers belong to `job/run` (`worker_exit`).
//!
//! What stays on [`OrbitRuntime`] here is the *child* process's own work:
//! executing the run it was handed (`execute`), its startup checks (`start`),
//! the diagnostics it records (`audit`) and the per-minute resource history
//! of its own scope on Linux, or its process tree on macOS (`usage`, keyed
//! by the `delivery_class` captured at admission), plus thin delegations to
//! the supervisor for callers that
//! already hold a runtime.

use std::sync::Arc;

use super::*;
use command::*;
use log::*;
use supervisor::{PipelineWorkerSupervisor, WorkerLaunchError};

use super::admission::pipeline_run_is_runnable;
use super::wait::PIPELINE_WAIT_MIN_POLL_SECONDS;
use crate::runtime::upgrade_handover;

mod audit;
pub(super) mod command;
mod cwd;
mod delivery_class;
mod execute;
pub(super) mod exit;
pub(super) mod log;
pub(super) mod scope;
mod start;
pub(super) mod supervisor;
mod usage;

#[cfg(test)]
mod tests;
