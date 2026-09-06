//! Causal incident semantics. Missing recovery facts withhold diagnosis.

use crate::{AutomationError, delivery::definition_epoch};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentFacts {
    pub workspace: String,
    pub episode: Option<String>,
    pub cause: Option<String>,
    pub failure: bool,
    pub recovery_settled: bool,
    pub current_failure_coupling: bool,
    pub diagnostic_origin: bool,
    pub cancellation: bool,
}

/// Diagnose only when the incident is a settled, still-coupled execution failure;
/// every other shape withholds with the reason that made it undiagnosable.
pub fn incident_key(facts: &IncidentFacts) -> Result<String, AutomationError> {
    let withheld = if facts.diagnostic_origin {
        Some("diagnostic_recursion")
    } else if facts.cancellation {
        Some("cancellation_withheld")
    } else if !facts.failure {
        Some("not_execution_failure")
    } else if !facts.current_failure_coupling {
        Some("human_intent_or_coupling_changed")
    } else if !facts.recovery_settled {
        Some("recovery_pending")
    } else {
        None
    };

    if let Some(reason) = withheld {
        return Err(AutomationError::Deferred(reason.into()));
    }

    let (Some(episode), Some(cause)) = (&facts.episode, &facts.cause) else {
        return Err(AutomationError::Deferred("incident_unresolved".into()));
    };
    if episode.is_empty() || cause.is_empty() {
        return Err(AutomationError::Deferred("incident_unresolved".into()));
    }

    definition_epoch(&(&facts.workspace, episode, cause))
}
