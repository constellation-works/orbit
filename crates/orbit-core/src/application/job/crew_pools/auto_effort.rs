//! `<crew>@auto` pool members resolved to a concrete `<crew>@<effort>` at the
//! draw that admits them ([`orbit_config::AutoEffortPolicy`]).
//!
//! Every conversion from a pool entry to a draw candidate goes through
//! [`OrbitRuntime::pool_entry_candidate`], which takes the draw's
//! [`AutoEffortSubject`]. An implementer draw passes the task's assessed
//! complexity and declared footprint ([`task_auto_effort_subject`]); a task
//! pilot passes its partition's filed complexity; a reviewer passes the
//! reviewable diff and the settled complexity. A draw without a subject runs
//! the crew's configured effort. The candidate carries the resolved crew, so
//! the run, its selection record and every scorecard name `<crew>@<effort>`,
//! never `@auto`.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::identity::{AutoEffortChoice, AutoEffortSignal, Crew, auto_effort_base};
use orbit_types::task::Task;

use crate::OrbitRuntime;
use crate::application::codebase::Anchor;
use crate::application::contracts::{AutoEffortSubject, CrewCandidate};

use super::draw::seeded_crew_ticket;

/// The subject an implementer draw for `task` resolves `@auto` against: its
/// assessed complexity and the number of distinct files its context
/// selectors declare (the task-risk forecast's pre-dispatch size; the final
/// diff never feeds it). Exploration is seeded by the task, so every
/// selection of one task's crew resolves alike.
pub(crate) fn task_auto_effort_subject(task: &Task) -> AutoEffortSubject {
    AutoEffortSubject {
        signal: footprint_signal(task.complexity, &task.context_files),
        seed: Some(format!("auto-effort:{}", task.id)),
        forced: None,
    }
}

/// The signal of a task not yet created: no seed, so it never explores.
pub(crate) fn creation_auto_effort_subject(
    complexity: Option<orbit_types::task::TaskComplexity>,
    context_files: &[String],
) -> AutoEffortSubject {
    AutoEffortSubject {
        signal: footprint_signal(complexity, context_files),
        seed: None,
        forced: None,
    }
}

fn footprint_signal(
    complexity: Option<orbit_types::task::TaskComplexity>,
    context_files: &[String],
) -> AutoEffortSignal {
    let anchors = context_files
        .iter()
        .filter_map(|selector| Anchor::parse(selector))
        .collect::<BTreeSet<_>>();
    AutoEffortSignal {
        complexity: complexity.filter(|tier| tier.is_assessed()),
        lines: None,
        files: (!anchors.is_empty()).then_some(anchors.len() as u64),
    }
}

impl OrbitRuntime {
    /// The draw candidate one pool entry `name` contributes, or `None` when
    /// it names no enabled crew. A `<crew>@auto` entry resolves against
    /// `subject` here, before any filter or weight sees it.
    pub(crate) fn pool_entry_candidate(
        &self,
        name: &str,
        weight: u32,
        subject: Option<&AutoEffortSubject>,
    ) -> Result<Option<CrewCandidate>, OrbitError> {
        let registry = self.context.settings().crews();
        let Some(base) = auto_effort_base(name) else {
            if !orbit_types::identity::find_crew(name, registry).is_some_and(|crew| crew.enabled) {
                return Ok(None);
            }
            return Ok(Some(CrewCandidate {
                crew: self.resolve_crew_for_task(Some(name), None)?,
                weight,
                auto_effort: None,
            }));
        };
        let Some(crew) = registry.get(base).filter(|crew| crew.enabled) else {
            return Ok(None);
        };
        let choice = self.resolve_auto_effort(crew, subject);
        Ok(Some(CrewCandidate {
            crew: self.resolve_crew_for_task(Some(&choice.resolved), None)?,
            weight,
            auto_effort: Some(choice),
        }))
    }

    /// How `crew` (the `[crews.<name>]` entry of a `<crew>@auto` member)
    /// resolves for `subject`.
    pub(crate) fn resolve_auto_effort(
        &self,
        crew: &Crew,
        subject: Option<&AutoEffortSubject>,
    ) -> AutoEffortChoice {
        let policy = self.context.settings().auto_effort();
        let Some(subject) = subject else {
            return policy.resolve(crew, &AutoEffortSignal::default(), None);
        };
        if let Some(effort) = subject.forced {
            return policy.escalated(crew, &subject.signal, effort);
        }
        let ticket = subject
            .seed
            .clone()
            .and_then(|seed| seeded_crew_ticket(seed)().ok());
        #[cfg(feature = "test-support")]
        let ticket =
            ticket.filter(|_| !super::auto_effort_exploration::auto_effort_exploration_off());
        policy.resolve(crew, &subject.signal, ticket)
    }

    /// Re-resolve `candidates` whose `@auto` member resolved without a
    /// subject (a pool read with no task in hand) against `subject`.
    pub(crate) fn reresolve_auto_candidates(
        &self,
        candidates: Vec<CrewCandidate>,
        subject: &AutoEffortSubject,
    ) -> Result<Vec<CrewCandidate>, OrbitError> {
        let registry = self.context.settings().crews();
        let mut resolved = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let Some(base) = candidate
                .auto_effort
                .as_ref()
                .and_then(|choice| auto_effort_base(&choice.requested))
            else {
                resolved.push(candidate);
                continue;
            };
            let Some(crew) = registry.get(base) else {
                resolved.push(candidate);
                continue;
            };
            let choice = self.resolve_auto_effort(crew, Some(subject));
            resolved.push(CrewCandidate {
                crew: self.resolve_crew_for_task(Some(&choice.resolved), None)?,
                weight: candidate.weight,
                auto_effort: Some(choice),
            });
        }
        Ok(merge_same_crew(resolved))
    }
}

/// Collapse candidates that resolved to the same crew reference
/// (`opus@auto` and `opus@medium` in one pool) into one, holding both
/// members' tickets. The merged candidate keeps the `@auto` record, so an
/// exploration stays visible in the draw.
pub(crate) fn merge_same_crew(candidates: Vec<CrewCandidate>) -> Vec<CrewCandidate> {
    let mut merged: Vec<CrewCandidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match merged
            .iter_mut()
            .find(|existing| existing.crew.name == candidate.crew.name)
        {
            Some(existing) => {
                existing.weight = existing.weight.saturating_add(candidate.weight);
                if existing.auto_effort.is_none() {
                    existing.auto_effort = candidate.auto_effort;
                }
            }
            None => merged.push(candidate),
        }
    }
    merged
}

/// Whether `candidate` is the member `assigned` (a task's recorded crew)
/// came from: the same reference, or for an `@auto` member any effort of
/// its crew, so a task keeps its member while the resolution moves.
pub(crate) fn candidate_holds_assignment(
    candidate: &CrewCandidate,
    assigned: Option<&str>,
) -> bool {
    let Some(assigned) = assigned else {
        return false;
    };
    candidate.crew.name == assigned
        || (candidate.auto_effort.is_some()
            && orbit_types::identity::same_crew_family(&candidate.crew.name, assigned))
}
