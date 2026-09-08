use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::{cursor_state_path, load_cursor_state, upsert_cursor};
use orbit_types::workflow::automation::AutomationDiagnostic;
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskPendingClaim, DedupePolicy,
};
use tempfile::tempdir;

use crate::auto_tasks::loader::auto_tasks_dir;
use crate::auto_tasks::scheduler::{
    AutoTaskDispatch, SchedulerFault, SchedulerOptions, inject_scheduler_fault,
    run_auto_task_scheduler_at, set_admission_overlap_barrier,
};

struct TestDispatch {
    definition_root: PathBuf,
    state_dir: PathBuf,
    minted: AtomicUsize,
    mint_ids: Mutex<Vec<String>>,
    mint_should_fail: AtomicBool,
    open_instance: AtomicBool,
}

impl TestDispatch {
    fn new(definition_root: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            definition_root,
            state_dir,
            minted: AtomicUsize::new(0),
            mint_ids: Mutex::new(Vec::new()),
            mint_should_fail: AtomicBool::new(false),
            open_instance: AtomicBool::new(false),
        }
    }
}

impl AutoTaskDispatch for TestDispatch {
    fn evaluate_delivery(
        &self,
        _definition: &AutoTaskDefinition,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<AutomationDiagnostic, OrbitError> {
        panic!("interval fixtures must not take the delivery path")
    }

    fn definition_root(&self) -> PathBuf {
        self.definition_root.clone()
    }

    fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    fn has_open_instance(&self, _definition: &AutoTaskDefinition) -> Result<bool, OrbitError> {
        Ok(self.open_instance.load(Ordering::SeqCst))
    }

    fn mint_task(&self, _definition: &AutoTaskDefinition) -> Result<String, OrbitError> {
        if self.mint_should_fail.load(Ordering::SeqCst) {
            return Err(OrbitError::Execution("injected mint failure".to_string()));
        }
        let n = self.minted.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("ORB-{n:05}");
        self.mint_ids.lock().expect("ids").push(id.clone());
        Ok(id)
    }
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

fn write_interval_definition(root: &std::path::Path, name: &str, dedupe: &str) {
    let definitions = auto_tasks_dir(root);
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    fs::write(
        definitions.join(format!("{name}.yaml")),
        format!(
            r#"schemaVersion: 1
name: {name}
schedule:
  every_minutes: 60
dedupe: {dedupe}
template:
  title: Fixture {name}
"#
        ),
    )
    .expect("definition fixture");
}

fn cursor(baseline: &str, last_slot: Option<&str>) -> AutoTaskCursor {
    AutoTaskCursor {
        baseline_at: baseline.to_string(),
        last_slot: last_slot.map(str::to_string),
        last_fired_at: last_slot.map(|_| "2026-01-01T01:00:05+00:00".to_string()),
        last_task_id: last_slot.map(|_| "ORB-00000".to_string()),
        pending: None,
    }
}

fn due_fixture(name: &str) -> (tempfile::TempDir, TestDispatch, DateTime<Utc>) {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, name, "always");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    let t0 = at(2026, 1, 1, 0, 0);
    upsert_cursor(
        &cursor_state_path(&dispatch.state_dir),
        name,
        cursor(&t0.to_rfc3339(), None),
    )
    .expect("baseline cursor");
    (root, dispatch, t0)
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

    let dispatch = TestDispatch::new(definition_root, state_dir);
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

#[test]
fn overlapping_due_passes_mint_one_task() {
    let (_root, dispatch, t0) = due_fixture("chore");
    let dispatch = Arc::new(dispatch);
    let barrier = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        for _ in 0..2 {
            let dispatch = Arc::clone(&dispatch);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                set_admission_overlap_barrier(Some(barrier));
                let outcome = run_auto_task_scheduler_at(
                    dispatch.as_ref(),
                    t0 + Duration::minutes(65),
                    SchedulerOptions::default(),
                );
                set_admission_overlap_barrier(None);
                outcome.expect("overlapping pass")
            });
        }
    });

    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert_eq!(
        state.definitions["chore"].last_task_id.as_deref(),
        Some("ORB-00001")
    );
    assert!(state.definitions["chore"].pending.is_none());
}

#[test]
fn mint_failure_does_not_consume_the_slot_and_retry_can_fire() {
    let (_root, dispatch, t0) = due_fixture("chore");
    dispatch.mint_should_fail.store(true, Ordering::SeqCst);

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert!(
        outcome.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("mint failed; slot not consumed")),
        "{:?}",
        outcome.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_none());
    assert!(state.definitions["chore"].pending.is_none());

    dispatch.mint_should_fail.store(false, Ordering::SeqCst);
    let retry = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("retry");
    assert_eq!(retry.reports[0].action, "fired");
    assert_eq!(retry.reports[0].task_id.as_deref(), Some("ORB-00001"));
}

#[test]
fn interruption_after_claim_reports_unresolved_and_does_not_remint() {
    let (_root, dispatch, t0) = due_fixture("chore");
    inject_scheduler_fault(Some(SchedulerFault::AfterClaim));

    let interrupted = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("interrupted");
    assert_eq!(interrupted.reports[0].action, "skipped");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert_eq!(
        state.definitions["chore"]
            .pending
            .as_ref()
            .map(|pending| pending.task_id.as_deref()),
        Some(None)
    );

    let retry = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("retry");
    assert_eq!(retry.reports[0].action, "skipped");
    assert!(
        retry.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("unresolved_pending")),
        "{:?}",
        retry.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_none());
}

#[test]
fn interruption_after_mint_reconciles_on_retry_without_reminting() {
    let (_root, dispatch, t0) = due_fixture("chore");
    inject_scheduler_fault(Some(SchedulerFault::AfterMintBeforeCheckpoint));

    let interrupted = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("interrupted");
    assert_eq!(interrupted.reports[0].action, "skipped");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert_eq!(
        state.definitions["chore"]
            .pending
            .as_ref()
            .and_then(|pending| pending.task_id.as_deref()),
        Some("ORB-00001")
    );
    assert!(state.definitions["chore"].last_slot.is_none());

    let retry = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("retry");
    assert_eq!(retry.reports[0].action, "fired");
    assert_eq!(retry.reports[0].task_id.as_deref(), Some("ORB-00001"));
    assert!(
        retry.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("reconciled pending mint")),
        "{:?}",
        retry.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].pending.is_none());
    assert!(state.definitions["chore"].last_slot.is_some());
}

#[test]
fn checkpoint_write_failure_records_mint_evidence_and_retry_reconciles() {
    let (_root, dispatch, t0) = due_fixture("chore");
    inject_scheduler_fault(Some(SchedulerFault::CheckpointWrite));

    let failed = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("checkpoint fail");
    assert_eq!(failed.reports[0].action, "fired");
    assert_eq!(failed.reports[0].task_id.as_deref(), Some("ORB-00001"));
    assert!(
        failed.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("cursor not advanced")),
        "{:?}",
        failed.reports[0]
    );
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert_eq!(
        state.definitions["chore"]
            .pending
            .as_ref()
            .and_then(|pending| pending.task_id.as_deref()),
        Some("ORB-00001")
    );
    assert!(state.definitions["chore"].last_slot.is_none());

    let retry = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("retry");
    assert_eq!(retry.reports[0].action, "fired");
    assert_eq!(retry.reports[0].task_id.as_deref(), Some("ORB-00001"));
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
}

#[test]
fn dry_run_creates_nothing_and_persists_no_cursor() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "chore", "skip_if_open");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    let t0 = at(2026, 1, 1, 0, 0);

    let outcome = run_auto_task_scheduler_at(&dispatch, t0, SchedulerOptions { dry_run: true })
        .expect("dry run");
    assert_eq!(outcome.reports[0].action, "would_baseline");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    assert!(!cursor_state_path(&dispatch.state_dir).exists());

    let again = run_auto_task_scheduler_at(&dispatch, t0, SchedulerOptions { dry_run: true })
        .expect("dry run 2");
    assert_eq!(again.reports[0].action, "would_baseline");
}

#[test]
fn skip_if_open_does_not_claim_or_mint() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "chore", "skip_if_open");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    dispatch.open_instance.store(true, Ordering::SeqCst);
    let t0 = at(2026, 1, 1, 0, 0);
    upsert_cursor(
        &cursor_state_path(&dispatch.state_dir),
        "chore",
        cursor(&t0.to_rfc3339(), None),
    )
    .expect("baseline");

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(outcome.reports[0].reason.as_deref(), Some("dedupe_open"));
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_none());
    assert!(state.definitions["chore"].pending.is_none());
}

#[test]
fn dry_run_would_fire_without_writing_when_due() {
    let (_root, dispatch, t0) = due_fixture("chore");
    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions { dry_run: true },
    )
    .expect("dry run");
    assert_eq!(outcome.reports[0].action, "would_fire");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_none());
    assert!(state.definitions["chore"].pending.is_none());
}

#[test]
fn malformed_cursor_is_not_baselined_or_rewritten() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "chore", "always");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    let path = cursor_state_path(&dispatch.state_dir);
    fs::create_dir_all(&dispatch.state_dir).expect("state dir");
    fs::write(&path, "{not json").expect("corrupt");

    let outcome =
        run_auto_task_scheduler_at(&dispatch, at(2026, 1, 1, 0, 0), SchedulerOptions::default())
            .expect("pass");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert!(
        outcome.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("malformed auto-task cursor state")),
        "{:?}",
        outcome.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_to_string(&path).expect("raw"), "{not json");
}

#[test]
fn missing_state_still_baselines_on_first_observation() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "chore", "always");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    let t0 = at(2026, 1, 1, 0, 0);

    let outcome =
        run_auto_task_scheduler_at(&dispatch, t0, SchedulerOptions::default()).expect("pass");
    assert_eq!(outcome.reports[0].action, "baselined");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_none());
    assert!(state.definitions["chore"].pending.is_none());
}

#[test]
fn seeded_pending_without_task_id_is_unresolved() {
    let (_root, dispatch, t0) = due_fixture("chore");
    let path = cursor_state_path(&dispatch.state_dir);
    upsert_cursor(
        &path,
        "chore",
        AutoTaskCursor {
            pending: Some(AutoTaskPendingClaim {
                slot: (t0 + Duration::minutes(60)).to_rfc3339(),
                task_id: None,
            }),
            ..cursor(&t0.to_rfc3339(), None)
        },
    )
    .expect("seed pending");

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert!(
        outcome.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("unresolved_pending")),
        "{:?}",
        outcome.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
}

#[test]
fn skip_if_open_dry_run_reports_dedupe_without_writing() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "chore", "skip_if_open");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    dispatch.open_instance.store(true, Ordering::SeqCst);
    let t0 = at(2026, 1, 1, 0, 0);
    upsert_cursor(
        &cursor_state_path(&dispatch.state_dir),
        "chore",
        cursor(&t0.to_rfc3339(), None),
    )
    .expect("baseline");
    let before = fs::read_to_string(cursor_state_path(&dispatch.state_dir)).expect("before");

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions { dry_run: true },
    )
    .expect("dry run");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(outcome.reports[0].reason.as_deref(), Some("dedupe_open"));
    assert_eq!(
        fs::read_to_string(cursor_state_path(&dispatch.state_dir)).expect("after"),
        before
    );
}

// DedupePolicy is referenced so a rename of the production default fails this file closed.
#[test]
fn skip_if_open_is_the_named_default_policy() {
    assert_eq!(DedupePolicy::default(), DedupePolicy::SkipIfOpen);
}
