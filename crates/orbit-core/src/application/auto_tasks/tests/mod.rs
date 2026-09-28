use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use chrono::{DateTime, Utc};
use orbit_automation::auto_tasks::scheduler::{AutoTaskDispatch, ChangeProbe};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::upsert_cursor;
use orbit_types::task::{TaskPriority, TaskType};
use orbit_types::workflow::automation::AutomationDiagnostic;
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy,
    SkipIfUnchanged,
};

use crate::OrbitRuntime;
use crate::application::auto_tasks::crud::AutoTaskAddParams;
use crate::application::auto_tasks::cursor_state_path;

mod change_probe;
mod crud;
mod delete;
mod mint;
mod scheduler;
mod shipped;

/// A minimal, valid template for tests.
pub(super) fn template(title: &str) -> AutoTaskTemplate {
    AutoTaskTemplate {
        title: title.to_string(),
        description: "Recurring chore body.".to_string(),
        acceptance_criteria: vec!["Chore is observable.".to_string()],
        task_type: TaskType::Chore,
        tags: vec![],
        required_tools: Vec::new(),
        priority: TaskPriority::Medium,
        complexity: None,
        crew: None,
        status: orbit_types::task::TaskStatus::Backlog,
    }
}

/// Add-params for a definition on an N-minute interval schedule.
pub(super) fn interval_params(name: &str, every_minutes: u64) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Auto-task {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes },
        template: template(&format!("Chore for {name}")),
        dedupe: DedupePolicy::SkipIfOpen,
    }
}

/// Seed a scheduler cursor baselined now, so a pass an hour later is due.
pub(super) fn seed_cursor(runtime: &OrbitRuntime, name: &str) {
    upsert_cursor(
        &cursor_state_path(&runtime.paths().state_dir),
        name,
        serde_json::from_value::<AutoTaskCursor>(serde_json::json!({
            "baseline_at": chrono::Utc::now().to_rfc3339(),
        }))
        .expect("cursor fixture"),
    )
    .expect("seed scheduler cursor");
}

/// A runtime-backed scheduler host that can pause a pass at two points.
///
/// `admission` pauses after discovery loaded the definitions and before the
/// pass takes the cursor lock; `mint` pauses at task creation while the lock
/// is held. Each pair is `(reached, resume)`.
pub(super) struct PausedDispatch<'a> {
    pub(super) runtime: &'a OrbitRuntime,
    pub(super) mint: Option<(Arc<Barrier>, Arc<Barrier>)>,
    pub(super) admission: Option<(Arc<Barrier>, Arc<Barrier>)>,
}

impl AutoTaskDispatch for PausedDispatch<'_> {
    fn evaluate_delivery(
        &self,
        definition: &AutoTaskDefinition,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<AutomationDiagnostic, OrbitError> {
        self.runtime.evaluate_delivery(definition, dry_run, now)
    }

    fn definition_root(&self) -> PathBuf {
        self.runtime.definition_root()
    }

    fn state_dir(&self) -> PathBuf {
        self.runtime.state_dir()
    }

    fn has_open_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        self.runtime.has_open_instance(definition)
    }

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError> {
        if let Some((reached, resume)) = &self.mint {
            reached.wait();
            resume.wait();
        }
        self.runtime.mint_task(definition)
    }

    fn skip_reason(&self, definition: &AutoTaskDefinition) -> Option<String> {
        if let Some((reached, resume)) = &self.admission {
            reached.wait();
            resume.wait();
        }
        self.runtime.skip_reason(definition)
    }

    fn probe_change_since_last_sweep(
        &self,
        definition: &AutoTaskDefinition,
        precondition: &SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError> {
        self.runtime
            .probe_change_since_last_sweep(definition, precondition)
    }
}
