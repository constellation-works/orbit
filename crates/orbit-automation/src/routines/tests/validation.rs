use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::{RoutineFireIntentParams, Store};
use orbit_types::workflow::{JobRunState, RoutineDefinition};
use tempfile::tempdir;

use crate::routines::loader::{LoadedRoutine, RoutineCollection, RoutineOrigin};
use crate::routines::sweep::RunOwnerLiveness;
use crate::routines::sweep::{RoutineDispatch, SweepOptions, run_sweep_core_with_registry};
use crate::routines::validation::{
    RoutineDiagnosticSeverity, RoutineHostIdentity, RoutineRegistryView, validate_routine_pins,
};
use orbit_common::OrbitError;

fn ts(minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 18, 12, minute, second)
        .single()
        .expect("valid timestamp")
}

fn identity(machine_id: &str, host_id: &str) -> RoutineHostIdentity {
    RoutineHostIdentity {
        machine_id: machine_id.to_string(),
        host_id: host_id.to_string(),
    }
}

fn local_view(known_elsewhere: &[&str]) -> RoutineRegistryView {
    RoutineRegistryView {
        owner_host_ids: known_elsewhere
            .iter()
            .map(|name| (*name).to_string())
            .collect::<BTreeSet<_>>(),
    }
}

fn codes(validation: &crate::routines::validation::RoutinePinValidation) -> Vec<&'static str> {
    validation
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn local_validation_reports_own_elsewhere_and_unresolvable_outcomes() {
    let local = identity("hm_local", "local");
    let view = local_view(&["remote"]);

    let own = validate_routine_pins(
        &local,
        RoutineOrigin::Committed,
        &["local".to_string()],
        &view,
    );
    assert!(own.eligible, "own-host pin must remain decidable offline");
    assert!(own.diagnostics.is_empty());

    let elsewhere = validate_routine_pins(
        &local,
        RoutineOrigin::Committed,
        &["remote".to_string()],
        &view,
    );
    assert!(!elsewhere.eligible);
    assert_eq!(codes(&elsewhere), vec!["host_belongs_elsewhere"]);
    assert_eq!(
        elsewhere.diagnostics[0].severity,
        RoutineDiagnosticSeverity::Warning
    );
    assert!(!elsewhere.diagnostics[0].stale);

    let unknown = validate_routine_pins(
        &local,
        RoutineOrigin::Committed,
        &["typo".to_string()],
        &view,
    );
    assert!(!unknown.eligible);
    assert_eq!(codes(&unknown), vec!["host_unresolvable"]);
    assert_eq!(
        unknown.diagnostics[0].severity,
        RoutineDiagnosticSeverity::Error
    );
    assert!(!unknown.diagnostics[0].message.contains("last_seen"));
}

#[test]
fn local_origin_remains_implicitly_local() {
    let validation = validate_routine_pins(
        &identity("hm_local", "local"),
        RoutineOrigin::Local,
        &["somewhere-else".to_string()],
        &local_view(&[]),
    );
    assert!(validation.eligible);
    assert!(validation.diagnostics.is_empty());
}

fn loaded_routine(pin: &str) -> LoadedRoutine {
    let yaml = format!(
        "schemaVersion: 1\nname: move-me\nenabled: true\nhosts: [{pin}]\n\
         trigger:\n  cron: \"* * * * *\"\n  missed_run: catch_up_once\n\
         target: job:noop\n"
    );
    let definition: RoutineDefinition = parse_routine_yaml(&yaml).expect("routine");
    LoadedRoutine {
        definition,
        origin: RoutineOrigin::Committed,
        source_workspace: "orbit".to_string(),
        source_orbit_dir: PathBuf::from("/orbit/.orbit"),
        path: PathBuf::from("/orbit/.orbit/routines/move-me.yaml"),
    }
}

#[derive(Default)]
struct FakeDispatch {
    submissions: Cell<u32>,
}

impl RoutineDispatch for FakeDispatch {
    fn submit(&self, _source: &Path, _job: &str, _actor: &str) -> Result<String, OrbitError> {
        let next = self.submissions.get() + 1;
        self.submissions.set(next);
        Ok(format!("run-{next}"))
    }

    fn run_state(&self, _source: &Path, _run_id: &str) -> Option<JobRunState> {
        None
    }

    fn run_owner_liveness(&self, _source: &Path, _run_id: &str) -> RunOwnerLiveness {
        RunOwnerLiveness::Stopped
    }
}

#[test]
fn reassignment_preserves_old_state_and_baselines_new_owner() {
    let roots = tempdir().expect("roots");
    let root_a = roots.path().join("a");
    let root_b = roots.path().join("b");
    std::fs::create_dir_all(&root_a).expect("root a");
    std::fs::create_dir_all(&root_b).expect("root b");
    let store_a = Store::open(&root_a.join("orbit.db")).expect("store a");
    let store_b = Store::open(&root_b.join("orbit.db")).expect("store b");
    let now = ts(30, 0);

    store_a
        .routine_record_baseline("move-me", &(now - Duration::hours(2)).to_rfc3339())
        .expect("a baseline");
    store_a.routine_pause("move-me", "test").expect("a pause");
    store_a
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "move-me".to_string(),
            slot: (now - Duration::hours(1)).to_rfc3339(),
            attempt: 1,
            source_workspace: "orbit".to_string(),
        })
        .expect("unresolved a fire");
    let before_cursor = store_a.routine_cursor("move-me").expect("cursor");
    let before_fires = store_a.routine_recent_fires("move-me", 10).expect("fires");
    let before_pauses = store_a.routine_pauses().expect("pauses");

    let moved = RoutineCollection {
        routines: vec![loaded_routine("b")],
        errors: Vec::new(),
    };
    let dispatch_a = FakeDispatch::default();
    let a_reports = run_sweep_core_with_registry(
        &store_a,
        &identity("hm_a", "a"),
        &local_view(&["b"]),
        &moved,
        &dispatch_a,
        SweepOptions::default(),
        now,
    )
    .expect("a sweep after reassignment");
    assert_eq!(a_reports[0].reason.as_deref(), Some("host_not_pinned"));
    assert_eq!(store_a.routine_cursor("move-me").unwrap(), before_cursor);
    assert_eq!(
        store_a.routine_recent_fires("move-me", 10).unwrap(),
        before_fires
    );
    assert_eq!(store_a.routine_pauses().unwrap(), before_pauses);
    assert_eq!(dispatch_a.submissions.get(), 0);

    let dispatch_b = FakeDispatch::default();
    let first_b = run_sweep_core_with_registry(
        &store_b,
        &identity("hm_b", "b"),
        &local_view(&["a"]),
        &moved,
        &dispatch_b,
        SweepOptions::default(),
        now,
    )
    .expect("first b sweep");
    assert_eq!(first_b[0].action, "baselined");

    let second_b = run_sweep_core_with_registry(
        &store_b,
        &identity("hm_b", "b"),
        &local_view(&["a"]),
        &moved,
        &dispatch_b,
        SweepOptions::default(),
        now + Duration::minutes(1) + Duration::seconds(1),
    )
    .expect("next natural b slot");
    assert_eq!(second_b[0].action, "fired");
    assert_eq!(dispatch_b.submissions.get(), 1);
}
