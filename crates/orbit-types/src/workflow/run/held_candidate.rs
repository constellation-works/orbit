//! A committed candidate an owner-local run held for a later run of its task
//! [ORB-14905].
//!
//! A red base, a missing validation tool or a failed provider ends a run
//! without judging its candidate, and the run's failure handoff keeps the
//! candidate rather than publishing it. Under a distributed drain the task's
//! next run may be a claim on another host, whose object store does not have
//! that commit. The handoff therefore pushes the candidate to the durable ref
//! a claimed leaf uses (`refs/orbit/candidates/<task>/<run>`, [ORB-14338]) and
//! records where it is in a [`CANDIDATE_HELD_EVENT`] history entry, whose note
//! carries the typed [`HeldCandidate`] as JSON straight after
//! [`CANDIDATE_HELD_MARKER`]. Admission reads it to hand the task's next
//! claim the candidate, whichever host claims it.

use serde::{Deserialize, Serialize};

/// Task history event recording a candidate an owner-local run held.
pub const CANDIDATE_HELD_EVENT: &str = "candidate_held";

/// The marker a held candidate's history note starts with, followed by the
/// candidate JSON.
pub const CANDIDATE_HELD_MARKER: &str = "[candidate_held]";

/// Where a held candidate is, and the task spec it answered to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldCandidate {
    /// The run that committed and held it.
    pub run_id: String,
    /// The machine that run executed on, whose object store has the commit.
    /// Absent when the host has no machine identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    pub branch: String,
    pub head_sha: String,
    /// The step the run failed at.
    pub failed_step_id: String,
    /// The ref on `origin` the candidate was pushed to, so a run on any host
    /// can fetch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_ref: Option<String>,
    /// Why the push to a durable ref failed; the candidate stays on the host
    /// that made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carry_failure: Option<String>,
    /// The task's spec digest when the candidate was held; a later change to
    /// the description or criteria retires it.
    pub task_spec_digest: String,
}

impl HeldCandidate {
    /// The history note: the marker, this candidate and a readable summary.
    #[must_use]
    pub fn text(&self) -> String {
        let held = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        let place = match (&self.durable_ref, &self.carry_failure) {
            (Some(reference), _) => format!("durable at {reference}"),
            (None, Some(failure)) => format!("host-local: {failure}"),
            (None, None) => "host-local".to_string(),
        };
        format!(
            "{CANDIDATE_HELD_MARKER} {held} run={}, candidate={}, branch={}; {place}",
            self.run_id, self.head_sha, self.branch
        )
    }

    /// The held candidate a history note carries, if any.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let (_, rest) = text.split_once(CANDIDATE_HELD_MARKER)?;
        serde_json::Deserializer::from_str(rest.trim_start())
            .into_iter::<Self>()
            .next()?
            .ok()
            .filter(|held| !held.run_id.trim().is_empty() && !held.head_sha.trim().is_empty())
    }
}
