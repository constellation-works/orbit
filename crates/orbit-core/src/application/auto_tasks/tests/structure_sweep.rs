//! Split tasks filed by the structure sweep must arrive with their new
//! module destinations already granted for creation, so task-pilot does not
//! hold them for a missing `context_creation_authorized` grant (the holds on
//! the sweep-filed split tasks that motivated this check).

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::task::{ContextCreationState, TaskStatus};
use orbit_types::workflow::AutoTaskDefinition;

use crate::OrbitRuntime;
use crate::application::auto_tasks::{DEFAULT_AUTO_TASK_FILES, render_default_auto_task};
use crate::application::task::{TaskAddParams, TaskWriteScreen};

const NEW_MODULE: &str = "file:src/execute/mod.rs";

fn shipped_structure_sweep() -> AutoTaskDefinition {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "structure-sweep")
        .expect("structure-sweep default");
    parse_auto_task_yaml(&render_default_auto_task(yaml, "agent-main"))
        .expect("parse structure-sweep")
}

fn split_task_params() -> TaskAddParams {
    TaskAddParams {
        title: "Split execute into its own module".to_string(),
        description: "Move the execute concern into execute/mod.rs.".to_string(),
        acceptance_criteria: vec!["execute/mod.rs exists at its destination.".to_string()],
        status: Some(TaskStatus::Proposed),
        context_files: vec![NEW_MODULE.to_string()],
        ..Default::default()
    }
}

#[test]
fn structure_sweep_filing_records_new_destinations_as_creation_intent() {
    let definition = shipped_structure_sweep();
    let instructions = &definition.template.description;

    assert!(
        instructions.contains("allow_missing_context: true"),
        "structure-sweep filing must record new destinations with \
         allow_missing_context; without it task-pilot holds split tasks for \
         missing context_creation_authorized"
    );
    assert!(
        instructions.contains("durable creation intent"),
        "structure-sweep filing must name the creation-intent grant it records"
    );
    assert!(
        !instructions.contains("Do not list files that do not exist yet."),
        "the old verified-only rule is what left split destinations ungranted"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.contains("creation intent")),
        "the sweep's own criteria must require the creation-intent filing"
    );
}

#[test]
fn split_filing_with_allow_missing_context_grants_the_exact_destination() {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");

    let refused = runtime.add_task_screened(
        split_task_params(),
        &TaskWriteScreen::unbound(false),
        None,
        None,
    );
    let error = refused
        .expect_err("undeclared new destination must be refused")
        .to_string();
    assert!(error.contains("allow_missing_context"), "{error}");

    let filed = runtime
        .add_task_screened(
            split_task_params(),
            &TaskWriteScreen::unbound(true),
            None,
            None,
        )
        .expect("filing with creation intent")
        .task;
    assert_eq!(filed.context_files, vec![NEW_MODULE.to_string()]);

    let state = runtime
        .context_creation_state(&filed)
        .expect("read creation grant");
    assert!(
        matches!(&state, ContextCreationState::Current(grant) if grant.selectors == vec![NEW_MODULE.to_string()]),
        "expected a current grant for {NEW_MODULE}, got {state:?}"
    );
}
