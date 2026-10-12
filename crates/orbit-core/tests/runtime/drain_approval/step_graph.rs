//! The approval drain's own step graph [ORB-15381]. The shipped
//! `workspace_auto_pipeline` job runs in this process the way its worker
//! would; every child it submits gets a substitute worker that never claims
//! its run. The test claims each pilot itself, so a pilot stays running until
//! the test ends it. Only the pilot's agent output is fixture data.

use std::time::{Duration, Instant};

use orbit_core::{CompletionPolicy, DrainAdmissionsStopRequest};
use orbit_types::workflow::JobRunTrigger;

use super::*;

const PILOT_JOB: &str = "task_pilot_pipeline";
const LEAF_JOB: &str = "task_auto_pipeline";

impl Workspace {
    /// Poll `probe` until it yields, failing after a generous deadline with
    /// what the drain `drain` last did.
    fn wait_for<T>(&self, drain: &str, what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(found) = probe() {
                return found;
            }
            if Instant::now() >= deadline {
                let state = self.runtime.read_run_state(drain).unwrap().unwrap();
                let children = state
                    .child_dispatches
                    .iter()
                    .map(|child| {
                        let run = self.run(&child.child_run_id);
                        format!("{} {} {}", child.job_name, run.run_id, run.state)
                    })
                    .collect::<Vec<_>>();
                panic!(
                    "timed out waiting for {what}; drain {}: children {children:?}, last pass {}",
                    self.run(drain).state,
                    state.pipeline
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The `index`th pilot `drain` started, claimed by this process so it
    /// runs until the test ends it, whatever its substitute worker does.
    fn claimed_pilot(&self, drain: &str, index: usize) -> String {
        let pilot = self.wait_for(drain, "an approval pilot", || {
            self.children(drain, PILOT_JOB).into_iter().nth(index)
        });
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        jobs.mark_job_run_running(&pilot, Utc::now(), std::process::id())
            .unwrap();
        pilot
    }
}

impl Workspace {
    /// The same fixture with every shipped job and activity deployed, as
    /// `orbit init` leaves a host.
    fn with_shipped_jobs() -> Self {
        let mut workspace = Self::new();
        let global = workspace._root.path().join("home/.orbit");
        orbit_core::bootstrap::init::init_workspace_at_root(
            &global,
            orbit_core::bootstrap::init::InitOptions {
                global_only: true,
                refresh_defaults: true,
                ..Default::default()
            },
        )
        .unwrap();
        workspace.runtime = OrbitRuntime::from_roots(&global, &workspace.repo.join(".orbit"))
            .unwrap()
            .with_host_resource_probe(crate::dispatch_admission::PressureProbe::calm())
            .with_automation_machine_identity(Some("fixture-machine".into()));
        workspace
    }

    /// A backlog task scoped to its own new file, so two never conflict.
    fn backlog_task(&self, title: &str) -> Task {
        let file = format!("{title}.md");
        self.commit_file(&file, "fixture\n", title);
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: format!("Ship {title}."),
                acceptance_criteria: vec!["The change is in place.".into()],
                plan: format!("Edit {file}."),
                status: Some(TaskStatus::Backlog),
                context_files: vec![format!("file:{file}")],
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
    }

    /// The children of `drain` running `job`, oldest first.
    fn children(&self, drain: &str, job: &str) -> Vec<String> {
        self.runtime
            .read_run_state(drain)
            .unwrap()
            .map(|state| {
                state
                    .child_dispatches
                    .into_iter()
                    .filter(|child| child.job_name == job)
                    .map(|child| child.child_run_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The leaf `drain` started for `task`, once it has.
    fn leaf_for(&self, drain: &str, task: &Task) -> Option<String> {
        self.children(drain, LEAF_JOB).into_iter().find(|leaf| {
            self.run(leaf).input.is_some_and(|input| {
                input["task_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(task.id)))
            })
        })
    }

    fn run(&self, run_id: &str) -> orbit_types::workflow::JobRun {
        self.runtime.show_job_run(run_id).unwrap()
    }

    fn live(&self, run_id: &str) -> bool {
        !self.run(run_id).state.is_terminal()
    }

    /// A slot's task leaves the backlog and then its leaf ends, as a delivery
    /// that reaches review does. In the other order a pass between the two
    /// writes sees a free slot and the task still in the backlog, and admits
    /// it again: that duplicate never runs, so it holds the drain's only slot
    /// until its substitute worker gives up.
    fn settle_leaf(&self, leaf: &str, task: &Task) {
        self.edit(
            task,
            TaskUpdateParams {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        );
        self.finish(leaf, JobRunState::Success);
    }
}

#[test]
fn backlog_work_is_admitted_while_the_approval_pilot_runs_and_the_drain_waits_for_it() {
    if !crate::dispatch_admission::isolated(
        "drain_approval::step_graph::backlog_work_is_admitted_while_the_approval_pilot_runs_and_the_drain_waits_for_it",
    ) {
        return;
    }
    let workspace = Workspace::with_shipped_jobs();
    let release = workspace._root.path().join("release-workers");
    crate::worker_fixture::install(&release, "created");
    let proposed = workspace.task("proposed", &[], &["file:README.md"], TaskComplexity::Low);
    let eligible = workspace.backlog_task("eligible");

    let drain = workspace
        .runtime
        .submit_workspace_auto_run_with_containment(
            Some(3600),
            Some(1),
            CompletionPolicy::Review,
            &[],
            &Default::default(),
            None,
            None,
            JobRunTrigger::cli(),
            false,
            true,
        )
        .unwrap()
        .run_id;
    // A one-second poll, and an idle sleep longer than this test may wait:
    // a pass that slept it while a pilot ran would time the test out.
    let store = workspace.runtime.sqlite_store().unwrap();
    let workspace_id = workspace.runtime.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx
                .connection()
                .execute(
                    "UPDATE job_runs SET input_json = json_set(input_json, \
                     '$.poll_sleep_seconds', 1, '$.idle_sleep_seconds', 600) \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    [workspace_id.as_str(), drain.as_str()],
                )
                .unwrap();
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
    let executor = {
        let runtime = workspace.runtime.clone();
        let drain = drain.clone();
        std::thread::spawn(move || runtime.execute_pipeline_run_worker(&drain))
    };

    // The first pass starts the pilot, and admission does not wait for it.
    let pilot = workspace.claimed_pilot(&drain, 0);
    let eligible_leaf = workspace.wait_for(&drain, "the eligible task's leaf", || {
        workspace.leaf_for(&drain, &eligible)
    });
    assert!(
        workspace.live(&pilot),
        "the eligible task was admitted while the pilot ran"
    );

    // A slot freed mid-pilot is refilled while the pilot still runs, and the
    // idle passes in between waited on the pilot, not the idle sleep.
    workspace.settle_leaf(&eligible_leaf, &eligible);
    let refill = workspace.backlog_task("refill");
    let refill_leaf = workspace.wait_for(&drain, "the refill's leaf", || {
        workspace.leaf_for(&drain, &refill)
    });
    assert!(workspace.live(&pilot), "the slot was refilled mid-pilot");
    assert_eq!(
        workspace.children(&drain, PILOT_JOB),
        vec![pilot.clone()],
        "a live pilot's task is never piloted again, and no second pilot starts"
    );

    // The pilot approves its task and finishes: the pass that sees it records
    // the approval, admits the task and pilots what was filed meanwhile.
    workspace.settle_leaf(&refill_leaf, &refill);
    let late = workspace.task("late", &[], &["file:README.md"], TaskComplexity::Low);
    let applied = workspace
        .apply(
            &pilot,
            vec![assessment(&proposed)],
            json!({"drain_promotion": {"run_id": drain}}),
        )
        .unwrap();
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    workspace.finish(&pilot, JobRunState::Success);
    workspace.wait_for(&drain, "the approved task's leaf", || {
        workspace.leaf_for(&drain, &proposed)
    });
    let second_pilot = workspace.claimed_pilot(&drain, 1);

    // Admissions close with that pilot still live: the loop ends, and the
    // drain stays open for it.
    workspace
        .runtime
        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "operator",
            source: "fixture",
            reason: None,
            claim_token: None,
            force: false,
        })
        .unwrap();
    workspace.wait_for(&drain, "the drain loop to end", || {
        let state = workspace.runtime.read_run_state(&drain).unwrap()?;
        (state.pipeline["drain"]["broke"] == true).then_some(())
    });
    // approval_join polls live pilots for up to its 3600s bound, so a claimed
    // pilot cannot let the drain end early. The apply below is the handshake
    // that this drain's authority is still valid.
    assert!(workspace.live(&drain), "the drain waits for its pilot");

    // So the pilot's apply step still finds the drain's authority valid.
    let applied = workspace
        .apply(
            &second_pilot,
            vec![assessment(&late)],
            json!({"drain_promotion": {"run_id": drain}}),
        )
        .unwrap();
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    workspace.finish(&second_pilot, JobRunState::Success);
    executor.join().unwrap().unwrap();
    std::fs::write(&release, "").unwrap();

    assert_eq!(workspace.run(&drain).state, JobRunState::Success);
    assert_eq!(workspace.status(&late), TaskStatus::Backlog);
    let report = workspace
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_approvals
        .unwrap();
    assert_eq!(report.approved_total, 2);
    assert_eq!(report.approved, vec![proposed.id.clone(), late.id.clone()]);
}
