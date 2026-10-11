use orbit_engine::DispatchError;
use orbit_types::task::{ReadinessGap, ReadinessStage, Task, TaskOsRequirement, readiness_gaps};

use super::super::auto_admission::{DeferralReason, DeferredAdmission};
use super::{BacklogTaskConflict, BacklogTaskExclusion, BacklogTaskExclusionReason};
use crate::OrbitRuntime;
use crate::application::task::PilotAdmissionHold;

/// A ship's exclusion for a task the unknown-footprint rule withheld. A
/// whole-tree conflict is a context lock on every path, named by its holder.
pub(super) fn footprint_exclusion(deferred: DeferredAdmission) -> BacklogTaskExclusion {
    let reason = match deferred.reason {
        DeferralReason::Conflict => BacklogTaskExclusionReason::ContextLockConflict,
        DeferralReason::AwaitingFootprint => BacklogTaskExclusionReason::AwaitingFootprint,
        DeferralReason::AwaitingExclusiveSlot => BacklogTaskExclusionReason::AwaitingExclusiveSlot,
    };
    BacklogTaskExclusion {
        id: deferred.task_id,
        reason,
        conflicts: deferred
            .conflicts
            .into_iter()
            .map(|conflict| BacklogTaskConflict {
                requested_file: conflict.requested_selector,
                locking_task_id: conflict.blocking_task_id,
            })
            .collect(),
        crew: None,
        detail: deferred.detail,
    }
}

/// The exclusion for a task whose `os:` tags this host does not satisfy.
///
/// Shared by automatic selection and the explicit ship override, so a drain,
/// ship discovery and a gate carrying a named task withhold the same tasks and
/// name the same wait.
pub(super) fn host_os_exclusion(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Option<BacklogTaskExclusion> {
    let reason = TaskOsRequirement::from_tags(&task.tags).unsatisfied_reason(runtime.host_os())?;
    Some(BacklogTaskExclusion {
        id: task.id.clone(),
        reason: BacklogTaskExclusionReason::HostOsMismatch,
        conflicts: Vec::new(),
        crew: None,
        detail: Some(reason),
    })
}

/// The exclusion for an inherited-only `epic` root.
///
/// Shared by automatic backlog selection and the explicit ship override so both
/// withhold the same population and say the same thing about it. The detail
/// names the descendants that do declare context: those are what an operator
/// writes the root's own `context_files` from, and they are why this root is
/// withheld at all.
pub(super) fn inherited_only_epic_root_exclusion(
    task_id: &str,
    descendants_with_context: &[&str],
) -> BacklogTaskExclusion {
    BacklogTaskExclusion {
        id: task_id.to_string(),
        reason: BacklogTaskExclusionReason::InheritedOnlyEpicRoot,
        conflicts: Vec::new(),
        crew: None,
        detail: Some(format!(
            "this task carries the `epic` size tag and declares no `context_files` of its own, \
             while its descendants do. Admission no longer inherits their surface, so it would \
             run holding no reservation against the work it would touch. Declare its own surface \
             with `orbit task update --context`, or retire the root. Descendants that declare \
             context: {}.",
            descendants_with_context.join(", ")
        )),
    }
}

/// The one complexity-admission rule, shared by automatic backlog selection,
/// explicit ship selection, and the readiness diagnostic that reports their
/// exclusions.
///
/// An implementation lane needs a complexity assessment to size the work it is
/// about to do. Work tagged exactly `no-diff-expected` produces its durable
/// result outside the repository, so requiring task-pilot preparation only to
/// clear this gate withholds operational work for a judgement it does not
/// consume [ORB-12118]. The exemption is the tag alone: automated mint
/// provenance does not grant it, and nothing here rewrites the task's stored
/// complexity — an exempt task keeps `unassessed` and resolves its crew from
/// the configured crew or the workspace default.
/// Whether a backlog task has no blocking readiness gap. Missing context
/// files are advisory in the backlog, so this is the assessed-complexity gate
/// with its `no-diff-expected` exemption.
pub(super) fn clears_complexity_gate(task: &Task) -> bool {
    !readiness_gaps(
        ReadinessStage::Backlog,
        &task.tags,
        &task.context_files,
        task.complexity,
    )
    .iter()
    .any(ReadinessGap::is_blocking)
}

pub(super) fn pilot_finding_exclusion(
    runtime: &OrbitRuntime,
    action: &str,
    task: &Task,
) -> Result<Option<BacklogTaskExclusion>, DispatchError> {
    let hold = runtime.pilot_admission_hold(&task.id).map_err(|error| {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("read pilot findings: {error}"),
        }
    })?;
    Ok(hold.and_then(|hold| {
        let (reason, detail) = match hold {
            PilotAdmissionHold::HostOperational(hold) => (
                BacklogTaskExclusionReason::HostOperationalHandoff,
                hold.detail(),
            ),
            PilotAdmissionHold::OperatorValidation(hold) => (
                BacklogTaskExclusionReason::OperatorValidationHandoff,
                hold.detail(),
            ),
            PilotAdmissionHold::NativeOs(hold) => (
                BacklogTaskExclusionReason::NativeOsRequired,
                hold.wait_on(
                    task,
                    runtime.host_os(),
                    runtime.automation_machine_identity(),
                )?,
            ),
            PilotAdmissionHold::Duplicate => (
                BacklogTaskExclusionReason::PilotDuplicate,
                pilot_decision_detail("duplicate_of"),
            ),
            PilotAdmissionHold::AlreadyLanded => (
                BacklogTaskExclusionReason::PilotAlreadyLanded,
                pilot_decision_detail("already_landed"),
            ),
        };
        Some(BacklogTaskExclusion {
            id: task.id.clone(),
            reason,
            conflicts: Vec::new(),
            crew: None,
            detail: Some(detail),
        })
    }))
}

fn pilot_decision_detail(field: &str) -> String {
    format!(
        "The latest task-pilot assessment records {field}. Run task-pilot again \
         to clear the finding, or append a human comment whose first line is \
         `task-pilot-admission: approve-anyway` or `task-pilot-admission: clear`. \
         A later pilot assessment supersedes that decision."
    )
}

pub(super) fn pilot_preparation_exclusion(task_id: &str) -> BacklogTaskExclusion {
    BacklogTaskExclusion {
        id: task_id.to_string(),
        reason: BacklogTaskExclusionReason::ActivePilotPreparation,
        conflicts: Vec::new(),
        crew: None,
        detail: Some(
            "An active task-pilot preparation holds this task until its run settles.".into(),
        ),
    }
}
