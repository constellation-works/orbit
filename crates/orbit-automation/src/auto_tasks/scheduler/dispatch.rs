use crate::condition::GuardVerdict;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskDefinition, SkipIfUnchanged};
use std::path::PathBuf;

/// A plugin-inactive skip with stable provenance for warning deduplication.
pub struct InactivePluginSkip {
    /// Plugin namespace that seeded the definition.
    pub plugin: String,
    /// Seeded plugin version.
    pub version: String,
    /// Operator-facing explanation.
    pub reason: String,
}

/// Lifecycle adapter. Core retains task creation, authorization and audit.
pub trait AutoTaskDispatch {
    fn evaluate_delivery(
        &self,
        definition: &AutoTaskDefinition,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError>;

    /// One pass of a condition definition's firing protocol: evaluate its
    /// subjects, record each new edge and mint under the edge's key. Hosts
    /// without condition support refuse, and nothing fires.
    fn evaluate_condition(
        &self,
        _definition: &AutoTaskDefinition,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<crate::condition::ConditionDiagnostic, OrbitError> {
        Err(OrbitError::Execution(
            "condition evaluation unavailable on this host".into(),
        ))
    }

    /// Evaluate the schedule's `when:` guard at a due slot. A host that
    /// cannot evaluate a configured guard holds the slot: unknown fails
    /// closed.
    fn evaluate_guard(&self, definition: &AutoTaskDefinition, _now: DateTime<Utc>) -> GuardVerdict {
        match definition
            .schedule
            .guard()
            .and_then(|guard| guard.when.as_ref())
        {
            Some(_) => GuardVerdict::unknown("guard evaluation unavailable on this host"),
            None => GuardVerdict::Open,
        }
    }

    fn definition_root(&self) -> PathBuf;

    fn state_dir(&self) -> PathBuf;

    /// The id of a still-open instance of `definition`'s prior mints, if any.
    /// `None` means `SkipIfOpen` dedupe should let the current fire proceed.
    fn has_open_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError>;

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError>;

    /// Why this definition is skipped this pass without being disabled or due.
    ///
    /// The one caller is a definition a plugin seeded whose plugin is no
    /// longer active: it stays on disk, does not fire, and the reason names
    /// the plugin. Hosts without plugins answer `None`.
    fn skip_reason(&self, _definition: &AutoTaskDefinition) -> Option<InactivePluginSkip> {
        None
    }

    /// Evidence for a `skip_if_unchanged` precondition: the tip of the
    /// configured ref, the cursor the last completed sweep recorded, and
    /// whether the tip is already covered by it. Hosts that cannot answer
    /// report [`ChangeProbe::Unknown`] (or an error) and the scheduler mints.
    fn probe_change_since_last_sweep(
        &self,
        definition: &AutoTaskDefinition,
        precondition: &SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError>;

    /// Evidence for a `skip_unless_blocked_candidates` precondition: whether
    /// any blocked task has a block episode awaiting triage at `now`. Hosts
    /// that cannot answer report [`BlockedCandidateProbe::Unknown`] (or an
    /// error) and the scheduler mints.
    fn probe_blocked_candidates(
        &self,
        definition: &AutoTaskDefinition,
        now: DateTime<Utc>,
    ) -> Result<BlockedCandidateProbe, OrbitError>;
}

/// What a host could establish about the integration branch since the last
/// completed sweep. Fail-open is the scheduler's rule, not the host's: only
/// [`ChangeProbe::Unchanged`] ever suppresses a mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeProbe {
    /// The ref's tip is already covered by the recorded cursor.
    Unchanged {
        cursor: String,
        tip: String,
        cursor_task_id: Option<String>,
    },
    /// The ref advanced past the recorded cursor.
    Changed { cursor: String, tip: String },
    /// Nothing conclusive — no completed sweep, an unreadable cursor, or git
    /// could not answer. The scheduler mints and records `reason`.
    Unknown { reason: String },
}

/// What a host could establish about blocked tasks awaiting triage. As with
/// [`ChangeProbe`], only [`BlockedCandidateProbe::None`] suppresses a mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedCandidateProbe {
    /// No block episode qualifies. `blocked` counts the blocked tasks read.
    None { blocked: usize },
    /// These tasks have a block episode awaiting triage.
    Qualifying { task_ids: Vec<String> },
    /// Nothing conclusive; the scheduler mints and records `reason`.
    Unknown { reason: String },
}
