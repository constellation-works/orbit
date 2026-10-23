//! Causal incident semantics. Missing recovery facts withhold diagnosis.

use crate::{AutomationError, DeferredReason, delivery::definition_epoch};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentFacts {
    pub workspace: String,
    pub episode: Option<String>,
    pub cause: Option<String>,
    pub failure: bool,
    pub recovery_settled: bool,
    pub current_failure_coupling: bool,
    pub cancellation: bool,
}

/// Diagnose only when the incident is a settled, still-coupled execution failure;
/// every other shape withholds with the reason that made it undiagnosable.
pub fn incident_key(facts: &IncidentFacts) -> Result<String, AutomationError> {
    let withheld = if facts.cancellation {
        Some(DeferredReason::CancellationWithheld)
    } else if !facts.failure {
        Some(DeferredReason::NotExecutionFailure)
    } else if !facts.current_failure_coupling {
        Some(DeferredReason::HumanIntentOrCouplingChanged)
    } else if !facts.recovery_settled {
        Some(DeferredReason::RecoveryPending)
    } else {
        None
    };

    if let Some(reason) = withheld {
        return Err(AutomationError::Deferred(reason));
    }

    let (Some(episode), Some(cause)) = (&facts.episode, &facts.cause) else {
        return Err(AutomationError::Deferred(
            DeferredReason::IncidentUnresolved,
        ));
    };
    if episode.is_empty() || cause.is_empty() {
        return Err(AutomationError::Deferred(
            DeferredReason::IncidentUnresolved,
        ));
    }

    definition_epoch(&(&facts.workspace, episode, cause))
}
