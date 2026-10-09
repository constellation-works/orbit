//! What a task-pilot apply admits: the prepared snapshot of each task, the
//! validated task and its promotion decision, and the promotion steps that run
//! once the pilot write has landed.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{NoDiffClosure, PILOT_VERIFIED_NO_DIFF};

use super::drain_promotion::{self, Approval, DrainAuthority};
use super::{CONTEXT_CREATION_IDENTITY, action_failed};

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

pub(super) fn material_components(
    entry: &Value,
    action: &str,
) -> Result<Option<BTreeMap<String, String>>, DispatchError> {
    let Some(value) = entry.get("material_components") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(action_failed(
            action,
            "prepared task material_components must be an object of component digests",
        ));
    };
    let mut components = BTreeMap::new();
    for (key, digest) in object {
        let Some(digest) = digest.as_str() else {
            return Err(action_failed(
                action,
                format!("prepared task material_components.{key} must be a string digest"),
            ));
        };
        components.insert(key.clone(), digest.to_string());
    }
    Ok(Some(components))
}

pub(super) fn context_creation_identity(
    entry: &Value,
    action: &str,
) -> Result<Option<String>, DispatchError> {
    match entry.get(CONTEXT_CREATION_IDENTITY) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(identity)) => Ok(Some(identity.clone())),
        Some(_) => Err(action_failed(
            action,
            format!("prepared task {CONTEXT_CREATION_IDENTITY} must be a string or null"),
        )),
    }
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
