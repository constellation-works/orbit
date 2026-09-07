use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::cursor_state_path;
use orbit_types::workflow::AutoTaskDefinition;
use orbit_types::workflow::automation::AutomationDiagnostic;
use tempfile::tempdir;

use crate::auto_tasks::loader::auto_tasks_dir;
use crate::auto_tasks::scheduler::{
    AutoTaskDispatch, SchedulerOptions, run_auto_task_scheduler_at,
};

struct TestDispatch {
    definition_root: PathBuf,
    state_dir: PathBuf,
}

impl AutoTaskDispatch for TestDispatch {
    fn evaluate_delivery(
        &self,
        _definition: &AutoTaskDefinition,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<AutomationDiagnostic, OrbitError> {
        panic!("invalid cron definitions must not be dispatched")
    }

    fn definition_root(&self) -> PathBuf {
        self.definition_root.clone()
    }

    fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    fn has_open_instance(&self, _definition: &AutoTaskDefinition) -> Result<bool, OrbitError> {
        panic!("invalid cron definitions must not be dispatched")
    }

    fn mint_task(&self, _definition: &AutoTaskDefinition) -> Result<String, OrbitError> {
        panic!("invalid cron definitions must not be dispatched")
    }
}

#[test]
fn invalid_cron_definition_does_not_create_a_cursor() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    let definitions = auto_tasks_dir(&definition_root);
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    fs::write(
        definitions.join("invalid-cron.yaml"),
        r#"schemaVersion: 1
name: invalid-cron
schedule:
  cron: "every day"
template:
  title: Invalid cron fixture
"#,
    )
    .expect("definition fixture");

    let dispatch = TestDispatch {
        definition_root,
        state_dir,
    };
    let outcome = run_auto_task_scheduler_at(&dispatch, Utc::now(), SchedulerOptions::default())
        .expect("scheduler pass");

    assert!(outcome.reports.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(
        outcome.errors[0]
            .message
            .contains("invalid cron expression 'every day'")
    );
    assert!(!cursor_state_path(&dispatch.state_dir).exists());
}
