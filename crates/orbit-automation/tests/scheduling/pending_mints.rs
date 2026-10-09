//! Pending-mint recovery through the scheduler boundary, including dry runs.

use super::{AutoHost, at, isolated};
use orbit_automation::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_store::compose::auto_task::{cursor_state_path, load_cursor_state, upsert_cursor};
use orbit_types::workflow::{AutoTaskCursor, AutoTaskPendingClaim};
use std::cell::{Cell, RefCell};
use std::fs;

#[test]
fn dry_run_matches_pending_recovery_before_enabled_check_without_writes() {
    if isolated(
        "pending_mints::dry_run_matches_pending_recovery_before_enabled_check_without_writes",
    ) {
        return;
    }
    let baseline = at("2026-10-09T00:00:00Z").to_rfc3339();
    let slot = at("2026-10-09T01:00:00Z").to_rfc3339();
    let now = at("2026-10-09T03:05:00Z");

    for enabled in [true, false] {
        for task_id in [Some("fixture-recorded-mint"), None] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("auto_tasks")).unwrap();
            fs::write(
                root.path().join("auto_tasks/pending.yaml"),
                format!(
                    "schemaVersion: 1\nname: pending\nenabled: {enabled}\nschedule:\n  every_minutes: 60\ndedupe: always\ntemplate:\n  title: Pending mint fixture\n"
                ),
            )
            .unwrap();
            let host = AutoHost {
                root: root.path().to_path_buf(),
                minted: Cell::new(0),
                skip_version: RefCell::new(None),
            };
            let state_path = cursor_state_path(&host.root.join("state"));
            let cursor = AutoTaskCursor {
                baseline_at: baseline.clone(),
                last_slot: None,
                last_fired_at: None,
                last_task_id: None,
                pending: Some(AutoTaskPendingClaim {
                    slot: slot.clone(),
                    task_id: task_id.map(str::to_string),
                }),
                last_skip: None,
            };
            upsert_cursor(&state_path, "pending", cursor.clone()).unwrap();
            let before = fs::read(&state_path).unwrap();

            let preview =
                run_auto_task_scheduler_at(&host, now, SchedulerOptions { dry_run: true }).unwrap();
            assert!(preview.errors.is_empty(), "{:?}", preview.errors);
            assert_eq!(preview.reports.len(), 1);
            assert_eq!(
                fs::read(&state_path).unwrap(),
                before,
                "dry-run recovery must leave auto-tasks.json byte-identical"
            );
            assert_eq!(host.minted.get(), 0, "dry run must never mint");
            let report = &preview.reports[0];
            assert_eq!(report.slot.as_deref(), Some(slot.as_str()));
            assert_eq!(report.task_id.as_deref(), task_id);
            assert_eq!(
                report.action,
                if task_id.is_some() {
                    "fired"
                } else {
                    "skipped"
                }
            );
            assert!(
                report.reason.is_some(),
                "pending recovery must explain its outcome"
            );
            assert_eq!(
                report
                    .reason
                    .as_deref()
                    .unwrap()
                    .starts_with("unresolved_pending:"),
                task_id.is_none(),
                "only a claim without mint evidence is unresolved"
            );

            let live = run_auto_task_scheduler_at(&host, now, SchedulerOptions::default()).unwrap();
            assert!(live.errors.is_empty(), "{:?}", live.errors);
            assert_eq!(
                preview.reports, live.reports,
                "pending recovery must agree for enabled={enabled}, task_id={task_id:?}"
            );
            assert_eq!(host.minted.get(), 0, "recovery must never remint");
            let state = load_cursor_state(&state_path).unwrap();
            let recovered = &state.definitions["pending"];
            if task_id.is_some() {
                assert!(recovered.pending.is_none());
                assert_eq!(recovered.last_slot.as_deref(), Some(slot.as_str()));
                assert_eq!(recovered.last_task_id.as_deref(), task_id);
                assert_eq!(recovered.last_fired_at, Some(now.to_rfc3339()));
            } else {
                assert_eq!(
                    recovered, &cursor,
                    "unresolved recovery must retain the claim"
                );
                assert_eq!(fs::read(&state_path).unwrap(), before);
            }
        }
    }
}
