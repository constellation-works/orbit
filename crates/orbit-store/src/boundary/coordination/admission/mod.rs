//! Owner admission foundation. Reached only through `orbit-core`'s trusted
//! owner seams — the `orbit.task.pull` tool and the owner-local drain adapter —
//! which supply the caller identity from the session, never from the request.

mod admit;
mod census;
mod footprint;
mod lookup;
mod refusal;
mod retention;

use super::TaskCommitBoundary;
use crate::contract::*;
pub(super) use admit::{
    ClaimReceiptRowsInput, admission_placeholder_rows, build_claim_receipt_rows,
};
pub(super) use lookup::decode_claim;
#[cfg(test)]
pub(super) use lookup::{claim_decodes, reset_claim_decodes};
use orbit_common::OrbitError;
use orbit_common::fs::selector::canonical_selector_in_workspace;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry};
pub use refusal::admission_refusal;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) const RECEIPT_KIND: &str = "distributed-admission-receipt-v1";

pub const ADMISSION_RESERVATION_TTL_SECONDS: u32 = 14_400;
/// The in-section re-check of assessment-scoped holds a caller gives
/// [`TaskCommitBoundary::admit_task`]: the candidate with the comments and
/// history read with it, to the hold's diagnostic, or `None` when it is free.
pub type ValidationHold<'a> =
    dyn Fn(&Task, &[TaskComment], &[TaskHistoryEntry]) -> Result<Option<String>, OrbitError> + 'a;
/// Owner callback invoked only for the selected task, before claim persistence.
pub type AdmissionGuidanceCapture<'a> =
    dyn Fn(&Task) -> Result<AdmissionTaskGuidance, OrbitError> + 'a;

/// The owner's fresh crew policy for a candidate: the task it screens and summarizes.
pub type EffectiveTaskPolicy<'a> = dyn Fn(&Task) -> Result<Task, OrbitError> + 'a;

/// The inputs of one owner admission section, borrowed from the caller: the
/// trusted identity and its request, the ordering and workspace it admits
/// into, the holds that defer candidates and the owner's callbacks.
#[derive(Clone, Copy)]
pub struct AdmissionCall<'a> {
    pub identity: &'a AdmissionIdentity,
    pub request: &'a AdmissionRequest,
    pub ordering: &'a AdmissionOrdering,
    pub owner_version: &'a str,
    pub repo_root: &'a Path,
    pub orbit_dir: &'a Path,
    /// Tasks a live owner-local delivery or pilot finding holds, with the
    /// trusted diagnostic for each.
    pub admission_holds: &'a BTreeMap<String, String>,
    /// `backlog` tasks the owner withholds for a red base, with why.
    pub held: &'a BTreeMap<String, String>,
    pub validation_hold: &'a ValidationHold<'a>,
    /// The owner's fresh crew policy for screening and the captured summary.
    /// `None` keeps the stored task.
    pub effective_task: Option<&'a EffectiveTaskPolicy<'a>>,
    /// The owner's guidance for the selected task, captured before the claim
    /// is persisted. `None` captures no experiments or artifacts.
    pub guidance_capture: Option<&'a AdmissionGuidanceCapture<'a>>,
}

impl AdmissionCall<'_> {
    /// The fresh-policy callback in force, or the stored task unchanged.
    pub(super) fn effective_task_or_default(&self) -> &EffectiveTaskPolicy<'_> {
        self.effective_task.unwrap_or(&keep_task)
    }

    /// The guidance callback in force, or none captured.
    pub(super) fn guidance_capture_or_default(&self) -> &AdmissionGuidanceCapture<'_> {
        self.guidance_capture.unwrap_or(&no_guidance)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum StoredReceipt {
    Full {
        receipt: Box<AdmissionReceipt>,
    },
    Tombstone {
        machine_id: String,
        request_id: String,
        input_digest: String,
    },
}

pub(super) fn digest<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    Ok(sha256_hex(
        super::lifecycle::codec::encode(value)?.as_bytes(),
    ))
}
fn receipt_key(machine: &str, request: &str) -> Result<String, OrbitError> {
    digest(&(machine, request))
}
pub(super) fn canonical_footprint(
    files: &[String],
    root: &Path,
) -> Result<Vec<String>, OrbitError> {
    files
        .iter()
        .map(|f| {
            canonical_selector_in_workspace(f, root)
                .map_err(|e| OrbitError::InvalidInput(format!("invalid context selector {f}: {e}")))
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|files| files.into_iter().collect())
}

/// The stored task stands: no fresh crew policy is applied.
fn keep_task(task: &Task) -> Result<Task, OrbitError> {
    Ok(task.clone())
}

/// Nothing is captured: the claim records no experiments or artifacts.
fn no_guidance(task: &Task) -> Result<AdmissionTaskGuidance, OrbitError> {
    Ok(AdmissionTaskGuidance {
        experiments: orbit_types::workflow::CapturedExperiments {
            task_id: task.id.clone(),
            assignments: Vec::new(),
        },
        artifacts: Vec::new(),
    })
}
