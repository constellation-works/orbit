//! [ORB-14823] The owner's clock tick never runs a baseline-red hold's
//! required command under the host's sweep lock. It judges each hold from
//! recorded base results and leaves a base tip nobody has checked to one
//! detached `baseline_hold_refresh_pipeline` run, which records the verdict.
//! Ticks while that run's check is in flight still fire routines and
//! auto-tasks.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_core::application::routines::loader::{DiscoveredWorkspaces, RoutineWorkspaceProvider};
use orbit_core::application::routines::{RoutineMachineIdentity, SweepOptions, SweepOutcome};
use orbit_core::application::task::BASELINE_HOLD_REFRESH_JOB;
use orbit_core::{JobRunState, OrbitError, OrbitRuntime};
use orbit_store::contracts::JobRunQuery;
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::Value;

use super::review_baseline_hold::{git, held_on_red_base};
use super::review_gate_audit::Fixture;

const SHIPPED_JOB: &str = include_str!("../../assets/jobs/baseline_hold_refresh_pipeline.yaml");
const SHIPPED_ACTIVITY: &str = include_str!("../../assets/activities/refresh_baseline_holds.yaml");
const ROUTINE: &str = "every-minute";
const AUTO_TASK: &str = "every-minute-duty";
/// The marker the base check writes and the FIFO it blocks reading, in the
/// Git common directory, which every checkout of the repository shares.
const STARTED: &str = "base-check-started";
const RELEASE: &str = "base-check-release";
const RUNS: &str = "base-check-runs.log";

struct SingleWorkspace(OrbitRuntime);

impl RoutineWorkspaceProvider for SingleWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let workspace = Workspace {
            id: self.0.workspace_id()?,
            name: "hold-workspace".into(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, self.0.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}

fn common_dir(fixture: &Fixture) -> PathBuf {
    fixture.repo.join(".git").canonicalize().unwrap()
}

/// Move `main` to a check that logs each run, then runs `body`.
fn move_base(fixture: &Fixture, body: &str) {
    git(&fixture.repo, &["checkout", "--quiet", "main"]);
    std::fs::write(
        fixture.repo.join("check.sh"),
        format!(
            "#!/bin/sh\ncommon=\"$(git rev-parse --path-format=absolute --git-common-dir)\"\n\
             echo run >> \"$common/{RUNS}\"\n{body}"
        ),
    )
    .unwrap();
    git(
        &fixture.repo,
        &["commit", "--quiet", "-am", "move the base"],
    );
    git(&fixture.repo, &["checkout", "--quiet", "candidate"]);
}

fn base_check_runs(fixture: &Fixture) -> usize {
    std::fs::read_to_string(common_dir(fixture).join(RUNS))
        .map(|log| log.lines().count())
        .unwrap_or(0)
}

/// The shipped refresh job and activity, resolvable from the fixture's
/// global catalog. Each dispatched run's substitute worker stays alive until
/// the test has executed the run in process.
fn install_refresh_job(fixture: &Fixture) {
    crate::worker_fixture::install(
        fixture._root.path().join("ran-{run_id}").as_path(),
        "created",
    );
    let resources = fixture.runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    std::fs::write(
        resources.join("activities/refresh_baseline_holds.yaml"),
        SHIPPED_ACTIVITY,
    )
    .unwrap();
    std::fs::write(
        resources.join(format!("jobs/{BASELINE_HOLD_REFRESH_JOB}.yaml")),
        SHIPPED_JOB,
    )
    .unwrap();
}

/// An every-minute routine and a one-minute auto-task, the clock's own work.
fn install_clock_work(fixture: &Fixture) {
    let resources = fixture.runtime.paths().global_dir.join("resources");
    std::fs::write(
        resources.join("jobs/fixture_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();
    let routines = fixture.runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join(format!("{ROUTINE}.yaml")),
        format!(
            "schemaVersion: 1\nname: {ROUTINE}\nenabled: true\ntrigger:\n  cron: '* * * * *'\n  \
             missed_run: skip\ntarget: job:fixture_pipeline\npolicy:\n  overlap: allow\n  \
             timeout_minutes: 30\n"
        ),
    )
    .unwrap();
    let auto_tasks = fixture.runtime.paths().local_dir.join("auto_tasks");
    std::fs::create_dir_all(&auto_tasks).unwrap();
    std::fs::write(
        auto_tasks.join(format!("{AUTO_TASK}.yaml")),
        format!(
            "schemaVersion: 1\nname: {AUTO_TASK}\nenabled: true\nschedule:\n  every_minutes: 1\n\
             dedupe: always\ntemplate:\n  title: Fixture duty\n  description: A recurring fixture \
             duty.\n  acceptance_criteria:\n  - The duty is done.\n"
        ),
    )
    .unwrap();
}

fn tick(fixture: &Fixture, now: DateTime<Utc>) -> SweepOutcome {
    orbit_core::test_support::run_sweep_at(
        &fixture.runtime.paths().global_dir,
        SweepOptions::default(),
        RoutineMachineIdentity {
            machine_id: "hold-machine".into(),
            machine_name: "hold-host".into(),
        },
        &SingleWorkspace(fixture.runtime.clone()),
        now,
    )
    .expect("the tick runs")
}

/// Every refresh run, read from the store without reconciling any.
fn refresh_runs(fixture: &Fixture) -> Vec<String> {
    orbit_store::compose::workspace_job_run_store(
        fixture.runtime.sqlite_store().unwrap(),
        fixture.runtime.workspace_id().unwrap(),
    )
    .list_job_runs_filtered(&JobRunQuery {
        job_id: Some(BASELINE_HOLD_REFRESH_JOB.to_string()),
        include_steps: false,
        ..JobRunQuery::default()
    })
    .unwrap()
    .into_iter()
    .map(|run| run.run_id)
    .collect()
}

/// Every verdict recorded on the task since its hold, oldest first.
fn verdicts(fixture: &Fixture) -> Vec<Value> {
    fixture
        .runtime
        .get_task_history(&fixture.task_id)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "baseline_red_hold_verdict")
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
        .collect()
}

/// Run a dispatched refresh run's worker in process, then release its
/// substitute.
fn execute(fixture: &Fixture, run_id: &str) {
    let executed = fixture.runtime.execute_pipeline_run_worker(run_id);
    std::fs::write(fixture._root.path().join(format!("ran-{run_id}")), "").unwrap();
    executed.unwrap();
    assert_eq!(
        fixture.runtime.show_job_run(run_id).unwrap().state,
        JobRunState::Success
    );
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_hold_check_runs_off_the_tick_and_ticks_meanwhile_fire_routines_and_auto_tasks() {
    if !super::dispatch_admission::isolated(
        "baseline_hold_tick::a_hold_check_runs_off_the_tick_and_ticks_meanwhile_fire_routines_and_auto_tasks",
    ) {
        return;
    }
    let (fixture, _) = held_on_red_base();
    install_refresh_job(&fixture);
    install_clock_work(&fixture);
    let common = common_dir(&fixture);
    // The new base passes once it reads a line from a FIFO the test holds
    // open. The test writes that line to release the check. A watchdog
    // writes it after two minutes, so a tick that ran the check inline
    // returns and fails its timing assertion instead of hanging.
    let fifo = common.join(RELEASE);
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo: {made}");
    let mut release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let (_test_done, test_finished) = std::sync::mpsc::channel::<()>();
    let mut watchdog_end = release.try_clone().unwrap();
    std::thread::spawn(move || {
        if test_finished.recv_timeout(Duration::from_secs(120))
            == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        {
            let _ = std::io::Write::write_all(&mut watchdog_end, b"go\n");
        }
    });
    move_base(
        &fixture,
        &format!("touch \"$common/{STARTED}\"\nread go < \"$common/{RELEASE}\"\nexit 0\n"),
    );
    let slot = Utc::now();

    // The tick reads no recorded result for the new tip, so it dispatches
    // one refresh run and returns without starting the check.
    let started = Instant::now();
    let first = tick(&fixture, slot);
    let elapsed = started.elapsed();
    assert!(!first.lock_busy);
    assert!(
        elapsed < Duration::from_secs(60),
        "the tick waited {elapsed:?} on the held check"
    );
    assert!(
        !common.join(STARTED).exists(),
        "the tick ran the required command"
    );
    let runs = refresh_runs(&fixture);
    assert_eq!(runs.len(), 1, "one refresh run: {runs:?}");
    let run_id = runs[0].clone();
    assert!(verdicts(&fixture).is_empty(), "nothing is judged yet");

    std::thread::scope(|scope| {
        let worker = scope.spawn(|| execute(&fixture, &run_id));
        wait_for(&common.join(STARTED));

        // While the check runs, the next minute's tick is not lock-busy:
        // the routine fires, the auto-task mints, and no second refresh run
        // starts.
        let busy = tick(&fixture, slot + chrono::Duration::minutes(1));
        assert!(!busy.lock_busy, "{busy:?}");
        let routine = busy
            .reports
            .iter()
            .find(|report| report.routine == ROUTINE)
            .unwrap_or_else(|| panic!("routine not evaluated: {busy:?}"));
        assert_eq!(routine.action, "fired", "{routine:?}");
        let duty = busy
            .auto_task_reports
            .iter()
            .find(|report| report.name == AUTO_TASK)
            .unwrap_or_else(|| panic!("auto-task not evaluated: {busy:?}"));
        assert_eq!(duty.action, "minted", "{duty:?}");
        assert_eq!(refresh_runs(&fixture), vec![run_id.clone()]);
        if let Some(fired) = &routine.run_id {
            std::fs::write(fixture._root.path().join(format!("ran-{fired}")), "").unwrap();
        }

        std::io::Write::write_all(&mut release, b"go\n").unwrap();
        worker.join().unwrap();
    });

    // The refresh run recorded the lifting verdict.
    let recorded = verdicts(&fixture);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["lifted"], true, "{recorded:?}");
    assert_eq!(base_check_runs(&fixture), 1);

    // A lifted hold needs no more checks.
    tick(&fixture, slot + chrono::Duration::minutes(2));
    assert_eq!(refresh_runs(&fixture), vec![run_id]);
    assert_eq!(verdicts(&fixture).len(), 1);
}

#[test]
fn a_checked_tip_is_judged_from_its_recorded_result_without_another_run() {
    if !super::dispatch_admission::isolated(
        "baseline_hold_tick::a_checked_tip_is_judged_from_its_recorded_result_without_another_run",
    ) {
        return;
    }
    let (fixture, _) = held_on_red_base();
    install_refresh_job(&fixture);
    move_base(&fixture, "echo 'test suite::other ... FAILED'\nexit 1\n");
    let deadline = || Instant::now() + Duration::from_secs(60);

    let first = fixture.runtime.run_baseline_hold_tick(deadline()).unwrap();
    assert_eq!(first.unchecked, vec![fixture.task_id.clone()], "{first:?}");
    let run_id = first.dispatched.clone().expect("a refresh run");
    assert_eq!(base_check_runs(&fixture), 0, "the tick ran the command");
    execute(&fixture, &run_id);
    let output = fixture
        .runtime
        .read_run_state(&run_id)
        .unwrap()
        .unwrap()
        .pipeline["refresh"]
        .clone();
    assert_eq!(output["held"][0], fixture.task_id.as_str(), "{output}");
    let recorded = verdicts(&fixture);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["lifted"], false, "the new tip still fails");

    // The tip's result is recorded now: the tick judges it the same way,
    // records nothing new and starts no run.
    let next = fixture.runtime.run_baseline_hold_tick(deadline()).unwrap();
    assert!(next.unchecked.is_empty(), "{next:?}");
    assert_eq!(next.dispatched, None, "{next:?}");
    assert!(
        next.held.is_empty(),
        "an unchanged verdict is not re-recorded"
    );
    assert_eq!(verdicts(&fixture).len(), 1);
    assert_eq!(base_check_runs(&fixture), 1, "the tip ran once");
    assert_eq!(refresh_runs(&fixture), vec![run_id]);
}
