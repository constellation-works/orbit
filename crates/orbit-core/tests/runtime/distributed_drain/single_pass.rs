//! A pull drain submitted without a window makes one admission pass
//! [ORB-14174]: it claims up to its free slots once, then only settles. Its
//! window is expired from the first iteration, so the pass is recorded in run
//! state, which a retry or a resumed run reads, and a timed window that has
//! expired, a stop or a cancel never gains it.

use orbit_core::DrainAdmissionsStopRequest;

use super::*;

/// The pass input the job forwards for a drain submitted without `--for`:
/// `for_seconds` zero, and a window the shared drain window already reports
/// expired.
fn windowless() -> Value {
    json!({"for_seconds": "0", "window_expired": "true", "max_active_leaf_runs": 2})
}

fn single_pass_taken(pair: &Pair, drain: &str) -> bool {
    pair.follower
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .pull_single_pass
        .is_some()
}

/// Request ids the follower sent the owner, in order, repeats included.
fn pull_request_ids(pair: &Pair) -> Vec<String> {
    pair.wire
        .calls("orbit.task.pull")
        .iter()
        .map(|call| call["request_id"].as_str().unwrap().to_string())
        .collect()
}

/// `pair`'s follower over the same roots and store, bound to a registered
/// logical workspace, as `orbit run auto --pull` opens it.
fn bound_follower(pair: &Pair) -> (OrbitRuntime, String) {
    let logical = "ws_replica".to_string();
    let binding = orbit_core::WorkspaceRuntimeBinding {
        logical_workspace_id: logical.clone(),
        task_partition_id: pair.follower.workspace_id().unwrap(),
        owner_machine_id: None,
        checkout_role: None,
        repo_root: pair.follower_repo.clone(),
        ship_mode: orbit_core::ShipMode::Local,
        base_branch: Some("main".to_string()),
    };
    let follower = OrbitRuntime::from_roots_with_binding(
        &pair.follower.global_root(),
        &pair.follower_repo.join(".orbit"),
        binding,
    )
    .expect("bound replica runtime")
    .with_automation_machine_identity(Some(FOLLOWER.into()))
    .with_coordination_write_owner(Some(OWNER.into()))
    .with_drain_owner_transport(pair.wire.clone());
    (follower, format!("{OWNER}/{logical}"))
}

fn install_pull_job_assets(pair: &Pair) {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let resources = pair.follower.global_root().join("resources");
    for (kind, name) in [
        ("jobs", "workspace_pull_pipeline"),
        ("activities", "drain_window"),
        ("activities", "pull_refill"),
        ("activities", "sleep"),
    ] {
        let file = format!("{name}.yaml");
        std::fs::create_dir_all(resources.join(kind)).unwrap();
        std::fs::copy(
            assets.join(kind).join(&file),
            resources.join(kind).join(&file),
        )
        .unwrap();
    }
}

/// Submit `orbit run auto --pull` with no window on `pair`'s follower,
/// restricted to `allowed_crews`, and execute it in this process the way its
/// detached worker would. Every leaf it launches has a worker that exits
/// before claiming its run, so each claim is released as a transient failure
/// a pass or two later, excluding its crew for the window [ORB-14257].
pub(super) fn run_windowless_drain(pair: &Pair, slots: u32, allowed_crews: &[String]) -> String {
    run_single_pass_drain(pair, slots, allowed_crews, None)
}

fn run_explicit_zero_duration_drain(pair: &Pair, slots: u32) -> String {
    run_single_pass_drain(pair, slots, &[], Some(0))
}

fn run_single_pass_drain(
    pair: &Pair,
    slots: u32,
    allowed_crews: &[String],
    for_seconds: Option<u64>,
) -> String {
    let root = pair._root.path().to_path_buf();
    // This test binary cannot be re-executed as a worker. The drain's
    // substitute waits until the drain has run here; a leaf's exits at once.
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".to_string(),
        "if [ -e \"$1/leaves-exit\" ]; then exit 3; fi; i=0; \
         while [ ! -e \"$1/started-$2\" ] && [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done"
            .to_string(),
        "worker".to_string(),
        root.to_string_lossy().into_owned(),
        orbit_core::test_support::RUN_ID_PLACEHOLDER.to_string(),
    ]);
    // The job and its activities as `orbit init` deploys them.
    install_pull_job_assets(pair);
    let (follower, selector) = bound_follower(pair);
    let submitted = follower
        .submit_workspace_pull_run(
            orbit_core::WorkspacePullRequest {
                selector: &selector,
                for_seconds,
                max_active_leaf_runs: Some(slots),
                allowed_crews,
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect("the replica submits its pull drain");
    let run_id = submitted.run_id;
    // Poll briskly while leaves settle; the default is 30s.
    let store = pair.follower.sqlite_store().unwrap();
    let workspace = pair.follower.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx
                .connection()
                .execute(
                    "UPDATE job_runs SET input_json = json_set(input_json, \
                     '$.poll_sleep_seconds', 1, '$.idle_sleep_seconds', 1) \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    [workspace.as_str(), run_id.as_str()],
                )
                .unwrap();
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
    std::fs::write(root.join("leaves-exit"), "").unwrap();
    let executed = follower.execute_pipeline_run_worker(&run_id);
    std::fs::write(root.join(format!("started-{run_id}")), "").unwrap();
    executed.expect("the drain runs to its end");
    run_id
}

/// [ORB-14174] A drain started without `--for` ran for 62ms and claimed
/// nothing, because its zero window was expired before its first request.
/// Through the real job, it now claims up to its slots in one pass, admits no
/// replacement as those claims settle, and ends once they have. The tasks'
/// crews differ so the second slot is filled whether or not the first leaf's
/// release excluded its crew before the second request; the owner keeps the
/// released task itself from this drain either way.
#[test]
fn a_windowless_pull_drain_claims_up_to_its_slots_once_and_then_only_settles() {
    if !isolated(
        module_path!(),
        "a_windowless_pull_drain_claims_up_to_its_slots_once_and_then_only_settles",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol"), Some("luna"), Some("sol")]);

    let drain = run_windowless_drain(&pair, 2, &[]);

    assert_eq!(pair.run_state(&drain), JobRunState::Success);
    let input = pair
        .follower_jobs
        .get_job_run(&drain)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(input["for_seconds"], 0, "{input}");
    let claimed = pair
        .owner_claims()
        .iter()
        .map(|claim| claim["claim"]["task_id"].as_str().unwrap().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(claimed.len(), 2, "one pass fills both slots: {claimed:?}");
    let left = pair
        .tasks
        .iter()
        .filter(|task| !claimed.contains(*task))
        .collect::<Vec<_>>();
    assert_eq!(left.len(), 1);
    assert_eq!(
        pair.owner_status(left[0]),
        "backlog",
        "no replacement is claimed as the first two settle"
    );
    assert_eq!(pull_request_ids(&pair).len(), 2);
    assert_eq!(
        pair.wire.calls("orbit.drain.probe").len(),
        4,
        "submission and the one pass each negotiate the fingerprint"
    );
    assert!(single_pass_taken(&pair, &drain));
    for claim in pair.owner_claims() {
        assert_ne!(
            claim["claim"]["phase"], "claimed",
            "every claim settled before the drain ended: {claim}"
        );
    }
    assert_eq!(
        pair.follower_jobs
            .unsettled_local_pull_admissions()
            .unwrap()
            .len(),
        0
    );
}

/// A windowless drain over an empty backlog asks once and ends, rather than
/// polling an idle owner.
#[test]
fn a_windowless_pull_drain_over_an_empty_backlog_asks_once_and_ends() {
    if !isolated(
        module_path!(),
        "a_windowless_pull_drain_over_an_empty_backlog_asks_once_and_ends",
    ) {
        return;
    }
    let pair = Pair::new(0);

    let drain = run_windowless_drain(&pair, 2, &[]);

    assert_eq!(pair.run_state(&drain), JobRunState::Success);
    assert_eq!(
        pull_request_ids(&pair).len(),
        1,
        "one idle answer ends the pass"
    );
    assert!(pair.owner_claims().is_empty());
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    assert!(state.pull_single_pass.is_some());
    assert!(
        state.iteration <= 1,
        "the drain ended on its first iteration: {}",
        state.iteration
    );
}

/// Explicit zero is the same bounded single-pass contract as an omitted
/// duration: it admits once, fills only the requested slot, then settles.
#[test]
fn an_explicit_zero_duration_pull_drain_admits_one_bounded_pass() {
    if !isolated(
        module_path!(),
        "an_explicit_zero_duration_pull_drain_admits_one_bounded_pass",
    ) {
        return;
    }
    let pair = Pair::new(2);

    let drain = run_explicit_zero_duration_drain(&pair, 1);

    assert_eq!(pair.run_state(&drain), JobRunState::Success);
    let input = pair
        .follower_jobs
        .get_job_run(&drain)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    assert_eq!(input["for_seconds"], 0);
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(pull_request_ids(&pair).len(), 1);
    assert!(single_pass_taken(&pair, &drain));
    let claimed = pair.owner_claims()[0]["claim"]["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    for task in &pair.tasks {
        assert_eq!(
            pair.owner_status(task),
            "backlog",
            "{task}: the claimed task is released, and no replacement is admitted after its one \
             slot settles (claimed {claimed})"
        );
    }
}

/// A positive window that has run out is not a single pass: an expired timed
/// drain requests nothing and ends.
#[test]
fn an_expired_timed_window_never_gains_the_single_pass() {
    if !isolated(
        module_path!(),
        "an_expired_timed_window_never_gains_the_single_pass",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();

    let pass = pair.pass_over(
        &drain,
        json!({"for_seconds": "3600", "window_expired": "true"}),
    );

    assert_eq!(pass["admitted"], 0, "{pass}");
    assert_eq!(pass["admitting"], false, "{pass}");
    assert_eq!(pass["done"], true, "{pass}");
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(!single_pass_taken(&pair, &drain));
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// `orbit run auto --stop` before the pass takes it away: the drain requests
/// nothing and ends, and records no pass it never made.
#[test]
fn a_stop_before_the_single_pass_takes_it_away() {
    if !isolated(
        module_path!(),
        "a_stop_before_the_single_pass_takes_it_away",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let stopped = pair
        .follower
        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
            actor: "operator",
            source: "run_auto_stop",
            reason: None,
            claim_token: None,
            force: false,
        })
        .expect("stop admissions");
    assert_eq!(
        stopped.coordinators.len(),
        1,
        "the live pull drain is stopped"
    );

    let pass = pair.pass_over(&drain, windowless());

    assert_eq!(pass["admitted"], 0, "{pass}");
    assert_eq!(pass["done"], true, "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(!single_pass_taken(&pair, &drain));
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// A graceful cancel recorded before the pass ends the drain `cancelled`
/// without a request.
#[test]
fn a_cancel_before_the_single_pass_admits_nothing() {
    if !isolated(
        module_path!(),
        "a_cancel_before_the_single_pass_admits_nothing",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("graceful cancel");

    let pass = pair.pass_over(&drain, windowless());

    assert_eq!(pass["cancelling"], true, "{pass}");
    assert_eq!(pass["done"], true, "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(!single_pass_taken(&pair, &drain));
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// The pass is recorded before its first request, so a retry after a lost
/// reply re-sends only the request it already recorded — the owner's claim
/// is carried, not duplicated — and a resumed run, which carries the run
/// state, finds the pass taken and only settles.
#[test]
fn a_retried_or_resumed_single_pass_carries_its_claim_and_requests_nothing_new() {
    if !isolated(
        module_path!(),
        "a_retried_or_resumed_single_pass_carries_its_claim_and_requests_nothing_new",
    ) {
        return;
    }
    let pair = Pair::new(3);
    let drain = pair.run_drain();
    pair.wire.lose_next_reply("orbit.task.pull");

    let lost = pair.pass_over(&drain, windowless());
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    assert_eq!(
        lost["done"], false,
        "the recorded request holds the drain: {lost}"
    );
    assert!(single_pass_taken(&pair, &drain));
    assert_eq!(
        pair.owner_claims().len(),
        1,
        "the owner committed the claim"
    );

    let retried = pair.pass_over(&drain, windowless());
    assert_eq!(retried["admitted"], 0, "{retried}");
    let sent = pull_request_ids(&pair);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0], sent[1], "the retry re-sends the recorded request");
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(
        retried["unsettled"], 0,
        "the carried claim settled: {retried}"
    );
    assert_eq!(retried["done"], true, "{retried}");

    // A resume seeds the new run with the source's run state under its own id.
    let resumed = pair.run_drain();
    let mut state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    state.run_id = resumed.clone();
    pair.follower.write_run_state(&resumed, &state).unwrap();
    let pass = pair.pass_over(&resumed, windowless());
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert_eq!(pass["done"], true, "{pass}");
    assert_eq!(
        pull_request_ids(&pair).len(),
        2,
        "the resumed run requests nothing"
    );
    let backlog = pair
        .tasks
        .iter()
        .filter(|task| pair.owner_status(task) == "backlog")
        .count();
    assert_eq!(backlog, 2);
}

/// An old zero-window checkpoint has no `pull_single_pass` field. The public
/// resume submission carries it into a linked run, which must not treat that
/// missing field as fresh admission authority.
#[test]
fn a_legacy_zero_window_checkpoint_resumed_through_the_job_api_cannot_claim() {
    if !isolated(
        module_path!(),
        "a_legacy_zero_window_checkpoint_resumed_through_the_job_api_cannot_claim",
    ) {
        return;
    }
    let pair = Pair::new(1);
    install_pull_job_assets(&pair);
    orbit_core::test_support::install_substitute_pipeline_worker([
        "sh".to_string(),
        "-c".to_string(),
        "exit 3".to_string(),
        "worker".to_string(),
        orbit_core::test_support::RUN_ID_PLACEHOLDER.to_string(),
    ]);
    let (follower, selector) = bound_follower(&pair);
    let source = follower
        .submit_workspace_pull_run(
            orbit_core::WorkspacePullRequest {
                selector: &selector,
                for_seconds: None,
                max_active_leaf_runs: Some(1),
                allowed_crews: &[],
                actor: Some("review-test"),
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect("submit a zero-window source run")
        .run_id;

    // This is the shape of a checkpoint written before pull_single_pass was
    // added: the open-window step has completed, but no pass marker exists.
    let mut checkpoint = follower.read_run_state(&source).unwrap().unwrap();
    checkpoint.next_step_index = 1;
    checkpoint.step_states.insert(0, JobRunState::Success);
    follower.write_run_state(&source, &checkpoint).unwrap();
    pair.follower_jobs
        .mark_job_run_running(&source, Utc::now(), std::process::id())
        .unwrap();
    pair.follower_jobs
        .finalize_job_run(&source, JobRunState::Failed, Utc::now(), Some(1))
        .unwrap();

    let resumed = follower
        .submit_resume_run(&source, Some("review-test"), None)
        .expect("resume through the supported job API")
        .run_id;
    let run = pair
        .follower_jobs
        .get_job_run(&resumed)
        .unwrap()
        .expect("resumed job run");
    assert_eq!(run.retry_source_run_id.as_deref(), Some(source.as_str()));
    assert!(
        follower
            .read_run_state(&resumed)
            .unwrap()
            .unwrap()
            .pull_single_pass
            .is_none(),
        "the legacy checkpoint remains marker-free"
    );

    let pass = follower
        .run_deterministic(
            "pull_refill",
            &json!({}),
            &json!({
                "run_id": resumed,
                "destination": pair.destination,
                "for_seconds": "0",
                "window_expired": "true",
                "max_active_leaf_runs": 1,
            }),
            ToolContext::default(),
        )
        .expect("the resumed pass reports its admission result");

    assert_eq!(pass["admitted"], 0, "{pass}");
    assert_eq!(pass["done"], true, "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert!(
        follower
            .read_run_state(&run.run_id)
            .unwrap()
            .unwrap()
            .pull_single_pass
            .is_none(),
        "a legacy retry cannot mint a new marker"
    );

    // Whole-run replays use the same immutable source link as resumes. Seed
    // that persisted replay shape directly: replica admission correctly
    // refuses `submit_replay_run` before it can create a pull replay.
    let replay_input = json!({"destination": pair.destination, "for_seconds": 0});
    let replay = pair
        .follower_jobs
        .insert_job_run(
            "workspace_pull_pipeline",
            2,
            Utc::now(),
            Some(replay_input.clone()),
            Some(source.clone()),
        )
        .unwrap();
    follower
        .write_run_state(
            &replay.run_id,
            &PipelineState::new(replay.run_id.clone(), replay.job_id.clone(), replay_input),
        )
        .unwrap();
    let replayed = replay.run_id;
    assert_eq!(replay.retry_source_run_id.as_deref(), Some(source.as_str()));
    assert_eq!(replay.retry_source_run_id.as_deref(), Some(source.as_str()));
    let replayed_pass = follower
        .run_deterministic(
            "pull_refill",
            &json!({}),
            &json!({
                "run_id": replayed,
                "destination": pair.destination,
                "for_seconds": "0",
                "window_expired": "true",
                "max_active_leaf_runs": 1,
            }),
            ToolContext::default(),
        )
        .expect("the replayed pass reports its admission result");
    assert_eq!(replayed_pass["admitted"], 0, "{replayed_pass}");
    assert_eq!(replayed_pass["done"], true, "{replayed_pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert!(
        follower
            .read_run_state(&replayed)
            .unwrap()
            .unwrap()
            .pull_single_pass
            .is_none(),
        "a replay cannot mint a new marker"
    );
}
