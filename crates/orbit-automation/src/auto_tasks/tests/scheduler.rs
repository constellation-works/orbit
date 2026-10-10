use crate::auto_tasks::loader::auto_tasks_dir;
use crate::auto_tasks::scheduler::{
    AutoTaskDispatch, ChangeProbe, SchedulerFault, SchedulerOptions, inject_scheduler_fault,
    run_auto_task_scheduler_at, set_admission_overlap_barrier, set_after_load_barriers,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::{
    cursor_state_path, load_cursor_state, upsert_cursor, with_cursor_lock,
};
use orbit_types::workflow::automation::AutomationDiagnostic;
use orbit_types::workflow::{AutoTaskCursor, AutoTaskDefinition};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use tempfile::tempdir;

struct TestDispatch {
    definition_root: PathBuf,
    state_dir: PathBuf,
    minted: AtomicUsize,
    mint_ids: Mutex<Vec<String>>,
    mint_titles: Mutex<Vec<String>>,
    mint_should_fail: AtomicBool,
    delivery_enabled: AtomicBool,
    delivery_evaluations: AtomicUsize,
    open_instance: Mutex<Option<String>>,
    probe: Mutex<Option<Result<ChangeProbe, String>>>,
}

impl TestDispatch {
    fn new(definition_root: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            definition_root,
            state_dir,
            minted: AtomicUsize::new(0),
            mint_ids: Mutex::new(Vec::new()),
            mint_titles: Mutex::new(Vec::new()),
            mint_should_fail: AtomicBool::new(false),
            delivery_enabled: AtomicBool::new(false),
            delivery_evaluations: AtomicUsize::new(0),
            open_instance: Mutex::new(None),
            probe: Mutex::new(None),
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
        assert!(
            self.delivery_enabled.load(Ordering::SeqCst),
            "delivery evaluation must not run for this fixture"
        );
        self.delivery_evaluations.fetch_add(1, Ordering::SeqCst);
        Ok(AutomationDiagnostic {
            reason: "delivery fixture evaluated".to_string(),
            state: None,
            receipts: Vec::new(),
            waivers: Vec::new(),
            ownership: None,
            batch: Vec::new(),
            refusals: Vec::new(),
        })
    }

    fn definition_root(&self) -> PathBuf {
        self.definition_root.clone()
    }

    fn state_dir(&self) -> PathBuf {
        self.state_dir.clone()
    }

    fn has_open_instance(
        &self,
        _definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        Ok(self.open_instance.lock().expect("open_instance").clone())
    }

    fn probe_change_since_last_sweep(
        &self,
        _definition: &AutoTaskDefinition,
        _precondition: &orbit_types::workflow::SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError> {
        match self.probe.lock().expect("probe").clone() {
            Some(Ok(probe)) => Ok(probe),
            Some(Err(error)) => Err(OrbitError::Store(error)),
            None => panic!("definition without skip_if_unchanged must not probe"),
        }
    }

    fn mint_task(&self, definition: &AutoTaskDefinition) -> Result<String, OrbitError> {
        if self.mint_should_fail.load(Ordering::SeqCst) {
            return Err(OrbitError::Execution("injected mint failure".to_string()));
        }
        let n = self.minted.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("ORB-{n:05}");
        self.mint_ids.lock().expect("ids").push(id.clone());
        self.mint_titles
            .lock()
            .expect("titles")
            .push(definition.template.title.clone());
        Ok(id)
    }
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

fn write_interval_definition(root: &std::path::Path, name: &str, dedupe: &str) {
    write_interval_revision(root, name, dedupe, true, &format!("Fixture {name}"))
        .expect("definition fixture");
}

fn write_interval_revision(
    root: &std::path::Path,
    name: &str,
    dedupe: &str,
    enabled: bool,
    title: &str,
) -> std::io::Result<()> {
    let definitions = auto_tasks_dir(root);
    fs::create_dir_all(&definitions)?;
    fs::write(
        definitions.join(format!("{name}.yaml")),
        format!(
            r#"schemaVersion: 1
name: {name}
enabled: {enabled}
schedule:
  every_minutes: 60
dedupe: {dedupe}
template:
  title: {title}
"#
        ),
    )
}

fn cursor(baseline: &str, last_slot: Option<&str>) -> AutoTaskCursor {
    AutoTaskCursor {
        baseline_at: baseline.to_string(),
        last_slot: last_slot.map(str::to_string),
        last_fired_at: last_slot.map(|_| "2026-01-01T01:00:05+00:00".to_string()),
        last_task_id: last_slot.map(|_| "ORB-00000".to_string()),
        pending: None,
        last_skip: None,
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
fn loaded_definition_deleted_before_admission_cannot_recreate_cursor() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    write_interval_definition(&definition_root, "removed", "always");
    write_interval_definition(&definition_root, "survivor", "always");
    let dispatch = TestDispatch::new(definition_root.clone(), state_dir);
    let t0 = at(2026, 1, 1, 0, 0);
    let state_path = cursor_state_path(&dispatch.state_dir);
    for name in ["removed", "survivor"] {
        upsert_cursor(&state_path, name, cursor(&t0.to_rfc3339(), None)).expect("seed cursor");
    }
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        let loaded_for_pass = Arc::clone(&loaded);
        let resume_for_pass = Arc::clone(&resume);
        let pass = scope.spawn(|| {
            set_after_load_barriers(Some((loaded_for_pass, resume_for_pass)));
            let outcome = run_auto_task_scheduler_at(
                &dispatch,
                t0 + Duration::minutes(65),
                SchedulerOptions::default(),
            );
            set_after_load_barriers(None);
            outcome.expect("scheduler pass")
        });
        loaded.wait();
        with_cursor_lock(&state_path, |session| {
            fs::remove_file(auto_tasks_dir(&definition_root).join("removed.yaml"))
                .expect("delete loaded definition");
            assert!(session.state.definitions.remove("removed").is_some());
            session.save()
        })
        .expect("delete cursor under admission lock");
        resume.wait();

        let outcome = pass.join().expect("scheduler thread");
        assert_eq!(outcome.reports.len(), 2, "both definitions were loaded");
        assert_eq!(outcome.reports[0].name, "removed");
        assert_eq!(outcome.reports[0].action, "skipped");
        assert_eq!(
            outcome.reports[0].reason.as_deref(),
            Some("definition_removed")
        );
        assert_eq!(outcome.reports[1].name, "survivor");
        assert_eq!(outcome.reports[1].action, "fired");
        assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
        let state = load_cursor_state(&state_path).expect("cursor state");
        assert!(!state.definitions.contains_key("removed"));
        assert_eq!(
            state.definitions["survivor"].last_task_id.as_deref(),
            Some("ORB-00001")
        );
    });
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
    assert_eq!(interrupted.reports[0].action, "error");
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
    assert_eq!(interrupted.reports[0].action, "error");
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
