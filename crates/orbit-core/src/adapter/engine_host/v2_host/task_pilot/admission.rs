//! What a task-pilot apply admits: the prepared snapshot of each task, the
//! validated task and its promotion decision, and the promotion steps that run
//! once the pilot write has landed.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::{TaskComplexity, TaskRelation, TaskStatus};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{NoDiffClosure, PILOT_VERIFIED_NO_DIFF};

use super::drain_promotion::{self, Approval, DrainAuthority};

#[derive(Clone)]
pub(super) struct PreparedTaskSnapshot {
    pub(super) context_files: Vec<String>,
    pub(super) status: TaskStatus,
    pub(super) complexity: Option<TaskComplexity>,
    pub(super) title: String,
    pub(super) tags: Vec<String>,
    pub(super) material: Option<(String, String)>,
    pub(super) status_neutral_fingerprint: Option<String>,
    /// Freshness-component digests captured with the fingerprint. Absent on a
    /// payload prepared before components were recorded; malformed input fails
    /// the apply instead of dropping the names.
    pub(super) material_components: Option<BTreeMap<String, String>>,
    /// History boundary used to prove a durable edit superseded preparation.
    /// Older payloads retain their existing drift refusal without that evidence.
    pub(super) history_len: Option<usize>,
    /// Deterministic feasibility findings for the tools this task's acceptance
    /// criteria require, computed at preparation [ORB-11980].
    pub(super) validation_tool_warnings: Vec<String>,
    /// Identity of the durable context creation grant the task held at
    /// preparation, `None` for none (or a payload prepared before grants).
    pub(super) context_creation_identity: Option<String>,
}

pub(super) struct ValidatedTask {
    pub(super) task_id: String,
    pub(super) after: Vec<String>,
    pub(super) assessment: Value,
    pub(super) admission: Option<Admission>,
    /// Write `backlog` in the atomic pilot mutation itself (CI sweep).
    pub(super) promote: bool,
    /// Appended to the pilot's history entry, e.g. a drain hold marker.
    pub(super) history_marker: Option<String>,
    pub(super) complexity: TaskComplexity,
    pub(super) operation_id: String,
    /// The typed native-OS finding, persisted as the audit's
    /// `native_os_hold`.
    pub(super) required_os: Vec<crate::application::task::NativeOsRequirement>,
    /// The typed owner-machine finding, persisted in the same hold.
    pub(super) required_machine: Option<crate::application::task::MachineRequirement>,
    /// Exact material certified by the atomic write, recovered from its audit
    /// on replay rather than adopted from a later task read.
    pub(super) applied_material: Option<AppliedTaskMaterial>,
}

/// The pilot's authorized output, sealed in the same atomic mutation as its
/// receipt. Selectors and complexity are already carried by `ValidatedTask`.
#[derive(Serialize, Deserialize)]
pub(super) struct AppliedTaskMaterial {
    pub(super) tags: Vec<String>,
    pub(super) crew: Option<String>,
    pub(super) crew_source: Option<String>,
    pub(super) relations: Vec<TaskRelation>,
    pub(super) material: Option<(String, String)>,
    pub(super) status_neutral_fingerprint: Option<String>,
    pub(super) material_components: Option<BTreeMap<String, String>>,
}

/// One task's promotion decision, by the authority that requested it.
pub(super) enum Admission {
    CiSweep(Value),
    Drain(Value),
}

/// Who, if anyone, authorized this apply to move `proposed` work to backlog.
pub(super) enum PromotionAuthority<'a> {
    None,
    /// A CI sweep's exact filing record; `promotion_authorized` is the literal
    /// authority, carried into each decision.
    CiSweep(&'a Value, bool),
    /// A verified `--approve-proposed` drain.
    Drain(DrainAuthority),
}

pub(super) enum CheckedAdmission {
    Apply(Option<Admission>),
    Superseded(Value),
}

/// Approve a drain-promoted task once its pilot write landed, recording on
/// the decision whether this apply made the transition. A hold found at the
/// approval boundary turns the decision into a withhold with its own
/// classification.
pub(super) fn approve_promoted(
    runtime: &OrbitRuntime,
    validated: &mut ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
    drain: &DrainAuthority,
) -> Result<(), OrbitError> {
    let Some(Admission::Drain(decision)) = validated.admission.as_ref() else {
        return Ok(());
    };
    if decision["decision"] != "promote" {
        return Ok(());
    }
    let approval = drain_promotion::approve(runtime, validated, snapshot, policy, &drain.run_id)?;
    let Some(Admission::Drain(decision)) = validated.admission.as_mut() else {
        return Ok(());
    };
    decision["approved"] = json!(matches!(approval, Approval::Approved));
    if let Approval::Held {
        classification,
        evidence,
    } = approval
    {
        decision["decision"] = json!("withhold");
        decision["classification"] = json!(classification);
        decision["evidence"] = evidence;
    }
    Ok(())
}

/// Close a task either authority held as `pilot_verified_no_diff` once its
/// pilot write landed, recording on the decision whether it was archived and
/// on which commits, or why it stays proposed. The pilot write already
/// stands, so a failed close is reported rather than failing the apply.
pub(super) fn close_verified_no_diff(runtime: &OrbitRuntime, validated: &mut ValidatedTask) {
    let decision = match validated.admission.as_mut() {
        Some(Admission::CiSweep(decision) | Admission::Drain(decision))
            if decision["classification"] == PILOT_VERIFIED_NO_DIFF =>
        {
            decision
        }
        _ => return,
    };
    match runtime.close_verified_no_diff(&validated.task_id) {
        Ok(Some(NoDiffClosure::Archived { covering_commits })) => {
            decision["closed"] = json!(true);
            decision["covering_commits"] = json!(covering_commits);
        }
        Ok(Some(NoDiffClosure::Held { reason, .. })) => {
            decision["closed"] = json!(false);
            decision["not_closed_reason"] = json!(reason);
        }
        Ok(None) => decision["closed"] = json!(false),
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.task_pilot",
                task_id = %validated.task_id,
                %error,
                "could not close a task task-pilot verified as already fixed"
            );
            decision["closed"] = json!(false);
            decision["not_closed_reason"] = json!(error.to_string());
        }
    }
}
