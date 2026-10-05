#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! Scheduling and coverage guarantees through the public automation boundary.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as WaitDuration;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::{Europe::Berlin, Tz, UTC};
use orbit_automation::AutomationError;
use orbit_automation::auto_tasks::schedule::{
    AutoTaskDueDecision, decide_due, next_scheduled_slot, validate_schedule,
};
use orbit_automation::auto_tasks::scheduler::{
    AutoTaskDispatch, ChangeProbe, SchedulerOptions, run_auto_task_scheduler_at,
};
use orbit_automation::members::{
    MemberAdmission, MemberEvaluation, MemberHost, MemberOutcome, MemberPage, evaluate,
};
use orbit_automation::routines::due::{
    DueDecision, due_decision_with_grace, natural_slot_grace_for_cadence, next_occurrence,
    parse_cron, truncate_to_minute,
};
use orbit_automation::routines::loader::{RoutineCatalogLookup, RoutineSource, collect_routines};
use orbit_automation::routines::sweep::{
    RoutineDispatch, RunOwnerLiveness, SweepOptions, run_sweep_core,
};
use orbit_common::{OrbitError, process::run_bounded_capped, test_env};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::{
    AutomationDiagnostic, SourceRevision,
    members::{
        MemberAttempt, MemberBatchEvidence, MemberEvidence, StateMember, StateTrigger,
        StateTriggerKind,
    },
};
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, JobRunState, MAX_AUTO_TASK_INTERVAL_MINUTES,
    MissedRunPolicy,
};

#[path = "scheduling/failed_members.rs"]
mod failed_members;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

/// Re-exec only the requested test, without the enclosing managed run's authority.
/// The shared supervisor drains both pipes and kills/reaps the child on timeout.
fn isolated(test: &str) -> bool {
    if std::env::var("ORBIT_AUTOMATION_TEST_CHILD").as_deref() == Ok(test) {
        return false;
    }
    let fixture = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let home = fixture.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .current_dir(fixture.path())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("ORBIT_HOME")
        .env("TZ", "UTC")
        .env("ORBIT_AUTOMATION_TEST_CHILD", test);
    let output = run_bounded_capped(&mut command, WaitDuration::from_secs(30), 64 * 1024)
        .expect("isolated automation fixture completes within its deadline");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{test}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[test]
fn due_decisions_respect_dst_cadence_and_collapse_downtime() {
    struct Case {
        name: &'static str,
        zone: Tz,
        cron: &'static str,
        policy: MissedRunPolicy,
        cadence: u64,
        lower: &'static str,
        now: &'static str,
        fire: Option<(&'static str, bool)>,
    }
    use MissedRunPolicy::{CatchUpOnce, Skip};
    let cases = [
        Case {
            name: "delayed five-minute poll remains natural",
            zone: UTC,
            cron: "5 * * * *",
            policy: Skip,
            cadence: 300,
            lower: "2026-09-07T00:05:00Z",
            now: "2026-09-07T01:08:43Z",
            fire: Some(("2026-09-07T01:05:00Z", false)),
        },
        Case {
            name: "inclusive cadence grace",
            zone: UTC,
            cron: "5 * * * *",
            policy: Skip,
            cadence: 300,
            lower: "2026-09-07T00:05:00Z",
            now: "2026-09-07T01:15:00Z",
            fire: Some(("2026-09-07T01:05:00Z", false)),
        },
        Case {
            name: "real downtime skips",
            zone: UTC,
            cron: "5 * * * *",
            policy: Skip,
            cadence: 300,
            lower: "2026-09-07T00:05:00Z",
            now: "2026-09-07T01:15:01Z",
            fire: None,
        },
        Case {
            name: "real downtime catches up once",
            zone: UTC,
            cron: "5 * * * *",
            policy: CatchUpOnce,
            cadence: 300,
            lower: "2026-09-07T00:05:00Z",
            now: "2026-09-07T01:15:01Z",
            fire: Some(("2026-09-07T01:05:00Z", true)),
        },
        Case {
            name: "week collapses to latest cron slot",
            zone: UTC,
            cron: "0 22 * * *",
            policy: CatchUpOnce,
            cadence: 60,
            lower: "2026-07-01T22:00:00Z",
            now: "2026-07-08T09:30:00Z",
            fire: Some(("2026-07-07T22:00:00Z", true)),
        },
        Case {
            name: "consumed slot never refires",
            zone: UTC,
            cron: "0 22 * * *",
            policy: CatchUpOnce,
            cadence: 60,
            lower: "2026-07-07T22:00:00Z",
            now: "2026-07-08T09:30:00Z",
            fire: None,
        },
        Case {
            name: "fold first occurrence fires",
            zone: Berlin,
            cron: "30 2 * * *",
            policy: Skip,
            cadence: 60,
            lower: "2025-10-26T00:29:00Z",
            now: "2025-10-26T00:30:45Z",
            fire: Some(("2025-10-26T00:30:00Z", false)),
        },
        // Croner's daily fold policy chooses the first occurrence, so a second
        // wall-clock 02:30 must not fire the already-consumed daily schedule.
        Case {
            name: "fold second occurrence does not duplicate",
            zone: Berlin,
            cron: "30 2 * * *",
            policy: Skip,
            cadence: 60,
            lower: "2025-10-26T00:30:00Z",
            now: "2025-10-26T01:30:45Z",
            fire: None,
        },
        Case {
            // Croner's backward search resolves a fixed wall time in the gap
            // to the last real minute before it. Guard that real slot and its
            // consumption rather than constructing a nonexistent local time.
            name: "fixed spring gap resolves to a real preceding minute",
            zone: Berlin,
            cron: "30 2 * * *",
            policy: CatchUpOnce,
            cadence: 60,
            lower: "2025-03-29T01:30:00Z",
            now: "2025-03-30T01:30:45Z",
            fire: Some(("2025-03-30T00:59:00Z", true)),
        },
        Case {
            name: "spring gap crosses directly to the next real slot",
            zone: Berlin,
            cron: "*/30 * * * *",
            policy: CatchUpOnce,
            cadence: 60,
            lower: "2025-03-30T00:30:00Z",
            now: "2025-03-30T01:00:45Z",
            fire: Some(("2025-03-30T01:00:00Z", false)),
        },
        Case {
            name: "consumed spring boundary cannot invent a gap slot",
            zone: Berlin,
            cron: "*/30 * * * *",
            policy: CatchUpOnce,
            cadence: 60,
            lower: "2025-03-30T01:00:00Z",
            now: "2025-03-30T01:00:45Z",
            fire: None,
        },
        Case {
            name: "day after spring gap fires",
            zone: Berlin,
            cron: "30 2 * * *",
            policy: Skip,
            cadence: 60,
            lower: "2025-03-29T01:30:00Z",
            now: "2025-03-31T00:30:45Z",
            fire: Some(("2025-03-31T00:30:00Z", false)),
        },
    ];
    for case in cases {
        let cron = parse_cron(case.cron).unwrap();
        let lower = at(case.lower).with_timezone(&case.zone);
        let now = at(case.now).with_timezone(&case.zone);
        let actual = due_decision_with_grace(
            &cron,
            case.policy,
            &lower,
            &now,
            natural_slot_grace_for_cadence(case.cadence).unwrap(),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        let expected = case
            .fire
            .map_or(DueDecision::NotDue, |(slot, is_catch_up)| {
                DueDecision::Fire {
                    slot: at(slot).with_timezone(&case.zone),
                    is_catch_up,
                }
            });
        assert_eq!(actual, expected, "{}", case.name);
        if let DueDecision::Fire { slot, .. } = actual {
            assert_eq!(
                due_decision_with_grace(
                    &cron,
                    case.policy,
                    &slot,
                    &now,
                    natural_slot_grace_for_cadence(case.cadence).unwrap()
                )
                .unwrap(),
                DueDecision::NotDue,
                "{}: consuming the latest slot must collapse every older slot",
                case.name,
            );
        }
    }
    let cron = parse_cron("*/30 * * * *").unwrap();
    assert_eq!(
        next_occurrence(&cron, &at("2025-03-30T00:59:45Z").with_timezone(&Berlin))
            .unwrap()
            .with_timezone(&Utc),
        at("2025-03-30T01:00:00Z"),
        "the spring projection jumps from 01:59 to the real 03:00 slot",
    );
    for slot in ["2025-10-26T00:30:00Z", "2025-10-26T01:30:00Z"] {
        let slot = at(slot).with_timezone(&Berlin);
        assert_eq!(
            truncate_to_minute(slot + Duration::seconds(45)),
            slot,
            "fold slot retains its offset"
        );
    }
}

#[test]
fn interval_decisions_validate_ranges_and_preserve_the_anchor() {
    let baseline = at("2026-01-01T00:07:30Z");
    let schedule = AutoTaskSchedule::Interval { every_minutes: 60 };
    for (name, now, consumed, expected, next) in [
        (
            "before baseline",
            baseline - Duration::seconds(1),
            None,
            None,
            baseline + Duration::hours(1),
        ),
        (
            "before boundary",
            baseline + Duration::minutes(59),
            None,
            None,
            baseline + Duration::hours(1),
        ),
        (
            "inclusive boundary",
            baseline + Duration::hours(1),
            None,
            Some(baseline + Duration::hours(1)),
            baseline + Duration::hours(2),
        ),
        (
            "week collapse",
            baseline + Duration::days(7) + Duration::minutes(29),
            None,
            Some(baseline + Duration::days(7)),
            baseline + Duration::days(7) + Duration::hours(1),
        ),
        (
            "consumed latest boundary",
            baseline + Duration::days(7) + Duration::minutes(29),
            Some(baseline + Duration::days(7)),
            None,
            baseline + Duration::days(7) + Duration::hours(1),
        ),
    ] {
        assert_eq!(
            decide_due(&schedule, baseline, consumed, now).unwrap(),
            expected.map_or(AutoTaskDueDecision::NotDue, |slot| {
                AutoTaskDueDecision::Fire {
                    slot: slot.to_rfc3339(),
                }
            }),
            "{name}"
        );
        assert_eq!(
            next_scheduled_slot(&schedule, Some(baseline), now).unwrap(),
            Some(next),
            "{name}: projection stays strictly ahead of now"
        );
    }
    for minutes in [0, MAX_AUTO_TASK_INTERVAL_MINUTES + 1, u64::MAX] {
        let schedule = AutoTaskSchedule::Interval {
            every_minutes: minutes,
        };
        assert!(
            validate_schedule(&schedule).is_err(),
            "invalid interval {minutes} is rejected by validation"
        );
        assert!(
            decide_due(&schedule, baseline, None, baseline + Duration::days(7)).is_err(),
            "invalid interval {minutes} cannot reach due arithmetic"
        );
        assert!(
            next_scheduled_slot(&schedule, Some(baseline), baseline).is_err(),
            "invalid interval {minutes} cannot reach projection arithmetic"
        );
    }
    for minutes in [1, MAX_AUTO_TASK_INTERVAL_MINUTES] {
        let schedule = AutoTaskSchedule::Interval {
            every_minutes: minutes,
        };
        validate_schedule(&schedule).unwrap();
        let boundary = baseline + Duration::minutes(i64::try_from(minutes).unwrap());
        assert_eq!(
            decide_due(&schedule, baseline, None, boundary).unwrap(),
            AutoTaskDueDecision::Fire {
                slot: boundary.to_rfc3339()
            }
        );
    }
    assert!(
        next_scheduled_slot(
            &schedule,
            Some(DateTime::<Utc>::MAX_UTC),
            DateTime::<Utc>::MAX_UTC
        )
        .is_err(),
        "unrepresentable future boundary must fail without wrapping"
    );
}

#[derive(Default)]
struct RoutineHost {
    submitted: RefCell<Vec<String>>,
    states: RefCell<BTreeMap<String, JobRunState>>,
}

impl RoutineDispatch for RoutineHost {
    fn live_workspace_drain(&self, _: &Path) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }
    fn submit(&self, _: &Path, _: &str, _: &str, slot: &str) -> Result<String, OrbitError> {
        let mut submitted = self.submitted.borrow_mut();
        submitted.push(slot.into());
        Ok(format!("fixture-run-{}", submitted.len()))
    }
    fn run_state(&self, _: &Path, run: &str) -> Option<JobRunState> {
        self.states.borrow().get(run).copied()
    }
    fn run_owner_liveness(&self, _: &Path, _: &str) -> RunOwnerLiveness {
        RunOwnerLiveness::Alive
    }
}

struct AutoHost {
    root: PathBuf,
    minted: Cell<usize>,
}

impl AutoTaskDispatch for AutoHost {
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
    ) -> Result<AutomationDiagnostic, OrbitError> {
        panic!("time fixture cannot evaluate deliveries")
    }
    fn has_open_instance(&self, _: &AutoTaskDefinition) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }
    fn mint_task(&self, _: &AutoTaskDefinition) -> Result<String, OrbitError> {
        let count = self.minted.get() + 1;
        self.minted.set(count);
        Ok(format!("fixture-task-{count}"))
    }
    fn probe_change_since_last_sweep(
        &self,
        _: &AutoTaskDefinition,
        _: &orbit_types::workflow::SkipIfUnchanged,
    ) -> Result<ChangeProbe, OrbitError> {
        panic!("time fixture has no change precondition")
    }
}

#[test]
fn scheduler_ticks_collapse_catch_up_and_wait_for_terminal_overlap() {
    if isolated("scheduler_ticks_collapse_catch_up_and_wait_for_terminal_overlap") {
        return;
    }
    let root = std::env::current_dir().unwrap().join(".orbit");
    std::fs::create_dir_all(root.join("routines")).unwrap();
    std::fs::write(root.join("routines/overlap.yaml"), "schemaVersion: 1\nname: overlap\nenabled: true\ntrigger:\n  cron: '* * * * *'\n  missed_run: catch_up_once\ntarget: job:fixture\npolicy:\n  overlap: forbid\n  timeout_minutes: 30\n").unwrap();
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
    assert_eq!(collection.routines.len(), 1);
    let store = Store::open(&root.join("routine.db")).unwrap();
    let host = RoutineHost::default();
    for (now, terminal, action, count, slot) in [
        ("2026-10-03T00:00:30Z", false, "baselined", 0, None),
        (
            "2026-10-03T00:01:20Z",
            false,
            "fired",
            1,
            Some("2026-10-03T00:01:00Z"),
        ),
        (
            "2026-10-03T00:02:20Z",
            false,
            "skipped",
            1,
            Some("2026-10-03T00:02:00Z"),
        ),
        (
            "2026-10-03T00:03:20Z",
            false,
            "skipped",
            1,
            Some("2026-10-03T00:03:00Z"),
        ),
        (
            "2026-10-03T00:03:20Z",
            true,
            "fired",
            2,
            Some("2026-10-03T00:03:00Z"),
        ),
        ("2026-10-03T00:03:45Z", true, "skipped", 2, None),
    ] {
        host.states.borrow_mut().insert(
            "fixture-run-1".into(),
            if terminal {
                JobRunState::Success
            } else {
                JobRunState::Running
            },
        );
        let reports =
            run_sweep_core(&store, &collection, &host, SweepOptions::default(), at(now)).unwrap();
        assert_eq!(
            reports[0].action, action,
            "routine tick {now}, terminal={terminal}: {:?}",
            reports[0]
        );
        assert_eq!(
            reports[0].slot,
            slot.map(|s| at(s).to_rfc3339()),
            "routine tick {now}"
        );
        assert_eq!(
            host.submitted.borrow().len(),
            count,
            "overlap must wait, then submit only the latest slot once"
        );
        if !terminal && action == "skipped" {
            assert_eq!(reports[0].reason.as_deref(), Some("overlap_in_flight"));
        }
    }
    assert_eq!(store.routine_recent_fires("overlap", 20).unwrap().len(), 2);
    assert_eq!(
        store.routine_cursor("overlap").unwrap().unwrap().last_slot,
        Some(at("2026-10-03T00:03:00Z").to_rfc3339())
    );

    for (name, schedule) in [
        ("interval", "every_minutes: 60"),
        ("cron", "cron: '7 * * * *'"),
    ] {
        let root = root.join(name);
        std::fs::create_dir_all(root.join("auto_tasks")).unwrap();
        std::fs::write(root.join("auto_tasks/catchup.yaml"), format!("schemaVersion: 1\nname: catchup\nenabled: true\nschedule:\n  {schedule}\ndedupe: always\ntemplate:\n  title: Catch-up fixture\n")).unwrap();
        let host = AutoHost {
            root,
            minted: Cell::new(0),
        };
        for (now, action, count, slot) in [
            ("2026-10-01T00:07:00Z", "baselined", 0, None),
            (
                "2026-10-08T00:36:00Z",
                "fired",
                1,
                Some("2026-10-08T00:07:00Z"),
            ),
            ("2026-10-08T00:36:00Z", "skipped", 1, None),
            (
                "2026-10-08T01:07:00Z",
                "fired",
                2,
                Some("2026-10-08T01:07:00Z"),
            ),
        ] {
            let outcome =
                run_auto_task_scheduler_at(&host, at(now), SchedulerOptions::default()).unwrap();
            assert!(outcome.errors.is_empty(), "{name}: {:?}", outcome.errors);
            assert_eq!(outcome.reports.len(), 1);
            assert_eq!(
                outcome.reports[0].action, action,
                "{name} tick {now}: {:?}",
                outcome.reports[0]
            );
            assert_eq!(
                outcome.reports[0].slot,
                slot.map(|s| at(s).to_rfc3339()),
                "{name} tick {now}"
            );
            assert_eq!(
                host.minted.get(),
                count,
                "{name}: downtime must mint at most one task per latest slot"
            );
        }
        let cursors = orbit_store::compose::auto_task::load_cursor_state(
            &orbit_store::compose::auto_task::cursor_state_path(&host.state_dir()),
        )
        .unwrap();
        assert_eq!(
            cursors.definitions["catchup"].last_slot,
            Some(at("2026-10-08T01:07:00Z").to_rfc3339())
        );
        assert!(cursors.definitions["catchup"].pending.is_none());
    }
}

struct CoverageHost {
    evidence: RefCell<Option<MemberBatchEvidence>>,
    member: StateMember,
}

impl MemberHost for CoverageHost {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("fixture-repo".into(), self.member.source.clone()))
    }
    fn observe(&self, _: Option<&str>, _: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: vec![self.member.clone()],
            withheld: BTreeMap::new(),
            next: None,
        })
    }
    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(MemberAdmission::Admit)
    }
    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }
    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }
    fn admit(&self, _: &MemberAttempt) -> Result<String, AutomationError> {
        Ok("fixture-run".into())
    }
    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        Ok(self
            .evidence
            .borrow()
            .clone()
            .map_or(MemberOutcome::Pending, MemberOutcome::Settled))
    }
}

fn member_tick(
    store: &dyn AutomationStoreBackend,
    host: &CoverageHost,
    minute: i64,
) -> Result<AutomationDiagnostic, AutomationError> {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "fixture/member-coverage",
            epoch: "fixture-epoch",
            enabled: true,
            dry_run: false,
            now: host.member.first_seen + Duration::minutes(minute),
            trigger: &StateTrigger {
                kind: StateTriggerKind::PreparationEligible,
                owner_machine: "fixture-host".into(),
                branch: "fixture-branch".into(),
                debounce_minutes: 2,
                max_wait_minutes: 10,
                max_items: 1,
                retries: 1,
                deadline_minutes: 30,
                batch_size: Some(1),
                eligibility: Default::default(),
                freshness: Default::default(),
            },
        },
    )
}

#[test]
fn forged_member_outputs_never_advance_coverage() {
    if isolated("forged_member_outputs_never_advance_coverage") {
        return;
    }
    for forgery in [
        "outer run",
        "outer attempt",
        "member run",
        "member attempt",
        "unknown member",
        "input fingerprint",
        "empty resulting fingerprint",
        "null result",
        "duplicate member",
    ] {
        let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
        let host = CoverageHost {
            evidence: RefCell::new(None),
            member: StateMember {
                key: "fixture-member".into(),
                task_ids: vec!["fixture-task".into()],
                fingerprint: "fixture-input".into(),
                source: SourceRevision {
                    commit: "fixture-head".into(),
                    tree: "fixture-tree".into(),
                },
                evidence: serde_json::json!({}),
                first_seen: at("2026-01-01T00:00:00Z"),
                changed_at: at("2026-01-01T00:00:00Z"),
                crew: None,
            },
        };
        assert_eq!(
            member_tick(store.as_ref(), &host, 0).unwrap().reason,
            "debouncing"
        );
        assert_eq!(
            member_tick(store.as_ref(), &host, 2).unwrap().reason,
            "fired"
        );
        let before = store
            .automation_state("fixture/member-coverage")
            .unwrap()
            .unwrap();
        let attempt = before.members.as_ref().unwrap().active.as_ref().unwrap();
        let valid = MemberBatchEvidence {
            action_id: attempt.action_id.clone().unwrap(),
            attempt_id: attempt.id.clone(),
            failed: BTreeMap::new(),
            applied: vec![MemberEvidence {
                action_id: attempt.action_id.clone().unwrap(),
                attempt_id: attempt.id.clone(),
                member_key: host.member.key.clone(),
                input_fingerprint: host.member.fingerprint.clone(),
                resulting_fingerprint: host.member.fingerprint.clone(),
                ready: true,
                result: serde_json::json!({"task_id": "fixture-task"}),
            }],
        };
        let mut forged = valid.clone();
        match forgery {
            "outer run" => forged.action_id = "wrong-run".into(),
            "outer attempt" => forged.attempt_id = "wrong-attempt".into(),
            "member run" => forged.applied[0].action_id = "wrong-run".into(),
            "member attempt" => forged.applied[0].attempt_id = "wrong-attempt".into(),
            "unknown member" => forged.applied[0].member_key = "wrong-member".into(),
            "input fingerprint" => forged.applied[0].input_fingerprint = "wrong-input".into(),
            "empty resulting fingerprint" => forged.applied[0].resulting_fingerprint.clear(),
            "null result" => forged.applied[0].result = serde_json::Value::Null,
            "duplicate member" => forged.applied.push(forged.applied[0].clone()),
            _ => unreachable!(),
        }
        *host.evidence.borrow_mut() = Some(forged);
        assert!(
            matches!(
                member_tick(store.as_ref(), &host, 3),
                Err(AutomationError::Evidence(_))
            ),
            "{forgery}: forged output must be rejected"
        );
        assert_eq!(
            store
                .automation_state("fixture/member-coverage")
                .unwrap()
                .unwrap(),
            before,
            "{forgery}: failed evidence cannot change the checkpoint or assessments"
        );
        assert!(
            store
                .automation_receipts("fixture/member-coverage", 20)
                .unwrap()
                .is_empty(),
            "{forgery}: failed evidence cannot mint a coverage receipt"
        );
        *host.evidence.borrow_mut() = Some(valid);
        member_tick(store.as_ref(), &host, 4).unwrap();
        let state = store
            .automation_state("fixture/member-coverage")
            .unwrap()
            .unwrap();
        let members = state.members.unwrap();
        assert!(members.active.is_none());
        assert!(members.pending.is_empty());
        assert_eq!(
            members.assessed[&host.member.key].resulting_fingerprint,
            host.member.fingerprint
        );
        assert_eq!(
            store
                .automation_receipts("fixture/member-coverage", 20)
                .unwrap()
                .len(),
            1,
            "{forgery}: correct evidence still advances coverage exactly once"
        );
    }
}
