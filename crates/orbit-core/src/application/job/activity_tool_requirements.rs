//! Which activities consume a task's required-tool contract, and which tools
//! a requirement or an experiment arm may name.
//!
//! The application owns this policy; the runtime-host adapter applies it
//! while translating a tool grant for the engine.

#[cfg(test)]
use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    self, GrantedToolEffect, ToolGrantClassification, ToolGrantability,
};
use orbit_engine::activity_job::catalog_error_to_orbit;
use orbit_types::tool::is_exact_canonical_tool_name;
use orbit_types::workflow::activity_job::{ActivityToolDenyPolicy, ActivityV2Spec, tool_allowed};

use crate::OrbitRuntime;

/// The activity whose tool contract `required_tools` and `granted_tools` name.
pub(crate) const IMPLEMENTING_ACTIVITY: &str = "agent_implement";

/// How a disallow entry relates to a grant, using the same order admission uses.
///
/// `NotCovered` and [`DisallowGrant::Liftable`] may be required. The other
/// variants cannot be lifted by any grant, so filing refuses them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DisallowGrant {
    /// The disallow list does not cover this exact name.
    NotCovered,
    /// An exact read/repeat-safe denial a later grant could lift.
    Liftable,
    /// Covered, and classified mutating, so a grant never runs it live.
    Mutating,
    /// Covered, and always operator-only, so a grant cannot lift the denial.
    OperatorOnly,
    /// An exact grant would still leave a wildcard entry covering the name.
    Wildcard { entry: String },
}

/// Classify `tool_name` against `disallow_list` the way implementer admission
/// classifies a grant. Callers share this so filing and dispatch cannot drift.
pub(crate) fn classify_disallow_grant(
    disallow_list: &[String],
    tool_name: &str,
    classification: ToolGrantClassification,
) -> DisallowGrant {
    if !tool_allowed(tool_name, disallow_list) {
        return DisallowGrant::NotCovered;
    }
    if classification.grantability == ToolGrantability::AlwaysOperatorOnly {
        return DisallowGrant::OperatorOnly;
    }
    if let Some(entry) = disallow_list.iter().find(|entry| {
        entry.as_str() != tool_name && tool_allowed(tool_name, std::slice::from_ref(*entry))
    }) {
        return DisallowGrant::Wildcard {
            entry: entry.clone(),
        };
    }
    if classification.effect == GrantedToolEffect::Mutating {
        return DisallowGrant::Mutating;
    }
    DisallowGrant::Liftable
}

/// Grant classification admission and filing both read.
///
/// Tests may substitute one tool's classification. Production uses the
/// reviewed list, which defaults unclassified tools to mutating.
pub(crate) fn agent_tool_grant_classification(tool_name: &str) -> ToolGrantClassification {
    #[cfg(test)]
    if let Some(classification) = GRANT_CLASSIFICATION_OVERRIDE.with(|slot| {
        slot.borrow()
            .as_ref()
            .and_then(|(name, classification)| (name == tool_name).then_some(*classification))
    }) {
        return classification;
    }
    authorization::classify_agent_tool_grant(tool_name)
}

#[cfg(test)]
thread_local! {
    static GRANT_CLASSIFICATION_OVERRIDE: RefCell<Option<(String, ToolGrantClassification)>> =
        const { RefCell::new(None) };
}

/// Install a one-tool classification override for the current thread and
/// clear it when the guard drops, including on panic.
#[cfg(test)]
pub(crate) fn set_grant_classification_override(
    tool_name: &str,
    classification: ToolGrantClassification,
) -> GrantClassificationOverrideGuard {
    GRANT_CLASSIFICATION_OVERRIDE.with(|slot| {
        *slot.borrow_mut() = Some((tool_name.to_string(), classification));
    });
    GrantClassificationOverrideGuard
}

/// Clears [`set_grant_classification_override`] on drop.
#[cfg(test)]
pub(crate) struct GrantClassificationOverrideGuard;

#[cfg(test)]
impl Drop for GrantClassificationOverrideGuard {
    fn drop(&mut self) {
        GRANT_CLASSIFICATION_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Why `required_tools` cannot name `tool_name` on `activity`'s disallow list.
///
/// `None` when the name is not covered, or when an exact grant of a
/// read/repeat-safe tool could lift the denial later. The text names the
/// tool, the activity, the denial, and an operator step instead of the
/// requirement.
pub(crate) fn unliftable_required_tool_error(
    activity: &str,
    disallow_list: &[String],
    tool_name: &str,
) -> Option<String> {
    let why = match classify_disallow_grant(
        disallow_list,
        tool_name,
        agent_tool_grant_classification(tool_name),
    ) {
        DisallowGrant::NotCovered | DisallowGrant::Liftable => return None,
        DisallowGrant::Mutating => {
            "it is classified mutating, and a mutating granted tool is never called live"
                .to_string()
        }
        DisallowGrant::OperatorOnly => {
            "the tool is always operator-only, so a grant cannot lift its denial".to_string()
        }
        DisallowGrant::Wildcard { entry } => format!(
            "a grant lifts only an exact disallow entry, and `{entry}` still covers it for {activity}"
        ),
    };
    let denial = ActivityToolDenyPolicy {
        activity: activity.to_string(),
        disallow_list: disallow_list.to_vec(),
    }
    .denial_message(tool_name);
    Some(format!(
        "required_tools cannot require '{tool_name}': {denial}; {why}. Have an operator perform that step, or file it as a separate operator-owned task"
    ))
}

/// The implementing activity's deny list, when that activity is a deny-mode
/// agent loop.
///
/// `Ok(None)` when the activity is absent, not an agent loop, or still in
/// allow mode: filing does not invent a refusal the catalog does not state.
/// A catalog that fails to load is an error, so a corrupt catalog cannot
/// skip the check.
pub(crate) fn implementing_activity_disallow_list(
    runtime: &OrbitRuntime,
) -> Result<Option<Vec<String>>, OrbitError> {
    let catalog = runtime
        .v2_activity_catalog()
        .map_err(catalog_error_to_orbit)?;
    let Some(activity) = catalog.get(IMPLEMENTING_ACTIVITY) else {
        return Ok(None);
    };
    let ActivityV2Spec::AgentLoop(spec) = &activity.spec else {
        return Ok(None);
    };
    Ok(spec.tool_disallow_list.clone())
}

/// Shipped agent activities that are not the task's implementer.
///
/// These activities still validate every requirement, but a tool their
/// disallow list covers is omitted. A name absent from this list, including
/// a custom implementer, keeps fail-closed admission. Add a new shipped
/// recovery or review activity here when it should start even though the
/// task requires a tool it disallows.
const NON_IMPLEMENTER_ACTIVITIES: &[&str] = &[
    "agent_review_repair",
    "final_recovery",
    "image_input_probe",
    "pr_conflict_recovery",
    "resident_cycle",
    "review_reconciliation_review",
    "step_failure_recovery",
    "task_pilot",
    "triage_escalation",
];

/// Whether a deny-mode activity omits covered task requirements rather than
/// refusing admission. Implementers retain the fail-closed default.
pub(crate) fn drops_disallowed_requirements(activity: &str) -> bool {
    NON_IMPLEMENTER_ACTIVITIES.contains(&activity)
}

/// Why `tool_name` cannot be granted beyond an activity's baseline: it is not
/// an exact canonical name, or not a registered, agent-facing, enabled tool.
/// `None` when it can be. Task requirements, grants and experiment arm additions
/// share this availability rule; task grants also check governed capability.
pub(crate) fn granted_tool_refusal(
    runtime: &OrbitRuntime,
    tool_name: &str,
) -> Result<Option<String>, OrbitError> {
    let reason = if tool_name.contains('*') {
        "wildcard and prefix requirements are not allowed"
    } else if !is_exact_canonical_tool_name(tool_name) {
        "malformed canonical tool name"
    } else if !runtime.tool_registry().has(tool_name) {
        "unknown registered tool"
    } else if !runtime.tool_registry().is_active(tool_name) {
        "tool is not agent-facing"
    } else if runtime
        .stores()
        .tools()
        .get_tool(tool_name)?
        .is_some_and(|tool| !tool.enabled)
    {
        "tool is inactive"
    } else {
        return Ok(None);
    };
    Ok(Some(reason.to_string()))
}
