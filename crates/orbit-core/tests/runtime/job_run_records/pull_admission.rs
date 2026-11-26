//! Job-run ids and local pull occupancy, each fixture in an isolated child.

use std::collections::HashSet;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use chrono::Utc;
use orbit_common::{process, test_env};
use orbit_core::runtime::JobRuns;
use orbit_store::Store;
use orbit_store::contract::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract, PullDestination,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use tempfile::TempDir;

/// How long one isolated test may run before it is killed and fails.
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;
/// Run `test` alone in a child with authority cleared and a disposable `HOME`; `true` inside the child.
fn allocation_admission_isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_STORE_ALLOCATION_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args([
            "--exact",
            &format!("job_run_records::pull_admission::{test}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = process::run_bounded_capped(&mut command, CHILD_DEADLINE, 64 * 1024)
        .expect("run isolated allocation fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}
// Keep each test's exact-name child and deadline; only its setup guard is shared.
macro_rules! allocation_test {
    ($name:ident, $body:block) => {
        #[test]
        fn $name() {
            if allocation_admission_isolated(stringify!($name)) $body
        }
    };
}
type JobRun = orbit_types::workflow::JobRun;
/// One pull-admission fixture: a store, its drain parent and destination.
struct PullFixture {
    _root: TempDir,
    jobs: Arc<JobRuns>,
    parent: JobRun,
    destination: PullDestination,
}
impl PullFixture {
    fn new(job: &str, limits: Option<(u32, u32)>) -> Self {
        let root = TempDir::new().unwrap();
        let jobs = JobRuns::shared(Store::open(&root.path().join("pull.db")).unwrap(), "ws");
        let parent = jobs.insert_job_run(job, 1, Utc::now(), None, None).unwrap();
        let (run_id, job_id) = (parent.run_id.clone(), parent.job_id.clone());
        let mut state = PipelineState::new(run_id, job_id, serde_json::json!({}));
        if let Some((retuned, submitted)) = limits {
            assert!(state.set_drain_worker_limit(retuned, submitted, "cli".into(), None, None));
        }
        jobs.write_run_state(&parent.run_id, &state).unwrap();
        let destination = PullDestination {
            owner_machine_id: "owner".into(),
            owner_workspace_id: "ws".into(),
            selector: "owner/ws".into(),
            execution_machine_id: "owner".into(),
        };
        Self {
            _root: root,
            destination,
            jobs,
            parent,
        }
    }
    fn admit(&self, request_id: &str, ceiling: usize) -> bool {
        let request = pull_request(&self.parent.run_id, request_id);
        self.jobs
            .allocate_pull_request(&self.destination, &request, ceiling)
            .unwrap()
            .is_some()
    }
    fn occupancy(&self) -> orbit_store::contract::DrainLeafOccupancy {
        self.jobs.drain_leaf_occupancy().unwrap()
    }
    fn mark_retrying(&self, run_id: &str) {
        let store = Store::open(&self._root.path().join("pull.db")).unwrap();
        let run_id = run_id.to_string();
        store
            .with_transaction(|tx| {
                let changed = tx.connection().execute(
                    "UPDATE job_runs SET state = 'retrying' WHERE workspace_id = ?1 AND run_id = ?2",
                    ["ws", run_id.as_str()],
                ).unwrap();
                assert_eq!(changed, 1, "retrying fixture run exists");
                Ok(())
            })
            .unwrap();
    }
    fn leaf(&self, job: &str, task: &str) -> JobRun {
        self.jobs
            .insert_job_run(
                job,
                1,
                Utc::now(),
                Some(serde_json::json!({ "task_ids": [task] })),
                None,
            )
            .unwrap()
    }
}
// Job-run ids and pull occupancy.
fn finish(jobs: &JobRuns, run_id: &str) {
    let at = Utc::now();
    let running = jobs.mark_job_run_running(run_id, at, 42).unwrap();
    assert!(running.owns_execution());
    assert!(
        jobs.finalize_job_run(run_id, JobRunState::Success, at, Some(1))
            .unwrap()
    );
}
// [ORB-13599] Archive/delete remove a run's row; its id stays reserved and is never reminted in the same minute.
allocation_test!(removed_job_run_ids_are_never_reallocated, {
    let root = TempDir::new().unwrap();
    let db = root.path().join("orbit.db");
    let open = || JobRuns::shared(Store::open(&db).unwrap(), "ws_a");
    let mut minted = Vec::new();
    for delete in [false, true] {
        // The id stem is the wall-clock minute; retry the cycle if it crossed a minute boundary, which
        // would make the comparison vacuous.
        let (removed, next) = (0..3)
            .find_map(|_| {
                let jobs = open();
                let removed = jobs
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                finish(jobs.as_ref(), &removed.run_id);
                if delete {
                    jobs.delete_job_run(&removed.run_id).unwrap();
                } else {
                    jobs.archive_job_run(&removed.run_id).unwrap();
                }
                assert!(jobs.get_job_run(&removed.run_id).unwrap().is_none());
                drop(jobs);
                // Deleted identities also stay reserved for another workspace
                // opening its own connection to the host's database.
                let reopened = if delete {
                    JobRuns::shared(Store::open(&db).unwrap(), "ws_b")
                } else {
                    open()
                };
                let next = reopened
                    .insert_job_run("job-a", 1, Utc::now(), None, None)
                    .unwrap();
                (removed.run_id.rsplit_once('-').unwrap().0
                    == next.run_id.rsplit_once('-').unwrap().0)
                    .then_some((removed.run_id, next.run_id))
            })
            .expect("two runs minted within one minute");
        assert_ne!(next, removed, "a removed run's id was minted again");
        minted.extend([removed, next]);
    }
    let unique = minted.iter().collect::<HashSet<_>>();
    let got = unique.len();
    assert_eq!(got, minted.len(), "every id minted once: {minted:?}");
});
fn admission_request(
    request_id: &str,
    caller_version: &str,
    run_id: &str,
    job_name: &str,
    machine_name: Option<&str>,
    ship: AdmissionShipContract,
) -> AdmissionRequest {
    AdmissionRequest {
        labels: Vec::new(),
        request_id: request_id.into(),
        caller_version: caller_version.into(),
        caller_schema: orbit_store::contract::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        caller_fingerprint: None,
        caller_before_pr: false,
        review_gate: false,
        selective_review: false,
        visual_resolution: 0,
        run_context: AdmissionRunContext {
            run_id: run_id.into(),
            job_name: job_name.into(),
            machine_name: machine_name.map(str::to_string),
        },
        ship,
        crews: None,
        os: None,
        browser_kit: false,
        admissible: None,
    }
}
fn ship_contract(mode: &str, branch: &str) -> AdmissionShipContract {
    AdmissionShipContract {
        mode: mode.into(),
        base_branch: branch.into(),
        landing_branch: branch.into(),
        before_pr: false,
        before_landing: false,
        completion: "review".into(),
        authorization_reference: None,
        review: None,
    }
}
fn pull_request(run_id: &str, request_id: &str) -> AdmissionRequest {
    admission_request(
        request_id,
        "1",
        run_id,
        "workspace_auto_pipeline",
        None,
        ship_contract("local", "main"),
    )
}
// Twelve connections race for pull admission under a ceiling of three: exactly three are admitted, one slot each.
allocation_test!(concurrent_pull_admission_obeys_the_shared_ceiling, {
    const RACERS: usize = 12;
    const CEILING: usize = 3;
    let fixture = PullFixture::new("workspace_auto_pipeline", None);
    let db = fixture._root.path().join("pull.db");
    let (jobs, parent, destination) = (
        fixture.jobs.clone(),
        fixture.parent.clone(),
        fixture.destination.clone(),
    );
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers = (0..RACERS)
        .map(|index| {
            let jobs = JobRuns::shared(Store::open(&db).unwrap(), "ws");
            let destination = destination.clone();
            let request = pull_request(&parent.run_id, &format!("request-{index}"));
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                jobs.allocate_pull_request(&destination, &request, CEILING)
                    .unwrap()
                    .is_some()
            })
        })
        .collect::<Vec<_>>();
    let admitted = racers
        .into_iter()
        .map(|racer| usize::from(racer.join().unwrap()))
        .sum::<usize>();
    assert_eq!(admitted, CEILING);
    assert_eq!(jobs.local_pull_admissions().unwrap().len(), CEILING);
    assert_eq!(fixture.occupancy().occupied, CEILING);
    let inherited = |run_id: &str| jobs.drain_leaf_occupancy_for_run(run_id).unwrap().inherited;
    assert_eq!(inherited(&parent.run_id), Some(0));
    let replacement = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    assert_eq!(inherited(&replacement.run_id), Some(CEILING));
    let ok = !fixture.admit("late", CEILING);
    assert!(ok, "a full ceiling admits nothing more");
});
// The drain's live (retuned) limit is the only ceiling on pulled leaves, past any per-leaf limit.
allocation_test!(pull_admission_is_bound_only_by_the_drains_live_limit, {
    const SUBMITTED: usize = 3;
    const RETUNED: u32 = 24;
    let fixture = PullFixture::new("workspace_pull_pipeline", Some((RETUNED, SUBMITTED as u32)));
    let admitted = (0..RETUNED + 6)
        .filter(|index| fixture.admit(&format!("request-{index}"), SUBMITTED))
        .count();
    assert_eq!(admitted, RETUNED as usize, "the retuned limit governs");
    let occupancy = fixture.occupancy();
    assert_eq!(occupancy.occupied, RETUNED as usize);
    assert_eq!(
        occupancy.per_pipeline.get("task_claimed_local_pipeline"),
        Some(&(RETUNED as usize)),
        "one leaf definition holds every slot, with no ceiling of its own"
    );
});
// A retrying claimed leaf owns one drain slot; terminal leaf history releases its slot.
allocation_test!(
    retrying_claimed_leaf_occupies_capacity_and_terminal_history_does_not,
    {
        const CEILING: usize = 1;
        let fixture = PullFixture::new(
            "workspace_pull_pipeline",
            Some((CEILING as u32, CEILING as u32)),
        );
        let retrying = fixture.leaf("task_claimed_local_pipeline", "ORB-TEST");
        let terminal = fixture.leaf("task_claimed_pr_pipeline", "ORB-OLD");
        finish(fixture.jobs.as_ref(), &terminal.run_id);
        fixture.mark_retrying(&retrying.run_id);
        let occupancy = fixture.occupancy();
        assert_eq!(
            occupancy.occupied, CEILING,
            "the retrying leaf occupies capacity and the terminal leaf is excluded"
        );
        let got = occupancy.per_pipeline.get("task_claimed_local_pipeline");
        assert_eq!(
            got,
            Some(&1),
            "retrying claimed leaves count toward their pipeline"
        );
        let got = occupancy.per_pipeline.get("task_claimed_pr_pipeline");
        assert_eq!(got, None, "terminal leaf history does not count");
        let ok = !fixture.admit("retrying-leaf-at-capacity", CEILING);
        assert!(
            ok,
            "a retrying claimed leaf prevents admission at the ceiling"
        );
    }
);
/// Record a schema-valid before-landing admission result, as the leaf's
/// `landing_review_gate_admit` checkpoint does.
fn checkpoint_landing_review(jobs: &JobRuns, run_id: &str, applies: bool) {
    let mut state = PipelineState::new(
        run_id.into(),
        "task_claimed_pr_pipeline".into(),
        serde_json::json!({}),
    );
    let output = if applies {
        let admitted: orbit_types::workflow::ReviewGateAdmitted =
            serde_json::from_value(serde_json::json!({
                "decision": "admitted", "first_task_id": "ORB-REVIEW",
                "attempt_id": "attempt", "attempt_index": 1, "lineage_key": "lineage",
                "timing": "before-landing", "timing_source": "workspace",
                "reviewer": {"crew": "reviewers", "crew_source": "workspace",
                    "provider": "codex", "model": "review-model"},
                "base_sha": "0".repeat(40), "base_tree": "0".repeat(40),
                "head_sha": "1".repeat(40), "head_tree": "1".repeat(40),
                "implementation_commit_count": 1, "task_meaning_digest": "digest",
                "task_selectors": {}, "manifest_artifact": "manifest.json",
                "report_artifact": "report.md",
                "budget": orbit_types::workflow::ReviewBudget::default(),
                "remaining": {"seconds": 1800}, "started_at": Utc::now(),
            }))
            .unwrap();
        let admitted =
            orbit_types::workflow::ReviewGateAdmissionOutput::Admitted(Box::new(admitted));
        serde_json::to_value(admitted).unwrap()
    } else {
        let na =
            orbit_types::workflow::ReviewGateNotApplicable::new("review_before_landing_off", None);
        serde_json::to_value(orbit_types::workflow::ReviewGateAdmissionOutput::NotApplicable(na))
            .unwrap()
    };
    state.record_pipeline_output("landing_review_gate_admit", output);
    jobs.write_run_state(run_id, &state).unwrap();
}
// [ORB-15194] A reviewing claimed leaf keeps its slot but admits one replacement, up to the ceiling of reviewing leaves.
allocation_test!(
    reviewing_claimed_leaves_admit_replacements_up_to_the_ceiling,
    {
        const CEILING: usize = 2;
        let fixture = PullFixture::new("workspace_pull_pipeline", None);
        let jobs = fixture.jobs.clone();
        let leaf = |task: &str| fixture.leaf("task_claimed_pr_pipeline", task).run_id;
        let mut requests = 0;
        let mut admit = || {
            requests += 1;
            fixture.admit(&format!("request-{requests}"), CEILING)
        };
        let implementing = leaf("ORB-IMPL");
        checkpoint_landing_review(jobs.as_ref(), &implementing, false);
        let reviewing = leaf("ORB-REVIEW");
        checkpoint_landing_review(jobs.as_ref(), &reviewing, true);
        let occupancy = fixture.occupancy();
        assert_eq!(
            (occupancy.occupied, occupancy.reviewing),
            (CEILING, 1),
            "the reviewing leaf still occupies its slot and is reported apart"
        );
        let ok = admit();
        assert!(
            ok,
            "a full drain admits a replacement beside its reviewing leaf"
        );
        assert!(!admit(), "only one replacement per reviewing leaf");
        // The other leaf reaches its review too: one more replacement.
        checkpoint_landing_review(jobs.as_ref(), &implementing, true);
        assert!(admit(), "each reviewing leaf admits its own replacement");
        assert!(!admit());
        // A reviewing leaf past the ceiling's worth frees nothing more.
        let third = leaf("ORB-REVIEW-3");
        checkpoint_landing_review(jobs.as_ref(), &third, true);
        let occupancy = fixture.occupancy();
        let got = (occupancy.occupied, occupancy.reviewing);
        assert_eq!(
            got,
            (CEILING + 3, 3),
            "every live leaf and admission still occupies a slot"
        );
        let ok = !admit();
        assert!(
            ok,
            "at most the ceiling of reviewing leaves admit replacements"
        );
    }
);
