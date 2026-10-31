//! The observed-revision proof an escalation triager passes with each
//! `orbit.task.block_triage` decision.
//!
//! A task a live escalation triage run holds may be decided only with proof
//! that the decider read that run's pinned source: `observed_revision`
//! equal to the run's `source_revision` and `observed_tree` equal to that
//! revision's tree, recomputed here. The envelope carries the revision but
//! never its tree, so a triager whose native tools never ran cannot forge
//! the pair. A non-Git run pins no revision: its decisions pass
//! `source_revision` null and report no observed values, as a task pilot
//! does. A task no such run holds (an operator's or the blocked-task triage
//! auto-task's decision) needs no proof, and a run prepared before the proof
//! existed is held to it only when the decision reports one.

use orbit_common::OrbitError;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;
use crate::application::task::pilot::source::{
    inspection_proof_error, inspection_proof_required, is_commit_id, tree_of,
};

/// The proof fields of one triage decision, as the caller passed them.
#[derive(Debug, Clone, Default)]
pub(crate) struct TriageInspectionProof {
    pub(crate) source_revision: Option<String>,
    pub(crate) observed_revision: Option<String>,
    pub(crate) observed_tree: Option<String>,
}

impl TriageInspectionProof {
    fn is_empty(&self) -> bool {
        self.source_revision.is_none()
            && self.observed_revision.is_none()
            && self.observed_tree.is_none()
    }
}

/// The source a live triage run pinned: `None` for a non-Git workspace.
type Pin = Option<String>;

impl OrbitRuntime {
    /// Refuse a triage decision on `task_id` whose proof does not establish
    /// that the decider read the pinned source. Nothing has been recorded
    /// when this refuses.
    pub(crate) fn check_triage_inspection_proof(
        &self,
        task_id: &str,
        proof: &TriageInspectionProof,
    ) -> Result<(), OrbitError> {
        let refuse =
            |message: String| OrbitError::InvalidInput(format!("{message}; nothing was recorded"));
        let source_revision = proof
            .source_revision
            .as_deref()
            .map(str::trim)
            .filter(|revision| !revision.is_empty())
            .map(|revision| {
                is_commit_id(revision)
                    .then(|| revision.to_ascii_lowercase())
                    .ok_or_else(|| {
                        refuse(format!(
                            "block triage inspection proof: source_revision `{revision}` is not a \
                             full commit id (40 or 64 hex characters)"
                        ))
                    })
            })
            .transpose()?;
        let holding = self.escalation_triage_checkpoints(task_id)?;
        let pins = holding
            .iter()
            .filter(|(_, prepared)| inspection_proof_required(prepared))
            .map(|(run_id, prepared)| (run_id.as_str(), pinned_revision(prepared)))
            .collect::<Vec<_>>();
        if pins.is_empty() && proof.is_empty() {
            return Ok(());
        }
        let expected = if pins.is_empty() {
            // No proof is owed, but one was offered: check it as given.
            source_revision.clone().ok_or_else(|| {
                refuse(
                    "block triage inspection proof needs `source_revision` as a full commit id, \
                     with `observed_revision` and `observed_tree` from the inspection checkout"
                        .into(),
                )
            })?
        } else {
            match pins
                .iter()
                .find(|(_, pin)| pin.as_deref() == source_revision.as_deref())
            {
                // A non-Git run pins nothing, and its checkout has no HEAD to
                // report.
                Some((_, None)) => {
                    if proof.observed_revision.is_some() || proof.observed_tree.is_some() {
                        return Err(refuse(format!(
                            "task {task_id} is held by an escalation triage run in a non-Git \
                             workspace: pass `source_revision` null and omit \
                             `observed_revision` and `observed_tree`"
                        )));
                    }
                    return Ok(());
                }
                Some((_, Some(revision))) => revision.clone(),
                None => {
                    let (run_id, pin) = &pins[0];
                    return Err(refuse(match pin {
                        Some(revision) => format!(
                            "task {task_id} is held by escalation triage run {run_id}, which \
                             pinned source_revision {revision}: pass that `source_revision` \
                             with `observed_revision` (`git rev-parse HEAD`) and \
                             `observed_tree` (`git rev-parse 'HEAD^{{tree}}'`) from its \
                             inspection checkout"
                        ),
                        None => format!(
                            "task {task_id} is held by escalation triage run {run_id} in a \
                             non-Git workspace: pass `source_revision` null"
                        ),
                    }));
                }
            }
        };
        let source_tree = tree_of(&self.paths().repo_root, &expected)
            .map_err(|error| refuse(format!("read the pinned source tree: {error}")))?
            .map_err(|_| {
                refuse(format!(
                    "block triage inspection proof: source_revision {expected} has no readable \
                     tree in this workspace"
                ))
            })?;
        match inspection_proof_error(
            true,
            proof.observed_revision.as_deref(),
            proof.observed_tree.as_deref(),
            &expected,
            &source_tree,
        ) {
            Some(error) => Err(refuse(error.message().to_string())),
            None => Ok(()),
        }
    }
}

/// The revision a triage checkpoint pinned, lowercase; `None` for a non-Git
/// workspace.
fn pinned_revision(prepared: &Value) -> Pin {
    prepared
        .get("source")
        .and_then(|source| source.get("source_revision"))
        .and_then(Value::as_str)
        .filter(|revision| !revision.is_empty())
        .map(str::to_ascii_lowercase)
}
