//! A routine file on the retired `block_escalated` trigger — the shape the
//! seeded `blocked-task-triage-<workspace>` routine carried — loads inert
//! with a migration message instead of failing the loader, and a pilot-warning
//! routine on the same target job still loads.

use std::fs;

use orbit_automation::routines::loader::{
    ROUTINES_DIR, RoutineCatalogLookup, RoutineSource, collect_routines,
    is_retired_block_trigger_reason,
};
use tempfile::tempdir;

const BLOCK_ROUTINE: &str = "\
schemaVersion: 1
name: blocked-task-triage-polaris
enabled: true
trigger:
  state:
    kind: block_escalated
    owner_machine: hm_test
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
target: job:escalation_triage_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries: {max: 1, backoff_minutes: 5}
";

const PILOT_ROUTINE: &str = "\
schemaVersion: 1
name: pilot-warning-triage-polaris
enabled: true
trigger:
  state:
    kind: pilot_warnings_held
    owner_machine: hm_test
    branch: main
    debounce_minutes: 2
    max_wait_minutes: 10
    max_items: 50
    batch_size: 5
    retries: 1
    deadline_minutes: 90
target: job:escalation_triage_pipeline
policy:
  timeout_minutes: 90
  overlap: forbid
  retries: {max: 1, backoff_minutes: 5}
";

#[test]
fn a_blocked_task_triage_routine_file_loads_inert_with_a_migration_message() {
    let root = tempdir().expect("temporary Orbit root");
    let routines = root.path().join(ROUTINES_DIR);
    fs::create_dir_all(&routines).expect("routines directory");
    fs::write(
        routines.join("blocked-task-triage-polaris.yaml"),
        BLOCK_ROUTINE,
    )
    .expect("retired routine file");
    fs::write(
        routines.join("pilot-warning-triage-polaris.yaml"),
        PILOT_ROUTINE,
    )
    .expect("pilot routine file");

    let collection = collect_routines(
        &[RoutineSource {
            workspace: "polaris".to_string(),
            orbit_dir: root.path().to_path_buf(),
        }],
        &|_, _| RoutineCatalogLookup {
            resolves: true,
            error: None,
        },
    );

    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert_eq!(
        collection
            .routines
            .iter()
            .map(|routine| routine.definition.name.as_str())
            .collect::<Vec<_>>(),
        ["pilot-warning-triage-polaris"],
        "only the pilot-warning routine stays live"
    );
    assert_eq!(collection.retired.len(), 1, "{:?}", collection.retired);
    let retired = &collection.retired[0];
    assert_eq!(retired.name, "blocked-task-triage-polaris");
    assert!(
        is_retired_block_trigger_reason(&retired.reason)
            && retired.reason.contains("final recovery"),
        "the reason names the retirement and where blocked tasks go now: {}",
        retired.reason
    );
}
