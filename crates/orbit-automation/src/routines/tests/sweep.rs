//! Sweep-orchestration tests exercise the fire / idempotency /
//! overlap / retry / outcome-sync logic in `routines/sweep.rs` that shipped
//! untested in [ORB-10021].
//!
//! Two layers:
//! - `run_sweep_core` against an in-memory store, a hand-built
//!   [`RoutineCollection`], a fake [`RoutineDispatch`], and an explicit `now`
//!   — deterministic, no pipeline workers spawned.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::{RoutineFireIntentParams, RoutineFireState, Store};
use orbit_types::workflow::{JobRunState, RoutineDefinition};

use crate::routines::loader::{LoadedRoutine, RoutineCollection, RoutineOrigin};
use crate::routines::sweep::RunOwnerLiveness;
use crate::routines::sweep::{RoutineDispatch, SweepOptions, run_sweep_core};

const SOURCE_DIR: &str = "/ws/.orbit";

// ---- fixtures -------------------------------------------------------------

/// Build a validated routine with the common knobs the tests vary.
fn routine(
    name: &str,
    cron: &str,
    enabled: bool,
    overlap: &str,
    retries_max: u32,
) -> LoadedRoutine {
    let yaml = format!(
        "schemaVersion: 1\n\
         name: {name}\n\
         enabled: {enabled}\n\
         trigger:\n  cron: \"{cron}\"\n\
         target: job:noop\n\
         policy:\n  timeout_minutes: 10\n  overlap: {overlap}\n  \
         retries: {{ max: {retries_max}, backoff_minutes: 1 }}\n"
    );
    loaded(parse_routine_yaml(&yaml).expect("valid routine yaml"))
}

fn loaded(definition: RoutineDefinition) -> LoadedRoutine {
    let name = definition.name.clone();
    LoadedRoutine {
        definition,
        origin: RoutineOrigin::Committed,
        source_workspace: "polaris".to_string(),
        source_orbit_dir: PathBuf::from(SOURCE_DIR),
        path: PathBuf::from(format!("{SOURCE_DIR}/routines/{name}.yaml")),
    }
}

fn collection(routines: Vec<LoadedRoutine>) -> RoutineCollection {
    RoutineCollection {
        routines,
        errors: Vec::new(),
    }
}

fn store() -> Store {
    Store::open_in_memory().expect("in-memory store")
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
    fail_submit: Cell<bool>,
    counter: Cell<u32>,
    submits: RefCell<Vec<(PathBuf, String)>>,
    states: RefCell<HashMap<String, JobRunState>>,
    /// Owner liveness per run id [ORB-10597]. Absent means `Stopped` — the
    /// historical assumption that a terminal run has finished working.
    liveness: RefCell<HashMap<String, RunOwnerLiveness>>,
}

impl FakeDispatch {
    fn submit_count(&self) -> usize {
        self.submits.borrow().len()
    }

    fn set_state(&self, run_id: &str, state: JobRunState) {
        self.states.borrow_mut().insert(run_id.to_string(), state);
    }

    fn set_liveness(&self, run_id: &str, liveness: RunOwnerLiveness) {
        self.liveness
            .borrow_mut()
            .insert(run_id.to_string(), liveness);
    }

    /// Make the next `submit` calls fail (dispatch-time error) until cleared.
    fn set_fail(&self, fail: bool) {
        self.fail_submit.set(fail);
    }
}

impl RoutineDispatch for FakeDispatch {
    fn submit(
        &self,
        dir: &Path,
        job: &str,
        _actor: &str,
        _slot: &str,
    ) -> Result<String, OrbitError> {
        if self.fail_submit.get() {
            return Err(OrbitError::Execution("dispatch boom".to_string()));
        }
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

fn fires(store: &Store, name: &str) -> Vec<orbit_store::RoutineFireRecord> {
    store.routine_recent_fires(name, 32).expect("recent fires")
}

// ---- baseline & fire ------------------------------------------------------

#[test]
fn first_sweep_baselines_and_fires_nothing_then_next_slot_fires() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("nightly", "* * * * *", true, "allow", 0)]);

    // First observation: baseline is recorded, nothing fires.
    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 0, 30),
    )
    .expect("sweep 1");
    assert_eq!(reports[0].action, "baselined");
    assert!(store.routine_cursor("nightly").unwrap().is_some());
    assert!(fires(&store, "nightly").is_empty());
    assert_eq!(dispatch.submit_count(), 0);

    // A later natural slot fires exactly once.
    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 1, 20),
    )
    .expect("sweep 2");
    assert_eq!(reports[0].action, "fired");
    let rows = fires(&store, "nightly");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, RoutineFireState::Dispatched);
    assert_eq!(dispatch.submit_count(), 1);
}

#[test]
fn same_slot_second_sweep_does_not_double_fire() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("nightly", "* * * * *", true, "allow", 0)]);
    // Baseline in the past so the target minute is already fireable.
    store
        .routine_record_baseline("nightly", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();

    let opts = SweepOptions::default();
    let first = run_sweep_core(&store, &coll, &dispatch, opts, ts(2026, 1, 1, 0, 1, 10)).unwrap();
    assert_eq!(first[0].action, "fired");

    // Second sweep in the SAME minute: the consumed slot is not re-fired.
    let second = run_sweep_core(&store, &coll, &dispatch, opts, ts(2026, 1, 1, 0, 1, 50)).unwrap();
    assert_eq!(second[0].action, "skipped");
    assert_eq!(second[0].reason.as_deref(), Some("not_due"));

    assert_eq!(
        fires(&store, "nightly").len(),
        1,
        "exactly one fire for the slot"
    );
    assert_eq!(dispatch.submit_count(), 1);
}

#[test]
fn five_minute_clock_fires_the_incident_slot_once_after_a_phase_gap() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("hourly", "5 * * * *", true, "allow", 0)]);
    store
        .routine_record_baseline("hourly", &ts(2026, 9, 7, 0, 5, 0).to_rfc3339())
        .expect("baseline");
    let options = SweepOptions {
        sweep_cadence_seconds: 300,
        ..SweepOptions::default()
    };

    // The first poll is before the 01:05 slot. The next poll is 305 seconds
    // later, matching the observed 01:03:38 -> 01:08:43 phase gap.
    let before_slot = run_sweep_core(&store, &coll, &dispatch, options, ts(2026, 9, 7, 1, 3, 38))
        .expect("pre-slot sweep");
    assert_eq!(before_slot[0].reason.as_deref(), Some("not_due"));

    let fired = run_sweep_core(&store, &coll, &dispatch, options, ts(2026, 9, 7, 1, 8, 43))
        .expect("delayed sweep");
    assert_eq!(fired[0].action, "fired");
    assert_eq!(fired[0].slot.as_deref(), Some("2026-09-07T01:05:00+00:00"));

    let later_poll = run_sweep_core(&store, &coll, &dispatch, options, ts(2026, 9, 7, 1, 13, 43))
        .expect("later sweep");
    assert_eq!(later_poll[0].reason.as_deref(), Some("not_due"));
    assert_eq!(dispatch.submit_count(), 1, "one ordinary fire for the slot");
    assert_eq!(
        fires(&store, "hourly").len(),
        1,
        "one persisted fire intent"
    );
}

// ---- toggles --------------------------------------------------------------

#[test]
fn toggles_suppress_the_fire_with_the_right_reason() {
    let store = store();
    let dispatch = FakeDispatch::default();
    store
        .routine_record_baseline("disabled", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    store
        .routine_record_baseline("paused", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    store.routine_pause("paused", "test").unwrap();

    let coll = collection(vec![
        routine("disabled", "* * * * *", false, "allow", 0),
        routine("paused", "* * * * *", true, "allow", 0),
    ]);

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 5, 10),
    )
    .unwrap();

    let reason = |name: &str| {
        reports
            .iter()
            .find(|r| r.routine == name)
            .and_then(|r| r.reason.clone())
    };
    assert_eq!(
        reason("disabled").as_deref(),
        Some("disabled_in_definition")
    );
    assert_eq!(reason("paused").as_deref(), Some("paused_locally"));
    assert_eq!(dispatch.submit_count(), 0);
}

/// [ORB-12236] Eligibility is registration plus the definition's own switches;
/// a retired `hosts:` pin naming another machine changes nothing.
#[test]
fn a_retired_host_pin_does_not_suppress_the_fire() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let yaml = "schemaVersion: 1\nname: pinned\nenabled: true\nhosts: [some-other-host]\n\
                trigger:\n  cron: \"* * * * *\"\n\
                target: job:noop\n";
    let coll = collection(vec![loaded(parse_routine_yaml(yaml).expect("valid yaml"))]);
    store
        .routine_record_baseline("pinned", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 5, 10),
    )
    .unwrap();

    assert_eq!(reports[0].action, "fired");
    assert_eq!(dispatch.submit_count(), 1);
}

// ---- overlap: forbid ------------------------------------------------------

#[test]
fn overlap_forbid_skips_while_in_flight_then_fires_once_terminal() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("job", "* * * * *", true, "forbid", 0)]);

    // Seed a dispatched, still-in-flight fire at 00:05 and point the cursor at it.
    store
        .routine_record_baseline("job", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();
    let slot_in_flight = ts(2026, 1, 1, 0, 5, 0).to_rfc3339();
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "job".to_string(),
            slot: slot_in_flight.clone(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .unwrap();
    store
        .routine_mark_fire_dispatched("job", &slot_in_flight, 1, "inflight")
        .unwrap();

    // A new slot comes due while the prior fire is non-terminal -> skipped.
    // `now` sits before the seeded fire's real-clock created_at, so the outcome
    // sync cannot reclaim it as stale — it is genuinely in flight.
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
    assert_eq!(dispatch.submit_count(), 0);

    // Once the in-flight run reaches a terminal state, the next slot fires.
    dispatch.set_state("inflight", JobRunState::Success);
    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        ts(2026, 1, 1, 0, 7, 20),
    )
    .unwrap();
    assert_eq!(reports[0].action, "fired");
    assert_eq!(dispatch.submit_count(), 1);
    // The reclaimed fire is now terminal.
    let seeded = fires(&store, "job")
        .into_iter()
        .find(|f| f.slot == slot_in_flight)
        .unwrap();
    assert_eq!(seeded.state, RoutineFireState::Succeeded);
}

// ---- overlap: forbid + interrupted source run [ORB-10597] -----------------

/// Seed an `overlap: forbid` routine with one dispatched fire whose run is
/// marked `interrupted`, and return the store, dispatch, collection, and the
/// seeded slot. `created_at` is real-clock now, so a `now_utc` in the fixture's
/// past keeps the fire inside its policy timeout.
fn interrupted_forbid_fixture(
    liveness: RunOwnerLiveness,
) -> (Store, FakeDispatch, RoutineCollection, String) {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("job", "* * * * *", true, "forbid", 0)]);

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
    dispatch.set_state("condemned", JobRunState::Interrupted);
    dispatch.set_liveness("condemned", liveness);
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

#[test]
fn running_run_orphaned_by_restart_releases_the_forbid_slot_immediately() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("job", "* * * * *", true, "forbid", 0)]);
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
        .routine_mark_fire_dispatched("job", &slot, 1, "orphaned")
        .unwrap();
    dispatch.set_state("orphaned", JobRunState::Running);
    dispatch.set_liveness("orphaned", RunOwnerLiveness::Stopped);

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
    let orphaned = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(orphaned.state, RoutineFireState::Failed);
    assert_eq!(
        orphaned.detail.as_deref(),
        Some("run owner stopped before recording a terminal outcome")
    );
}

#[test]
fn running_run_with_a_live_owner_keeps_the_forbid_slot() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("job", "* * * * *", true, "forbid", 0)]);
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
        .routine_mark_fire_dispatched("job", &slot, 1, "live")
        .unwrap();
    dispatch.set_state("live", JobRunState::Running);
    dispatch.set_liveness("live", RunOwnerLiveness::Alive);

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
    assert_eq!(dispatch.submit_count(), 0);
    let live = fires(&store, "job")
        .into_iter()
        .find(|fire| fire.slot == slot)
        .unwrap();
    assert_eq!(live.state, RoutineFireState::Dispatched);
}

// ---- outcome sync / staleness horizon -------------------------------------

#[test]
fn sync_reclaims_stale_intent_and_dispatched_past_timeout() {
    let store = store();
    let dispatch = FakeDispatch::default();
    // enabled:false so the per-routine pass is a no-op and only the outcome
    // sync at the top of the pass touches these fires.
    let coll = collection(vec![routine("job", "* * * * *", false, "forbid", 0)]);

    let now = Utc::now();
    store
        .routine_record_baseline("job", &now.to_rfc3339())
        .unwrap();
    // Stale intent (sweep died before dispatch).
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "job".to_string(),
            slot: "2026-01-01T00:01:00+00:00".to_string(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .unwrap();
    // Dispatched but the run is unqueryable (fake returns no state).
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "job".to_string(),
            slot: "2026-01-01T00:02:00+00:00".to_string(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .unwrap();
    store
        .routine_mark_fire_dispatched("job", "2026-01-01T00:02:00+00:00", 1, "lost-run")
        .unwrap();

    // Advance `now` past the 10-minute policy timeout so both are reclaimable.
    let reclaim_at = now + Duration::hours(2);
    run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        reclaim_at,
    )
    .unwrap();

    let by_slot: HashMap<String, RoutineFireState> = fires(&store, "job")
        .into_iter()
        .map(|f| (f.slot, f.state))
        .collect();
    assert_eq!(
        by_slot.get("2026-01-01T00:01:00+00:00"),
        Some(&RoutineFireState::Error),
        "stale intent reclaimed as error"
    );
    assert_eq!(
        by_slot.get("2026-01-01T00:02:00+00:00"),
        Some(&RoutineFireState::TimedOut),
        "stale dispatched reclaimed as timed_out"
    );
}

#[test]
fn malformed_timeout_in_one_dispatched_fire_reports_only_that_routine() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let mut malformed = routine("malformed", "* * * * *", true, "forbid", 0);
    malformed.definition.policy.timeout_minutes = 1_000_000_000_000_000;
    let coll = collection(vec![
        malformed,
        routine("healthy", "* * * * *", true, "forbid", 0),
    ]);

    let slot = ts(2026, 1, 1, 0, 5, 0).to_rfc3339();
    store
        .routine_record_fire_intent(&RoutineFireIntentParams {
            routine_name: "malformed".to_string(),
            slot: slot.clone(),
            attempt: 1,
            source_workspace: "polaris".to_string(),
        })
        .unwrap();
    store
        .routine_mark_fire_dispatched("malformed", &slot, 1, "malformed-run")
        .unwrap();

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions::default(),
        Utc::now(),
    )
    .expect("malformed policy is isolated to its routine");

    let malformed_report = reports
        .iter()
        .find(|report| report.routine == "malformed")
        .expect("malformed routine report");
    assert_eq!(malformed_report.action, "error");
    assert!(
        malformed_report
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("policy.timeout_minutes"))
    );
    assert!(
        reports
            .iter()
            .filter(|report| report.routine != "malformed")
            .all(|report| report.action != "error")
    );
}

// ---- dispatch-error retry --------------------------------------------------

#[test]
fn dispatch_error_is_retry_eligible_under_the_same_slot() {
    let store = store();
    let dispatch = FakeDispatch::default();
    // Daily catch-up routine so exactly one slot is due across both sweeps
    // (a frequent cron would surface a *new* slot before any retry). backoff 0
    // keeps the retry immediately eligible without wall-clock waiting.
    let yaml = "schemaVersion: 1\nname: job\nenabled: true\n\
                trigger:\n  cron: \"0 0 * * *\"\n  missed_run: catch_up_once\n\
                target: job:noop\n\
                policy:\n  timeout_minutes: 10\n  overlap: forbid\n  \
                retries: { max: 2, backoff_minutes: 0 }\n";
    let coll = collection(vec![loaded(parse_routine_yaml(yaml).unwrap())]);
    // `now` sits just ahead of wall-clock so the errored fire's stored
    // updated_at is strictly in the past (backoff 0 is then satisfied).
    let now = Utc::now() + Duration::minutes(1);
    store
        .routine_record_baseline("job", &(now - Duration::days(2)).to_rfc3339())
        .unwrap();

    // Sweep 1: the slot is due; the synchronous dispatch fails.
    dispatch.set_fail(true);
    let r1 = run_sweep_core(&store, &coll, &dispatch, SweepOptions::default(), now).unwrap();
    assert_eq!(
        r1[0].action, "error",
        "dispatch failure is reported as error"
    );
    let errored = store.routine_latest_fire("job").unwrap().unwrap();
    // Recorded as retryable Failed (nothing dispatched), not terminal Error.
    assert_eq!(errored.state, RoutineFireState::Failed);
    assert_eq!(errored.attempt, 1);
    assert_eq!(
        dispatch.submit_count(),
        0,
        "a failed submit dispatches nothing"
    );
    let slot = errored.slot.clone();

    // Sweep 2: no new slot is due, but the dispatch error is now retryable.
    // With submit healthy it re-dispatches attempt 2 under the SAME slot.
    dispatch.set_fail(false);
    let r2 = run_sweep_core(&store, &coll, &dispatch, SweepOptions::default(), now).unwrap();
    assert_eq!(r2[0].action, "retry_fired");
    let retried = store.routine_latest_fire("job").unwrap().unwrap();
    assert_eq!(retried.state, RoutineFireState::Dispatched);
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.slot, slot, "retry re-uses the same scheduled slot");
    assert_eq!(dispatch.submit_count(), 1);

    // Idempotency: exactly two fires for the one slot (no double-dispatch).
    let rows = fires(&store, "job");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|f| f.slot == slot));
}

// ---- dry-run --------------------------------------------------------------

#[test]
fn dry_run_records_no_state() {
    let store = store();
    let dispatch = FakeDispatch::default();
    let coll = collection(vec![routine("nightly", "* * * * *", true, "allow", 0)]);
    store
        .routine_record_baseline("seen", &ts(2026, 1, 1, 0, 0, 0).to_rfc3339())
        .unwrap();

    let reports = run_sweep_core(
        &store,
        &coll,
        &dispatch,
        SweepOptions {
            dry_run: true,
            ..SweepOptions::default()
        },
        ts(2026, 1, 1, 0, 5, 10),
    )
    .unwrap();

    // First-observation routine reports would_baseline but records no cursor.
    assert_eq!(reports[0].action, "would_baseline");
    assert!(store.routine_cursor("nightly").unwrap().is_none());
    assert!(fires(&store, "nightly").is_empty());
    assert_eq!(dispatch.submit_count(), 0);
}

#[test]
fn state_and_temporal_owners_of_same_pipeline_are_withheld_in_preview() {
    let mut legacy = routine("legacy", "* * * * *", true, "forbid", 0);
    legacy.definition.target =
        orbit_types::workflow::RoutineTarget::Job("task_pilot_pipeline".into());
    let mut state = legacy.clone();
    state.definition.name = "state-pilot".into();
    state.definition.trigger.cron.clear();
    state.definition.trigger.state =
        Some(orbit_types::workflow::automation::members::StateTrigger {
            kind: orbit_types::workflow::automation::members::StateTriggerKind::PreparationEligible,
            owner_machine: "test-machine".into(),
            branch: "agent-main".into(),
            debounce_minutes: 2,
            max_wait_minutes: 10,
            max_items: 50,
            retries: 1,
            deadline_minutes: 90,
        });
    let reports = run_sweep_core(
        &store(),
        &collection(vec![legacy, state]),
        &FakeDispatch::default(),
        SweepOptions {
            dry_run: true,
            ..SweepOptions::default()
        },
        ts(2026, 9, 6, 0, 0, 0),
    )
    .unwrap();
    assert_eq!(reports.len(), 2);
    assert!(
        reports
            .iter()
            .all(|r| r.reason.as_deref() == Some("duplicate_routine_ownership"))
    );
}
