use std::fs;
use std::path::PathBuf;

use chrono::{Duration, Local, TimeZone, Timelike, Utc};
use orbit_common::protocol::yaml::parse_routine_yaml;
use tempfile::tempdir;

use super::super::loader::{LoadedRoutine, RoutineOrigin};
use super::super::status::{
    RoutineStatus, RoutineToggleOutcome, ScheduleDisplayState, next_scheduled_occurrence,
    set_routine_enabled,
};

fn loaded(path: std::path::PathBuf) -> LoadedRoutine {
    let raw = fs::read_to_string(&path).expect("read fixture");
    LoadedRoutine {
        definition: parse_routine_yaml(&raw).expect("parse fixture"),
        origin: RoutineOrigin::Committed,
        source_workspace: "polaris".to_string(),
        source_orbit_dir: path.parent().expect("fixture parent").to_path_buf(),
        path,
    }
}

#[test]
fn routine_toggle_preserves_comments_and_every_field_but_enabled() {
    let root = tempdir().expect("temp root");
    let path = root.path().join("nightly.yaml");
    fs::write(
        &path,
        "schemaVersion: 1\nname: nightly\ndescription: keep this text\nenabled: true # reviewed\ntrigger:\n  cron: '0 2 * * *'\ntarget: job:nightly\n",
    )
    .expect("write fixture");
    let routine = loaded(path.clone());

    assert_eq!(
        set_routine_enabled(&routine, true, false).expect("disable"),
        RoutineToggleOutcome::Changed
    );
    let changed = fs::read_to_string(&path).expect("read changed fixture");
    assert!(changed.contains("enabled: false # reviewed"));
    assert!(changed.contains("description: keep this text"));
    assert!(!parse_routine_yaml(&changed).expect("parse changed").enabled);
}

#[test]
fn routine_toggle_inserts_missing_default_and_rejects_stale_duplicate() {
    let root = tempdir().expect("temp root");
    let path = root.path().join("nightly.yaml");
    fs::write(
        &path,
        "schemaVersion: 1\nname: nightly\ntrigger:\n  cron: '0 2 * * *'\ntarget: job:nightly\n",
    )
    .expect("write fixture");
    let routine = loaded(path.clone());

    assert_eq!(
        set_routine_enabled(&routine, true, false).expect("first disable"),
        RoutineToggleOutcome::Changed
    );
    let after_first = fs::read_to_string(&path).expect("read first result");
    assert_eq!(
        set_routine_enabled(&routine, true, false).expect("stale duplicate"),
        RoutineToggleOutcome::Conflict {
            actual_enabled: false
        }
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read duplicate result"),
        after_first,
        "stale duplicate must not rewrite the definition"
    );
}

#[test]
fn routine_toggle_reports_an_exact_noop_without_rewriting() {
    let root = tempdir().expect("temp root");
    let path = root.path().join("nightly.yaml");
    fs::write(
        &path,
        "schemaVersion: 1\nname: nightly\nenabled: false\ntrigger:\n  cron: '0 2 * * *'\ntarget: job:nightly\n",
    )
    .expect("write fixture");
    let routine = loaded(path.clone());
    let before = fs::metadata(&path)
        .expect("fixture metadata")
        .modified()
        .expect("fixture mtime");

    assert_eq!(
        set_routine_enabled(&routine, false, false).expect("noop"),
        RoutineToggleOutcome::Unchanged
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("fixture metadata after noop")
            .modified()
            .expect("fixture mtime after noop"),
        before
    );
}

fn display_status(
    yaml: &str,
    paused: bool,
    observed: bool,
    next_due: Option<&str>,
    automation: Option<serde_json::Value>,
) -> RoutineStatus {
    RoutineStatus {
        routine: LoadedRoutine {
            definition: parse_routine_yaml(yaml).expect("parse fixture"),
            origin: RoutineOrigin::Committed,
            source_workspace: "orbit".to_string(),
            source_orbit_dir: PathBuf::from("/tmp"),
            path: PathBuf::from("/tmp/nightly.yaml"),
        },
        paused_at: paused.then(|| "2026-09-07T21:00:00+00:00".to_string()),
        first_observed_at: observed.then(|| "2026-09-07T20:00:00+00:00".to_string()),
        last_evaluated_slot: None,
        next_due: next_due.map(str::to_string),
        last_fire: None,
        automation,
    }
}

const CRON_YAML: &str = "schemaVersion: 1\nname: nightly\nenabled: true\ntrigger:\n  cron: '0 2 * * *'\ntarget: job:nightly\n";
const DISABLED_YAML: &str = "schemaVersion: 1\nname: nightly\nenabled: false\ntrigger:\n  cron: '0 2 * * *'\ntarget: job:nightly\n";
const DELIVERY_YAML: &str = "schemaVersion: 1\nname: ship\nenabled: true\npolicy:\n  overlap: forbid\ntrigger:\n  deliveries_landed:\n    branch: agent-main\n    threshold: 1\n    max_wait_minutes: 60\n    coverage: integrated_qa_v1\ntarget: job:ship\n";

#[test]
fn schedule_display_state_distinguishes_paused_disabled_waiting_and_unobserved() {
    let next = Some("2026-09-07T21:30:00-07:00");
    assert_eq!(
        display_status(DISABLED_YAML, false, true, next, None).schedule_display_state(),
        ScheduleDisplayState::Disabled
    );
    assert_eq!(
        display_status(CRON_YAML, true, true, next, None).schedule_display_state(),
        ScheduleDisplayState::Paused
    );
    assert_eq!(
        display_status(DELIVERY_YAML, false, true, None, None).schedule_display_state(),
        ScheduleDisplayState::Waiting
    );
    assert_eq!(
        display_status(CRON_YAML, false, false, None, None).schedule_display_state(),
        ScheduleDisplayState::NeverObserved
    );
    assert_eq!(
        display_status(CRON_YAML, false, true, next, None).schedule_display_state(),
        ScheduleDisplayState::Scheduled
    );
    assert!(
        display_status(DISABLED_YAML, false, true, next, None)
            .schedule_display_state()
            .is_hypothetical()
    );
    assert_eq!(
        display_status(
            DELIVERY_YAML,
            false,
            true,
            None,
            Some(serde_json::json!({"reason": "state_unavailable"})),
        )
        .schedule_display_state(),
        ScheduleDisplayState::Unavailable
    );
}

#[test]
fn next_scheduled_occurrence_is_minute_pinned_and_ahead_of_a_sub_minute_now() {
    // The routine projection uses the shared cron owner, so a poll carrying
    // seconds and nanos still yields a stable minute-pinned slot.
    let now = Utc
        .with_ymd_and_hms(2026, 7, 2, 22, 0, 42)
        .single()
        .expect("valid ts")
        + Duration::nanoseconds(785_766_897);
    let now = now.with_timezone(&Local);

    let projected = next_scheduled_occurrence("* * * * *", &now).expect("every-minute cron");
    let parsed = chrono::DateTime::parse_from_rfc3339(&projected).expect("rfc3339 slot");

    assert_eq!(parsed.second(), 0, "slot must be pinned to its minute");
    assert_eq!(parsed.nanosecond(), 0, "slot must be pinned to its minute");
    assert!(parsed.with_timezone(&Utc) > now.with_timezone(&Utc));
}

#[test]
fn next_scheduled_occurrence_is_absent_for_an_unparseable_cron() {
    let now = Utc
        .with_ymd_and_hms(2026, 7, 2, 22, 0, 0)
        .single()
        .expect("valid ts")
        .with_timezone(&Local);

    // A malformed trigger projects nothing; the display state, not a fabricated
    // timestamp, tells the operator why.
    assert_eq!(next_scheduled_occurrence("not a cron", &now), None);
}
