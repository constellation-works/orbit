//! Which job delivers a shipped task, and which jobs hold a task's delivery
//! slot (design `docs/design/plugins/1_scope.md` §4.5).
//!
//! A job opts in with `spec.task_delivery`. A task opts in with one
//! `delivery:<job>` tag. Without the tag a shipped task goes through
//! `task_<mode>_pipeline`, unchanged; with it, the named job must be active and
//! deliver the ship mode, or shipping refuses. Nothing here falls back.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::{DELIVERY_JOB_TAG_PREFIX, Task};
use orbit_types::workflow::{JobKind, JobV2, ShipMode};

use super::catalog::{DEFAULT_JOB_FILES, is_default_job_name, shipped_job_spec};
use crate::OrbitRuntime;

/// The job a gate dispatches for one bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryRoute {
    pub(crate) job_name: String,
    /// A `delivery:<job>` tag chose the job; `false` is the default pipeline.
    pub(crate) selected: bool,
    /// The plugin contributing the selected job, when it is a plugin job.
    pub(crate) plugin: Option<String>,
}

impl OrbitRuntime {
    /// Jobs whose live runs hold the delivery slot of every task in their
    /// `input.task_ids`: those declaring `spec.task_delivery`.
    ///
    /// Shipped names are read from the binary's own assets, which is what
    /// name-based execution runs (L-0060) even where the workspace catalog was
    /// never seeded; every other name from the active catalog, plugin layer
    /// included.
    pub(crate) fn task_delivery_job_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let mut ids = BTreeSet::new();
        for (name, _) in DEFAULT_JOB_FILES {
            if shipped_job_spec(name)?.holds_task_delivery() {
                ids.insert((*name).to_string());
            }
        }
        for (name, _, spec) in self.load_v2_job_assets()?.iter() {
            if !is_default_job_name(name) && spec.holds_task_delivery() {
                ids.insert(name.to_string());
            }
        }
        Ok(ids)
    }

    /// Resolve the job that delivers `tasks` together in `mode`.
    ///
    /// Refused, never defaulted: a malformed or conflicting selection, a job
    /// no active catalog layer provides (naming its plugin when one is
    /// installed but not serving), a subroutine, or a job whose
    /// `task_delivery.modes` does not include `mode`.
    pub(crate) fn resolve_delivery_route(
        &self,
        tasks: &[Task],
        mode: ShipMode,
    ) -> Result<DeliveryRoute, OrbitError> {
        let Some((job, task_id)) = bundle_selection(tasks)? else {
            return Ok(DeliveryRoute {
                job_name: format!("task_{}_pipeline", mode.as_input_value()),
                selected: false,
                plugin: None,
            });
        };
        let refuse = |reason: String| {
            OrbitError::InvalidInput(format!(
                "task '{task_id}' selects delivery job '{job}' (tag \
                 `{DELIVERY_JOB_TAG_PREFIX}{job}`), which {reason}; it will not be shipped \
                 through the default pipeline instead"
            ))
        };
        let (spec, plugin) = match self.selected_delivery_spec(job) {
            Ok(found) => found,
            Err(OrbitError::NotFound { .. }) => {
                return Err(refuse(match self.plugin_load().inactive_job_owner(job) {
                    Some(owner) => format!(
                        "plugin '{}' ships, but that plugin {}; enable it or remove the tag",
                        owner.plugin, owner.state
                    ),
                    None => "no active plugin or workspace job provides and no installed \
                             plugin ships; install and enable its plugin or remove the tag"
                        .to_string(),
                }));
            }
            Err(error) => return Err(error),
        };
        if spec.kind != JobKind::Workflow {
            return Err(refuse(format!(
                "declares `kind: {}` and cannot be dispatched",
                spec.kind
            )));
        }
        if !spec.delivers_mode(mode) {
            return Err(refuse(format!(
                "does not declare `spec.task_delivery.modes` containing '{}'",
                mode.as_input_value()
            )));
        }
        Ok(DeliveryRoute {
            job_name: job.to_string(),
            selected: true,
            plugin,
        })
    }

    /// The spec a selected job name executes under, and the active plugin
    /// contributing it, if any.
    fn selected_delivery_spec(&self, job: &str) -> Result<(JobV2, Option<String>), OrbitError> {
        if is_default_job_name(job) {
            return Ok((self.resolved_job_spec(job)?, None));
        }
        let (path, spec) = self.load_v2_job_asset_by_name(job)?;
        let plugin = self
            .plugin_load()
            .active_job_owner(&path)
            .map(str::to_string);
        Ok((spec, plugin))
    }
}

/// The one job every task of a bundle selects, with a task naming it, or
/// `None` when no task selects one. Tasks that disagree — including a task
/// with no selection bundled beside one with a selection — are refused: the
/// gate dispatches a single job for the whole bundle.
fn bundle_selection(tasks: &[Task]) -> Result<Option<(&str, &str)>, OrbitError> {
    let mut selections = Vec::with_capacity(tasks.len());
    for task in tasks {
        let selection = task
            .delivery_job_selection()
            .map_err(|error| OrbitError::InvalidInput(format!("task '{}': {error}", task.id)))?;
        selections.push((task.id.as_str(), selection));
    }
    let Some(&(first_task, first)) = selections.first() else {
        return Ok(None);
    };
    if let Some(&(other_task, other)) = selections.iter().find(|(_, other)| *other != first) {
        let describe = |selection: Option<&str>| {
            selection.map_or_else(
                || "the default pipeline".to_string(),
                |job| format!("'{job}'"),
            )
        };
        return Err(OrbitError::InvalidInput(format!(
            "bundled tasks select different delivery jobs: task '{first_task}' selects {}, task \
             '{other_task}' selects {}; ship them separately",
            describe(first),
            describe(other)
        )));
    }
    Ok(first.map(|job| (job, first_task)))
}
