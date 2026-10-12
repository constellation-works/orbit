//! Routine sweep dispatch through `run_sweep_core` against an in-memory store,
//! a hand-built collection, a scriptable dispatch and an explicit `now`: the
//! tick deadline and `overlap: forbid` with an interrupted source run
//! [ORB-10597].

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_automation::routines::loader::{LoadedRoutine, RoutineCollection, RoutineOrigin};
use orbit_automation::routines::sweep::{
    RoutineDispatch, RunOwnerLiveness, SweepOptions, run_sweep_core,
};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::{RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, Store};
use orbit_types::workflow::JobRunState;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SOURCE_DIR: &str = "/ws/.orbit";

/// Build a validated routine with the common knobs the tests vary.
fn routine(name: &str, overlap: &str) -> LoadedRoutine {
    let yaml = format!(
        "schemaVersion: 1\n\
         name: {name}\n\
         enabled: true\n\
         trigger:\n  cron: \"* * * * *\"\n\
         target: job:noop\n\
         policy:\n  timeout_minutes: 10\n  overlap: {overlap}\n  \
         retries: {{ max: 0, backoff_minutes: 1 }}\n"
    );
    LoadedRoutine {
        definition: parse_routine_yaml(&yaml).expect("valid routine yaml"),
        origin: RoutineOrigin::Workspace,
        source_workspace: "polaris".to_string(),
        source_orbit_dir: PathBuf::from(SOURCE_DIR),
        path: PathBuf::from(format!("{SOURCE_DIR}/routines/{name}.yaml")),
    }
}

fn collection(routines: Vec<LoadedRoutine>) -> RoutineCollection {
    RoutineCollection {
        routines,
        ..RoutineCollection::default()
    }
}

fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .expect("valid ts")
}

/// A scriptable dispatch double: records submissions, hands back deterministic
/// run ids, and answers `run_state` from a table the test primes.
#[derive(Default)]
struct FakeDispatch {
    counter: Cell<u32>,
    submits: RefCell<Vec<(PathBuf, String)>>,
    states: RefCell<HashMap<String, JobRunState>>,
    /// Owner liveness per run id. Absent means `Stopped`, the historical
    /// assumption that a terminal run has finished working.
    liveness: RefCell<HashMap<String, RunOwnerLiveness>>,
}

impl FakeDispatch {
    fn submit_count(&self) -> usize {
        self.submits.borrow().len()
    }
}

impl RoutineDispatch for FakeDispatch {
    fn live_workspace_drain(&self, _dir: &Path) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn submit(
        &self,
        dir: &Path,
        job: &str,
        _actor: &str,
        _slot: &str,
    ) -> Result<String, OrbitError> {
        let n = self.counter.get() + 1;
        self.counter.set(n);
        self.submits
            .borrow_mut()
            .push((dir.to_path_buf(), job.to_string()));
        Ok(format!("run-{n}"))
    }

    fn run_state(&self, _dir: &Path, run_id: &str) -> Option<JobRunState> {
        self.states.borrow().get(run_id).cloned()
    }

    fn run_owner_liveness(&self, _dir: &Path, run_id: &str) -> RunOwnerLiveness {
        self.liveness
            .borrow()
            .get(run_id)
            .copied()
            .unwrap_or(RunOwnerLiveness::Stopped)
    }
}

fn fires(store: &Store, name: &str) -> Vec<RoutineFireRecord> {
    store.routine_recent_fires(name, 32).expect("recent fires")
}

/// A dispatch that overruns the tick deadline: finish the in-flight fire, then
/// leave the next workspace's cursor and history untouched for the next tick.
#[test]
fn a_dispatch_that_exhausts_the_deadline_defers_the_following_workspace() {
    struct BlockingDispatch {
        delegate: FakeDispatch,
        deadline: std::time::Instant,
    }
    impl RoutineDispatch for BlockingDispatch {
        fn live_workspace_drain(&self, _: &Path) -> Result<Option<String>, OrbitError> {
            Ok(None)
        }
        fn submit(
            &self,
            dir: &Path,
            job: &str,
            actor: &str,
            slot: &str,
        ) -> Result<String, OrbitError> {
            std::thread::sleep(
                self.deadline
                    .saturating_duration_since(std::time::Instant::now())
                    + std::time::Duration::from_millis(20),
            );
            self.delegate.submit(dir, job, actor, slot)
        }
        fn run_state(&self, _: &Path, _: &str) -> Option<JobRunState> {
            None
        }
        fn run_owner_liveness(&self, _: &Path, _: &str) -> RunOwnerLiveness {
            RunOwnerLiveness::Unknown
        }
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let first = routine("first", "forbid");
    let mut next = routine("next", "forbid");
    next.source_workspace = "following".into();
    next.source_orbit_dir = "/following/.orbit".into();
    let baseline = ts(2026, 1, 1, 0, 0, 0).to_rfc3339();
    for name in ["first", "next"] {
        store.routine_record_baseline(name, &baseline).unwrap();
    }
    let next_cursor = store.routine_cursor("next").unwrap();
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_millis(100);
    let dispatch = BlockingDispatch {
        delegate: FakeDispatch::default(),
        deadline,
    };
    let reports = run_sweep_core(
        &store,
        &collection(vec![first, next]),
        &dispatch,
        SweepOptions {
            deadline: Some(deadline),
            ..SweepOptions::default()
        },
        ts(2026, 1, 1, 0, 1, 0),
    )
    .unwrap();
    assert_eq!(
        reports[0].action, "fired",
        "in-flight dispatch finishes and is recorded"
    );
    assert_eq!(reports[1].source, "following");
    assert_eq!(reports[1].reason.as_deref(), Some("tick_deadline"));
    assert_eq!(dispatch.delegate.submit_count(), 1);
    assert_eq!(store.routine_cursor("next").unwrap(), next_cursor);
    assert!(fires(&store, "next").is_empty());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

/// Seed an `overlap: forbid` routine with one dispatched fire whose run is
/// marked `interrupted`, and return the store, dispatch, collection, and the
/// seeded slot. `created_at` is real-clock now, so a `now_utc` in the fixture's
/// past keeps the fire inside its policy timeout.
fn interrupted_forbid_fixture(
    liveness: RunOwnerLiveness,
) -> (Store, FakeDispatch, RoutineCollection, String) {
    let store = Store::open_in_memory().expect("in-memory store");
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("job", "forbid")]);

    store
        .routine_record_baseline("job", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    let slot = ts(2026, 1, 1, 0, 5, 0).to_rfc3339();
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "job".to_string(),
            slot: slot.clone(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .unwrap();
    store
        .routine_mark_fire_dispatched("job", &slot, 1, "condemned")
        .unwrap();

    // Condemned to `interrupted`. Marking a run interrupted attaches no
    // teardown, so this says nothing about whether the worker stopped.
    dispatch
        .states
        .borrow_mut()
        .insert("condemned".into(), JobRunState::Interrupted);
    dispatch
        .liveness
        .borrow_mut()
        .insert("condemned".into(), liveness);
    (store, dispatch, coll, slot)
}

/// The defect: a false interrupt used to resolve the fire, which released the
/// `overlap: forbid` slot and admitted a second instance against the same
/// surface while the first was still executing.
#[test]
fn interrupted_run_still_executing_keeps_the_forbid_slot_held() {
    let (store, dispatch, coll, slot) = interrupted_forbid_fixture(RunOwnerLiveness::Alive);

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 6, 20),
    )
    .unwrap();

    assert_eq!(reports[0].action, "skipped");
    assert_eq!(reports[0].reason.as_deref(), Some("overlap_in_flight"));
    assert_eq!(
        dispatch.submit_count(),
        0,
        "no second instance while the condemned run's worker is still alive"
    );
    let seeded = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(
        seeded.state,
        RoutineFireState::Dispatched,
        "the fire holding the slot must stay unresolved"
    );
}

/// The counterpart the fix must not break: a genuinely stopped run still
/// releases its slot, exactly as before.
#[test]
fn interrupted_run_that_genuinely_stopped_releases_the_forbid_slot() {
    let (store, dispatch, coll, slot) = interrupted_forbid_fixture(RunOwnerLiveness::Stopped);

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 6, 20),
    )
    .unwrap();

    assert_eq!(reports[0].action, "fired");
    assert_eq!(dispatch.submit_count(), 1);
    let seeded = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(seeded.state, RoutineFireState::Failed);
    assert_eq!(seeded.detail.as_deref(), Some("run interrupted"));
}

/// Holding the slot is bounded, not permanent: an owner that never becomes
/// conclusively stopped is still reclaimed by the policy timeout, the same
/// bound every genuinely in-flight run already lives under.
#[test]
fn interrupted_run_with_unprobeable_owner_is_reclaimed_at_the_policy_timeout() {
    let (store, dispatch, coll, slot) = interrupted_forbid_fixture(RunOwnerLiveness::Unknown);

    // Past the routine's 10-minute policy timeout, measured from the fire's
    // real-clock `created_at`.
    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        Utc::now() + Duration::minutes(20),
    )
    .unwrap();

    assert_eq!(reports[0].action, "fired");
    let seeded = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(seeded.state, RoutineFireState::TimedOut);
}
