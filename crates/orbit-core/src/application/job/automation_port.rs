//! What job runs need from state automation [ORB-17938].
//!
//! A starting run records whether its bundle may name tasks of different
//! crews, the delivery projection names the repository it delivers to, and
//! a drain submission warns while no task-pilot routine is owned here.
//! `automation` owns those rules, and it also drives job runs, so a direct
//! call from here would close a cycle. This port is the way in:
//! `automation` implements it on [`OrbitRuntime`](crate::OrbitRuntime), so
//! composing the runtime binds it.

use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::runtime::engine::crew::BundleCrews;

/// Automation reads job code calls, implemented by `automation`.
pub(in crate::application) trait JobAutomationPort {
    /// Whether `run`, starting with `input`, may name tasks whose crews
    /// differ: only the live task-pilot preparation attempt's own run may.
    fn run_bundle_crews(&self, run: &JobRun, input: &Value) -> BundleCrews;

    /// The repository this workspace's source names, or `None` when it
    /// cannot be read.
    fn source_repository(&self) -> Option<String>;

    /// The warning a local drain reports while no enabled, unpaused
    /// task-pilot routine of this workspace is owned by this machine.
    fn pilot_routine_warning(&self) -> Option<String>;

    /// A live task-pilot run that holds `task_id` by its reservation or its
    /// prepare checkpoint, or `None` when no run does.
    fn pilot_holder_run(&self, task_id: &str) -> Result<Option<String>, OrbitError>;
}
