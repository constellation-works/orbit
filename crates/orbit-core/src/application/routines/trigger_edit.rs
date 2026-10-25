//! Replace a routine definition's trigger in place: the one write every
//! surface (dashboard, `orbit.routine.control`) runs to reschedule a routine.
//! Name, target, policy and `enabled` stay read-only here; the toggle owns
//! `enabled`.

use std::path::Path;

use orbit_automation::routines::loader::retired_routine_job_reason;
use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_types::workflow::automation::members::StateTriggerKind;
use orbit_types::workflow::{OverlapPolicy, RoutineTarget, RoutineTrigger};
use serde::Serialize;

use super::due::parse_cron;
use super::loader::LoadedRoutine;
use super::status::{
    RoutineStatusReport, RoutineToggleRefusal, checkout_edit_report, checkout_refusal,
    select_listed_routine,
};
use super::template::is_seeded_routine_file;
use crate::OrbitRuntime;

/// Every state-trigger kind; [`trigger_options`] filters it per definition.
const STATE_TRIGGER_KINDS: [StateTriggerKind; 6] = [
    StateTriggerKind::PreparationEligible,
    StateTriggerKind::ExecutionFailed,
    StateTriggerKind::FindingUnverified,
    StateTriggerKind::PilotWarningsHeld,
    StateTriggerKind::BlockEscalated,
    StateTriggerKind::DecisionRequested,
];

/// Optimistic outcome for a routine trigger edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutineTriggerEditOutcome {
    /// The definition was atomically changed.
    Changed,
    /// The requested trigger already matched the current definition.
    Unchanged,
    /// The caller's expected trigger was stale, so no write occurred.
    Conflict {
        /// The trigger the definition holds now.
        actual_trigger: Box<RoutineTrigger>,
    },
    /// The definition now dispatches a different target than the one the
    /// caller selected, so no write occurred.
    TargetConflict {
        /// The target the definition holds now.
        actual_target: RoutineTarget,
    },
}

/// Which trigger kinds a definition may switch to without changing its
/// target or policy, which this edit never touches. A state trigger must
/// target its kind's job and, like a delivery trigger, forbid overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutineTriggerOptions {
    /// Trigger families: `cron`, and `state` / `deliveries_landed` where the
    /// definition's policy admits them.
    pub kinds: Vec<&'static str>,
    /// State-trigger kinds whose job is this definition's target. Kinds whose
    /// job this Orbit no longer ships are never offered.
    pub state_kinds: Vec<StateTriggerKind>,
    /// The definition is a file Orbit seeds: once edited, sync preserves it
    /// and stops refreshing it from the shipped template.
    pub seeded: bool,
}

/// The trigger kinds `routine` may be switched to by [`set_routine_trigger`].
pub fn trigger_options(routine: &LoadedRoutine) -> RoutineTriggerOptions {
    let definition = &routine.definition;
    let forbid = definition.policy.overlap == OverlapPolicy::Forbid;
    let job = definition.target.job_name();
    let state_kinds = STATE_TRIGGER_KINDS
        .into_iter()
        .filter(|kind| forbid && kind.job_name() == job)
        .filter(|kind| retired_routine_job_reason(kind.job_name()).is_none())
        .collect::<Vec<_>>();
    let mut kinds = vec!["cron"];
    if !state_kinds.is_empty() {
        kinds.push("state");
    }
    if forbid {
        kinds.push("deliveries_landed");
    }
    RoutineTriggerOptions {
        kinds,
        state_kinds,
        seeded: is_seeded_routine_file(&routine.path),
    }
}

/// Select routine `name` from `report` and replace its trigger.
///
/// Selection is [`select_listed_routine`]; the caller's `target` must still
/// be the definition's, and `expected` the trigger it observed, before
/// [`set_routine_trigger`] writes. The outer error is a failed or invalid
/// write.
pub fn edit_listed_routine_trigger(
    report: &RoutineStatusReport,
    name: &str,
    source: Option<(&str, &Path)>,
    target: &str,
    expected: &RoutineTrigger,
    trigger: RoutineTrigger,
) -> Result<Result<RoutineTriggerEditOutcome, RoutineToggleRefusal>, OrbitError> {
    let routine = match select_listed_routine(report, name, source) {
        Ok(routine) => routine,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if routine.definition.target.as_ref_string() != target {
        return Ok(Ok(RoutineTriggerEditOutcome::TargetConflict {
            actual_target: routine.definition.target.clone(),
        }));
    }
    set_routine_trigger(routine, expected, trigger).map(Ok)
}

/// Replace one routine's trigger in a selected checkout, under the same role
/// guard and selection as the toggle.
pub(crate) fn edit_checkout_routine_trigger(
    runtime: &OrbitRuntime,
    name: &str,
    target: &str,
    expected: &RoutineTrigger,
    trigger: RoutineTrigger,
) -> Result<RoutineTriggerEditOutcome, OrbitError> {
    let report = checkout_edit_report(runtime)?;
    edit_listed_routine_trigger(&report, name, None, target, expected, trigger)?
        .map_err(checkout_refusal)
}

/// Change only the `trigger` block of a routine definition.
///
/// The path comes from a freshly loaded [`LoadedRoutine`], never from a
/// transport payload. The new trigger passes the loader's checks (definition
/// validation and, for a cron trigger, cron parsing) before anything is
/// written. Only the top-level `trigger:` block is rewritten; comments inside
/// it are not kept, every other byte is. The rewritten document is parsed and
/// compared before the atomic rename, so an edit cannot alter any other field.
pub fn set_routine_trigger(
    routine: &LoadedRoutine,
    expected: &RoutineTrigger,
    trigger: RoutineTrigger,
) -> Result<RoutineTriggerEditOutcome, OrbitError> {
    let raw = std::fs::read_to_string(&routine.path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", routine.path.display())))?;
    let current = parse_routine_yaml(&raw)?;
    if current.name != routine.definition.name {
        return Err(OrbitError::InvalidInput(format!(
            "routine definition at {} changed identity from '{}' to '{}'",
            routine.path.display(),
            routine.definition.name,
            current.name
        )));
    }
    if current.target != routine.definition.target {
        return Ok(RoutineTriggerEditOutcome::TargetConflict {
            actual_target: current.target,
        });
    }
    if &current.trigger != expected {
        return Ok(RoutineTriggerEditOutcome::Conflict {
            actual_trigger: Box::new(current.trigger),
        });
    }
    if current.trigger == trigger {
        return Ok(RoutineTriggerEditOutcome::Unchanged);
    }

    let mut edited = current;
    edited.trigger = trigger;
    edited
        .validate()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    if edited.trigger.deliveries_landed.is_none() && edited.trigger.state.is_none() {
        parse_cron(&edited.trigger.cron)?;
    }
    let rendered = rewrite_trigger_block(&raw, &edited.trigger)?;
    if parse_routine_yaml(&rendered)? != edited {
        return Err(OrbitError::Execution(
            "routine trigger edit changed fields other than trigger".to_string(),
        ));
    }
    atomic_write_text(&routine.path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", routine.path.display())))?;
    Ok(RoutineTriggerEditOutcome::Changed)
}

/// Replace the top-level `trigger:` block of a routine document. The block
/// runs from its key to the next top-level key; blank and column-0 comment
/// lines just before that key stay with it.
fn rewrite_trigger_block(raw: &str, trigger: &RoutineTrigger) -> Result<String, OrbitError> {
    #[derive(Serialize)]
    struct Block<'a> {
        trigger: &'a RoutineTrigger,
    }
    let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let lines = raw.split_inclusive('\n').collect::<Vec<_>>();
    let content = |line: &str| line.trim_end_matches(['\r', '\n']).to_string();
    let Some(start) = lines
        .iter()
        .position(|line| content(line).starts_with("trigger:"))
    else {
        return Err(OrbitError::InvalidInput(
            "routine definition has no canonical top-level `trigger:` field".to_string(),
        ));
    };
    let mut end = lines[start + 1..]
        .iter()
        .position(|line| {
            let text = content(line);
            !text.is_empty() && !text.starts_with([' ', '\t', '#'])
        })
        .map_or(lines.len(), |offset| start + 1 + offset);
    while end > start + 1 {
        let text = content(lines[end - 1]);
        if text.trim().is_empty() || text.starts_with('#') {
            end -= 1;
        } else {
            break;
        }
    }
    let block = serde_yaml::to_string(&Block { trigger })
        .map_err(|error| OrbitError::Io(format!("encode routine trigger: {error}")))?;
    let mut rendered = String::with_capacity(raw.len() + block.len());
    rendered.extend(lines[..start].iter().copied());
    for line in block.lines() {
        rendered.push_str(line);
        rendered.push_str(newline);
    }
    rendered.extend(lines[end..].iter().copied());
    Ok(rendered)
}
