//! The worker side of a pipeline run, split by who owns which half.
//!
//! - `command` / `log` — how a worker process is launched and where its stdio
//!   lands.
//! - `supervisor` — the parent-side [`PipelineWorkerSupervisor`] that spawns a
//!   worker, watches its startup, and settles a run whose worker died.
//! - `record` — the store writes both sides share.
//!
//! What stays on [`OrbitRuntime`] here is the *child* process's own work:
//! executing the run it was handed (`execute`), its startup checks (`start`)
//! and the diagnostics it records (`audit`), plus thin delegations to the
//! supervisor for callers that already hold a runtime.

use std::sync::Arc;

use super::*;
use command::*;
use log::*;
use supervisor::PipelineWorkerSupervisor;

use super::admission::pipeline_run_is_runnable;
use super::wait::PIPELINE_WAIT_MIN_POLL_SECONDS;
use crate::runtime::upgrade_handover;

mod audit;
pub(super) mod command;
mod execute;
pub(super) mod log;
mod record;
pub(super) mod scope;
mod start;
pub(super) mod supervisor;

#[cfg(test)]
mod tests;
