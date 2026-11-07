//! Task crew assignment and durable assignment provenance.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComplexity, TaskHistoryEntry, TaskOsRequirement};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::{
    CreationCrewAssignment, CrewCandidate, PURPOSE_SOURCE_PREFIX, parse_type_crew_source,
};
use crate::runtime::run_input::non_empty;

use super::draw::{permitted_candidates, random_crew_ticket, weighted_candidates, weighted_draw};
use super::purpose::task_crew_purpose;

impl OrbitRuntime {
    /// Recover legacy assignment provenance only from the latest assignment
    /// that names the current crew. Missing evidence preserves an explicit pin.
    pub(crate) fn task_crew_source(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        if task.crew_source.is_some() {
            return Ok(task.crew_source.clone());
        }
        let Some(crew) = task.crew.as_deref() else {
            return Ok(None);
        };
        let history = self.get_task_history(&task.id)?;
        let Some(note) = history
            .iter()
            .rev()
            .find(|entry| matches!(entry.event.as_str(), "crew_assigned" | "crew_redrawn"))
            .and_then(|entry| entry.note.as_deref())
        else {
            return Ok(None);
        };
        if let Some(source) = note.strip_prefix(&format!("assigned crew `{crew}` from ")) {
            return Ok(Some(source.to_string()));
        }
        if let Some((_, source)) = note.split_once(&format!(" to `{crew}` via ")) {
            return Ok(if source == "explicit name" {
                Some("explicit".to_string())
            } else {
                source
                    .strip_prefix("pool draw (")
                    .and_then(|source| source.strip_suffix(')'))
                    .map(ToOwned::to_owned)
            });
        }
        Ok(None)
    }

    /// Redraw a pool assignment from another tier or a default fallback before
    /// publishing the new complexity, or a pool or default assignment whose
    /// edited tags or context files route it to another type pool. `task`
    /// already carries the edited tags and context files; `complexity` is
    /// the tier the write leaves it at. The caller commits crew, source and
    /// this history together under the task write lock. Active or reserved
    /// deliveries keep their admitted crew and source; a later fresh admission
    /// evaluates the edited classification independently.
    pub(crate) fn rerate_task_crew(
        &self,
        task: &mut Task,
        complexity: Option<TaskComplexity>,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        // A task assigned to a human crew keeps it: no re-rate draws it onto
        // an agent.
        if self.is_human_crew(task.crew.as_deref()) || self.task_crew_has_admitted_delivery(task)? {
            return Ok(None);
        }
        let Some(source) = self.task_crew_source(task)? else {
            return Ok(None);
        };
        let assigned_tier = source
            .strip_prefix("pool:")
            .map(ToOwned::to_owned)
            .or_else(|| parse_type_crew_source(&source).map(|(_, tier)| tier.to_string()));
        if source != "default" && assigned_tier.is_none() {
            return Ok(None);
        }
        let complexity_changed = complexity != task.complexity;
        let route = self.task_type_pool_route(complexity, &task.tags, &task.context_files);
        let target = route.as_ref().map_or_else(
            || complexity.map(|complexity| format!("pool:{complexity}")),
            |route| Some(route.crew_source()),
        );
        let unchanged = if source == "default" {
            !complexity_changed && route.is_none()
        } else {
            target.as_deref() == Some(source.as_str())
        };
        if unchanged {
            return Ok(None);
        }
        let before = task.crew.clone();
        let assignment = self.creation_crew_assignment(
            complexity,
            None,
            &task.tags,
            &task.requires_labels,
            &task.context_files,
            &mut random_crew_ticket,
        )?;
        // A type pool none of whose members can run here leaves the task on
        // its pool; a tag edit alone does not re-roll that draw.
        if !complexity_changed
            && assignment
                .as_ref()
                .map(|assignment| assignment.source.as_str())
                == Some(source.as_str())
        {
            return Ok(None);
        }
        task.crew = assignment
            .as_ref()
            .map(|assignment| assignment.crew.clone());
        task.crew_source = assignment.map(|assignment| assignment.source);
        let reason = if complexity_changed {
            format!(
                "complexity changed to {}",
                complexity.map_or("unassessed", TaskComplexity::as_str)
            )
        } else {
            match &route {
                Some(route) => format!("the task was reclassified as type {}", route.task_type),
                None => "the task no longer classifies to a type pool".to_string(),
            }
        };
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: "system".to_string(),
            event: "crew_redrawn".to_string(),
            note: Some(format!(
                "crew redrawn from {} (`{}`) to {} (`{}`) after {reason}",
                source,
                before.as_deref().unwrap_or("(none)"),
                task.crew_source.as_deref().unwrap_or("unassigned"),
                task.crew.as_deref().unwrap_or("(none)"),
            )),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        }))
    }

    /// Decide the crew a task is created with [ORB-12717].
    ///
    /// An explicit crew wins; then the pool of the purpose the task's tags
    /// name, when one is configured (source `purpose:<purpose>`); then the
    /// pool `workflow.type_complexity_crews` gives the task's type at its
    /// complexity (source `type:<type>/<tier>`).
    /// Creation and complexity re-rates draw from the complexity pools;
    /// a status transition alone never revisits the choice, and
    /// `task update --crew ""` re-enters here for the task's current
    /// complexity. Complexity pools exclude crews incompatible with the tags
    /// this write leaves the task with, before drawing. An explicit crew is
    /// kept exactly as the caller wrote it.
    /// [ORB-17770] Under usage pressure, both pool draws weigh their members
    /// by crew pressure; this covers task add, `task update --crew ""` and
    /// [`Self::rerate_task_crew`]. The recorded source stays the pool's.
    /// Pool draws skip this host's live provider limits before image filtering
    /// and weighting. A wholly limited pool leaves the crew unset for admission
    /// to retry, without falling through to another pool or the default.
    /// `None` also means this workspace can name no crew at all.
    pub(crate) fn creation_crew_assignment(
        &self,
        complexity: Option<TaskComplexity>,
        explicit: Option<&str>,
        tags: &[String],
        requires_labels: &[String],
        context_files: &[String],
        random: &mut impl FnMut() -> Result<u64, OrbitError>,
    ) -> Result<Option<CreationCrewAssignment>, OrbitError> {
        if let Some(explicit) = explicit.and_then(non_empty) {
            return Ok(Some(CreationCrewAssignment {
                crew: explicit.to_string(),
                source: "explicit".to_string(),
            }));
        }
        // A `<crew>@auto` member resolves from the tier and footprint this
        // write leaves the task with; admission resolves it again.
        let subject = super::auto_effort::creation_auto_effort_subject(complexity, context_files);
        if let Some(purpose) = task_crew_purpose(tags)
            && let Some((candidates, _)) = self.purpose_pool_candidates(purpose, false)?
        {
            let source = format!("{PURPOSE_SOURCE_PREFIX}{purpose}");
            let candidates = self.reresolve_auto_candidates(candidates, &subject)?;
            let Some(candidates) = self.creation_pool_filtered(candidates) else {
                return Ok(None);
            };
            let candidates = self.creation_weighted(candidates);
            let candidates = permitted_candidates(candidates, &source, None)?;
            let selected = weighted_draw(&candidates, &source, random)?;
            return Ok(Some(CreationCrewAssignment {
                crew: selected.crew.name.clone(),
                source,
            }));
        }
        let task_os = self.label_narrowed_os(TaskOsRequirement::from_tags(tags), requires_labels);
        if let Some(route) = self.task_type_pool_route(complexity, tags, context_files)
            && let Some((candidates, _)) = self.type_pool_candidates(&route, Some(&task_os))?
        {
            let source = route.crew_source();
            let candidates = self.reresolve_auto_candidates(candidates, &subject)?;
            let Some(candidates) = self.creation_pool_filtered(candidates) else {
                return Ok(None);
            };
            let candidates = self.creation_weighted(candidates);
            let candidates = permitted_candidates(candidates, &source, None)?;
            let selected = weighted_draw(&candidates, &source, random)?;
            return Ok(Some(CreationCrewAssignment {
                crew: selected.crew.name.clone(),
                source,
            }));
        }
        let pools = self.capture_auto_crew_pools(&Value::Null)?;
        if let Some(complexity) = complexity
            && let Some((candidates, _)) =
                self.creation_complexity_pool_candidates(Some(complexity), &pools, &task_os)?
        {
            let source = format!("pool:{complexity}");
            let candidates = self.reresolve_auto_candidates(candidates, &subject)?;
            let Some(candidates) = self.creation_pool_filtered(candidates) else {
                return Ok(None);
            };
            let candidates = self.creation_weighted(candidates);
            let candidates = permitted_candidates(candidates, &source, None)?;
            let selected = weighted_draw(&candidates, &source, random)?;
            return Ok(Some(CreationCrewAssignment {
                crew: selected.crew.name.clone(),
                source,
            }));
        }
        if self.context.settings().default_crew().is_none() {
            return Ok(None);
        }
        // A disabled default is not pinned onto the task: creation stays
        // possible on a host with no enabled crew, and the unset field lets
        // dispatch refuse against `workflow.default_crew` by name.
        let default = self.lookup_crew_for_task(None, None)?;
        if !default.enabled {
            return Ok(None);
        }
        // A default no serving host can run is not pinned: the field stays
        // unset rather than assigning a crew every host would withhold. A
        // default only a follower can run is pinned, like a creation pool
        // draw, and waits for that follower. An explicit crew is returned
        // above and is never replaced.
        let declared = self.declared_executor_host_os()?;
        if !self.crew_runs_on_serving_host(&default, &declared) {
            return Ok(None);
        }
        Ok(Some(CreationCrewAssignment {
            crew: default.name,
            source: "default".to_string(),
        }))
    }

    /// `task_os` narrowed to the OSes of the known hosts that carry
    /// `requires_labels`, so a creation draw excludes a crew whose executor
    /// `host_os` no such host runs, exactly as it does for `os:` tags. A task
    /// with no label, invalid tags, or no carrier of a known OS keeps its
    /// tag requirement.
    fn label_narrowed_os(
        &self,
        task_os: TaskOsRequirement,
        requires_labels: &[String],
    ) -> TaskOsRequirement {
        let Some(carriers) = self.label_carrier_oses(requires_labels) else {
            return task_os;
        };
        if !task_os.invalid.is_empty() {
            return task_os;
        }
        let narrowed = if task_os.any_of.is_empty() {
            carriers
        } else {
            task_os.any_of.intersection(&carriers).copied().collect()
        };
        if narrowed.is_empty() {
            return task_os;
        }
        TaskOsRequirement {
            any_of: narrowed,
            ..task_os
        }
    }

    /// A creation draw's positive-weight members without this host's limited
    /// providers, or `None` when none remain. Admission re-applies the
    /// filter; the task keeps its pool source.
    fn creation_pool_filtered(
        &self,
        mut candidates: Vec<CrewCandidate>,
    ) -> Option<Vec<CrewCandidate>> {
        let gate = self.provider_limit_gate(chrono::Utc::now());
        candidates
            .retain(|candidate| candidate.weight > 0 && gate.limit_for(&candidate.crew).is_none());
        (!candidates.is_empty()).then_some(candidates)
    }

    /// A creation-seam pool draw's members, weighed by usage pressure
    /// [ORB-17770] when it applies.
    fn creation_weighted(&self, mut candidates: Vec<CrewCandidate>) -> Vec<CrewCandidate> {
        weighted_candidates(
            &mut candidates,
            &self.provider_limit_policy().usage_pressure,
            || self.usage_pressure_read(chrono::Utc::now()),
        );
        candidates
    }
}
