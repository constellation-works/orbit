use super::dispatch::{AutoTaskDispatch, BlockedCandidateProbe, ChangeProbe};
use chrono::{DateTime, Utc};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSkipRecord, SkipIfUnchanged};

/// Machine-readable reason token recorded for a suppressed mint.
pub const UNCHANGED_SINCE_LAST_SWEEP: &str = "unchanged_since_last_sweep";

/// Reason token recorded when `skip_unless_blocked_candidates` suppresses a
/// mint.
pub const NO_BLOCKED_CANDIDATES: &str = "no_blocked_candidates";

/// What a definition's mint-time preconditions decided for one due slot.
pub(super) enum Precondition {
    /// None is configured, or every configured one found work.
    Proceed,
    /// A precondition found nothing to do.
    Skip(AutoTaskSkipRecord),
    /// A precondition could not answer; the fire mints anyway.
    Inconclusive { token: &'static str, reason: String },
}

/// Evaluate every configured precondition. A conclusive skip from either
/// suppresses the mint; an inconclusive one never does.
pub(super) fn preconditions(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    slot: &str,
    now: DateTime<Utc>,
) -> Precondition {
    let mut decision = Precondition::Proceed;
    if let Some(precondition) = &definition.skip_if_unchanged {
        match probe(host, definition, precondition) {
            ChangeProbe::Unchanged {
                cursor,
                tip,
                cursor_task_id,
            } => {
                return Precondition::Skip(AutoTaskSkipRecord {
                    at: now.to_rfc3339(),
                    slot: slot.to_string(),
                    reason: UNCHANGED_SINCE_LAST_SWEEP.to_string(),
                    reference: precondition.reference.clone(),
                    cursor_sha: cursor,
                    tip_sha: tip,
                    cursor_task_id,
                    detail: None,
                });
            }
            ChangeProbe::Changed { .. } => {}
            ChangeProbe::Unknown { reason } => {
                decision = Precondition::Inconclusive {
                    token: UNCHANGED_SINCE_LAST_SWEEP,
                    reason,
                };
            }
        }
    }
    if definition.skip_unless_blocked_candidates {
        let probe = host
            .probe_blocked_candidates(definition, now)
            .unwrap_or_else(|error| BlockedCandidateProbe::Unknown {
                reason: format!("probe failed: {error}"),
            });
        match probe {
            BlockedCandidateProbe::None { blocked } => {
                return Precondition::Skip(AutoTaskSkipRecord {
                    at: now.to_rfc3339(),
                    slot: slot.to_string(),
                    reason: NO_BLOCKED_CANDIDATES.to_string(),
                    reference: String::new(),
                    cursor_sha: String::new(),
                    tip_sha: String::new(),
                    cursor_task_id: None,
                    detail: Some(format!(
                        "{blocked} blocked task(s) read; no block episode awaits triage"
                    )),
                });
            }
            BlockedCandidateProbe::Qualifying { .. } => {}
            BlockedCandidateProbe::Unknown { reason } => {
                if matches!(decision, Precondition::Proceed) {
                    decision = Precondition::Inconclusive {
                        token: NO_BLOCKED_CANDIDATES,
                        reason,
                    };
                }
            }
        }
    }
    decision
}

/// Ask the host, folding a host-side error into the fail-open answer so a
/// broken probe can never stop a definition from minting.
fn probe(
    host: &dyn AutoTaskDispatch,
    definition: &AutoTaskDefinition,
    precondition: &SkipIfUnchanged,
) -> ChangeProbe {
    match host.probe_change_since_last_sweep(definition, precondition) {
        Ok(probe) => probe,
        Err(error) => ChangeProbe::Unknown {
            reason: format!("probe failed: {error}"),
        },
    }
}
