//! Scheduler-pass tests [ORB-10149]: baseline-on-first-sight, provenance,
//! catch-up collapse, dedupe, disabled skip, dry-run inertness, and admission
//! against edits committed after discovery.

use std::sync::{Arc, Barrier, mpsc};
use std::thread;

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_automation::auto_tasks::scheduler as automation;
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, auto_task_tag};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::application::auto_tasks::crud::AutoTaskUpdateParams;
use crate::application::auto_tasks::cursor_state_path;
use crate::application::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};
use crate::application::task::TaskUpdateParams;

use super::{PausedDispatch, interval_params, seed_cursor, template};

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

fn fire(runtime: &OrbitRuntime, now: DateTime<Utc>) -> Vec<(String, Option<String>)> {
    let outcome = run_auto_task_scheduler_at(runtime, now, SchedulerOptions::default())
        .expect("scheduler pass");
    outcome
        .reports
        .iter()
        .map(|report| (report.action.to_string(), report.task_id.clone()))
        .collect()
}

#[test]
fn first_observation_baselines_without_firing() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    let reports = fire(&runtime, t0);
    assert_eq!(reports, vec![("baselined".to_string(), None)]);
    assert!(runtime.list_tasks().expect("tasks").is_empty());
}

#[test]
fn fires_and_stamps_provenance() {
    let runtime = runtime();
    let mut params = interval_params("chore", 60);
    params.template.required_tools = vec![
        "github.run.list".to_string(),
        "github.auth.status".to_string(),
        "github.run.list".to_string(),
    ];
    runtime.auto_task_add(params).expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let reports = fire(&runtime, t0 + Duration::minutes(65));
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].0, "fired");
    let task_id = reports[0].1.clone().expect("task id");

    let task = runtime.get_task(&task_id).expect("get task");
    assert_eq!(task.status, TaskStatus::Backlog);
    assert!(
        task.tags.contains(&auto_task_tag("chore")),
        "expected provenance tag, got {:?}",
        task.tags
    );
    assert_eq!(
        task.required_tools,
        vec!["github.auth.status", "github.run.list"]
    );
}

/// Claim is persisted before mint, so a state directory that cannot accept
/// atomic replacement must fail closed: no task, baseline cursor left intact.
#[cfg(unix)]
#[test]
fn unwritable_cursor_directory_does_not_mint_or_rewrite_baseline() {
    use std::os::unix::fs::PermissionsExt;

    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);
    fire(&runtime, t0); // baseline writes the cursor file

    let state_path = cursor_state_path(&runtime.paths().state_dir);
    let before = std::fs::read_to_string(&state_path).expect("baseline cursor");
    let state_dir = runtime.paths().state_dir.clone();
    let writable = std::fs::metadata(&state_dir)
        .expect("state dir")
        .permissions();
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o555))
        .expect("make cursor directory read-only so atomic replace cannot create a temp file");

    let outcome = run_auto_task_scheduler_at(
        &runtime,
        t0 + Duration::minutes(65),
        SchedulerOptions::default(),
    )
    .expect("scheduler pass");
    std::fs::set_permissions(&state_dir, writable).expect("restore permissions");

    assert_eq!(outcome.reports.len(), 1);
    let report = &outcome.reports[0];
    assert_eq!(report.action, "error", "{report:?}");
    assert!(
        report
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("write") || reason.contains("Permission")),
        "{report:?}"
    );
    assert!(runtime.list_tasks().expect("tasks").is_empty());
    assert_eq!(
        std::fs::read_to_string(&state_path).expect("cursor after denied write"),
        before
    );
}

#[test]
fn catch_up_collapses_downtime_to_one_task() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    // Six hours of downtime: a single make-up task, not six.
    let reports = fire(&runtime, t0 + Duration::minutes(370));
    assert_eq!(reports.iter().filter(|(a, _)| a == "fired").count(), 1);
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);
}

#[test]
fn skip_if_open_never_files_a_second_open_instance() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let first = fire(&runtime, t0 + Duration::minutes(60));
    assert_eq!(first[0].0, "fired");
    let task_id = first[0].1.clone().expect("task id");

    // Prior instance still open → skip.
    let blocked = fire(&runtime, t0 + Duration::minutes(180));
    assert_eq!(blocked[0].0, "skipped");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);

    // Close the instance; the pending occurrence now fires (once).
    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Rejected),
                ..Default::default()
            },
        )
        .expect("close task");
    let drained = fire(&runtime, t0 + Duration::minutes(240));
    assert_eq!(drained[0].0, "fired");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

/// A `someday`-parked instance is an explicit "not now", not an active
/// instance: it must not block every later mint of the auto-task [ORB-12148].
#[test]
fn skip_if_open_ignores_a_someday_instance() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let first = fire(&runtime, t0 + Duration::minutes(60));
    assert_eq!(first[0].0, "fired");
    let task_id = first[0].1.clone().expect("task id");

    let definition = runtime
        .auto_task_show("chore")
        .expect("show")
        .expect("chore");
    assert_eq!(
        runtime
            .open_auto_task_instance(&definition)
            .expect("open check"),
        Some(task_id.clone())
    );

    runtime
        .update_task(
            &task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Someday),
                ..Default::default()
            },
        )
        .expect("park task");

    // The someday-parked instance does not count as open, so open_auto_task_instance
    // returns None and the next due slot fires and mints a second task.
    assert_eq!(
        runtime
            .open_auto_task_instance(&definition)
            .expect("open check"),
        None
    );
    let second = fire(&runtime, t0 + Duration::minutes(180));
    assert_eq!(second[0].0, "fired");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

#[test]
fn dedupe_open_report_names_the_blocking_task() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    let first = fire(&runtime, t0 + Duration::minutes(60));
    let task_id = first[0].1.clone().expect("task id");

    let outcome = run_auto_task_scheduler_at(
        &runtime,
        t0 + Duration::minutes(180),
        SchedulerOptions::default(),
    )
    .expect("scheduler pass");
    assert_eq!(outcome.reports[0].action, "skipped");
    assert_eq!(
        outcome.reports[0].reason.as_deref(),
        Some("dedupe_open"),
        "{:?}",
        outcome.reports[0]
    );
    assert_eq!(
        outcome.reports[0].blocking_task_id.as_deref(),
        Some(task_id.as_str())
    );
}

#[test]
fn weekly_cron_fires_once_and_dedupes_while_audit_is_open() {
    let runtime = runtime();
    let mut params = interval_params("model-price-audit", 60);
    params.schedule = AutoTaskSchedule::Cron {
        cron: "0 6 * * 1".to_string(),
    };
    runtime.auto_task_add(params).expect("add");
    let first_monday = at(2026, 1, 5, 6, 0);

    assert_eq!(fire(&runtime, first_monday)[0].0, "baselined");
    assert_eq!(fire(&runtime, at(2026, 1, 12, 6, 1))[0].0, "fired");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);

    // The following weekly slot is deferred rather than duplicated while the
    // report task remains open.
    assert_eq!(fire(&runtime, at(2026, 1, 19, 6, 1))[0].0, "skipped");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 1);
}

#[test]
fn always_dedupe_files_even_with_an_open_instance() {
    let runtime = runtime();
    let mut params = interval_params("chore", 60);
    params.dedupe = orbit_types::workflow::DedupePolicy::Always;
    runtime.auto_task_add(params).expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    fire(&runtime, t0); // baseline
    fire(&runtime, t0 + Duration::minutes(60)); // fired, open
    let again = fire(&runtime, t0 + Duration::minutes(180)); // fires again
    assert_eq!(again[0].0, "fired");
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

#[test]
fn disabled_definitions_are_skipped() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    runtime
        .auto_task_toggle("chore", false)
        .expect("toggle off");
    let t0 = at(2026, 1, 1, 0, 0);

    let reports = fire(&runtime, t0 + Duration::minutes(120));
    assert_eq!(reports[0].0, "skipped");
    assert!(runtime.list_tasks().expect("tasks").is_empty());
}

#[test]
fn dry_run_creates_nothing_and_persists_no_cursor() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    let t0 = at(2026, 1, 1, 0, 0);

    let outcome = run_auto_task_scheduler_at(&runtime, t0, SchedulerOptions { dry_run: true })
        .expect("dry run");
    assert_eq!(outcome.reports[0].action, "would_baseline");
    assert!(runtime.list_tasks().expect("tasks").is_empty());

    // Cursor was not persisted: a second dry run still sees a fresh definition.
    let again = run_auto_task_scheduler_at(&runtime, t0, SchedulerOptions { dry_run: true })
        .expect("dry run 2");
    assert_eq!(again.reports[0].action, "would_baseline");
}

#[test]
fn linked_worktree_scheduler_reads_local_definition_and_writes_shared_cursor() {
    let root = tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let primary_orbit = root.path().join("primary/.orbit");
    let worktree_orbit = root.path().join("worktree/.orbit");
    for path in [&global_root, &primary_orbit, &worktree_orbit] {
        std::fs::create_dir_all(path).expect("runtime root");
    }
    let runtime = OrbitRuntime::from_resolved_roots(&global_root, &primary_orbit, &worktree_orbit)
        .expect("two-root runtime");
    runtime
        .auto_task_add(interval_params("local-chore", 60))
        .expect("local definition");

    let outcome =
        run_auto_task_scheduler_at(&runtime, at(2026, 1, 1, 0, 0), SchedulerOptions::default())
            .expect("scheduler pass");

    assert_eq!(outcome.reports[0].name, "local-chore");
    assert!(
        primary_orbit.join("state/auto-tasks.json").is_file(),
        "cursor state remains shared"
    );
    assert!(
        !worktree_orbit.join("state/auto-tasks.json").exists(),
        "linked worktree must not fork host-local cursor state"
    );
    assert!(
        !primary_orbit.join("auto_tasks/local-chore.yaml").exists(),
        "scheduler/CRUD must not materialize tracked definitions in primary"
    );
}

/// Pause a live pass after discovery, run `edit` to completion, then let the
/// pass resume admission. The edit's result is checked only after the pass is
/// released, so a failed edit reports instead of stranding the barrier.
fn pass_with_edit_after_load(
    runtime: &OrbitRuntime,
    edit: impl FnOnce() -> Result<AutoTaskDefinition, OrbitError>,
) -> Vec<crate::application::auto_tasks::AutoTaskFireReport> {
    let loaded = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    let dispatch = PausedDispatch {
        runtime,
        mint: None,
        admission: Some((Arc::clone(&loaded), Arc::clone(&resume))),
    };
    thread::scope(|scope| {
        let pass = scope.spawn(|| {
            automation::run_auto_task_scheduler_at(
                &dispatch,
                Utc::now() + Duration::minutes(65),
                SchedulerOptions::default(),
            )
            .expect("scheduler pass")
        });
        loaded.wait(); // The pass holds the enabled, original revision.
        let edited = edit();
        resume.wait();
        let reports = pass.join().expect("scheduler thread").reports;
        edited.expect("edit completes while the pass is paused");
        reports
    })
}

#[test]
fn disable_completed_after_load_admits_nothing_from_the_stale_snapshot() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    seed_cursor(&runtime, "chore");

    let reports = pass_with_edit_after_load(&runtime, || runtime.auto_task_toggle("chore", false));

    assert_eq!(reports.len(), 1, "the enabled revision was loaded");
    assert_eq!(reports[0].action, "skipped");
    assert_eq!(reports[0].reason.as_deref(), Some("definition_changed"));
    assert!(runtime.list_tasks().expect("tasks").is_empty());
    assert!(!runtime.auto_task_show("chore").unwrap().unwrap().enabled);
}

#[test]
fn template_edit_completed_after_load_mints_only_the_committed_revision() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    seed_cursor(&runtime, "chore");

    let reports = pass_with_edit_after_load(&runtime, || {
        runtime.auto_task_update(
            "chore",
            AutoTaskUpdateParams {
                template: Some(template("Edited chore")),
                ..Default::default()
            },
        )
    });

    assert_eq!(reports[0].reason.as_deref(), Some("definition_changed"));
    assert!(runtime.list_tasks().expect("tasks").is_empty());

    let next = fire(&runtime, Utc::now() + Duration::minutes(65));
    assert_eq!(next[0].0, "fired");
    let tasks = runtime.list_tasks().expect("tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "[auto-task] Edited chore");
}

#[test]
fn edits_queued_behind_admission_keep_each_committed_change() {
    let runtime = runtime();
    runtime
        .auto_task_add(interval_params("chore", 60))
        .expect("add");
    seed_cursor(&runtime, "chore");
    let mint_reached = Arc::new(Barrier::new(2));
    let mint_resume = Arc::new(Barrier::new(2));
    let dispatch = PausedDispatch {
        runtime: &runtime,
        mint: Some((Arc::clone(&mint_reached), Arc::clone(&mint_resume))),
        admission: None,
    };

    thread::scope(|scope| {
        let pass = scope.spawn(|| {
            automation::run_auto_task_scheduler_at(
                &dispatch,
                Utc::now() + Duration::minutes(65),
                SchedulerOptions::default(),
            )
            .expect("scheduler pass")
        });
        mint_reached.wait(); // Admission holds the cursor lock at mint.
        let (sender, receiver) = mpsc::channel();
        let runtime_ref = &runtime;
        let toggle_sender = sender.clone();
        let toggle = scope.spawn(move || {
            let result = runtime_ref.auto_task_toggle("chore", false);
            let _ = toggle_sender.send(());
            result
        });
        let update = scope.spawn(move || {
            let result = runtime_ref.auto_task_update(
                "chore",
                AutoTaskUpdateParams {
                    template: Some(template("Edited chore")),
                    ..Default::default()
                },
            );
            let _ = sender.send(());
            result
        });
        let edited_during_admission = receiver
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_ok();
        mint_resume.wait();
        assert!(
            !edited_during_admission,
            "definition writes must wait for the admission holding the lock"
        );

        let outcome = pass.join().expect("scheduler thread");
        assert_eq!(outcome.reports[0].action, "fired");
        toggle.join().expect("toggle thread").expect("toggle");
        update.join().expect("update thread").expect("update");
    });

    let definition = runtime.auto_task_show("chore").unwrap().unwrap();
    assert!(
        !definition.enabled,
        "the toggle survived the concurrent edit"
    );
    assert_eq!(
        definition.template.title, "Edited chore",
        "the edit survived the concurrent toggle"
    );
    let tasks = runtime.list_tasks().expect("tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].title, "[auto-task] Chore for chore",
        "the pass that won admission minted the revision it validated"
    );
}
