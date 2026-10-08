//! [ORB-14257] A launched claimed leaf's settlement carries a typed failure
//! class, and the owner blocks the task only for the candidate's own failure
//! or the task's; every other class releases it, within a per-task budget.

use orbit_core::application::distributed::PullCrewWindow;
use orbit_store::contracts::ClaimSettlementKind;
use orbit_types::workflow::{
    BaselineRedHold, ClaimFailureClass, CrewExclusion, CrewExclusionSource,
    FORGE_UNAVAILABLE_ERROR_CODE, ForgeUnavailableHold,
};

use super::*;

/// The candidate branch and head the claimed leaves below commit.
const BRANCH: &str = "orbit/claimed-candidate";
const HEAD: &str = "c0ffee0000000000000000000000000000000001";

/// Record `outputs` as the steps `leaf` completed, the way its pipeline
/// state holds them when it stops.
fn leaf_completed(pair: &Pair, leaf: &str, outputs: Value) {
    let mut state = pair
        .follower
        .read_run_state(leaf)
        .unwrap()
        .expect("the leaf's pipeline state");
    for (step, output) in outputs.as_object().expect("step outputs") {
        state.pipeline[step] = output.clone();
    }
    pair.follower.write_run_state(leaf, &state).unwrap();
}

/// What a claimed PR leaf's steps leave once its candidate is committed and
/// its branch prepared, before it is synchronized onto the base.
fn prepared() -> Value {
    json!({
        "commit": {"commit_sha": HEAD},
        "prepare_branch": {"head": BRANCH, "head_sha": HEAD, "base": "main", "base_sha": "b0"},
    })
}

/// The drain's crew window, as `orbit run show <drain>` reports it.
fn window(pair: &Pair, drain: &str) -> PullCrewWindow {
    pair.follower
        .pull_drain_crew_window(drain)
        .unwrap()
        .expect("a pull drain has a crew window")
}

/// The settlement the follower delivered for `leaf`'s claim.
fn settlement_of(pair: &Pair, leaf: &str) -> Value {
    let claim_id = pair
        .admission(leaf)
        .receipt
        .and_then(|receipt| receipt.claim)
        .expect("claim")
        .claim_id;
    pair.wire
        .calls("orbit.drain.claim.settle")
        .into_iter()
        .rev()
        .find(|settle| settle["claim_id"] == claim_id.as_str())
        .map(|settle| settle["settlement"].clone())
        .expect("the leaf's settlement was delivered")
}

/// A pass that only settles: its window is over, so it requests no claim.
fn settle_only(pair: &Pair, drain: &str) -> Value {
    pair.pass_over(drain, json!({"window_expired": true}))
}

/// ORB-14717: settle the actual handoff fetch error as the last failed
/// step of a claimed leaf. A timeout releases finished work; an unknown
/// remote ref retains the existing candidate failure and blocks the task.
#[test]
fn handoff_fetch_timeout_releases_the_owner_but_missing_ref_still_blocks() {
    if !isolated(
        module_path!(),
        "handoff_fetch_timeout_releases_the_owner_but_missing_ref_still_blocks",
    ) {
        return;
    }
    for timeout in [true, false] {
        let leaf = super::claimed_review::ReviewedLeaf::admit();
        let pair = &leaf.pair;
        let repo = &pair.follower_repo;
        let action = |name: &str, input: &Value| {
            orbit_engine::execute_deterministic_action(
                &leaf.bound,
                name,
                &json!({}),
                input,
                false,
                &std::collections::HashMap::new(),
                None,
            )
        };
        let mut input = json!({
            "workspace_path": repo,
            "base_sync": "local",
            "base_sha": leaf.base.commit,
            "pull_request": "7",
        });
        let validated = action("claim_validate", &input).unwrap();
        input["candidate"] = validated["candidate"].clone();
        input["validation"] = validated["validation"].clone();
        input["base_sync"] = json!("remote");
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        if timeout {
            let origin = format!(
                "git://{}/owner/repository.git",
                silent.local_addr().unwrap()
            );
            git(repo, &["remote", "set-url", "origin", &origin]);
            input["git_timeouts"] = json!({"fetch": 1});
        } else {
            let remote = pair._root.path().join("empty-origin.git");
            git(
                pair._root.path(),
                &["init", "--bare", remote.to_str().unwrap()],
            );
            git(
                repo,
                &["remote", "set-url", "origin", remote.to_str().unwrap()],
            );
        }
        let error = action("claim_handoff", &input)
            .expect_err("the final fetch cannot resolve the remote base")
            .to_string();
        assert!(
            error.contains(if timeout {
                "timed out after 1ms"
            } else {
                "couldn't find remote ref"
            }),
            "{error}"
        );
        let head = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
        leaf_completed(
            pair,
            &leaf.leaf,
            json!({
                "commit": {"commit_sha": head},
                "sync_base": {"base_sha": leaf.base.commit, "head_sha": head},
                "review_gate_settle": {"gate": "passed", "reviewed_head_sha": head},
                "validate": validated,
                "pin_validation": validated,
            }),
        );
        let job = orbit_engine::activity_job::load_job_asset(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets/jobs/task_claimed_pr_pipeline.yaml"),
            )
            .unwrap(),
        )
        .unwrap()
        .spec;
        let step_index = job
            .steps
            .iter()
            .position(|step| step.id == "handoff")
            .unwrap();
        let now = Utc::now();
        pair.follower_jobs
            .complete_job_run_step(
                &leaf.leaf,
                &JobRunStepParams {
                    step_index,
                    target_type: JobTargetType::Activity,
                    target_id: "claim_handoff".into(),
                    started_at: now,
                    finished_at: now,
                    duration_ms: None,
                    exit_code: Some(1),
                    agent_response_json: None,
                    state: JobRunState::Failed,
                    error_code: None,
                    error_message: Some(error),
                },
            )
            .unwrap();
        pair.follower_jobs
            .finalize_job_run(&leaf.leaf, JobRunState::Failed, now, None)
            .unwrap();
        let pass = settle_only(pair, &leaf.drain);
        let settlement = settlement_of(pair, &leaf.leaf);
        let (kind, class, status) = if timeout {
            ("Release", "transient", "backlog")
        } else {
            ("Fail", "candidate", "blocked")
        };
        assert_eq!(settlement[kind]["failure"]["class"], class, "{settlement}");
        assert!(
            settlement[kind]["failure"]["reason"]
                .as_str()
                .unwrap()
                .contains("fetch"),
            "the release names the fetch failure: {settlement}"
        );
        assert_eq!(pair.owner_status(&leaf.task), status, "{pass}");
        assert_eq!(
            pair.follower
                .pull_leaf_claim(&leaf.leaf)
                .unwrap()
                .unwrap()
                .failure_class
                .map(|class| class.as_str()),
            Some(class)
        );
    }
}

/// A claimed `sol` leaf that fails on `error`, settled by one pass: the
/// owner's task returns to the backlog, and the release names `class`.
/// Returns that settle-only pass and the drain, for the caller's window
/// assertions.
fn a_released_failure(error: &str, class: &str) -> (Pair, String, Value) {
    a_released_failure_after(error, class, |_, _| {})
}

/// [`a_released_failure`] with the owner's tasks naming `crews`; the first
/// task, on `sol`, is the one claimed.
fn a_released_failure_on(
    crews: &[Option<&str>],
    error: &str,
    class: &str,
) -> (Pair, String, Value) {
    released(Pair::with_crews(crews), error, class, |_, _| {})
}

/// [`a_released_failure`] with `progress` recording what the leaf did
/// before it failed.
fn a_released_failure_after(
    error: &str,
    class: &str,
    progress: impl Fn(&Pair, &str),
) -> (Pair, String, Value) {
    released(Pair::with_crews(&[Some("sol")]), error, class, progress)
}

fn released(
    pair: Pair,
    error: &str,
    class: &str,
    progress: impl Fn(&Pair, &str),
) -> (Pair, String, Value) {
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    assert_eq!(task, pair.tasks[0], "the `sol` task is claimed first");
    progress(&pair, &leaf);
    pair.leaf_fails_with(&leaf, error);

    let pass = settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(
        pass["consecutive_failures"], 0,
        "a release is no failure: {pass}"
    );
    let release = &settlement_of(&pair, &leaf)["Release"];
    assert_eq!(release["failure"]["class"], class, "{release}");
    assert_eq!(release["failure"]["crew"], "sol", "{release}");
    assert!(
        release["failure"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.contains("[") && !reason.is_empty()),
        "the reason quotes the error without Orbit's marker: {release}"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "revoked");
    assert!(
        comments_of(&pair.owner_task(&task)).contains(class),
        "the release comment names the class: {}",
        pair.owner_task(&task)
    );
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(
        claim.failure_class.map(|class| class.as_str()),
        Some(class),
        "`orbit run show <leaf>` exposes the class under its pull claim"
    );
    (pair, drain, pass)
}

/// Whether the drain's window still runs `crew`, and the exclusion naming it.
fn sol_exclusion(pair: &Pair, drain: &str) -> Option<CrewExclusion> {
    window(pair, drain)
        .excluded
        .into_iter()
        .find(|exclusion| exclusion.crew == "sol")
}

/// A failure of the leaf's crew on this host releases the claim and
/// excludes that crew for the drain's window, so the same task is not pulled
/// straight back to fail the same way. A transient failure after the leaf
/// pushed keeps the published candidate and its pull request.
#[test]
fn crew_failures_release_the_claim_and_exclude_the_crew_for_the_window() {
    if !isolated(
        module_path!(),
        "crew_failures_release_the_claim_and_exclude_the_crew_for_the_window",
    ) {
        return;
    }
    for (error, class) in [
        (
            "[transient_failure] action pin check could not connect (status 000)",
            "transient",
        ),
        (
            "[provider_unavailable] claude provider authentication failure (HTTP 401)",
            "provider",
        ),
    ] {
        let pushed = class == "transient";
        let (pair, drain, _) = a_released_failure_after(error, class, |pair, leaf| {
            if pushed {
                let mut outputs = prepared();
                outputs["sync_base"] = json!({"head": BRANCH, "head_sha": HEAD});
                outputs["review_gate_admit"] = json!({"applies": false});
                outputs["review_gate_settle"] = json!({"applies": false});
                outputs["validate"] = json!({"decision": "passed"});
                outputs["push"] = json!({"branch": BRANCH, "local_sha": HEAD});
                outputs["pr_open"] = json!({"pr_number": "42"});
                leaf_completed(pair, leaf, outputs);
            }
        });
        let exclusion = sol_exclusion(&pair, &drain)
            .unwrap_or_else(|| panic!("{class} excludes the crew for the window"));
        let source = if class == "provider" {
            CrewExclusionSource::ProviderUnavailable
        } else {
            CrewExclusionSource::LeafReleased
        };
        assert_eq!(exclusion.source, source, "{class}: {exclusion:?}");
        let task = &pair.tasks[0];
        assert!(
            exclusion.reason.contains(task.as_str()),
            "{class}: {exclusion:?}"
        );
        assert_eq!(window(&pair, &drain).host_suppressed, None, "{class}");

        let pass = pair.pass(&drain);
        assert_eq!(pass["admitted"], 0, "{class}: {pass}");
        let last = pair.wire.calls("orbit.task.pull");
        let excluded = last.last().expect("the pass asked again")["crews"]["excluded"].clone();
        assert!(
            excluded.as_array().is_some_and(|excluded| excluded
                .iter()
                .any(|exclusion| exclusion["crew"] == "sol")),
            "{class}: the owner is told the crew is excluded: {excluded}"
        );
        assert_eq!(pair.owner_claims().len(), 1, "{class}: not pulled back");

        let leaf = pair.leaf_runs()[0].clone();
        let candidate = &settlement_of(&pair, &leaf)["Release"]["failure"]["candidate"];
        if pushed {
            assert_eq!(candidate["branch"], BRANCH, "{candidate}");
            assert_eq!(candidate["head_sha"], HEAD, "{candidate}");
            assert_eq!(candidate["published"], true, "{candidate}");
            assert_eq!(candidate["pull_request"], "42", "{candidate}");
            assert_eq!(candidate["failed_step_id"], "pin_validation", "{candidate}");
        } else {
            assert!(candidate.is_null(), "nothing was committed: {candidate}");
        }
    }
}

/// [ORB-14439] The owner's run history never holds a follower's leaf, so the
/// owner keeps what the leaf's settlement said: a failure scan on the owner
/// reads a follower's provider outage, typed, from the owner's own store.
#[test]
fn the_owner_lists_a_follower_leaf_release_with_its_evidence_class() {
    if !isolated(
        module_path!(),
        "the_owner_lists_a_follower_leaf_release_with_its_evidence_class",
    ) {
        return;
    }
    let (pair, _, _) = a_released_failure(
        "[provider_unavailable] claude provider authentication failure (HTTP 401)",
        "provider",
    );
    let leaf = pair.leaf_runs()[0].clone();
    let settlements = pair.wire.owner.leaf_settlements(None, false).unwrap();
    let [settlement] = settlements.as_slice() else {
        panic!("one settled claim: {settlements:?}");
    };
    assert_eq!(settlement.task_id, pair.tasks[0]);
    assert_eq!(settlement.machine_id, pair.wire.caller);
    assert_eq!(settlement.leaf_run_id.as_deref(), Some(leaf.as_str()));
    assert_eq!(settlement.kind, Some(ClaimSettlementKind::Release));
    assert_eq!(settlement.evidence, "provider_unavailable");
    assert_eq!(settlement.failure_class, Some(ClaimFailureClass::Provider));
    assert_eq!(settlement.crew.as_deref(), Some("sol"));
    assert!(
        settlement
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("HTTP 401")),
        "{settlement:?}"
    );
    let later = chrono::Utc::now() + chrono::Duration::minutes(1);
    assert!(
        pair.wire
            .owner
            .leaf_settlements(Some(later), false)
            .unwrap()
            .is_empty(),
        "`since` bounds the scan window"
    );
}

/// A failure of the host itself — its validation environment, or its route
/// to the owner — releases the claim and suppresses the host for the drain's
/// window, whatever crew a task names: the drain asks the owner for nothing
/// more, and the owner holds every task from that drain even when its
/// request was built before the release reached it.
#[test]
fn host_failures_release_the_claim_and_suppress_the_host_for_the_window() {
    if !isolated(
        module_path!(),
        "host_failures_release_the_claim_and_suppress_the_host_for_the_window",
    ) {
        return;
    }
    for (error, class) in [
        (
            "[validation_environment] required validation 'make ci-fast' could not run: \
             python3 lacks tomllib",
            "environment",
        ),
        (
            "[owner_route_unavailable] owner unavailable: the owner could not be reached from \
             the sandboxed worker",
            "owner_route",
        ),
    ] {
        let (pair, drain, _) = a_released_failure_on(&[Some("sol"), Some("luna")], error, class);
        let window = window(&pair, &drain);
        let suppressed = window
            .host_suppressed
            .clone()
            .unwrap_or_else(|| panic!("{class} suppresses the host: {window:?}"));
        assert!(suppressed.contains(class), "{class}: {suppressed}");
        assert!(window.runs_nothing(), "{class}");
        assert!(
            window
                .describe()
                .iter()
                .any(|line| line.starts_with("host suppressed:")),
            "{class}: {:?}",
            window.describe()
        );

        let requests = pair.wire.calls("orbit.task.pull").len();
        let pass = pair.pass(&drain);
        assert_eq!(pass["admitted"], 0, "{class}: {pass}");
        assert!(
            pass["refusal"]
                .as_str()
                .is_some_and(|refusal| refusal.starts_with("host_suppressed:")),
            "{class}: {pass}"
        );
        assert_eq!(
            pair.wire.calls("orbit.task.pull").len(),
            requests,
            "{class}: a suppressed host asks the owner for nothing"
        );

        // A request this drain built before the release reached it, which
        // would otherwise claim the other crew's task.
        let mut request = pair.wire.calls("orbit.task.pull")[0].clone();
        request["request_id"] = json!(format!("built-before-the-{class}-release"));
        let same = pair
            .wire
            .call("", "orbit.task.pull", request.clone())
            .unwrap();
        assert!(same["receipt"]["claim"].is_null(), "{class}: {same:#}");
        let held = same["receipt"]["crew_unavailable"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            held.iter()
                .any(|held| held["task_id"] == pair.tasks[1].as_str()
                    && held["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("host is suppressed"))),
            "{class}: the other crew's task is held from this drain too: {same:#}"
        );

        request["request_id"] = json!(format!("another-drain-{class}"));
        request["run_context"]["run_id"] = json!(format!("jrun-another-drain-{class}"));
        let other = pair.wire.call("", "orbit.task.pull", request).unwrap();
        assert!(
            other["receipt"]["claim"]["task_id"].is_string(),
            "{class}: another drain may still claim: {other:#}"
        );
    }
}

/// A red base is not the host's crew either: the claim is released and the
/// crew keeps running. (`admission` covers the owner holding the task until
/// the base turns green.)
#[test]
fn a_baseline_red_failure_releases_the_claim_without_excluding_the_crew() {
    if !isolated(
        module_path!(),
        "a_baseline_red_failure_releases_the_claim_without_excluding_the_crew",
    ) {
        return;
    }
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: "b0".into(),
        command: "make ci-fast".into(),
        run_id: String::new(),
    };
    let (pair, drain, _) = a_released_failure(
        &hold.text("`make ci-fast` fails on the base as well: rustfmt diff in listing.rs"),
        "baseline_red",
    );
    assert_eq!(sol_exclusion(&pair, &drain), None);
    assert_eq!(window(&pair, &drain).host_suppressed, None);
}

/// [ORB-14617] A claimed leaf whose delivery push the forge kept refusing
/// ends held, not failed. [ORB-14634] It held its claim while its push
/// retried inside the push's window, so once that window closes the
/// settlement releases the task as `transient` naming the reviewed head it
/// kept and why. The forge refused it, not this host or the crew: the drain
/// keeps offering the crew, and the owner hands the task straight back to it
/// with the kept candidate, which only this host has.
#[test]
fn a_forge_held_leaf_releases_the_claim_naming_its_head_and_keeps_the_crew() {
    if !isolated(
        module_path!(),
        "a_forge_held_leaf_releases_the_claim_naming_its_head_and_keeps_the_crew",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let mut outputs = prepared();
    outputs["sync_base"] = json!({"head": BRANCH, "head_sha": HEAD});
    outputs["review_gate_admit"] = json!({"applies": true});
    outputs["review_gate_settle"] = json!({"applies": true, "reviewed_head_sha": HEAD});
    outputs["validate"] = json!({"decision": "passed"});
    leaf_completed(&pair, &leaf, outputs);
    let now = Utc::now();
    let hold = ForgeUnavailableHold {
        target_ref: format!("refs/heads/{BRANCH}"),
        head_sha: HEAD.into(),
        attempts: 30,
        waited_ms: 7_190_000,
        diagnostic: "! [remote rejected] (Internal Server Error)".into(),
        step_id: "push".into(),
        held_at: now,
        held_since: now - chrono::TimeDelta::hours(2),
    };
    pair.follower_jobs
        .complete_job_run_step(
            &leaf,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "diagnostic".into(),
                started_at: now,
                finished_at: now,
                duration_ms: None,
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Held,
                error_code: Some(FORGE_UNAVAILABLE_ERROR_CODE.into()),
                error_message: Some(hold.text("step `push` held: the forge refused the push")),
            },
        )
        .unwrap();
    pair.follower_jobs
        .finalize_job_run(&leaf, JobRunState::Held, now, None)
        .unwrap();

    let pass = settle_only(&pair, &drain);

    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    let release = &settlement_of(&pair, &leaf)["Release"];
    let failure = &release["failure"];
    assert_eq!(failure["class"], "transient", "{failure}");
    let reason = failure["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("the forge refused the push of") && reason.contains(HEAD),
        "{failure}"
    );
    assert!(!reason.contains('['), "no Orbit marker or JSON: {failure}");
    assert_eq!(release["forge_hold"]["head_sha"], HEAD, "{release}");
    let candidate = &failure["candidate"];
    assert_eq!(candidate["head_sha"], HEAD, "{candidate}");
    assert_eq!(candidate["failed_step_id"], "push", "{candidate}");
    let comments = comments_of(&pair.owner_task(&task));
    assert!(
        comments.contains(HEAD) && comments.contains("held this claim while the forge refused"),
        "the release names the kept head and why: {comments}"
    );

    assert_eq!(sol_exclusion(&pair, &drain), None, "the crew stays offered");
    assert_eq!(window(&pair, &drain).host_suppressed, None);
    let next = pair.running_leaf(&drain, 1);
    assert_eq!(
        pair.claimed_task(&next),
        task,
        "the owner hands the task back to the same drain"
    );
    let resumed = pair
        .admission(&next)
        .receipt
        .and_then(|receipt| receipt.task)
        .and_then(|task| task.resume_candidate)
        .expect("the claim carries the kept candidate");
    assert_eq!(
        (resumed.branch.as_str(), resumed.head_sha.as_str()),
        (BRANCH, HEAD)
    );
}

/// The candidate's own failure, and a task its final recovery judged, still
/// block the task — typed, so `orbit run show` says which.
#[test]
fn a_candidate_failure_still_blocks_the_task() {
    if !isolated(module_path!(), "a_candidate_failure_still_blocks_the_task") {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, "cargo test: 3 tests failed");

    let pass = settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "blocked", "{pass}");
    let failed = &settlement_of(&pair, &leaf)["Fail"];
    assert_eq!(failed["failure"]["class"], "candidate", "{failed}");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");
    let owner_record = &pair.wire.owner.leaf_settlements(None, false).unwrap()[0];
    assert_eq!(
        (owner_record.kind, owner_record.evidence),
        (Some(ClaimSettlementKind::Fail), "failure"),
        "the owner keeps the failure settlement typed: {owner_record:?}"
    );
    assert_eq!(
        pair.follower
            .pull_leaf_claim(&leaf)
            .unwrap()
            .expect("claimed leaf")
            .failure_class,
        Some(ClaimFailureClass::Candidate)
    );
    assert_eq!(
        sol_exclusion(&pair, &drain),
        None,
        "a candidate failure is no host's"
    );
}

/// Cancelling one launched claimed leaf, as `orbit run cancel <leaf>` and the
/// dashboard's cancel do, gives its claim back with the operator's reason
/// rather than failing the task, and excludes its crew for the drain window.
#[test]
fn cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason() {
    if !isolated(
        module_path!(),
        "cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let cancel = pair
        .follower
        .cancel_job_run_with_options_and_policy(
            &leaf,
            "operator",
            "cli",
            Some("wrong crew for this task"),
            false,
            false,
        )
        .expect("cancel the leaf");
    assert_eq!(cancel.outcome, "cancelled", "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);

    let owner = pair.owner_task(&task);
    assert_eq!(owner["status"], "backlog", "{owner:#}");
    assert!(
        comments_of(&owner).contains("wrong crew for this task"),
        "{owner:#}"
    );
    let release = &settlement_of(&pair, &leaf)["Release"];
    assert_eq!(release["failure"]["class"], "operator_cancel", "{release}");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "revoked");
    let exclusion = sol_exclusion(&pair, &drain).expect("a cancel excludes its crew");
    assert_eq!(exclusion.source, CrewExclusionSource::LeafReleased);
    assert!(
        exclusion.reason.contains("operator_cancel"),
        "{exclusion:?}"
    );
}

/// An operator who cancels a launched claimed leaf with `--block` asks for the
/// task to stay blocked: the cancel is still typed, but it fails the claim.
#[test]
fn cancelling_a_launched_claimed_leaf_with_block_blocks_its_task() {
    if !isolated(
        module_path!(),
        "cancelling_a_launched_claimed_leaf_with_block_blocks_its_task",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let cancel = pair
        .follower
        .cancel_job_run_with_options_and_policy(
            &leaf,
            "operator",
            "cli",
            Some("needs a human look"),
            false,
            true,
        )
        .expect("cancel the leaf");
    assert_eq!(cancel.outcome, "cancelled", "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");

    assert_eq!(pair.owner_status(&task), "blocked");
    let fail = &settlement_of(&pair, &leaf)["Fail"];
    assert_eq!(fail["failure"]["class"], "operator_cancel", "{fail}");
    assert!(
        fail["failure"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("needs a human look")),
        "{fail}"
    );
}

/// A task released twice within the budget window for failures that were
/// not the candidate's — here its committed candidate conflicting with a
/// moving base, which excludes nothing, so the same drain pulls it again —
/// is blocked by the third, with one comment listing every reason,
/// including an operator's cancel.
#[test]
fn a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason() {
    if !isolated(
        module_path!(),
        "a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.run_drain();
    let reasons = [
        "rebase onto main conflicted in src/f0.rs: first",
        "rebase onto main conflicted in src/f0.rs: second",
    ];

    // An operator's cancel first: released to the backlog, and counted.
    let (cancelled, worker) = pair.running_leaf_with_worker(&drain, 1);
    pair.follower
        .cancel_job_run_with_options_and_policy(
            &cancelled,
            "operator",
            "cli",
            Some("rebalancing hosts"),
            false,
            false,
        )
        .expect("cancel the leaf");
    assert!(worker.stopped());
    assert_eq!(pair.owner_status(&task), "backlog");

    for (n, reason) in reasons.iter().enumerate() {
        // A cancel excludes its crew only for the drain that admitted it;
        // later drains can retry the task while the task-wide budget persists.
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        assert_eq!(
            pair.claimed_task(&leaf),
            task,
            "the same task is pulled again"
        );
        leaf_completed(&pair, &leaf, prepared());
        pair.leaf_fails_with(&leaf, reason);
        let pass = settle_only(&pair, &drain);
        let expected = if n == 0 { "backlog" } else { "blocked" };
        assert_eq!(pair.owner_status(&task), expected, "release {n}: {pass}");
    }

    let owner = pair.owner_task(&task);
    let budget_comments: Vec<&Value> = owner["comments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|comment| {
            comment
                .to_string()
                .contains("released this claim 2 times within 24h")
        })
        .collect();
    assert_eq!(budget_comments.len(), 1, "{owner:#}");
    let comment = budget_comments[0].to_string();
    for reason in reasons {
        assert!(
            comment.contains(reason),
            "every reason is listed: {comment}"
        );
    }
    assert!(
        comment.contains("rebalancing hosts"),
        "the operator cancel is counted: {comment}"
    );
    let phases = pair
        .owner_claims()
        .iter()
        .map(|claim| claim["claim"]["phase"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        phases.iter().filter(|phase| *phase == "revoked").count(),
        2,
        "{phases:?}"
    );
    assert_eq!(
        phases.iter().filter(|phase| *phase == "failed").count(),
        1,
        "{phases:?}"
    );
}

/// The owner keeps a task a drain released for its host's failure from that
/// drain for the rest of its window, even when the drain's next request was
/// built before the release reached it and so excludes no crew. Another
/// drain may still take it.
#[test]
fn the_owner_does_not_hand_a_host_released_task_back_to_the_same_drain() {
    if !isolated(
        module_path!(),
        "the_owner_does_not_hand_a_host_released_task_back_to_the_same_drain",
    ) {
        return;
    }
    let (pair, _, _) = a_released_failure(
        "[validation_environment] required validation could not run: python3 lacks tomllib",
        "environment",
    );
    let task = pair.tasks[0].clone();
    let mut request = pair.wire.calls("orbit.task.pull")[0].clone();
    assert!(request["crews"].get("excluded").is_none(), "{request}");

    request["request_id"] = json!("built-before-the-release");
    let same = pair
        .wire
        .call("", "orbit.task.pull", request.clone())
        .unwrap();
    let receipt = &same["receipt"];
    assert!(receipt["claim"].is_null(), "{same:#}");
    let held = receipt["crew_unavailable"]
        .as_array()
        .and_then(|held| held.iter().find(|held| held["task_id"] == task.as_str()))
        .unwrap_or_else(|| panic!("the task is held from this drain: {same:#}"));
    assert!(
        held["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("environment")),
        "the hold names the release's class: {held}"
    );

    request["request_id"] = json!("another-drain");
    request["run_context"]["run_id"] = json!("jrun-another-drain");
    let other = pair.wire.call("", "orbit.task.pull", request).unwrap();
    assert_eq!(
        other["receipt"]["claim"]["task_id"],
        task.as_str(),
        "{other:#}"
    );
}

/// [ORB-14260] A claimed PR leaf whose committed candidate could not be
/// synchronized onto a base that moved under it releases its claim as a
/// `base_conflict`, keeping the candidate's branch and head rather than
/// blocking the task. The task's next claim carries that candidate into its
/// leaf, whose `resume_candidate` step continues it; a spec change retires
/// it, and the claim after implements fresh.
#[test]
fn a_base_conflict_keeps_the_candidate_and_the_next_claim_resumes_it() {
    if !isolated(
        module_path!(),
        "a_base_conflict_keeps_the_candidate_and_the_next_claim_resumes_it",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    leaf_completed(&pair, &leaf, prepared());
    pair.leaf_fails_with(
        &leaf,
        "sync_base: rebase onto origin/main conflicted in src/f0.rs; conflict recovery could \
         not resolve it",
    );

    let pass = settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    let failure = &settlement_of(&pair, &leaf)["Release"]["failure"];
    assert_eq!(failure["class"], "base_conflict", "{failure}");
    let candidate = &failure["candidate"];
    assert_eq!(candidate["branch"], BRANCH, "{candidate}");
    assert_eq!(candidate["head_sha"], HEAD, "{candidate}");
    assert_eq!(candidate["failed_step_id"], "sync_base", "{candidate}");
    assert_eq!(candidate["source_run_id"], leaf.as_str(), "{candidate}");
    assert!(
        candidate.get("published").is_none(),
        "never pushed: {candidate}"
    );
    assert!(
        comments_of(&pair.owner_task(&task)).contains(BRANCH),
        "the release names the kept candidate: {}",
        pair.owner_task(&task)
    );
    assert_eq!(sol_exclusion(&pair, &drain), None, "no crew is excluded");
    assert_eq!(window(&pair, &drain).host_suppressed, None);

    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), task, "the task is pulled again");
    let resumed = pair
        .admission(&next)
        .receipt
        .and_then(|receipt| receipt.task)
        .and_then(|task| task.resume_candidate)
        .expect("the claim carries the kept candidate");
    assert_eq!(
        (resumed.branch.as_str(), resumed.head_sha.as_str()),
        (BRANCH, HEAD)
    );
    let input = pair
        .follower_jobs
        .get_job_run(&next)
        .unwrap()
        .and_then(|run| run.input)
        .expect("leaf input");
    assert_eq!(input["resume_candidate"]["head_sha"], HEAD, "{input}");
    assert_eq!(input["resume_candidate"]["source_run_id"], leaf.as_str());

    // The task's spec changes while its candidate waits: the candidate no
    // longer answers it.
    pair.leaf_fails_with(&next, "[transient_failure] the network dropped");
    settle_only(&pair, &drain);
    pair.wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "description": "A different change than the candidate made."}),
        )
        .expect("change the task's spec");
    // A new drain: this one's window may have excluded the crew the
    // transient release ran on.
    let fresh_drain = pair.run_drain();
    let fresh = pair.running_leaf(&fresh_drain, 1);
    assert_eq!(pair.claimed_task(&fresh), task);
    assert!(
        pair.admission(&fresh)
            .receipt
            .and_then(|receipt| receipt.task)
            .and_then(|task| task.resume_candidate)
            .is_none(),
        "a spec change retires the kept candidate"
    );
}

/// An operator's `orbit task update --discard-candidate` retires a kept
/// candidate: the task's next claim implements fresh.
#[test]
fn a_discarded_candidate_is_not_resumed() {
    if !isolated(module_path!(), "a_discarded_candidate_is_not_resumed") {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    leaf_completed(&pair, &leaf, prepared());
    pair.leaf_fails_with(&leaf, "rebase onto main conflicted in src/f0.rs");
    settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "backlog");

    pair.wire
        .owner
        .update_task_with_identity(
            &task,
            orbit_core::application::task::TaskUpdateParams {
                discard_candidate: true,
                ..Default::default()
            },
            None,
            None,
        )
        .expect("discard the candidate");
    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), task);
    assert!(
        pair.admission(&next)
            .receipt
            .and_then(|receipt| receipt.task)
            .and_then(|task| task.resume_candidate)
            .is_none(),
        "a discarded candidate is not resumed"
    );
}

/// [ORB-14257] A worker step whose call to the owner never reached it fails
/// typed as an owner-route failure, and the leaf it ends releases its claim
/// as `owner_route` — not as a failure of its candidate.
#[test]
fn a_lost_owner_route_fails_the_step_as_owner_route() {
    if !isolated(
        module_path!(),
        "a_lost_owner_route_fails_the_step_as_owner_route",
    ) {
        return;
    }
    struct Unreachable;
    impl orbit_tools::OwnerCoordinator for Unreachable {
        fn call(
            &self,
            _name: &str,
            _input: Value,
            _session: orbit_types::tool::ToolSessionContext,
        ) -> Result<Value, OrbitError> {
            Err(OrbitError::UnreachableDestination(
                "ssh: connect to host owner port 22: Connection timed out".into(),
            ))
        }
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            orbit_types::tool::WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.clone(),
            },
            Arc::new(Unreachable),
        )
        .unwrap();

    let error = bound
        .run_tool("orbit.task.show", json!({"id": claim.task_id}))
        .expect_err("the owner is unreachable");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "the variant is kept: {error:?}"
    );
    let message = error.to_string();
    assert_eq!(
        ClaimFailureClass::of_step_failure(None, Some(&message)),
        Some(ClaimFailureClass::OwnerRoute),
        "{message}"
    );

    pair.leaf_fails_with(&leaf, &message);
    settle_only(&pair, &drain);
    let failure = &settlement_of(&pair, &leaf)["Release"]["failure"];
    assert_eq!(failure["class"], "owner_route", "{failure}");
    assert_eq!(pair.owner_status(&claim.task_id), "backlog");
}
