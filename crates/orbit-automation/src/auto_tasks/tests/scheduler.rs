use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_store::compose::auto_task::{
    cursor_lock_path, cursor_state_path, load_cursor_state, upsert_cursor, with_cursor_lock,
};
use orbit_types::workflow::automation::AutomationDiagnostic;
use orbit_types::workflow::{
    AutoTaskCursor, AutoTaskDefinition, AutoTaskPendingClaim, DedupePolicy,
};
use tempfile::tempdir;

use crate::auto_tasks::loader::auto_tasks_dir;
use crate::auto_tasks::scheduler::{
    AutoTaskDispatch, ChangeProbe, SchedulerFault, SchedulerOptions, UNCHANGED_SINCE_LAST_SWEEP,
    inject_scheduler_fault, run_auto_task_scheduler_at, set_admission_overlap_barrier,
    set_after_load_barriers,
};

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

    fn set_probe(&self, probe: Result<ChangeProbe, String>) {
        *self.probe.lock().expect("probe") = Some(probe);
    }

    fn set_open_instance(&self, blocking_task_id: &str) {
        *self.open_instance.lock().expect("open_instance") = Some(blocking_task_id.to_string());
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

fn write_delivery_definition(root: &std::path::Path, name: &str) -> PathBuf {
    let definitions = auto_tasks_dir(root);
    fs::create_dir_all(&definitions).expect("definitions dir");
    let path = definitions.join(format!("{name}.yaml"));
    fs::write(
        &path,
        format!(
            r#"schemaVersion: 1
name: {name}
schedule:
  deliveries_landed:
    branch: agent-main
    threshold: 1
    max_wait_minutes: 60
    coverage: integrated_qa_v1
template:
  title: Delivery fixture
"#
        ),
    )
    .expect("delivery definition");
    path
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

/// A due definition carrying the opt-in `skip_if_unchanged` precondition.
fn unchanged_fixture(
    probe: Result<ChangeProbe, String>,
) -> (tempfile::TempDir, TestDispatch, DateTime<Utc>) {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let state_dir = root.path().join("state");
    let definitions = auto_tasks_dir(&definition_root);
    fs::create_dir_all(&definitions).expect("auto-task definitions directory");
    fs::write(
        definitions.join("chore.yaml"),
        r#"schemaVersion: 1
name: chore
schedule:
  every_minutes: 60
dedupe: skip_if_open
skip_if_unchanged:
  ref: agent-main
  cursor:
    tags:
    - code-review
    - no-diff-expected
template:
  title: Fixture chore
"#,
    )
    .expect("definition fixture");
    let dispatch = TestDispatch::new(definition_root, state_dir);
    dispatch.set_probe(probe);
    let t0 = at(2026, 1, 1, 0, 0);
    upsert_cursor(
        &cursor_state_path(&dispatch.state_dir),
        "chore",
        cursor(&t0.to_rfc3339(), None),
    )
    .expect("baseline cursor");
    (root, dispatch, t0)
}

fn unchanged(tip: &str, cursor_sha: &str) -> Result<ChangeProbe, String> {
    Ok(ChangeProbe::Unchanged {
        cursor: cursor_sha.to_string(),
        tip: tip.to_string(),
        cursor_task_id: Some("ORB-12696".to_string()),
    })
}

#[test]
fn unchanged_tip_skips_the_mint_and_records_both_shas() {
    let (_root, dispatch, t0) = unchanged_fixture(unchanged("58779d949b23", "58779d949b23"));

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");

    assert_eq!(outcome.reports[0].action, "skipped");
    let reason = outcome.reports[0].reason.as_deref().expect("reason");
    assert!(reason.contains(UNCHANGED_SINCE_LAST_SWEEP), "{reason}");
    assert!(reason.contains("58779d949b23"), "{reason}");
    assert!(reason.contains("ORB-12696"), "{reason}");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);

    // The slot stays unconsumed, so the first commit past the cursor fires it.
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    let stored = &state.definitions["chore"];
    assert!(stored.last_slot.is_none());
    assert!(stored.pending.is_none());
    let skip = stored.last_skip.as_ref().expect("recorded skip");
    assert_eq!(skip.reason, UNCHANGED_SINCE_LAST_SWEEP);
    assert_eq!(skip.reference, "agent-main");
    assert_eq!(skip.cursor_sha, "58779d949b23");
    assert_eq!(skip.tip_sha, "58779d949b23");
    assert_eq!(skip.cursor_task_id.as_deref(), Some("ORB-12696"));
    assert_eq!(skip.slot, outcome.reports[0].slot.clone().expect("slot"));
}

#[test]
fn advanced_tip_mints_exactly_as_before_and_clears_the_recorded_skip() {
    let (_root, dispatch, t0) = unchanged_fixture(unchanged("58779d949b23", "58779d949b23"));
    run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("quiet pass");

    dispatch.set_probe(Ok(ChangeProbe::Changed {
        cursor: "58779d949b23".to_string(),
        tip: "aa11bb22cc33".to_string(),
    }));
    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(125),
        SchedulerOptions::default(),
    )
    .expect("pass");

    assert_eq!(outcome.reports[0].action, "fired");
    assert_eq!(outcome.reports[0].task_id.as_deref(), Some("ORB-00001"));
    assert_eq!(outcome.reports[0].reason, None);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    let stored = &state.definitions["chore"];
    assert!(stored.last_slot.is_some());
    assert!(stored.last_skip.is_none());
}

#[test]
fn unresolvable_cursor_fails_open_and_says_why() {
    let (_root, dispatch, t0) = unchanged_fixture(Ok(ChangeProbe::Unknown {
        reason: "completed sweep ORB-12696 recorded no sweep-cursor.json".to_string(),
    }));

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");

    assert_eq!(outcome.reports[0].action, "fired");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
    let reason = outcome.reports[0].reason.as_deref().expect("reason");
    assert!(reason.contains("recorded no sweep-cursor.json"), "{reason}");
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_skip.is_none());
}

#[test]
fn probe_failure_fails_open_and_mints() {
    let (_root, dispatch, t0) =
        unchanged_fixture(Err("git rev-parse refs/heads/agent-main failed".to_string()));

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("pass");

    assert_eq!(outcome.reports[0].action, "fired");
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 1);
    let reason = outcome.reports[0].reason.as_deref().expect("reason");
    assert!(reason.contains("probe failed"), "{reason}");
    assert!(reason.contains("rev-parse"), "{reason}");
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_slot.is_some());
}

#[test]
fn dry_run_reports_the_precondition_skip_without_writing() {
    let (_root, dispatch, t0) = unchanged_fixture(unchanged("58779d949b23", "58779d949b23"));

    let outcome = run_auto_task_scheduler_at(
        &dispatch,
        t0 + Duration::minutes(65),
        SchedulerOptions { dry_run: true },
    )
    .expect("dry run");

    assert_eq!(outcome.reports[0].action, "skipped");
    assert!(
        outcome.reports[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains(UNCHANGED_SINCE_LAST_SWEEP)),
        "{:?}",
        outcome.reports[0]
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("load");
    assert!(state.definitions["chore"].last_skip.is_none());
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
fn loaded_delivery_deleted_before_admission_is_not_evaluated() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let path = write_delivery_definition(&definition_root, "delivery");
    let dispatch = TestDispatch::new(definition_root, root.path().join("state"));
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));

    thread::scope(|scope| {
        let loaded_for_pass = Arc::clone(&loaded);
        let resume_for_pass = Arc::clone(&resume);
        let pass = scope.spawn(|| {
            set_after_load_barriers(Some((loaded_for_pass, resume_for_pass)));
            let outcome = run_auto_task_scheduler_at(
                &dispatch,
                at(2026, 1, 1, 0, 0),
                SchedulerOptions::default(),
            );
            set_after_load_barriers(None);
            outcome.expect("scheduler pass")
        });
        loaded.wait();
        with_cursor_lock(&cursor_state_path(&dispatch.state_dir), |_session| {
            fs::remove_file(&path).expect("delete loaded delivery");
            Ok(())
        })
        .expect("delete under admission lock");
        resume.wait();

        let outcome = pass.join().expect("scheduler thread");
        assert_eq!(outcome.reports.len(), 1, "delivery definition was loaded");
        assert_eq!(outcome.reports[0].action, "skipped");
        assert_eq!(
            outcome.reports[0].reason.as_deref(),
            Some("definition_removed")
        );
        assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
        assert_eq!(dispatch.delivery_evaluations.load(Ordering::SeqCst), 0);
    });
}

/// Run one live pass that pauses after discovery, commit `edit` under the
/// admission lock the way CRUD does, then let the pass resume admission.
fn pass_with_edit_after_load(
    dispatch: &TestDispatch,
    now: DateTime<Utc>,
    edit: impl FnOnce() -> std::io::Result<()>,
) -> crate::auto_tasks::scheduler::AutoTaskSchedulerOutcome {
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    thread::scope(|scope| {
        let loaded_for_pass = Arc::clone(&loaded);
        let resume_for_pass = Arc::clone(&resume);
        let pass = scope.spawn(move || {
            set_after_load_barriers(Some((loaded_for_pass, resume_for_pass)));
            let outcome = run_auto_task_scheduler_at(dispatch, now, SchedulerOptions::default());
            set_after_load_barriers(None);
            outcome.expect("scheduler pass")
        });
        loaded.wait();
        let edited = with_cursor_lock(&cursor_state_path(&dispatch.state_dir), |_session| {
            edit().map_err(|error| OrbitError::Io(error.to_string()))
        });
        // Release the pass before checking the edit, so a failed edit
        // reports instead of stranding the barrier.
        resume.wait();
        let outcome = pass.join().expect("scheduler thread");
        edited.expect("edit under admission lock");
        outcome
    })
}

#[test]
fn loaded_definition_disabled_before_admission_is_not_minted() {
    let (_root, dispatch, t0) = due_fixture("chore");
    let now = t0 + Duration::minutes(65);
    let state_path = cursor_state_path(&dispatch.state_dir);
    let cursor_before = fs::read(&state_path).expect("cursor bytes");

    let outcome = pass_with_edit_after_load(&dispatch, now, || {
        write_interval_revision(
            &dispatch.definition_root,
            "chore",
            "always",
            false,
            "Fixture chore",
        )
    });

    assert_eq!(outcome.reports.len(), 1, "the enabled revision was loaded");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(
        outcome.reports[0].reason.as_deref(),
        Some("definition_changed")
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
    assert_eq!(
        fs::read(&state_path).expect("cursor bytes"),
        cursor_before,
        "a stale revision must not claim or consume the slot"
    );

    let next =
        run_auto_task_scheduler_at(&dispatch, now, SchedulerOptions::default()).expect("next pass");
    assert_eq!(next.reports[0].reason.as_deref(), Some("disabled"));
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
}

#[test]
fn loaded_template_edited_before_admission_mints_only_the_new_revision() {
    let (_root, dispatch, t0) = due_fixture("chore");
    let now = t0 + Duration::minutes(65);

    let outcome = pass_with_edit_after_load(&dispatch, now, || {
        write_interval_revision(
            &dispatch.definition_root,
            "chore",
            "always",
            true,
            "Edited chore",
        )
    });

    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(
        outcome.reports[0].reason.as_deref(),
        Some("definition_changed")
    );
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);

    let next =
        run_auto_task_scheduler_at(&dispatch, now, SchedulerOptions::default()).expect("next pass");
    assert_eq!(next.reports[0].action, "fired");
    assert_eq!(
        *dispatch.mint_titles.lock().expect("titles"),
        vec!["Edited chore".to_string()],
        "the slot is minted from the committed revision only"
    );
    let state = load_cursor_state(&cursor_state_path(&dispatch.state_dir)).expect("cursor");
    assert_eq!(
        state.definitions["chore"].last_task_id.as_deref(),
        Some("ORB-00001")
    );
}

#[test]
fn loaded_delivery_disabled_before_admission_is_not_evaluated() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    let path = write_delivery_definition(&definition_root, "delivery");
    let dispatch = TestDispatch::new(definition_root, root.path().join("state"));

    let outcome = pass_with_edit_after_load(&dispatch, at(2026, 1, 1, 0, 0), || {
        let enabled = fs::read_to_string(&path)?;
        fs::write(
            &path,
            enabled.replace("name: delivery\n", "name: delivery\nenabled: false\n"),
        )
    });

    assert_eq!(outcome.reports.len(), 1, "delivery definition was loaded");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(
        outcome.reports[0].reason.as_deref(),
        Some("definition_changed")
    );
    assert_eq!(dispatch.delivery_evaluations.load(Ordering::SeqCst), 0);
    assert_eq!(dispatch.minted.load(Ordering::SeqCst), 0);
}

#[test]
fn live_delivery_ignores_unrelated_malformed_cursor_state() {
    let root = tempdir().expect("temporary root");
    let definition_root = root.path().join("definitions");
    write_delivery_definition(&definition_root, "delivery");
    let dispatch = TestDispatch::new(definition_root, root.path().join("state"));
    dispatch.delivery_enabled.store(true, Ordering::SeqCst);
    fs::create_dir_all(&dispatch.state_dir).expect("state dir");
    let state_path = cursor_state_path(&dispatch.state_dir);
    fs::write(&state_path, "{not json").expect("malformed cursor fixture");

    let outcome =
        run_auto_task_scheduler_at(&dispatch, at(2026, 1, 1, 0, 0), SchedulerOptions::default())
            .expect("delivery pass");
    assert_eq!(outcome.reports.len(), 1);
    assert_eq!(outcome.reports[0].action, "delivery");
    assert_eq!(dispatch.delivery_evaluations.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read_to_string(&state_path).expect("raw cursor"),
        "{not json"
    );
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
    assert!(!cursor_lock_path(&cursor_state_path(&dispatch.state_dir)).exists());

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
    dispatch.set_open_instance("ORB-90001");
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
    assert_eq!(
        outcome.reports[0].blocking_task_id.as_deref(),
        Some("ORB-90001")
    );
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
    assert_eq!(outcome.reports[0].action, "error");
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
    dispatch.set_open_instance("ORB-90002");
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
        outcome.reports[0].blocking_task_id.as_deref(),
        Some("ORB-90002")
    );
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
