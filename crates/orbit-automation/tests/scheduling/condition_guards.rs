//! `when:` guards on cron and interval triggers: a closed guard holds a due
//! slot under `guard_miss: wait` and consumes it under `skip`.

use super::{at, isolated};
use chrono::{DateTime, Utc};
use orbit_automation::auto_tasks::scheduler::{
    AutoTaskDispatch, BlockedCandidateProbe, ChangeProbe, SchedulerOptions,
    run_auto_task_scheduler_at,
};
use orbit_automation::condition::{GUARD_FALSE, GuardVerdict};
use orbit_automation::routines::loader::{
    LoadedRoutine, RoutineCatalogLookup, RoutineSource, collect_routines,
};
use orbit_automation::routines::sweep::{
    RoutineDispatch, RunOwnerLiveness, SweepOptions, run_sweep_core,
};
use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_types::workflow::{AutoTaskDefinition, JobRunState};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

fn verdict(open: bool) -> GuardVerdict {
    if open {
        GuardVerdict::Open
    } else {
        GuardVerdict::Closed {
            reason: GUARD_FALSE,
            detail: "`board.eligible.empty` is false".into(),
        }
    }
}

struct GuardedAutoHost {
    root: PathBuf,
    minted: Cell<usize>,
    open: Cell<bool>,
}

impl AutoTaskDispatch for GuardedAutoHost {
    fn evaluate_guard(&self, _: &AutoTaskDefinition, _: DateTime<Utc>) -> GuardVerdict {
        verdict(self.open.get())
    }
    fn definition_root(&self) -> PathBuf {
        self.root.clone()
    }
    fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    fn evaluate_delivery(
        &self,
        _: &AutoTaskDefinition,
        _: bool,
        _: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        panic!("guard fixture has no delivery trigger")
    }
    fn has_open_instance(&self, _: &AutoTaskDefinition) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }
    fn mint_task(&self, _: &AutoTaskDefinition) -> Result<String, OrbitError> {
        self.minted.set(self.minted.get() + 1);
        Ok(format!("fixture-task-{}", self.minted.get()))
    }
    fn probe_change_since_last_sweep(
        &self,
        _: &AutoTaskDefinition,
        _: &orbit_types::workflow::SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError> {
        panic!("guard fixture has no change precondition")
    }
    fn probe_blocked_candidates(
        &self,
        _: &AutoTaskDefinition,
        _: DateTime<Utc>,
    ) -> Result<BlockedCandidateProbe, OrbitError> {
        panic!("guard fixture has no blocked-candidate precondition")
    }
}

#[test]
fn a_closed_auto_task_guard_holds_the_slot_under_wait_and_consumes_it_under_skip() {
    if isolated(
        "condition_guards::a_closed_auto_task_guard_holds_the_slot_under_wait_and_consumes_it_under_skip",
    ) {
        return;
    }
    for (miss, minted_when_open, minted_next_slot) in [("wait", 1, 2), ("skip", 0, 1)] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("auto_tasks")).unwrap();
        std::fs::write(
            root.path().join("auto_tasks/guarded.yaml"),
            format!(
                "schemaVersion: 1\nname: guarded\nenabled: true\nschedule:\n  every_minutes: 60\n  when: board.eligible.empty\n  guard_miss: {miss}\ndedupe: always\ntemplate:\n  title: Guarded fixture\n"
            ),
        )
        .unwrap();
        let host = GuardedAutoHost {
            root: root.path().to_path_buf(),
            minted: Cell::new(0),
            open: Cell::new(false),
        };
        let tick = |now: &str| {
            let outcome =
                run_auto_task_scheduler_at(&host, at(now), SchedulerOptions::default()).unwrap();
            assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
            outcome.reports.into_iter().next().unwrap()
        };
        assert_eq!(tick("2026-10-30T00:00:00Z").action, "baselined");
        let held = tick("2026-10-30T01:05:00Z");
        assert_eq!(held.action, "skipped", "{miss}: {held:?}");
        assert!(
            held.reason.as_deref().unwrap().starts_with(GUARD_FALSE),
            "{miss}: {held:?}"
        );
        assert_eq!(host.minted.get(), 0);

        host.open.set(true);
        tick("2026-10-30T01:30:00Z");
        assert_eq!(
            host.minted.get(),
            minted_when_open,
            "{miss}: a waiting slot fires when the guard opens; a skipped one is gone"
        );
        tick("2026-10-30T02:05:00Z");
        assert_eq!(host.minted.get(), minted_next_slot, "{miss}");
    }
}

#[derive(Default)]
struct GuardedRoutineHost {
    submitted: RefCell<Vec<String>>,
    open: Cell<bool>,
}

impl RoutineDispatch for GuardedRoutineHost {
    fn evaluate_guard(&self, _: &LoadedRoutine, _: DateTime<Utc>) -> GuardVerdict {
        verdict(self.open.get())
    }
    fn submit(&self, _: &Path, _: &str, _: &str, slot: &str) -> Result<String, OrbitError> {
        let mut submitted = self.submitted.borrow_mut();
        submitted.push(slot.into());
        Ok(format!("fixture-run-{}", submitted.len()))
    }
    fn run_state(&self, _: &Path, _: &str) -> Option<JobRunState> {
        Some(JobRunState::Success)
    }
    fn run_owner_liveness(&self, _: &Path, _: &str) -> RunOwnerLiveness {
        RunOwnerLiveness::Alive
    }
}

#[test]
fn a_closed_routine_guard_holds_the_slot_under_wait_and_consumes_it_under_skip() {
    if isolated(
        "condition_guards::a_closed_routine_guard_holds_the_slot_under_wait_and_consumes_it_under_skip",
    ) {
        return;
    }
    for (miss, submitted_when_open) in [("wait", 1), ("skip", 0)] {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("{miss}/.orbit"));
        std::fs::create_dir_all(root.join("routines")).unwrap();
        std::fs::write(
            root.join("routines/guarded.yaml"),
            format!(
                "schemaVersion: 1\nname: guarded\nenabled: true\ntrigger:\n  cron: '0 * * * *'\n  when: board.eligible.empty\n  guard_miss: {miss}\ntarget: job:fixture\npolicy:\n  overlap: forbid\n  timeout_minutes: 30\n"
            ),
        )
        .unwrap();
        let collection = collect_routines(
            &[RoutineSource {
                workspace: "fixture".into(),
                orbit_dir: root.clone(),
            }],
            &|_, _| RoutineCatalogLookup {
                resolves: true,
                error: None,
            },
        );
        assert!(collection.errors.is_empty(), "{:?}", collection.errors);
        let store = Store::open(&root.join("routine.db")).unwrap();
        let host = GuardedRoutineHost::default();
        let tick = |now: &str| {
            run_sweep_core(&store, &collection, &host, SweepOptions::default(), at(now))
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
        };
        assert_eq!(tick("2026-10-30T00:00:30Z").action, "baselined");
        let held = tick("2026-10-30T01:00:20Z");
        assert!(
            held.reason.as_deref().unwrap().starts_with(GUARD_FALSE),
            "{miss}: {held:?}"
        );
        // Past the slot's natural grace the waiting slot is still due.
        tick("2026-10-30T01:20:00Z");
        assert!(host.submitted.borrow().is_empty());

        host.open.set(true);
        tick("2026-10-30T01:40:00Z");
        assert_eq!(
            host.submitted.borrow().len(),
            submitted_when_open,
            "{miss}: {:?}",
            host.submitted.borrow()
        );
        if submitted_when_open == 1 {
            assert_eq!(host.submitted.borrow()[0], "2026-10-30T01:00:00+00:00");
        }
        tick("2026-10-30T02:00:20Z");
        assert_eq!(
            host.submitted.borrow().len(),
            submitted_when_open + 1,
            "{miss}: the next slot fires while the guard is open"
        );
    }
}
