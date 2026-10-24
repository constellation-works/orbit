//! Routine sweep dispatch through `run_sweep_core` against an in-memory store,
//! a hand-built collection, a scriptable dispatch and an explicit `now`: the
//! tick deadline and `overlap: forbid` with an interrupted source run
//! [ORB-10597].

use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use orbit_automation::routines::loader::{LoadedRoutine, RoutineCollection, RoutineOrigin};
use orbit_automation::routines::sweep::{
    RoutineDispatch, RunOwnerLiveness, SweepOptions, run_sweep_core,
};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::{RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, Store};
use orbit_types::workflow::{JobRunState, automation::AutomationDiagnostic};
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
    /// Running workspace drain, when the sweep should yield a ship slot.
    live_drain: RefCell<Option<String>>,
}

impl FakeDispatch {
    fn submit_count(&self) -> usize {
        self.submits.borrow().len()
    }
}

impl RoutineDispatch for FakeDispatch {
    fn live_workspace_drain(&self, _dir: &Path) -> Result<Option<String>, OrbitError> {
        Ok(self.live_drain.borrow().clone())
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

    fn evaluate_delivery(
        &self,
        _routine: &LoadedRoutine,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<AutomationDiagnostic, OrbitError> {
        Ok(AutomationDiagnostic {
            reason: "debouncing".into(),
            state: None,
            receipts: Vec::new(),
            waivers: Vec::new(),
            ownership: None,
            batch: Vec::new(),
            refusals: Vec::new(),
        })
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
    if super::isolated(
        "routine_sweep::interrupted_run_with_unprobeable_owner_is_reclaimed_at_the_policy_timeout",
    ) {
        return;
    }
    let (store, dispatch, coll, slot) = interrupted_forbid_fixture(RunOwnerLiveness::Unknown);

    // Past the routine's 10-minute policy timeout, measured from the fire's
    // real-clock `created_at`. The seeded slot is months behind that clock.
    // A `missed_run: skip` gap that long is recorded in bounded batches before
    // the next dispatch, so move the cursor next to `now` and let this pass
    // show the timeout release itself.
    let now = Utc::now() + Duration::minutes(20);
    let recent = now
        - Duration::seconds(i64::from(now.second()))
        - Duration::nanoseconds(i64::from(now.nanosecond()))
        - Duration::minutes(1);
    store
        .connection()
        .lock()
        .expect("store")
        .execute(
            "UPDATE routine_cursors SET last_slot = ?1 WHERE routine_name = 'job'",
            [recent.to_rfc3339()],
        )
        .expect("move cursor near now");
    let reports = run_sweep_core(&store, &coll, &dispatch, SweepOptions::default(), now).unwrap();

    assert_eq!(reports[0].action, "fired");
    let seeded = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(seeded.state, RoutineFireState::TimedOut);
}

fn routine_spec(
    name: &str,
    cron: &str,
    missed_run: &str,
    overlap: &str,
    job: &str,
    retries: (u32, u32),
    timeout_minutes: u32,
) -> LoadedRoutine {
    let (retries_max, backoff_minutes) = retries;
    let yaml = format!(
        "schemaVersion: 1\n\
         name: {name}\n\
         enabled: true\n\
         trigger:\n  cron: \"{cron}\"\n  missed_run: {missed_run}\n\
         target: job:{job}\n\
         policy:\n  timeout_minutes: {timeout_minutes}\n  overlap: {overlap}\n  \
         retries: {{ max: {retries_max}, backoff_minutes: {backoff_minutes} }}\n"
    );
    LoadedRoutine {
        definition: parse_routine_yaml(&yaml).expect("valid routine yaml"),
        origin: RoutineOrigin::Workspace,
        source_workspace: "polaris".to_string(),
        source_orbit_dir: PathBuf::from(SOURCE_DIR),
        path: PathBuf::from(format!("{SOURCE_DIR}/routines/{name}.yaml")),
    }
}

fn claim(
    store: &Store,
    name: &str,
    slot: DateTime<Utc>,
    state: RoutineFireState,
    detail: Option<&str>,
) {
    let slot = slot.to_rfc3339();
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: name.to_string(),
            slot: slot.clone(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .expect("claim slot");
    if state == RoutineFireState::Dispatched {
        store
            .routine_mark_fire_dispatched(name, &slot, 1, "live")
            .expect("dispatch");
        return;
    }
    store
        .routine_mark_fire_outcome(name, &slot, 1, state, detail)
        .expect("outcome");
}

/// A catch-up routine dispatches the latest missed slot once. A sibling that
/// kept `missed_run: skip`, even on the same job, records each missed slot
/// and does not dispatch.
#[test]
fn catch_up_once_fires_one_makeup_and_skip_records_each_missed_slot() {
    // Cron is host-local. The child pins UTC so the asserted slots stay stable.
    if super::isolated(
        "routine_sweep::catch_up_once_fires_one_makeup_and_skip_records_each_missed_slot",
    ) {
        return;
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let catch_up = routine_spec(
        "worktree-gc",
        "0 * * * *",
        "catch_up_once",
        "forbid",
        "worktree_gc_pipeline",
        (0, 1),
        30,
    );
    let skip = routine_spec(
        "operator-skip",
        "0 * * * *",
        "skip",
        "forbid",
        "worktree_gc_pipeline",
        (0, 1),
        30,
    );
    let baseline = ts(2026, 10, 14, 20, 0, 0).to_rfc3339();
    for name in ["worktree-gc", "operator-skip"] {
        store
            .routine_record_baseline(name, &baseline)
            .expect("baseline");
    }
    let dispatch = FakeDispatch::default();
    let now = ts(2026, 10, 15, 18, 30, 0);
    let reports = run_sweep_core(
        &store,
        &collection(vec![catch_up, skip]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_eq!(
        dispatch.submit_count(),
        1,
        "one make-up, not one per missed hour"
    );
    let gc = reports
        .iter()
        .find(|report| report.routine == "worktree-gc")
        .unwrap();
    assert_eq!(gc.action, "fired");
    assert_eq!(
        gc.slot.as_deref(),
        Some(ts(2026, 10, 15, 18, 0, 0).to_rfc3339().as_str())
    );
    let skipped = reports
        .iter()
        .find(|report| report.routine == "operator-skip")
        .unwrap();
    assert_eq!(skipped.action, "skipped");
    assert_eq!(skipped.reason.as_deref(), Some("missed_run: skip"));
    let gc_fires = fires(&store, "worktree-gc");
    assert_eq!(gc_fires.len(), 1);
    assert!(
        gc_fires
            .iter()
            .all(|fire| fire.state != RoutineFireState::Skipped)
    );
    let skip_fires = fires(&store, "operator-skip");
    assert_eq!(
        skip_fires.len(),
        22,
        "every missed hour from 21:00 through 18:00"
    );
    assert!(skip_fires.iter().all(|fire| {
        fire.state == RoutineFireState::Skipped
            && fire.detail.as_deref() == Some("missed_run: skip")
    }));

    let catch_up = routine_spec(
        "worktree-gc",
        "0 * * * *",
        "catch_up_once",
        "forbid",
        "worktree_gc_pipeline",
        (0, 1),
        30,
    );
    let skip = routine_spec(
        "operator-skip",
        "0 * * * *",
        "skip",
        "forbid",
        "worktree_gc_pipeline",
        (0, 1),
        30,
    );
    let again = run_sweep_core(
        &store,
        &collection(vec![catch_up, skip]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_eq!(dispatch.submit_count(), 1);
    assert!(again.iter().all(|report| report.action != "fired"));
    assert_eq!(fires(&store, "operator-skip").len(), 22);
}

/// A forbid routine whose fire is still running does not start another one
/// when newer skipped rows, or several later slots, would otherwise look due.
#[test]
fn overlap_forbid_holds_across_newer_skipped_slots() {
    if super::isolated("routine_sweep::overlap_forbid_holds_across_newer_skipped_slots") {
        return;
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let job = routine_spec(
        "ship",
        "* * * * *",
        "skip",
        "forbid",
        "workspace_ship_pipeline",
        (0, 1),
        120,
    );
    store
        .routine_record_baseline("ship", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    claim(
        &store,
        "ship",
        ts(2026, 1, 1, 0, 5, 0),
        RoutineFireState::Dispatched,
        None,
    );
    for minute in [6, 7] {
        claim(
            &store,
            "ship",
            ts(2026, 1, 1, 0, minute, 0),
            RoutineFireState::Skipped,
            Some("missed_run: skip"),
        );
    }
    let dispatch = FakeDispatch::default();
    dispatch
        .states
        .borrow_mut()
        .insert("live".into(), JobRunState::Running);
    dispatch
        .liveness
        .borrow_mut()
        .insert("live".into(), RunOwnerLiveness::Alive);
    let before = fires(&store, "ship").len();
    let reports = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 20, 0),
    )
    .unwrap();
    assert_eq!(dispatch.submit_count(), 0);
    assert_eq!(reports[0].reason.as_deref(), Some("overlap_in_flight"));
    assert_eq!(fires(&store, "ship").len(), before);
    assert_eq!(
        fires(&store, "ship")
            .into_iter()
            .filter(|fire| !fire.state.is_terminal())
            .count(),
        1
    );
}

/// Skip rows newer than a failed fire must not consume the retry budget's view
/// of that fire, and a later gap must not record more skips while the retry
/// is still owed.
#[test]
fn failed_fire_with_retry_budget_survives_skipped_slots() {
    if super::isolated("routine_sweep::failed_fire_with_retry_budget_survives_skipped_slots") {
        return;
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let job = routine_spec("hourly", "0 * * * *", "skip", "forbid", "noop", (2, 1), 30);
    store
        .routine_record_baseline("hourly", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    let failed_slot = ts(2026, 1, 1, 0, 0, 0);
    claim(
        &store,
        "hourly",
        failed_slot,
        RoutineFireState::Failed,
        Some("dispatch failed"),
    );
    for hour in [1, 2, 3] {
        claim(
            &store,
            "hourly",
            ts(2026, 1, 1, hour, 0, 0),
            RoutineFireState::Skipped,
            Some("missed_run: skip"),
        );
    }
    store
        .connection()
        .lock()
        .expect("store")
        .execute(
            "UPDATE routine_fires SET updated_at = '2026-01-01T00:10:00+00:00' \
             WHERE routine_name = 'hourly' AND state = 'failed'",
            [],
        )
        .expect("backdate failure");
    let dispatch = FakeDispatch::default();
    let reports = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 3, 30, 0),
    )
    .unwrap();
    assert_eq!(reports[0].action, "retry_fired");
    assert_eq!(dispatch.submit_count(), 1);
    assert_eq!(
        store
            .routine_cursor("hourly")
            .unwrap()
            .unwrap()
            .last_slot
            .as_deref(),
        Some(ts(2026, 1, 1, 3, 0, 0).to_rfc3339().as_str()),
        "retrying an older slot must not rewind the cursor"
    );
    let retry = fires(&store, "hourly")
        .into_iter()
        .find(|fire| fire.slot == failed_slot.to_rfc3339() && fire.attempt == 2)
        .unwrap();
    assert_eq!(retry.state, RoutineFireState::Dispatched);

    let store = Store::open_in_memory().expect("in-memory store");
    let job = routine_spec("hourly", "0 * * * *", "skip", "forbid", "noop", (2, 30), 30);
    store
        .routine_record_baseline("hourly", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    claim(
        &store,
        "hourly",
        ts(2026, 1, 1, 0, 0, 0),
        RoutineFireState::Failed,
        Some("dispatch failed"),
    );
    store
        .connection()
        .lock()
        .expect("store")
        .execute(
            "UPDATE routine_fires SET updated_at = '2026-01-01T03:20:00+00:00' \
             WHERE routine_name = 'hourly' AND state = 'failed'",
            [],
        )
        .expect("keep the failure inside its backoff");
    let dispatch = FakeDispatch::default();
    let reports = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 3, 30, 0),
    )
    .unwrap();
    assert_eq!(dispatch.submit_count(), 0);
    assert_eq!(reports[0].action, "skipped");
    assert_eq!(reports[0].reason.as_deref(), Some("not_due"));
    assert_eq!(
        fires(&store, "hourly").len(),
        1,
        "no skip row while a retry is owed"
    );
}

/// A drain-skipped retry occupies its attempt key. Later ticks must not
/// report `slot_already_claimed` for that key: while the drain is live the
/// sweep yields without writing another row, and once the drain is gone it
/// dispatches the next free attempt. [ORB-16417]
#[test]
fn drain_skipped_retry_dispatches_on_the_next_free_attempt() {
    if super::isolated("routine_sweep::drain_skipped_retry_dispatches_on_the_next_free_attempt") {
        return;
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let job = routine_spec(
        "ship",
        "0 * * * *",
        "skip",
        "allow",
        "workspace_ship_pipeline",
        (1, 1),
        30,
    );
    let slot = ts(2026, 1, 1, 0, 0, 0);
    let slot_s = slot.to_rfc3339();
    store
        .routine_record_baseline("ship", &slot_s)
        .expect("baseline");
    claim(
        &store,
        "ship",
        slot,
        RoutineFireState::Failed,
        Some("dispatch failed"),
    );
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "ship".to_string(),
            slot: slot_s.clone(),
            attempt: 2,
            source_workspace: "polaris".to_string(),
        })
        .expect("claim the drain-skipped retry");
    store
        .routine_mark_fire_outcome(
            "ship",
            &slot_s,
            2,
            RoutineFireState::Skipped,
            Some("workspace_drain_live: drain-1"),
        )
        .expect("record the drain skip");
    store
        .connection()
        .lock()
        .expect("store")
        .execute(
            "UPDATE routine_fires SET updated_at = '2026-01-01T00:10:00+00:00' \
             WHERE routine_name = 'ship' AND state = 'failed'",
            [],
        )
        .expect("backdate the failure past its backoff");

    let dispatch = FakeDispatch::default();
    *dispatch.live_drain.borrow_mut() = Some("drain-1".to_string());
    let now = ts(2026, 1, 1, 0, 30, 0);
    let held = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_ne!(
        held[0].reason.as_deref(),
        Some("slot_already_claimed"),
        "a drain-skipped retry must not stick later ticks on slot_already_claimed"
    );
    assert_eq!(
        held[0].reason.as_deref(),
        Some("workspace_drain_live: drain-1")
    );
    assert_eq!(dispatch.submit_count(), 0);
    assert_eq!(
        fires(&store, "ship").len(),
        2,
        "a live drain must not record another skipped retry"
    );

    *dispatch.live_drain.borrow_mut() = None;
    let job = routine_spec(
        "ship",
        "0 * * * *",
        "skip",
        "allow",
        "workspace_ship_pipeline",
        (1, 1),
        30,
    );
    let reports = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_ne!(
        reports[0].reason.as_deref(),
        Some("slot_already_claimed"),
        "clearing the drain must dispatch the retry, not repeat slot_already_claimed"
    );
    assert_eq!(reports[0].action, "retry_fired");
    assert_eq!(dispatch.submit_count(), 1);
    let retry = fires(&store, "ship")
        .into_iter()
        .find(|fire| fire.slot == slot_s && fire.attempt == 3)
        .expect("fresh attempt");
    assert_eq!(retry.state, RoutineFireState::Dispatched);
    let skipped = fires(&store, "ship")
        .into_iter()
        .find(|fire| fire.attempt == 2)
        .expect("drain-skipped attempt");
    assert_eq!(skipped.state, RoutineFireState::Skipped);

    let job = routine_spec(
        "ship",
        "0 * * * *",
        "skip",
        "allow",
        "workspace_ship_pipeline",
        (1, 1),
        30,
    );
    let again = run_sweep_core(
        &store,
        &collection(vec![job]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_ne!(again[0].reason.as_deref(), Some("slot_already_claimed"));
    assert_eq!(
        dispatch.submit_count(),
        1,
        "the fresh attempt is not re-fired"
    );
}

/// A long gap is recorded oldest-first in a bounded batch. The natural slot
/// waits until that batch has caught the cursor up.
#[test]
fn skip_records_a_bounded_batch_before_the_next_natural_fire() {
    if super::isolated("routine_sweep::skip_records_a_bounded_batch_before_the_next_natural_fire") {
        return;
    }
    let store = Store::open_in_memory().expect("in-memory store");
    let first = routine_spec("minute", "* * * * *", "skip", "allow", "noop", (0, 1), 30);
    store
        .routine_record_baseline("minute", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    let dispatch = FakeDispatch::default();
    let now = ts(2026, 1, 1, 2, 0, 30);
    let reports = run_sweep_core(
        &store,
        &collection(vec![first]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_eq!(dispatch.submit_count(), 0);
    assert_eq!(reports[0].reason.as_deref(), Some("missed_run: skip"));
    assert_eq!(store.routine_recent_fires("minute", 80).unwrap().len(), 64);
    assert_eq!(
        store
            .routine_cursor("minute")
            .unwrap()
            .unwrap()
            .last_slot
            .as_deref(),
        Some(ts(2026, 1, 1, 1, 4, 0).to_rfc3339().as_str())
    );

    let second = routine_spec("minute", "* * * * *", "skip", "allow", "noop", (0, 1), 30);
    let reports = run_sweep_core(
        &store,
        &collection(vec![second]),
        &dispatch,
        SweepOptions::default(),
        now,
    )
    .unwrap();
    assert_eq!(dispatch.submit_count(), 1);
    assert_eq!(reports[0].action, "fired");
    assert_eq!(
        reports[0].slot.as_deref(),
        Some(ts(2026, 1, 1, 2, 0, 0).to_rfc3339().as_str())
    );
}

/// A state-triggered routine shaped like the seeded triage definitions.
fn state_routine(name: &str, kind: &str) -> LoadedRoutine {
    let yaml = format!(
        r#"schemaVersion: 1
name: {name}
enabled: true
trigger:
  state:
    kind: {kind}
    owner_machine: hm_test
    branch: agent-main
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
  retries: {{ max: 1, backoff_minutes: 5 }}
"#
    );
    LoadedRoutine {
        definition: parse_routine_yaml(&yaml).expect("valid state routine yaml"),
        origin: RoutineOrigin::Workspace,
        source_workspace: "polaris".to_string(),
        source_orbit_dir: PathBuf::from(SOURCE_DIR),
        path: PathBuf::from(format!("{SOURCE_DIR}/routines/{name}.yaml")),
    }
}

/// The seeded pilot-warning and blocked-task triage routines share
/// `job:escalation_triage_pipeline` but watch different trigger kinds. Each
/// owns its own consumer and member set, so enabling both runs both rather
/// than skipping both as duplicate owners. [ORB-17918]
#[test]
fn state_routines_of_different_kinds_on_one_target_both_run() {
    let store = Store::open_in_memory().expect("in-memory store");
    let coll = collection(vec![
        state_routine("pilot-warning-triage", "pilot_warnings_held"),
        state_routine("blocked-task-triage", "block_escalated"),
    ]);

    let reports = run_sweep_core(
        &store,
        &coll,
        &FakeDispatch::default(),
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 0, 0),
    )
    .unwrap();

    for report in &reports {
        assert_eq!(report.action, "state", "{} should evaluate", report.routine);
        assert_eq!(report.reason.as_deref(), Some("debouncing"));
    }
}

/// The migration guard keeps the cases it was written for: a temporal routine
/// and a state routine on one target, or two state routines of one kind, still
/// skip each other as duplicate owners. [ORB-11331]
#[test]
fn temporal_and_same_kind_state_routines_on_one_target_still_skip_as_duplicates() {
    let store = Store::open_in_memory().expect("in-memory store");
    let temporal_and_state = collection(vec![
        routine_spec(
            "temporal-triage",
            "0 * * * *",
            "skip",
            "allow",
            "escalation_triage_pipeline",
            (0, 1),
            30,
        ),
        state_routine("pilot-warning-triage", "pilot_warnings_held"),
    ]);
    let same_kind = collection(vec![
        state_routine("pilot-warning-triage", "pilot_warnings_held"),
        state_routine("pilot-warning-copy", "pilot_warnings_held"),
    ]);

    for coll in [temporal_and_state, same_kind] {
        let reports = run_sweep_core(
            &store,
            &coll,
            &FakeDispatch::default(),
            SweepOptions::default(),
            ts(2026, 1, 1, 0, 0, 0),
        )
        .unwrap();

        for report in &reports {
            assert_eq!(report.action, "skipped", "{}", report.routine);
            assert_eq!(
                report.reason.as_deref(),
                Some("duplicate_routine_ownership"),
                "{}",
                report.routine
            );
        }
    }
}
