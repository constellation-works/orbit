//! Core task lifecycle adapter and scheduler JSON projection.

use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;
use chrono::{DateTime, Utc};
use orbit_automation::auto_tasks::scheduler::{AutoTaskDispatch, ChangeProbe};
pub use orbit_automation::auto_tasks::scheduler::{
    AutoTaskFireReport, AutoTaskSchedulerOutcome, SchedulerOptions,
};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{AutoTaskDefinition, SkipIfUnchanged, auto_task_tag};
use std::path::PathBuf;

impl AutoTaskDispatch for OrbitRuntime {
    fn evaluate_delivery(
        &self,
        definition: &AutoTaskDefinition,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        crate::application::automation::evaluate_auto_task(self, definition, dry_run, now)
    }

    fn definition_root(&self) -> PathBuf {
        self.paths().local_dir.clone()
    }

    fn state_dir(&self) -> PathBuf {
        self.paths().state_dir.clone()
    }

    fn has_open_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        open_auto_task_instance(self, definition)
    }

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError> {
        mint_task(self, definition).map(|task| task.id)
    }

    fn skip_reason(&self, definition: &AutoTaskDefinition) -> Option<String> {
        OrbitRuntime::auto_task_skip_reason(self, definition)
    }

    fn probe_change_since_last_sweep(
        &self,
        _definition: &AutoTaskDefinition,
        precondition: &SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError> {
        super::change_probe::probe_change_since_last_sweep(self, precondition)
    }
}

impl OrbitRuntime {
    /// Why an auto-task definition is skipped this pass, when it is.
    ///
    /// A definition a plugin seeded fires only while that plugin is enabled:
    /// its template belongs to the plugin, and firing it after a disable would
    /// mint chores nothing on this host can carry out (design §4.5). The file
    /// is left exactly where it is, edits and all.
    pub fn auto_task_skip_reason(&self, definition: &AutoTaskDefinition) -> Option<String> {
        let path = crate::application::auto_tasks::definition_path(
            &self.paths().local_dir,
            &definition.name,
        );
        let (namespace, version) = crate::application::plugin::read_definition_provenance(&path)?;
        if self.plugin_load().is_active(&namespace) {
            return None;
        }
        Some(format!(
            "seeded by plugin:{namespace}@{version}, which is not enabled on this host; run \
             `orbit plugin enable {namespace}` to fire it again, or delete '{}'",
            path.display()
        ))
    }

    /// The id of a still-open instance of `definition`'s prior mints, if any.
    /// Returns `None` if no prior mint is open, meaning `skip_if_open` dedupe
    /// will permit minting and the dashboard reports no open duplicate [ORB-12158].
    pub fn open_auto_task_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        open_auto_task_instance(self, definition)
    }
}

/// The id of a still-open instance of `definition`'s prior mints, if any.
///
/// Exactly one definition of "still-open auto-task instance" exists in the
/// system [ORB-12158]. Both scheduler dedupe (`skip_if_open`) and the dashboard
/// (`open_duplicate`, `may_create_open_duplicate`) consume this query.
pub fn open_auto_task_instance(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Result<Option<String>, OrbitError> {
    let tasks = runtime.list_tasks_by_tags(&[auto_task_tag(&definition.name)])?;
    // `someday` is an explicit "not now" park, not an active instance
    // [ORB-12148]: it must not block every later mint of this auto-task.
    Ok(tasks
        .into_iter()
        .find(|task| {
            !matches!(
                task.status,
                TaskStatus::Done
                    | TaskStatus::Archived
                    | TaskStatus::Rejected
                    | TaskStatus::Someday
            )
        })
        .map(|task| task.id))
}

pub fn run_auto_task_scheduler_at(
    runtime: &OrbitRuntime,
    now: DateTime<Utc>,
    options: SchedulerOptions,
) -> Result<AutoTaskSchedulerOutcome, OrbitError> {
    orbit_automation::auto_tasks::scheduler::run_auto_task_scheduler_at(runtime, now, options)
}

/// Mint one task from a definition's template — the single template→task
/// mapping in the system. Deliberately independent of due-math, cursors, and
/// dedupe: it needs only the definition, so the manual
/// [`OrbitRuntime::auto_task_mint`](crate::OrbitRuntime::auto_task_mint)
/// path reuses it verbatim and a manually minted task is field-for-field identical to
/// a fired one, provenance tag and `system_created` marker included.
pub(super) fn mint_task(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
) -> Result<Task, OrbitError> {
    let mut params = template_params(definition);
    // A task minted from a plugin's seeded definition carries `plugin:<ns>`
    // beside `auto-task:<name>`, so its provenance survives in task history
    // even after the plugin is removed (design §4.4).
    let path = crate::application::auto_tasks::definition_path(
        &runtime.paths().local_dir,
        &definition.name,
    );
    if let Some((namespace, _)) = crate::application::plugin::read_definition_provenance(&path) {
        let tag = format!("plugin:{namespace}");
        if !params.tags.contains(&tag) {
            params.tags.push(tag);
        }
    }
    runtime.add_task(params)
}

pub(crate) fn template_params(definition: &AutoTaskDefinition) -> TaskAddParams {
    let template = &definition.template;
    let mut tags = template.tags.clone();
    tags.push(auto_task_tag(&definition.name));

    TaskAddParams {
        title: template.title.clone(),
        description: template.description.clone(),
        acceptance_criteria: template.acceptance_criteria.clone(),
        tags,
        required_tools: template.required_tools.clone(),
        priority: template.priority,
        // Legacy/custom definitions may omit an assessment. Preserve their
        // historical automated-creation behavior while allowing assessed
        // templates to route directly through complexity-aware workflows.
        complexity: template
            .complexity
            .unwrap_or(orbit_types::task::TaskComplexity::Unassessed),
        task_type: Some(template.task_type),
        status: Some(template.status),
        crew: template.crew.clone(),
        system_created: true,
        ..TaskAddParams::default()
    }
}
