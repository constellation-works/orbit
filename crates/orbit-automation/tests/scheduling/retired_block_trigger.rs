//! Routine files on the retired `block_escalated` and `pilot_warnings_held`
//! triggers — the shapes the seeded `blocked-task-triage-<workspace>` and
//! `pilot-warning-triage-<workspace>` routines carried — load inert with a
//! migration message instead of failing the loader, although their target
//! job still ships for manual triage [ORB-22844].

use std::fs;

use orbit_automation::routines::loader::{
    ROUTINES_DIR, RoutineCatalogLookup, RoutineSource, collect_routines, retired_trigger_of_reason,
};
use orbit_types::workflow::automation::members::StateTriggerKind;
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
    assert!(
        collection.routines.is_empty(),
        "neither routine stays live: {:?}",
        collection
            .routines
            .iter()
            .map(|routine| routine.definition.name.as_str())
            .collect::<Vec<_>>()
    );
    let mut retired = collection
        .retired
        .iter()
        .map(|retired| (retired.name.as_str(), retired))
        .collect::<Vec<_>>();
    retired.sort_by_key(|(name, _)| *name);
    let [(block_name, block), (pilot_name, pilot)] = retired[..] else {
        panic!("both routines retire: {:?}", collection.retired);
    };
    assert_eq!(block_name, "blocked-task-triage-polaris");
    assert!(
        retired_trigger_of_reason(&block.reason) == Some(StateTriggerKind::BlockEscalated)
            && block.reason.contains("final recovery"),
        "the reason names the retirement and where blocked tasks go now: {}",
        block.reason
    );
    assert_eq!(pilot_name, "pilot-warning-triage-polaris");
    assert!(
        retired_trigger_of_reason(&pilot.reason) == Some(StateTriggerKind::PilotWarningsHeld)
            && pilot.reason.contains("Triage action"),
        "the reason names the retirement and the manual triage that remains: {}",
        pilot.reason
    );
}
