//! A local drain warns while no task pilot prepares its workspace's tasks on
//! this machine. Tasks that need a pilot assessment otherwise wait as "needs
//! pilot" with no sign that the pilot will never come. Drain start, each
//! admission pass, run show and readiness carry the same sentence.

use std::path::Path;

use orbit_core::OrbitRuntime;
use orbit_core::application::job::SubmissionKind;
use orbit_core::application::routines::{pause_routine, resume_routine};
use orbit_types::task::ExecutionLocation;
use serde_json::{Value, json};

use super::{as_operator, classify, running_run, runtime};

const MACHINE: &str = "fixture-machine";
const ROUTINE: &str = "task-pilot-repo";

/// The workspace's state-triggered task pilot, as `orbit init` seeds it.
fn write_pilot(repo: &Path, owner: &str, enabled: bool) {
    let routines = repo.join(".orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    let routine = json!({
        "schemaVersion": 1, "name": ROUTINE, "enabled": enabled,
        "target": "job:task_pilot_pipeline",
        "trigger": {"state": {
            "kind": "preparation_eligible", "owner_machine": owner, "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
        }},
    });
    std::fs::write(routines.join("task_pilot.yaml"), routine.to_string()).unwrap();
}

/// The drain-start warning, after asserting that the pass, run show and
/// readiness of the live drain all carry the same one, or none.
fn observed(runtime: &OrbitRuntime, drain: &str) -> Option<String> {
    let start = runtime
        .submission_notices(SubmissionKind::Drain)
        .task_pilot_warning;
    if let Some(warning) = &start {
        let lines = runtime.submission_notices(SubmissionKind::Drain).warnings();
        assert_eq!(
            lines.iter().filter(|line| *line == warning).count(),
            1,
            "drain start prints the warning once: {lines:#?}"
        );
    }
    classify(runtime, drain);
    let shown = as_operator(runtime, "orbit.workflow.run.show", json!({"id": drain}));
    let recorded = &shown["drain_last_pass"]["task_pilot_warning"];
    let readiness = runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let expected = start.as_ref().map_or(Value::Null, |warning| json!(warning));
    assert_eq!(*recorded, expected, "the pass records it: {shown:#}");
    assert_eq!(
        readiness["capacity"]["task_pilot_warning"], expected,
        "readiness reports the live drain's record: {:#}",
        readiness["capacity"]
    );
    start
}

/// Absent, owned by another machine, disabled and paused each warn, naming
/// the workspace, the machine and the fix; an enabled pilot here does not. A
/// pull drain, whose owner prepares its work, never warns.
fn a_drain_warns_until_a_task_pilot_is_enabled_on_this_machine() {
    let (_root, runtime, repo) = runtime();
    let runtime = runtime.with_automation_execution_location(Some(ExecutionLocation {
        machine_id: MACHINE.into(),
        machine_name: Some("fixture-host".into()),
    }));
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let names_workspace_and_machine = |warning: &str| {
        assert!(
            warning.contains("workspace 'repo'") && warning.contains("machine 'fixture-host'"),
            "{warning}"
        );
    };

    let missing = observed(&runtime, &drain).expect("no pilot routine warns");
    names_workspace_and_machine(&missing);
    assert!(missing.contains("orbit workspace sync"), "{missing}");
    assert_eq!(
        runtime
            .submission_notices(SubmissionKind::PullDrain)
            .task_pilot_warning,
        None
    );

    write_pilot(&repo, "another-machine", true);
    let elsewhere = observed(&runtime, &drain).expect("another owner warns");
    names_workspace_and_machine(&elsewhere);
    assert!(
        elsewhere.contains("another owner machine") && elsewhere.contains(MACHINE),
        "{elsewhere}"
    );

    write_pilot(&repo, MACHINE, false);
    let disabled = observed(&runtime, &drain).expect("a disabled pilot warns");
    names_workspace_and_machine(&disabled);
    assert!(
        disabled.contains("is disabled")
            && disabled.contains("enabled: true")
            && disabled.contains(".orbit/routines/task_pilot.yaml"),
        "{disabled}"
    );

    write_pilot(&repo, MACHINE, true);
    assert!(pause_routine(&runtime.global_root(), ROUTINE, "fixture").unwrap());
    let paused = observed(&runtime, &drain).expect("a paused pilot warns");
    names_workspace_and_machine(&paused);
    assert!(
        paused.contains(&format!("orbit routine resume {ROUTINE}")),
        "{paused}"
    );

    assert!(resume_routine(&runtime.global_root(), ROUTINE).unwrap());
    assert_eq!(
        observed(&runtime, &drain),
        None,
        "an enabled pilot is quiet"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_drain_warns_until_a_task_pilot_is_enabled_on_this_machine_scenarios,
    a_drain_warns_until_a_task_pilot_is_enabled_on_this_machine
);
