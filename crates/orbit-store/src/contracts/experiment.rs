//! Experiment records: operator-defined experiments, their arms and the
//! append-only event log of every change to them. The lifecycle rules and
//! the typed overlay, population and metric documents are owned by core; the
//! store keeps those documents as opaque JSON and makes each write atomic
//! against the revision it was computed from.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// One experiment's definition and lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentRow {
    pub workspace_id: String,
    pub id: String,
    pub name: String,
    pub hypothesis: String,
    /// Which tasks the experiment enrolls, as core's population document.
    pub population_json: String,
    pub primary_metric: Option<String>,
    /// Guardrail metrics and their thresholds, as core's guardrail document.
    pub guardrails_json: String,
    pub min_sample_per_arm: Option<u32>,
    pub max_days: Option<u32>,
    /// Keys the deterministic assignment draw.
    pub salt: String,
    /// `shadow` or `live`.
    pub mode: String,
    /// `draft`, `running`, `paused`, `concluded` or `abandoned`.
    pub status: String,
    /// The current segment: 0 in draft, 1 from start, one more on each
    /// reweight or arm retirement.
    pub segment: u32,
    pub concluded_arm: Option<String>,
    pub cloned_from: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Writes so far; each write's event carries the revision it produced.
    pub revision: u32,
}

/// One arm of an experiment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentArmRow {
    pub name: String,
    /// Declaration order, from 0.
    pub position: u32,
    pub control: bool,
    pub weight: u32,
    /// The arm's overlay as core's typed overlay document.
    pub overlay_json: String,
    /// `sha256:<hex>` of `overlay_json`.
    pub overlay_digest: String,
    /// The segment the arm was retired at, when retired.
    pub retired_segment: Option<u32>,
}

/// One row of an experiment's append-only event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentEventRow {
    /// The experiment revision this event produced, from 1.
    pub seq: u32,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub segment: u32,
    pub actor: String,
    pub at: DateTime<Utc>,
    pub reason: Option<String>,
    /// Event-specific detail as a JSON object.
    pub detail_json: String,
}

/// An experiment with its arms in declaration order and its events in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentRecord {
    pub experiment: ExperimentRow,
    pub arms: Vec<ExperimentArmRow>,
    pub events: Vec<ExperimentEventRow>,
}

/// One atomic experiment write: the experiment's new state, optionally its
/// new arm set, and the event that records the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentWrite {
    /// The new state. Its `revision` is the revision this write produces;
    /// the event's `seq` must equal it.
    pub experiment: ExperimentRow,
    /// Replaces every arm when present; keeps the stored arms when `None`.
    pub arms: Option<Vec<ExperimentArmRow>>,
    pub event: ExperimentEventRow,
    /// The revision the write was computed from; `None` inserts a new
    /// experiment.
    pub expected_revision: Option<u32>,
    /// Refuse the write when this many other experiments in the workspace
    /// are already `running`.
    pub running_limit: Option<u32>,
}

/// What an [`ExperimentWrite`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentWriteOutcome {
    Written,
    /// Another experiment in the workspace already has this name.
    NameTaken,
    /// The stored revision is not the expected one (or the experiment is
    /// gone): another write landed after the caller read it.
    Stale,
    /// `running_limit` other experiments are already running.
    RunningLimitReached {
        running: u32,
    },
}

/// A sticky assignment, or a durable surface-conflict exclusion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExperimentAssignment {
    pub experiment: String,
    pub task_id: String,
    pub arm: Option<String>,
    pub segment: Option<u32>,
    pub digest: Option<String>,
    pub assigned_at: DateTime<Utc>,
    pub forced: bool,
    pub exclusion_reason: Option<String>,
}

/// Pure assignment decision inside the store transaction. Existing rows cannot
/// be replaced, except that a forced assignment may replace an exclusion. `admitted` includes prior admissions that matched no population;
/// a marked admission never reaches the decision once the task is admitted.
pub type ExperimentAssignmentDecision<'a> = dyn Fn(
        &[ExperimentRecord],
        &[ExperimentAssignment],
        bool,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>
    + 'a;

pub trait ExperimentStoreBackend: Send + Sync {
    /// Serialize assignment decisions with lifecycle changes and other admissions.
    /// `experiments_enabled` is the caller's current effective enablement. While
    /// it is false, a marked admission writes nothing and returns existing rows.
    /// Once an enabled marked admission has run, later marked admissions return
    /// the existing rows unchanged, even when that first admission matched none.
    fn assign_experiments(
        &self,
        workspace_id: &str,
        task_id: &str,
        mark_admitted: bool,
        experiments_enabled: bool,
        decide: &ExperimentAssignmentDecision<'_>,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>;

    /// All assignments and exclusions for the task, in experiment creation order.
    fn task_experiment_assignments(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>;

    /// Every assignment and exclusion of one experiment, oldest first.
    fn experiment_assignments(
        &self,
        workspace_id: &str,
        experiment_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError>;

    /// Apply `write` in one immediate transaction.
    fn write_experiment(
        &self,
        write: &ExperimentWrite,
    ) -> Result<ExperimentWriteOutcome, OrbitError>;

    /// The experiment whose id, or else whose name, is `id_or_name`.
    fn experiment_record(
        &self,
        workspace_id: &str,
        id_or_name: &str,
    ) -> Result<Option<ExperimentRecord>, OrbitError>;

    /// The workspace's experiments, oldest first, optionally only those in
    /// `status`. Events are not loaded.
    fn experiment_records(
        &self,
        workspace_id: &str,
        status: Option<&str>,
    ) -> Result<Vec<ExperimentRecord>, OrbitError>;
}
