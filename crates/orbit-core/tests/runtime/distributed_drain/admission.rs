//! Pull admission: cancel-state reads, owner negotiation, the failure breaker, the crew window, `os:` tag routing, host throttling and local/claim exclusion.

use orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA;
use orbit_types::task::HostOs;

use super::*;

/// A replica that declares no `workflow.required_validation_commands` starts
/// its pull drain: an empty list means no required check, as it does for an
/// owner's own delivery, so submission notes it rather than refusing. This
/// fixture deploys no job asset, so a submission past every preflight — the
/// owner probe included — reaches the job catalog and stops there.
#[test]
fn a_follower_without_required_validation_commands_starts_a_pull_drain() {
    if !isolated(
        module_path!(),
        "a_follower_without_required_validation_commands_starts_a_pull_drain",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let follower = OrbitRuntime::in_memory()
        .expect("replica runtime")
        .with_automation_machine_identity(Some(FOLLOWER.into()))
        .with_coordination_write_owner(Some(OWNER.into()))
        .with_drain_owner_transport(pair.wire.clone());
    assert!(follower.workflow_required_validation_commands().is_empty());
    assert!(follower.required_validation_note().is_some());
    let logical = follower
        .workspace_runtime_binding()
        .expect("registered replica")
        .logical_workspace_id
        .clone();

    let submitted = follower
        .submit_workspace_pull_run(
            orbit_core::WorkspacePullRequest {
                selector: &format!("{OWNER}/{logical}"),
                for_seconds: Some(60),
                max_active_leaf_runs: Some(1),
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("a fixture without job assets never submits a run");

    assert!(
        matches!(
            submitted,
            OrbitError::NotFound { ref id, .. } if id == "workspace_pull_pipeline"
        ),
        "an empty requirement list must not refuse a pull drain: {submitted}"
    );
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), 1);
}

/// An unreadable persisted cancel request fails the whole pass visibly; it
/// cannot probe, request or launch work without readable control state.
#[test]
fn unreadable_cancel_state_fails_visibly_without_admission() {
    if !isolated(
        module_path!(),
        "unreadable_cancel_state_fails_visibly_without_admission",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("persist graceful cancellation");
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    assert!(state.drain_cancelling());
    let store = pair.follower.sqlite_store().unwrap();
    let workspace = pair.follower.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx.connection().execute(
                "UPDATE job_runs SET pipeline_state_json = '{' WHERE workspace_id = ?1 AND run_id = ?2",
                [workspace.as_str(), drain.as_str()],
            ).unwrap();
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
    let read_error = pair
        .follower
        .read_run_state(&drain)
        .unwrap_err()
        .to_string();

    for expired in [false, true] {
        let failure = pair
            .follower
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({"run_id": drain, "destination": pair.destination,
                "window_expired": expired}),
                ToolContext::default(),
            )
            .expect_err("unreadable pass health fails visibly, without touching admissions");
        assert!(failure.to_string().contains(&read_error), "{failure}");
    }
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    pair.follower.write_run_state(&drain, &state).unwrap();
    let recovered = pair.pass(&drain);
    assert!(recovered["error"].is_null(), "{recovered}");
    assert_eq!(recovered["cancelling"], true, "{recovered}");
    assert_eq!(recovered["done"], true, "{recovered}");
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
}

/// Older owners reject additive fields (`crews`, `os`) despite equal binary
/// versions. Negotiation must stop the newer follower before it sends them.
#[test]
fn an_older_owner_is_refused_before_a_newer_request_is_sent() {
    if !isolated(
        module_path!(),
        "an_older_owner_is_refused_before_a_newer_request_is_sent",
    ) {
        return;
    }
    let pair = Pair::new(1);
    *pair.wire.protocol.lock().unwrap() = Some(1);
    let drain = pair.start_drain();
    let pass = pair.pass(&drain);
    let refusal = pass["refusal"].as_str().unwrap();
    assert!(refusal.starts_with("protocol_mismatch:"), "{pass}");
    assert!(
        refusal.contains(&format!(
            "caller revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}; owner revision 1"
        )),
        "{pass}"
    );
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    // A newer owner's boundary likewise refuses an older follower by type.
    *pair.wire.protocol.lock().unwrap() = None;
    let probe = pair
        .wire
        .call(
            "",
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": 1, "caller_review_policy": "none",
            }),
        )
        .unwrap();
    assert_eq!(probe["refusal"], "protocol_mismatch");
    assert!(probe["diagnostics"].to_string().contains(&format!(
        "caller revision 1; owner revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}"
    )));
    let request = json!({"request_id": "old-request", "caller_version": probe["binary_version"],
        "caller_schema": 1, "caller_review_policy": "none", "ship": probe["ship"],
        "run_context": {"run_id": "old-drain", "job_name": "workspace_pull_pipeline"}});
    let failure = pair.wire.call("", "orbit.task.pull", request).unwrap_err();
    assert!(
        failure.to_string().contains(&format!(
            "protocol_mismatch: caller revision 1; owner revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}"
        )),
        "{failure}"
    );
    assert!(pair.owner_claims().is_empty());
}

/// Transport errors persist, a successful pass resets a transient streak,
/// and three failures latch a warning instead of idling invisibly forever.
#[test]
fn repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it() {
    if !isolated(
        module_path!(),
        "repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it",
    ) {
        return;
    }
    let pair = Pair::new(0);
    let drain = pair.start_drain();
    *pair.wire.unreachable.lock().unwrap() = true;
    assert_eq!(pair.pass(&drain)["consecutive_pass_failures"], 1);
    *pair.wire.unreachable.lock().unwrap() = false;
    let recovered = pair.pass(&drain);
    assert_eq!(recovered["consecutive_pass_failures"], 0);
    assert!(recovered["last_pass_error"].is_null());
    *pair.wire.unreachable.lock().unwrap() = true;
    for count in 1..=3 {
        let pass = pair.pass(&drain);
        assert_eq!(pass["consecutive_pass_failures"], count);
        assert_eq!(pass["degraded"], count == 3);
        let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
        let health = state.drain_last_pass.unwrap();
        assert_eq!(health.consecutive_pass_failures, count);
        assert!(
            health
                .last_pass_error
                .unwrap()
                .contains("Connection timed out")
        );
    }
    *pair.wire.unreachable.lock().unwrap() = false;
    let before = pair.wire.calls("orbit.drain.probe").len();
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false);
    assert_eq!(held["degraded"], true);
    assert!(
        held["refusal"]
            .as_str()
            .unwrap()
            .starts_with("pass_failures:")
    );
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), before);
    let fresh = pair.pass(&pair.start_drain());
    assert_eq!(fresh["degraded"], false);
    assert_eq!(fresh["consecutive_pass_failures"], 0);
}

/// A graceful cancel persisted while orphan reconciliation runs is observed
/// before this pass probes, requests, or launches anything.
#[test]
fn cancel_recorded_during_orphan_reconciliation_admits_nothing() {
    if !isolated(
        module_path!(),
        "cancel_recorded_during_orphan_reconciliation_admits_nothing",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    assert!(
        !pair
            .follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );
    let follower = pair.follower.clone();
    let drain_id = drain.clone();
    pair.follower.install_orphan_reconcile_hook(move || {
        follower
            .cancel_job_run_with_options(&drain_id, "operator", "cli", None, false)
            .expect("persist graceful cancellation during reconciliation");
    });

    let pass = pair.pass(&drain);
    assert_eq!(pass["cancelling"], true, "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert!(pass["error"].is_null(), "{pass}");
    assert!(pair.wire.calls("orbit.drain.probe").is_empty(), "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert!(
        pair.follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );
}

/// A readable run state with no cancellation still probes and admits work.
#[test]
fn readable_state_without_cancel_permits_refill() {
    if !isolated(
        module_path!(),
        "readable_state_without_cancel_permits_refill",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    assert!(
        !pair
            .follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );

    let pass = pair.pass(&drain);
    assert!(
        launch_refused(&pass),
        "the admitted leaf reaches launch: {pass}"
    );
    assert_eq!(pass["cancelling"], false, "{pass}");
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), 1);
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1);
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(pair.leaf_runs().len(), 1);
}

/// Three claims in a row that this drain settled as failures open its
/// breaker: the next pass requests nothing and reports why. A new drain
/// starts with a closed breaker and pulls again.
#[test]
fn three_failed_claims_open_the_breaker_and_a_new_drain_resets_it() {
    if !isolated(
        module_path!(),
        "three_failed_claims_open_the_breaker_and_a_new_drain_resets_it",
    ) {
        return;
    }
    let pair = Pair::new(4);
    let drain = pair.start_drain();

    for failed in 1..=3 {
        let pass = pair.pass(&drain);
        assert!(launch_refused(&pass), "{pass}");
        assert_eq!(pass["consecutive_failures"], failed, "{pass}");
    }
    let opened = pair.pass(&drain);
    assert_eq!(opened["admitting"], false, "{opened}");
    assert!(
        opened["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("circuit_open")),
        "{opened}"
    );
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 3);
    let backlog = |pair: &Pair| {
        pair.tasks
            .iter()
            .filter(|id| pair.owner_status(id) == "backlog")
            .count()
    };
    assert_eq!(backlog(&pair), 1, "the fourth task is left on the owner");

    let restarted = pair.start_drain();
    let reset = pair.pass(&restarted);
    assert!(launch_refused(&reset), "{reset}");
    assert_eq!(reset["consecutive_failures"], 1, "{reset}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 4);
    assert_eq!(backlog(&pair), 0);
    assert_eq!(pair.owner_claims().len(), 4);
}

fn excluded(pass: &Value, crew: &str) -> Value {
    pass["crews"]["excluded"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|exclusion| exclusion["crew"] == crew)
        .cloned()
        .unwrap_or(Value::Null)
}

/// A follower whose window preflight cannot find crew `antigravity`'s CLI is
/// never handed an `antigravity` task [ORB-13941]. The owner admits the next
/// task it can run instead and, once only the unrunnable one is left, answers
/// idle and names it, so the task stays in the backlog for the owner or
/// another follower rather than being claimed and failed here.
#[test]
fn a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run() {
    if !isolated(
        module_path!(),
        "a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("antigravity"), Some("sol")]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    let (unrunnable, runnable) = (&pair.tasks[0], &pair.tasks[1]);
    let drain = pair.start_drain();

    let first = pair.pass(&drain);
    assert!(launch_refused(&first), "{first}");
    let exclusion = excluded(&first, "antigravity");
    assert_eq!(exclusion["source"], "preflight", "{first}");
    assert!(
        exclusion["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit-test-no-such-provider-cli")),
        "{first}"
    );
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");

    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], runnable.as_str());
    assert_eq!(pair.owner_status(unrunnable), "backlog");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 2, "{pulls:?}");
    for pull in &pulls {
        let runnable = pull["crews"]["runnable"]
            .as_array()
            .expect("declared crews");
        assert!(runnable.iter().any(|crew| crew == "sol"), "{pull}");
        assert!(runnable.iter().all(|crew| crew != "antigravity"), "{pull}");
    }
    let idle_receipt = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .find(|record| record.phase == LocalPullPhase::Idle)
        .and_then(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        idle_receipt
            .crew_unavailable
            .iter()
            .any(|skipped| &skipped.task_id == unrunnable && skipped.reason.contains("antigravity")),
        "{idle_receipt:#?}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(window.checked_at.is_some());
    assert!(
        window
            .excluded
            .iter()
            .any(|exclusion| exclusion.crew == "antigravity"),
        "{window:#?}"
    );
}

/// The owner hands a follower only tasks whose `os:` tags its declared OS
/// satisfies [ORB-14005]. A Linux follower takes a task tagged for both OSes
/// and an untagged one, never the `os:macos` task, which the idle receipt
/// names and which stays in the owner's backlog until a macOS follower pulls
/// it.
#[test]
fn a_follower_is_handed_only_tasks_its_os_satisfies() {
    if !isolated(
        module_path!(),
        "a_follower_is_handed_only_tasks_its_os_satisfies",
    ) {
        return;
    }
    let mut pair = Pair::new(3);
    let (mac, either, anywhere) = (
        pair.tasks[0].clone(),
        pair.tasks[1].clone(),
        pair.tasks[2].clone(),
    );
    for (task, tags) in [
        (&mac, json!(["os:macos"])),
        (&either, json!(["os:linux", "os:macos"])),
    ] {
        pair.wire
            .owner
            .run_tool(
                "orbit.task.update",
                json!({"id": task, "tags": tags, "model": "codex"}),
            )
            .expect("tag owner task");
    }
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Linux));
    let drain = pair.start_drain();

    for _ in 0..3 {
        pair.pass(&drain);
    }
    let claimed = |pair: &Pair| {
        pair.owner_claims()
            .iter()
            .map(|claim| claim["claim"]["task_id"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        claimed(&pair),
        BTreeSet::from([either.clone(), anywhere.clone()])
    );
    assert_eq!(pair.owner_status(&mac), "backlog");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert!(pulls.iter().all(|pull| pull["os"] == "linux"), "{pulls:?}");
    let idle = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        idle.os_unavailable
            .iter()
            .any(|skipped| skipped.task_id == mac
                && skipped
                    .reason
                    .contains("waits for a macos host (os:macos); the executor runs linux")),
        "{idle:#?}"
    );

    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Macos));
    let mac_drain = pair.start_drain();
    pair.pass(&mac_drain);
    assert_eq!(claimed(&pair), BTreeSet::from([mac, either, anywhere]));
    assert_eq!(
        pair.wire.calls("orbit.task.pull").last().unwrap()["os"],
        "macos"
    );
}

/// A claimed leaf whose provider refused to authenticate gives its claim
/// back [ORB-13941]: the owner's task returns to the backlog rather than
/// `blocked`, the failure breaker does not count it, and the drain offers
/// that crew no more for the rest of its window, so the same task is not
/// pulled straight back to fail the same way.
#[test]
fn a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
    if !isolated(
        module_path!(),
        "a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(
        &leaf,
        "[provider_unavailable] claude provider authentication failure (HTTP 401): \
         Failed to authenticate: OAuth token revoked. Please log in again or contact your administrator.",
    );

    let pass = pair.pass(&drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(pass["consecutive_failures"], 0, "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    let exclusion = excluded(&pass, "sol");
    assert_eq!(exclusion["source"], "provider_unavailable", "{pass}");
    assert!(
        exclusion["reason"].as_str().is_some_and(
            |reason| reason.contains(task.as_str()) && reason.contains("OAuth token revoked")
        ),
        "{pass}"
    );

    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let release = &settles[0]["settlement"]["Release"];
    assert_eq!(
        release["provider_unavailable"]["crew"], "sol",
        "{settles:?}"
    );
    let claims = pair.owner_claims();
    assert_eq!(
        claims.len(),
        1,
        "the released task is not pulled back: {claims:#?}"
    );
    assert_eq!(claims[0]["claim"]["phase"], "revoked");
    assert!(
        comments_of(&pair.owner_task(&task)).contains("could not use the provider of crew `sol`"),
        "{}",
        pair.owner_task(&task)
    );
    let pulls = pair.wire.calls("orbit.task.pull");
    let last = pulls.last().expect("the pass asked again");
    assert!(
        last["crews"]["excluded"]
            .as_array()
            .is_some_and(|excluded| excluded.iter().any(|exclusion| exclusion["crew"] == "sol")),
        "{last}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(
        window
            .runnable
            .as_ref()
            .is_some_and(|runnable| !runnable.iter().any(|crew| crew == "sol")),
        "{window:#?}"
    );
}

/// [ORB-13901] While sustained host pressure throttles the follower, its
/// drain requests no claim but still delivers a settlement it owes; once
/// memory is back below its resume mark the next pass pulls again.
#[test]
fn a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume() {
    if !isolated(
        module_path!(),
        "a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume",
    ) {
        return;
    }
    let mut pair = Pair::new(2);
    let probe = crate::dispatch_admission::PressureProbe::calm();
    pair.follower = pair
        .follower
        .clone()
        .with_host_resource_probe(probe.clone());
    let drain = pair.start_drain();

    // The first claim fails at launch and its settlement reply is lost, so
    // the drain owes the owner that settlement.
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let owed = pair.pass(&drain);
    assert!(error_of(&owed).contains("dropped"), "{owed}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

    probe.sustain_memory(&pair.follower, 96.0);
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false, "{held}");
    assert_eq!(held["admitted"], 0, "{held}");
    assert_eq!(
        held["resource_throttle"]["resources"][0]["resource"], "memory",
        "{held}"
    );
    assert_eq!(held["sleep_seconds"], 30, "a throttled drain polls: {held}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "no new claim");
    assert_eq!(
        pair.follower.pending_pull_settlements().unwrap().count,
        0,
        "the owed settlement is delivered while throttled"
    );
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 2);
    let unclaimed = pair
        .tasks
        .iter()
        .filter(|id| pair.owner_status(id) == "backlog")
        .count();
    assert_eq!(unclaimed, 1, "the second task stays on the owner");
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .and_then(|pass| pass.resource_throttle)
        .expect("the pull pass records the throttle");
    assert_eq!(recorded.resources[0].percent, 96.0);

    probe.memory(70.0, Utc::now());
    let resumed = pair.pass(&drain);
    assert!(launch_refused(&resumed), "{resumed}");
    assert_eq!(resumed["resource_throttle"], Value::Null, "{resumed}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 2);
}

/// A local drain's admission of `task` on the owner, as its gate leaves it
/// while waiting for context locks: the wrapper and gate carry the task, and
/// the task is still `backlog` with nothing reserved.
fn local_drain_admission(owner: &OrbitRuntime, task: &str) -> Vec<String> {
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    ["task_auto_pipeline", "task_gate_pipeline"]
        .into_iter()
        .map(|job| {
            jobs.insert_job_run(job, 1, Utc::now(), Some(json!({"task_ids": [task]})), None)
                .expect("local run")
                .run_id
        })
        .collect()
}

/// [ORB-13918] A task the owner's local drain admitted is not pulled while
/// that admission is live, even though its gate has not yet moved it out of
/// `backlog` or reserved its footprint; once the local runs end, it is.
#[test]
fn a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends() {
    if !isolated(
        module_path!(),
        "a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let local = local_drain_admission(&pair.wire.owner, &task);
    for run in &local {
        set_run_state(&pair.wire.owner, run, "retrying");
    }
    let drain = pair.start_drain();

    let held = pair.pass(&drain);
    assert!(error_of(&held).is_empty(), "{held}");
    assert!(pair.owner_claims().is_empty(), "{:#?}", pair.owner_claims());
    assert_eq!(pair.owner_status(&task), "backlog");
    assert!(pair.leaf_runs().is_empty());

    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    for run in &local {
        set_run_state(&pair.wire.owner, run, "running");
        owner_jobs
            .finalize_job_run(
                run,
                orbit_types::workflow::JobRunState::Cancelled,
                Utc::now(),
                None,
            )
            .unwrap();
    }
    let released = pair.pass(&drain);
    assert!(launch_refused(&released), "{released}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], task.as_str());
}

/// [ORB-13918] While a follower's claim on a task is live, the owner's local
/// drain neither selects it nor lets a gate that queued it before the claim
/// landed dispatch it: the gate's pre-dispatch admission stops as a no-op and
/// no delivery run starts.
#[test]
fn a_task_under_a_live_claim_is_never_admitted_by_the_local_drain() {
    if !isolated(
        module_path!(),
        "a_task_under_a_live_claim_is_never_admitted_by_the_local_drain",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.start_drain();
    // The owner commits the claim; the lost reply leaves it unbound and live.
    pair.wire.lose_next_reply("orbit.task.pull");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["phase"], "claimed");
    let owner = &pair.wire.owner;

    let wave = owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");

    let gate = owner
        .run_deterministic(
            "invoke_and_wait",
            &json!({}),
            &json!({
                "job_name": "task_pr_pipeline",
                "run_input": {"task_ids": [task]},
                "admission_task_ids": [task],
                "admission_workflow": "worktree_setup",
                "timeout_seconds": 5,
            }),
            ToolContext::default(),
        )
        .expect("gate dispatch");
    assert_eq!(gate["skipped"], true, "{gate}");
    assert_eq!(gate["status"], "success", "{gate}");
    assert!(gate.get("error").is_none(), "{gate}");
    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    assert!(
        owner_jobs
            .list_job_runs("task_pr_pipeline")
            .unwrap()
            .is_empty(),
        "no local delivery run starts beside the claim"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "claimed");
}

/// Ask `owner`'s probe, as the routed follower does, whether a pull with
/// `caller_before_pr` would be admitted.
fn probe_owner(owner: &OrbitRuntime, caller_before_pr: bool) -> Value {
    owner
        .run_tool_with_context_and_role(
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                "caller_before_pr": caller_before_pr,
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    caller_machine_id: Some(FOLLOWER.to_string()),
                    process_machine_id: Some(OWNER.to_string()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("probe")
}

/// An owner opened over `config` and, when `after_landing` is set, an
/// enabled `delivery-code-review` auto-task.
fn owner_with(root: &Path, config: &str, after_landing: bool) -> OrbitRuntime {
    let orbit = root.join(OWNER).join("repo/.orbit");
    std::fs::create_dir_all(orbit.join("auto_tasks")).unwrap();
    std::fs::write(orbit.join("config.toml"), config).unwrap();
    if after_landing {
        let seed = include_str!("../../../assets/auto_tasks/delivery-code-review.yaml")
            .replace("__ORBIT_BASE_BRANCH__", "main")
            .replace("enabled: false", "enabled: true")
            .replace("updated_by: system", "updated_by: human:operator");
        std::fs::write(orbit.join("auto_tasks/delivery-code-review.yaml"), seed).unwrap();
    }
    open_runtime(root, OWNER).0
}

/// [ORB-13992] Distributed admission refuses only on `review.before_pr`, on
/// either endpoint. After-landing review — the `delivery-code-review`
/// auto-task, or the deprecated policy value that stands in for it — runs on
/// the owner after landing and never refuses a pull.
#[test]
fn only_before_pr_refuses_a_pull_and_after_landing_review_never_does() {
    if !isolated(
        module_path!(),
        "only_before_pr_refuses_a_pull_and_after_landing_review_never_does",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();

    let after_landing = owner_with(&root.path().join("auto-task"), "", true);
    let probe = probe_owner(&after_landing, false);
    assert_eq!(probe["review"]["after_landing"]["enabled"], true);
    assert_eq!(probe["review"]["before_pr"]["enabled"], false);
    assert_eq!(probe["admits"], true, "{probe}");
    assert_eq!(probe["ship"]["before_pr"], false);

    let legacy_after_landing = owner_with(
        &root.path().join("legacy-after-landing"),
        "[operation]\nreview_policy = \"after-landing\"\n",
        false,
    );
    assert_eq!(probe_owner(&legacy_after_landing, false)["admits"], true);

    let follower_before_pr = probe_owner(&after_landing, true);
    assert_eq!(follower_before_pr["admits"], false);
    assert_eq!(follower_before_pr["refusal"], "before_pr_unsupported");

    for config in [
        "[review]\nbefore_pr = true\n",
        "[operation]\nreview_policy = \"before-pr\"\n",
    ] {
        let owner = owner_with(&root.path().join(config.len().to_string()), config, false);
        let probe = probe_owner(&owner, false);
        assert_eq!(probe["review"]["before_pr"]["enabled"], true, "{config}");
        assert_eq!(probe["ship"]["before_pr"], true, "{config}");
        assert_eq!(probe["admits"], false, "{config}");
        assert_eq!(probe["refusal"], "before_pr_unsupported", "{config}");
    }
}
