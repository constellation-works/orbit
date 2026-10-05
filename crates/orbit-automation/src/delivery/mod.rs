//! One bounded delivery evaluator shared by task and job consumers.

use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_types::workflow::automation::*;

pub mod adopt;
mod digest;
mod evaluate;
pub mod evidence;
mod observe;
mod reconcile;
pub mod recovery;
pub mod reset;
pub mod stall;
#[cfg(test)]
mod tests;
mod waiver;

pub(crate) use digest::json_definition_epoch;
pub use digest::{definition_epoch, digest, input_digest};
pub use evaluate::evaluate;
pub use reconcile::{ActionLiveness, action_liveness};
pub use waiver::waive;

/// Authority/source owner, implemented by Core. It never decides coverage rules.
pub trait DeliveryHost {
    fn admission_deferral(&self) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    /// Minutes a deferred reason may persist before the evaluator escalates it
    /// to a warning and one friction record.
    fn stall_window_minutes(&self) -> u32 {
        stall::DEFAULT_WINDOW_MINUTES
    }

    /// The same replay proof `recover --replay-history` previews, for the
    /// evaluator's automatic repair of a diverged branch history. A host with
    /// no source proof refuses, which stalls the consumer for an operator.
    fn replay_history(
        &self,
        _branch: &str,
        _state: &AutomationState,
    ) -> Result<recovery::HistoryReplayInput, AutomationError> {
        Err(AutomationError::Refused(
            orbit_types::workflow::automation::recovery::refusal::PROVIDER_PROOF_UNAVAILABLE.into(),
        ))
    }

    /// True when the consumer's observed revision is reachable from the
    /// branch head again, so a recorded divergence no longer applies.
    fn history_converged(
        &self,
        _branch: &str,
        _state: &AutomationState,
    ) -> Result<bool, AutomationError> {
        Ok(false)
    }

    /// File one friction for a stall, deduped on the divergence it reports,
    /// and answer with the record it filed or found. Hosts without a friction
    /// corpus report nothing.
    fn report_stall(
        &self,
        _report: &stall::StallReport<'_>,
    ) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    /// Whether the evaluator adopts an edited definition on its own when
    /// `recover --adopt-settings` would accept the change. Hosts that do not
    /// opt in keep every edit at `definition_changed` for an operator.
    fn adopts_settings(&self) -> bool {
        false
    }

    /// File one friction for an automatic settings adoption, deduped on the
    /// consumer and the identity change, and answer with the record filed or
    /// found. Hosts without a friction corpus report nothing.
    fn report_adoption(
        &self,
        _report: &adopt::AdoptionReport<'_>,
    ) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError>;

    fn observe(&self, branch: &str, state: &AutomationState)
    -> Result<SourcePage, AutomationError>;

    /// Canonical action admission must resolve the same durable key on replay.
    fn admit(&self, attempt: &BatchAttempt) -> Result<String, AutomationError>;

    fn outcome(&self, attempt: &BatchAttempt) -> Result<ActionOutcome, AutomationError>;
}

/// Core's authoritative observation of an admitted task/job.
pub enum ActionOutcome {
    Pending,
    Evidence(evidence::EvidenceFacts),
    /// Only returned after proving the action stopped. Unknown liveness is Pending.
    Failed {
        retryable: bool,
        reason: String,
    },
}

/// An edited definition the evaluator may not adopt on its own pauses new
/// admission until it is restored or recovered. Hosts that layer their own
/// reasons over this one report it ahead of theirs.
pub const DEFINITION_CHANGED: &str = "definition_changed";

/// Inputs supplied by the existing sweep clock.
#[derive(Clone, Copy)]
pub struct Evaluation<'a> {
    pub consumer: &'a str,
    pub epoch: &'a str,
    pub trigger: &'a DeliveryTrigger,
    pub enabled: bool,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
}
