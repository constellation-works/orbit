//! [ORB-14257] A launched claimed leaf's settlement carries a typed failure
//! class. External prerequisites hold the task for an operator while retaining
//! release evidence; transient failures still release within a per-task budget.

use orbit_core::application::job::JobRunCancelOutcome;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_core::application::distributed::{
    FollowerRunObserver, LeafHydrationOutcome, LeafHydrationRequest, PullCrewWindow,
};
use orbit_store::contracts::ClaimSettlementKind;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, ClaimFailureClass, CrewExclusion,
    CrewExclusionSource, FORGE_UNAVAILABLE_ERROR_CODE, ForgeUnavailableHold,
    PROVIDER_FAILURE_HOLD_EVENT, ProviderFailureClass, ProviderFailureHold,
};

use super::*;

/// The candidate branch and head the claimed leaves below commit.
const BRANCH: &str = "orbit/claimed-candidate";
const HEAD: &str = "c0ffee0000000000000000000000000000000001";
/// The error a `claude` leaf fails with when its provider refuses the login.
const UNAUTHORIZED: &str =
    "[provider_unavailable] claude provider authentication failure (HTTP 401)";

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

/// [`prepared`] and what a leaf leaves once it has pushed its branch and
/// opened pull request 42, with its before-landing review admitted.
fn pushed() -> Value {
    let mut outputs = prepared();
    outputs["sync_base"] = json!({"head": BRANCH, "head_sha": HEAD});
    outputs["review_gate_admit"] = json!({"applies": false});
    outputs["review_gate_settle"] = json!({"applies": false});
    outputs["validate"] = json!({"decision": "passed"});
    outputs["push"] = json!({"branch": BRANCH, "local_sha": HEAD});
    outputs["pr_open"] = json!({"pr_number": "42"});
    outputs["landing_review_gate_admit"] = json!({"applies": true});
    outputs
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

/// `leaf` fails with `error` and one settle-only pass settles it.
fn fail_and_settle(pair: &Pair, drain: &str, leaf: &str, error: &str) -> Value {
    pair.leaf_fails_with(leaf, error);
    settle_only(pair, drain)
}

/// An owner with one backlog task per entry of `crews`, and a fresh drain
/// whose launched leaf claimed the first: the pair, drain, leaf and task.
fn claimed_on(crews: &[Option<&str>]) -> (Pair, String, String, String) {
    let pair = Pair::with_crews(crews);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    (pair, drain, leaf, task)
}

/// [`claimed_on`] for one task on the `sol` crew.
fn claimed_sol() -> (Pair, String, String, String) {
    claimed_on(&[Some("sol")])
}

/// The branch and head the claim of `leaf` carries to resume, if it carries one.
fn kept_candidate(pair: &Pair, leaf: &str) -> Option<(String, String)> {
    pair.admission(leaf)
        .receipt
        .and_then(|receipt| receipt.task)
        .and_then(|task| task.resume_candidate)
        .map(|candidate| (candidate.branch, candidate.head_sha))
}

/// `leaf`'s claim resumes the candidate [`prepared`] committed.
fn assert_resumes_candidate(pair: &Pair, leaf: &str, context: &str) {
    assert_eq!(
        kept_candidate(pair, leaf),
        Some((BRANCH.into(), HEAD.into())),
        "{context}: the claim carries the kept candidate"
    );
}

/// The first pull request of `pair`'s drain, replayed under `id`, as a drain
/// built it before an earlier release reached the owner. `another_drain`
/// sends it as a different drain's request.
fn replay_pull(pair: &Pair, id: &str, another_drain: bool) -> Value {
    let mut request = pair.wire.calls("orbit.task.pull")[0].clone();
    request["request_id"] = json!(id);
    if another_drain {
        request["run_context"]["run_id"] = json!(format!("jrun-{id}"));
    }
    pair.wire.call("", "orbit.task.pull", request).unwrap()
}

/// The entry of a pull's receipt `list` (say `crew_unavailable`) holding `task`.
fn held_entry<'a>(pull: &'a Value, list: &str, task: &str) -> Option<&'a Value> {
    pull["receipt"][list]
        .as_array()?
        .iter()
        .find(|held| held["task_id"] == task)
}

/// Cancel `leaf` as an operator does with `orbit run cancel`.
fn cancel_leaf(pair: &Pair, leaf: &str, reason: &str, block: bool) {
    let cancel = pair
        .follower
        .cancel_job_run_with_options_and_policy(leaf, "operator", "cli", Some(reason), false, block)
        .expect("cancel the leaf");
    assert_eq!(cancel.outcome, JobRunCancelOutcome::Cancelled, "{cancel:?}");
}

/// End `leaf` at its `step_index`th step, activity `target`, in `state`.
fn end_leaf_at(
    pair: &Pair,
    leaf: &str,
    (step_index, target): (usize, &str),
    state: JobRunState,
    error_code: Option<&str>,
    message: String,
) {
    let now = Utc::now();
    pair.follower_jobs
        .complete_job_run_step(
            leaf,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index,
                target_type: JobTargetType::Activity,
                target_id: target.into(),
                started_at: now,
                finished_at: now,
                duration_ms: None,
                exit_code: (state == JobRunState::Failed).then_some(1),
                agent_response_json: None,
                state,
                error_code: error_code.map(Into::into),
                error_message: Some(message),
            },
        )
        .unwrap();
    pair.follower_jobs
        .finalize_job_run(leaf, state, now, None)
        .unwrap();
}

/// `runtime` bound to `leaf`'s claim, as the leaf's worker is, calling the
/// owner through `route`.
fn bound_to(
    runtime: &OrbitRuntime,
    pair: &Pair,
    leaf: &str,
    route: Arc<dyn OwnerCoordinator>,
) -> OrbitRuntime {
    let record = pair.admission(leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    runtime
        .clone()
        .with_worker_invocation(
            orbit_types::tool::WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.into(),
            },
            route,
        )
        .unwrap()
}

/// ORB-14717: settle the actual handoff fetch error as the last failed
/// step of a claimed leaf. A timeout releases finished work; an unknown
/// remote ref retains the existing candidate failure and blocks the task.
fn handoff_fetch_timeout_releases_the_owner_but_missing_ref_still_blocks() {
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
        end_leaf_at(
            pair,
            &leaf.leaf,
            (step_index, "claim_handoff"),
            JobRunState::Failed,
            None,
            error,
        );
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

/// A claimed leaf on the first of the owner's `crews` that fails on `error`
/// after `progress` recorded what it did, settled by one pass: the owner's
/// task returns to the backlog, and the release names `class`. Returns that
/// settle-only pass and the drain, for the caller's window assertions.
fn released(
    crews: &[Option<&str>],
    error: &str,
    class: &str,
    progress: impl Fn(&Pair, &str),
) -> (Pair, String, Value) {
    let (pair, drain, leaf, task) = claimed_on(crews);
    assert_eq!(task, pair.tasks[0], "the `sol` task is claimed first");
    progress(&pair, &leaf);
    let pass = fail_and_settle(&pair, &drain, &leaf, error);
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
    assert_eq!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "revoked"
    );
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

/// [`released`] for one `sol` task and a leaf that did nothing before it failed.
fn a_released_failure(error: &str, class: &str) -> (Pair, String, Value) {
    released(&[Some("sol")], error, class, |_, _| {})
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
fn crew_failures_release_the_claim_and_exclude_the_crew_for_the_window() {
    for (error, class) in [
        (
            "[transient_failure] action pin check could not connect (status 000)",
            "transient",
        ),
        (UNAUTHORIZED, "provider"),
    ] {
        let after_push = class == "transient";
        let (pair, drain, _) = released(&[Some("sol")], error, class, |pair, leaf| {
            if after_push {
                let mut outputs = pushed();
                outputs["landing_review_gate_admit"] = json!({"applies": false});
                outputs["landing_review_gate_settle"] = json!({"applies": false});
                outputs["landing_review_validate"] = json!({"decision": "passed"});
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
        if after_push {
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

/// An Orbit upgrade replaced the follower's `orbit` while a claimed leaf's
/// before-landing reviewer ran, and its agent declared the refusal under one
/// of the kinds agents gave it. The claim releases as `transient`, as a leaf
/// interrupted at a step boundary by the same upgrade does: the task returns
/// to the backlog rather than blocked, the pushed candidate rides on the
/// release, and the task's next claim resumes it.
fn an_upgrade_refused_agent_releases_the_claim_and_the_next_claim_resumes_it() {
    for kind in [
        "upgrade_admission_refused",
        "orbit_upgrade_admission_refused",
        "orbit.generation_switch_pending",
    ] {
        let blocker = orbit_types::workflow::AgentBlocker {
            kind: kind.to_string(),
            evidence: "cannot record the pending generation switch: Operation not permitted \
                       (os error 1)"
                .to_string(),
        };
        let error = format!(
            "{}\noriginal error before recovery: the review report was never recorded",
            blocker.step_failure_message()
        );
        let (pair, _, _) = released(&[Some("sol")], &error, "transient", |pair, leaf| {
            leaf_completed(pair, leaf, pushed());
        });
        let leaf = pair.leaf_runs()[0].clone();
        let candidate = &settlement_of(&pair, &leaf)["Release"]["failure"]["candidate"];
        assert_eq!(candidate["branch"], BRANCH, "{kind}: {candidate}");
        assert_eq!(candidate["head_sha"], HEAD, "{kind}: {candidate}");
        assert_eq!(candidate["pull_request"], "42", "{kind}: {candidate}");

        // A new drain: this one's window excluded the crew the transient
        // release ran on.
        let drain = pair.run_drain();
        let next = pair.running_leaf(&drain, 1);
        assert_eq!(
            pair.claimed_task(&next),
            pair.tasks[0],
            "{kind}: pulled again"
        );
        assert_resumes_candidate(&pair, &next, kind);
    }
}

/// An upgrade's designed pause of a claimed leaf is accounted apart from
/// failures, from the leaf's recorded step code; a leaf interrupted any other
/// way stays a failure. The claim is released to the backlog either way.
fn an_upgrade_quiesced_leaf_settles_outside_the_failure_count() {
    let (pair, drain, leaf, task) = claimed_sol();
    pair.follower.record_upgrade_interruption_for_test(
        &leaf,
        4242,
        orbit_common::fs::generation::ParticipantRole::Drain,
    );
    assert_eq!(pair.run_state(&leaf), JobRunState::Interrupted);
    settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "backlog");
    let settlements = pair.wire.owner.leaf_settlements(None, false).unwrap();
    let [settlement] = settlements.as_slice() else {
        panic!("one settled claim: {settlements:?}");
    };
    assert_eq!(settlement.failure_class, Some(ClaimFailureClass::Transient));
    assert_eq!(
        serde_json::to_value(settlement).unwrap()["accounting"],
        json!({"outcome": "upgrade_paused", "counts_as_failure": false, "reason": "upgrade_quiesce"}),
    );

    let (pair, drain, leaf, _) = claimed_sol();
    end_leaf_at(
        &pair,
        &leaf,
        (1, "implement_one"),
        JobRunState::Interrupted,
        Some("process_not_found"),
        "the worker process is gone: reason=process_not_found".into(),
    );
    settle_only(&pair, &drain);
    let settlements = pair.wire.owner.leaf_settlements(None, false).unwrap();
    let [settlement] = settlements.as_slice() else {
        panic!("one settled claim: {settlements:?}");
    };
    assert_eq!(settlement.failure_class, Some(ClaimFailureClass::Transient));
    assert_eq!(
        serde_json::to_value(settlement).unwrap()["accounting"],
        json!({"outcome": "failure", "counts_as_failure": true, "reason": "settlement_failure"}),
    );
}

/// [ORB-14439] The owner's run history never holds a follower's leaf, so the
/// owner keeps what the leaf's settlement said: a failure scan on the owner
/// reads a follower's provider outage, typed, from the owner's own store.
fn the_owner_lists_a_follower_leaf_release_with_its_evidence_class() {
    let (pair, _, _) = a_released_failure(UNAUTHORIZED, "provider");
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
    // [ORB-20906] The leaf's job, build and failing step reach the owner with
    // the settlement, from the leaf's own run record, though the owner's run
    // history never holds that run.
    let run = pair.follower_jobs.get_job_run(&leaf).unwrap().unwrap();
    assert!(
        pair.wire
            .owner
            .run_tool("orbit.workflow.run.show", json!({"id": leaf}))
            .is_err(),
        "the owner holds no record of the follower's leaf"
    );
    let evidence = &settlement.run_evidence;
    assert_eq!(evidence.provenance, Some("settlement"), "{evidence:?}");
    assert_eq!(evidence.job_id.as_deref(), Some(run.job_id.as_str()));
    assert_eq!(evidence.build, run.build, "the build the run recorded");
    assert_eq!(evidence.build_state, "recorded");
    let cause = evidence.root_cause.as_ref().expect("the failing step");
    assert_eq!(cause.step.as_deref(), Some("implement_one"), "{cause:?}");
    assert!(
        cause
            .message
            .as_deref()
            .is_some_and(|message| message.contains("HTTP 401")),
        "{cause:?}"
    );
    assert_eq!(evidence.root_cause_state, "recorded");

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

/// An operator's read of one follower run record, through the follower's own
/// `orbit.workflow.run.observe`; `alter` stands in for a follower that answers
/// otherwise.
type Alter<'a> = Box<dyn Fn(&str, Value) -> Result<Value, OrbitError> + 'a>;

struct FollowerShow<'a> {
    pair: &'a Pair,
    alter: Alter<'a>,
    asked: std::cell::RefCell<Vec<(String, String)>>,
}

impl<'a> FollowerShow<'a> {
    fn new(pair: &'a Pair) -> Self {
        Self::altered(pair, |_, answer| Ok(answer))
    }

    fn altered(
        pair: &'a Pair,
        alter: impl Fn(&str, Value) -> Result<Value, OrbitError> + 'a,
    ) -> Self {
        Self {
            pair,
            alter: Box::new(alter),
            asked: Default::default(),
        }
    }

    /// A follower that answers as it does, except `edit` rewrites the answer
    /// about its drain run (`of_drain`) or about a leaf run (otherwise).
    fn edited(pair: &'a Pair, of_drain: bool, edit: fn(&mut Value)) -> Self {
        Self::altered(pair, move |_, mut answer| {
            if (answer["job_id"] == "workspace_pull_pipeline") == of_drain {
                edit(&mut answer);
            }
            Ok(answer)
        })
    }
}

impl FollowerRunObserver for FollowerShow<'_> {
    fn observe_run(&self, machine_id: &str, run_id: &str) -> Result<Value, OrbitError> {
        self.asked
            .borrow_mut()
            .push((machine_id.to_string(), run_id.to_string()));
        let mut answer = self.pair.follower.run_tool_with_context_and_role(
            "orbit.workflow.run.observe",
            json!({"id": run_id}),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )?;
        // The fixture's drain run is created without its input and host; a
        // real one records the destination it pulls from and where it ran.
        if answer["job_id"] == "workspace_pull_pipeline" {
            answer["input"] = json!({"destination": self.pair.destination});
            answer["executed_on"] = json!({"machine_id": self.pair.wire.caller});
        }
        (self.alter)(run_id, answer)
    }
}

/// What a follower that holds no run `run` answers.
fn no_such_run(run: &str) -> OrbitError {
    OrbitError::NotFound {
        kind: orbit_common::NotFoundKind::JobRun,
        id: run.to_string(),
        guidance: None,
    }
}

/// The settled claim's evidence as the owner lists it.
fn run_evidence_of(pair: &Pair) -> orbit_core::application::distributed::LeafRunEvidenceView {
    let [settlement] = &pair.wire.owner.leaf_settlements(None, false).unwrap()[..] else {
        panic!("one settled claim");
    };
    settlement.run_evidence.clone()
}

/// Settle `claims` claims as a build that predates the evidence: the owner
/// holds the settlements and none of the leaves' run records.
fn forget_leaf_evidence(pair: &Pair, claims: usize) {
    let connection = pair.wire.owner.sqlite_store().unwrap().connection();
    let removed = connection
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM task_coordination_rows WHERE kind = 'distributed-leaf-run-evidence-v1'",
            (),
        )
        .unwrap();
    assert_eq!(removed, claims, "each settlement recorded the evidence");
}

fn hydrate(
    pair: &Pair,
    observer: &FollowerShow<'_>,
    recheck: bool,
) -> Vec<(LeafHydrationOutcome, Option<String>)> {
    let report = pair
        .wire
        .owner
        .hydrate_leaf_run_evidence(
            &LeafHydrationRequest {
                recheck,
                ..LeafHydrationRequest::default()
            },
            observer,
        )
        .unwrap();
    assert_eq!(report.remaining, 0);
    report
        .rows
        .into_iter()
        .map(|row| (row.outcome, row.detail))
        .collect()
}

/// [ORB-20906] A claim settled before the follower sent its leaf's run
/// record has none of it: the owner does not guess it, from its own build or
/// from the settlement's text. An operator keeps what the follower's run
/// record says, once, as evidence distinct from the settlement; a replay
/// confirms it, a different observation never overwrites it, and a record
/// that is not this claim's leaf is refused. A worker bound to a claim is
/// refused hydration, which reads run records with an operator's own route.
fn the_owner_hydrates_a_legacy_leaf_settlement_from_its_follower_once() {
    let (pair, _, _) = a_released_failure(UNAUTHORIZED, "provider");
    let leaf = pair.leaf_runs()[0].clone();
    let sent = run_evidence_of(&pair);
    assert_eq!(sent.provenance, Some("settlement"));
    let claim = pair.owner_claim_for_leaf(&leaf);
    let task = pair.owner_task(&pair.tasks[0]);

    // The reproduced gap: nothing on the owner says what the leaf ran.
    forget_leaf_evidence(&pair, 1);
    let legacy = run_evidence_of(&pair);
    assert_eq!(legacy.provenance, None);
    assert_eq!(
        (legacy.build_state, legacy.root_cause_state),
        ("unrecorded", "unrecorded")
    );
    assert!(legacy.job_id.is_none() && legacy.build.is_none() && legacy.root_cause.is_none());
    assert!(
        pair.wire
            .owner
            .leaf_settlements(None, false)
            .unwrap()
            .iter()
            .all(|settlement| settlement.run_evidence.build.is_none()),
        "no current build is substituted"
    );

    // A bound worker is refused.
    let worker = bound_to(&pair.wire.owner, &pair, &leaf, Arc::new(NoWorkerRoute));
    let observer = FollowerShow::new(&pair);
    let error = worker
        .hydrate_leaf_run_evidence(&LeafHydrationRequest::default(), &observer)
        .unwrap_err();
    assert!(
        matches!(error, OrbitError::CapabilityRefused(_)),
        "{error:?}"
    );
    assert!(observer.asked.borrow().is_empty());
    assert_eq!(run_evidence_of(&pair).provenance, None);

    // Refusals leave the claim without evidence.
    let machine = pair.wire.caller.clone();
    let leaf_edit = |edit: fn(&mut Value)| FollowerShow::edited(&pair, false, edit);
    let drain_edit = |edit: fn(&mut Value)| FollowerShow::edited(&pair, true, edit);
    let cases: Vec<(&str, LeafHydrationOutcome, FollowerShow<'_>)> = vec![
        (
            "another machine's run",
            LeafHydrationOutcome::Mismatch,
            leaf_edit(|answer| answer["executed_on"]["machine_id"] = json!("hm_other")),
        ),
        (
            "another task's run",
            LeafHydrationOutcome::Mismatch,
            leaf_edit(|answer| answer["task_ids"] = json!(["ORB-9"])),
        ),
        (
            "not a claimed leaf job",
            LeafHydrationOutcome::Mismatch,
            leaf_edit(|answer| answer["job_id"] = json!("some_other_job")),
        ),
        (
            "a run still going",
            LeafHydrationOutcome::Mismatch,
            leaf_edit(|answer| answer["state"] = json!("running")),
        ),
        (
            "a drain that pulls from another owner",
            LeafHydrationOutcome::Mismatch,
            drain_edit(|answer| {
                answer["input"]["destination"]["owner_workspace_id"] = json!("ws_other")
            }),
        ),
        (
            "a drain of another machine",
            LeafHydrationOutcome::Mismatch,
            drain_edit(|answer| answer["executed_on"]["machine_id"] = json!("hm_other")),
        ),
        (
            "a follower that holds no such run",
            LeafHydrationOutcome::Absent,
            FollowerShow::altered(&pair, |run, _| Err(no_such_run(run))),
        ),
        (
            "a follower that cannot be reached",
            LeafHydrationOutcome::Unreachable,
            FollowerShow::altered(&pair, |_, _| {
                Err(OrbitError::UnreachableDestination(
                    "connection refused".into(),
                ))
            }),
        ),
    ];
    for (name, expected, observer) in cases {
        let rows = hydrate(&pair, &observer, false);
        assert_eq!(rows.len(), 1, "{name}: {rows:?}");
        assert_eq!(rows[0].0, expected, "{name}: {rows:?}");
        assert!(rows[0].1.is_some(), "{name}: says why");
        assert_eq!(
            run_evidence_of(&pair).provenance,
            None,
            "{name}: nothing was recorded"
        );
        assert!(
            observer
                .asked
                .borrow()
                .iter()
                .all(|(host, _)| *host == machine),
            "{name}: only the claim's own follower is asked"
        );
    }

    // The follower's record is kept, as hydrated evidence.
    let observer = FollowerShow::new(&pair);
    assert_eq!(
        hydrate(&pair, &observer, false),
        vec![(LeafHydrationOutcome::Hydrated, None)]
    );
    let hydrated = run_evidence_of(&pair);
    assert_eq!(hydrated.provenance, Some("hydrated"));
    assert_eq!(hydrated.job_id, sent.job_id);
    assert_eq!(hydrated.build, sent.build);
    assert_eq!(hydrated.build_state, sent.build_state);
    assert_eq!(hydrated.root_cause, sent.root_cause);
    assert_eq!(hydrated.root_cause_state, "recorded");

    // Hydration changed no claim, task or run.
    assert_eq!(pair.owner_claim_for_leaf(&leaf)["claim"], claim["claim"]);
    assert_eq!(pair.owner_task(&pair.tasks[0])["status"], task["status"]);
    assert_eq!(pair.leaf_runs(), vec![leaf.clone()]);

    // A claim holding evidence is not observed again unless asked.
    assert!(
        pair.wire
            .owner
            .hydrate_leaf_run_evidence(&LeafHydrationRequest::default(), &observer)
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        hydrate(&pair, &observer, true),
        vec![(LeafHydrationOutcome::Replayed, None)],
        "an identical replay is confirmed"
    );
    let changed = FollowerShow::edited(&pair, false, |answer| {
        answer["error_message"] = json!("a different failure")
    });
    let rows = hydrate(&pair, &changed, true);
    assert_eq!(rows[0].0, LeafHydrationOutcome::Conflict, "{rows:?}");
    assert_eq!(
        run_evidence_of(&pair),
        hydrated,
        "a conflicting observation never overwrites the held evidence"
    );
}

/// Hydration must not reconcile either follower record while gathering evidence.
fn hydration_observes_stored_runs_without_reconciliation() {
    for orphan in [true, false] {
        let (pair, drain, _) = a_released_failure(UNAUTHORIZED, "provider");
        let leaf = pair.leaf_runs()[0].clone();
        forget_leaf_evidence(&pair, 1);
        let store = pair.follower.sqlite_store().unwrap();
        let workspace = pair.follower.workspace_id().unwrap();
        // Leave a run unfinished, and, when `orphaned`, running under a dead process.
        let unfinish = |id: &str, orphaned: bool| {
            let mut run = pair.follower_jobs.get_job_run(id).unwrap().unwrap();
            run.finished_at = None;
            run.duration_ms = None;
            if orphaned {
                run.state = JobRunState::Running;
                run.started_at = Some(Utc::now() - chrono::Duration::minutes(5));
                run.pid = Some(u32::MAX);
                run.pid_start_time = None;
            }
            store
                .upsert_job_run_for_workspace(&workspace, &run, None)
                .unwrap();
        };
        unfinish(&leaf, orphan);
        unfinish(&drain, true);
        let run_json =
            |id: &str| serde_json::to_value(pair.follower_jobs.get_job_run(id).unwrap()).unwrap();
        let (before_leaf, before_drain) = (run_json(&leaf), run_json(&drain));
        let task = pair.owner_task(&pair.tasks[0]);
        let reservations = || {
            let conn = store.connection();
            let conn = conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT reservation_id, released_at FROM task_reservations ORDER BY reservation_id").unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .unwrap();
            rows.map(Result::unwrap).collect::<Vec<_>>()
        };
        {
            let conn = store.connection();
            conn.lock().unwrap().execute(
                "INSERT INTO task_reservations (reservation_id, workspace_orbit_dir, workspace_id, task_ids_json, files_json, actor, created_at, expires_at, owner_run_id) VALUES (?1, ?2, ?3, ?4, '[]', 'fixture', '2020-01-01T00:00:00Z', '2999-01-01T00:00:00Z', ?5)",
                rusqlite::params!["fixture-reservation", pair.follower_repo.join(".orbit").to_string_lossy().as_ref(), workspace, json!([pair.tasks[0]]).to_string(), leaf],
            ).unwrap();
        }
        let before_reservations = reservations();
        assert_eq!(
            before_reservations,
            vec![("fixture-reservation".to_string(), None)]
        );
        let denied = pair.follower.run_tool_with_context_and_role(
            "orbit.workflow.run.observe",
            json!({"id": leaf}),
            Role::Admin,
            ToolContext::default(),
        );
        assert!(
            denied.is_err(),
            "an agent without operator authority is refused"
        );
        let observer = FollowerShow::new(&pair);
        let result = hydrate(&pair, &observer, false);
        assert_eq!(
            result[0].0,
            if orphan {
                LeafHydrationOutcome::Mismatch
            } else {
                LeafHydrationOutcome::Hydrated
            }
        );
        assert_eq!(run_json(&leaf), before_leaf);
        assert_eq!(run_json(&drain), before_drain);
        assert_eq!(pair.owner_task(&pair.tasks[0]), task);
        assert_eq!(reservations(), before_reservations);
    }
}

/// [ORB-20936] Bounded hydration traverses its whole selection by
/// continuation: with `limit=1`, a later available claim is reached after an
/// earlier absent one, and `--recheck` paging visits every claim instead of
/// only the first page forever.
fn hydration_continuation_visits_every_selected_claim_at_limit_one() {
    let pair = Pair::with_crews(&[Some("sol"), Some("sol")]);
    let mut leaves = vec![];
    for _ in 0..2 {
        // A fresh drain: the previous release excluded `sol` from the old window.
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        fail_and_settle(&pair, &drain, &leaf, UNAUTHORIZED);
        leaves.push(leaf);
    }
    let [first, second] = &leaves[..] else {
        unreachable!()
    };
    assert_ne!(second, first);

    let settlements = pair.wire.owner.leaf_settlements(None, false).unwrap();
    assert_eq!(settlements.len(), 2, "{settlements:?}");
    let selected: BTreeSet<String> = settlements.iter().map(|s| s.claim_id.clone()).collect();
    forget_leaf_evidence(&pair, 2);

    let claims = || -> Vec<Value> {
        pair.owner_claims()
            .iter()
            .map(|entry| entry["claim"].clone())
            .collect()
    };
    let statuses =
        || -> Vec<String> { pair.tasks.iter().map(|id| pair.owner_status(id)).collect() };
    let (claims_before, statuses_before) = (claims(), statuses());

    // One leaf's run is gone from its follower; the other's is available,
    // whichever order the newest-first selection visits them in.
    let absent_leaf = first.clone();
    let observer = FollowerShow::altered(&pair, move |run, answer| {
        if run == absent_leaf {
            Err(no_such_run(run))
        } else {
            Ok(answer)
        }
    });
    let page = |after: Option<String>, recheck: bool| {
        pair.wire
            .owner
            .hydrate_leaf_run_evidence(
                &LeafHydrationRequest {
                    limit: Some(1),
                    recheck,
                    after_claim_id: after,
                    ..LeafHydrationRequest::default()
                },
                &observer,
            )
            .unwrap()
    };

    // Two bounded pages visit both claims exactly once, whether they hydrate
    // (`recheck` false) or, once hydrated, replay (`recheck`).
    for (recheck, kept) in [(false, "Hydrated"), (true, "Replayed")] {
        let page1 = page(None, recheck);
        assert_eq!(page1.rows.len(), 1, "{page1:?}");
        assert_eq!(page1.remaining, 1, "{page1:?}");
        let cursor = page1
            .next_after_claim_id
            .clone()
            .expect("a continuation while claims remain");
        assert_eq!(cursor, page1.rows[0].claim_id);
        let page2 = page(Some(cursor), recheck);
        assert_eq!(page2.rows.len(), 1, "{page2:?}");
        assert_eq!(page2.remaining, 0, "{page2:?}");
        assert_eq!(page2.next_after_claim_id, None, "{page2:?}");
        assert_ne!(page1.rows[0].claim_id, page2.rows[0].claim_id);
        let rows = || [&page1, &page2].into_iter().flat_map(|page| &page.rows);
        let visited: BTreeSet<String> = rows().map(|row| row.claim_id.clone()).collect();
        assert_eq!(visited, selected, "every selected claim is visited");
        let outcomes: BTreeSet<String> = rows().map(|row| format!("{:?}", row.outcome)).collect();
        assert_eq!(
            outcomes,
            BTreeSet::from(["Absent".to_string(), kept.to_string()]),
            "the unsuccessful outcome stays visible and the later claim is {kept}"
        );
        assert!(
            rows()
                .filter(|row| row.outcome == LeafHydrationOutcome::Absent)
                .all(|row| row.detail.is_some()),
            "the absent row says why"
        );
    }

    // A continuation outside the selection is refused, not silently restarted.
    let error = pair
        .wire
        .owner
        .hydrate_leaf_run_evidence(
            &LeafHydrationRequest {
                limit: Some(1),
                after_claim_id: Some("claim_not_selected".into()),
                ..LeafHydrationRequest::default()
            },
            &observer,
        )
        .unwrap_err();
    assert!(matches!(error, OrbitError::InvalidInput(_)), "{error:?}");

    assert_eq!(claims(), claims_before, "no claim changes");
    assert_eq!(statuses(), statuses_before, "no task changes");
    assert_eq!(pair.leaf_runs().len(), 2, "no run changes");
}

/// [ORB-20906] A settlement whose leaf run report is not the claim's own
/// (another run, another job's identity, nothing to name) settles the claim
/// as always and records no evidence: the owner never binds a report it
/// cannot tie to the run the claim bound.
fn a_settlement_naming_another_leaf_run_records_no_evidence() {
    for another_run in [true, false] {
        let (pair, drain, leaf, task) = claimed_sol();
        let report = if another_run {
            json!({"run_id": "someone-elses-leaf", "job_id": "task_claimed_pr_pipeline"})
        } else {
            json!({"run_id": leaf, "job_id": "  "})
        };
        *pair.wire.leaf_run_override.lock().unwrap() = Some(report.clone());
        fail_and_settle(
            &pair,
            &drain,
            &leaf,
            "[provider_unavailable] claude provider outage (HTTP 401)",
        );
        assert_eq!(pair.owner_status(&task), "backlog");
        assert_eq!(
            pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
            "revoked",
            "{report}: the claim settles"
        );
        let evidence = run_evidence_of(&pair);
        assert_eq!(evidence.provenance, None, "{report}: {evidence:?}");
        assert_eq!(evidence.build_state, "unrecorded");
        assert_eq!(evidence.job_id, None);
    }
}

/// A failure of the host itself — its validation environment, or its route
/// to the owner — releases the claim and suppresses the host for the drain's
/// window, whatever crew a task names: the drain asks the owner for nothing
/// more, and the owner holds every task from that drain even when its
/// request was built before the release reached it. Other drains may take
/// unrelated work; only an operator can requeue the held task.
fn host_failures_hold_the_task_and_suppress_the_host_for_the_window() {
    for (error, class) in [
        (
            "[validation_environment] required validation 'make ci-fast' could not run: \
             python3 lacks tomllib",
            "environment",
        ),
        (
            "[validation_route_unavailable] required check needs the owner validation lane",
            "environment",
        ),
        // An old-shape step record: no typed code field, only the
        // bracketed marker in its message. It still classifies through
        // the documented legacy fallback at this public boundary.
        (
            "[owner_route_unavailable] owner unavailable: the owner could not be reached from \
             the sandboxed worker",
            "owner_route",
        ),
    ] {
        let (pair, drain, leaf, task) = claimed_on(&[Some("sol"), Some("luna")]);
        leaf_completed(&pair, &leaf, prepared());
        let pass = fail_and_settle(&pair, &drain, &leaf, error);
        assert_eq!(pair.owner_status(&task), "blocked", "{class}: {pass}");
        let settlement = settlement_of(&pair, &leaf);
        assert_eq!(settlement["Release"]["failure"]["class"], class);
        assert_eq!(pair.owner_claim_for_leaf(&leaf)["claim"]["phase"], "failed");
        assert!(
            pair.wire
                .owner
                .blocked_recovery_view(Utc::now())
                .unwrap()
                .is_empty(),
            "operator prerequisites must not spend another recovery agent"
        );
        let attention = pair
            .wire
            .owner
            .execute_tool_command_dispatch_with_session_context(
                "orbit.attention",
                json!({"kind": "blocked"}),
                None,
                None,
                orbit_core::adapter::command::ToolEntryPoint::Mcp,
                ToolSessionContext {
                    effective_capabilities: [orbit_types::tool::McpCapability::Operator].into(),
                    ..Default::default()
                },
            )
            .map(|outcome| outcome.value)
            .expect("blocked attention");
        let item = attention["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["refs"]["task_id"] == task)
            .unwrap_or_else(|| panic!("held task needs operator attention: {attention:#}"));
        let reason = settlement["Release"]["failure"]["reason"].as_str().unwrap();
        assert!(
            item["reason"]
                .as_str()
                .is_some_and(|summary| summary.contains(reason)),
            "attention preserves the leaf reason: {item}"
        );
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
        let first = pair.wire.calls("orbit.task.pull")[0].clone();
        assert!(first["crews"].get("excluded").is_none(), "{class}: {first}");
        let same = replay_pull(&pair, &format!("built-before-the-{class}-release"), false);
        assert!(same["receipt"]["claim"].is_null(), "{class}: {same:#}");
        let held_reason = |task: &String| {
            held_entry(&same, "crew_unavailable", task)
                .and_then(|held| held["reason"].as_str())
                .map(str::to_string)
        };
        assert!(
            held_reason(&pair.tasks[1]).is_some_and(|reason| reason.contains("host is suppressed")),
            "{class}: the other crew's task is held from this drain too: {same:#}"
        );
        let other = replay_pull(&pair, &format!("another-drain-{class}"), true);
        assert_eq!(
            other["receipt"]["claim"]["task_id"], pair.tasks[1],
            "{class}: another drain gets unrelated work, never the held task: {other:#}"
        );
        assert_eq!(pair.owner_status(&task), "blocked");
        pair.wire
            .owner
            .requeue_blocked_task(&task, "operator resolved the prerequisite", false)
            .expect("operator requeues the held candidate");
        let resumed = replay_pull(&pair, &format!("operator-requeued-{class}"), true);
        assert_eq!(resumed["receipt"]["claim"]["task_id"], task, "{resumed:#}");
    }
}

/// A red base is not the host's crew either: the claim is released and the
/// crew keeps running. (`admission` covers the owner holding the task until
/// the base turns green.)
fn a_baseline_red_failure_releases_the_claim_without_excluding_the_crew() {
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: "b0".into(),
        command: "make ci-fast".into(),
        run_id: String::new(),
        selection: None,
        failures: Default::default(),
    };
    let (pair, drain, _) = a_released_failure(
        &hold.text("`make ci-fast` fails on the base as well: rustfmt diff in listing.rs"),
        "baseline_red",
    );
    assert_eq!(sol_exclusion(&pair, &drain), None);
    assert_eq!(window(&pair, &drain).host_suppressed, None);
}

/// [ORB-15956] A leaf whose base was red when its gate ran, but whose base
/// has since passed the same command, releases without a baseline hold and
/// keeps its candidate: the task's next claim resumes it on the green base
/// instead of waiting on a hold that no longer applies.
#[test]
fn a_leaf_whose_red_base_turned_green_keeps_its_candidate_without_a_hold() {
    if !isolated(
        module_path!(),
        "a_leaf_whose_red_base_turned_green_keeps_its_candidate_without_a_hold",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let repo = &pair.follower_repo;
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    std::fs::write(repo.join("Makefile"), "ci-lint:\n\t@exit 2\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "red base"]);
    let red = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    std::fs::write(repo.join("Makefile"), "ci-lint:\n\t@echo lint-ok\n").unwrap();
    git(repo, &["commit", "-q", "-am", "fix lint"]);
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: red.clone(),
        command: "make ci-lint".into(),
        run_id: String::new(),
        selection: None,
        failures: Default::default(),
    };
    // The host has already checked the moved base, as its hold refresh does;
    // settlement only reads that recorded result.
    assert!(matches!(
        orbit_engine::baseline_hold_status(&pair.follower, repo, &hold),
        orbit_engine::BaselineHoldStatus::Lifted(_)
    ));
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    leaf_completed(&pair, &leaf, prepared());
    let pass = fail_and_settle(
        &pair,
        &drain,
        &leaf,
        &hold.text(&format!(
            "required validation 'make ci-lint' fails on base {red} exactly as on the candidate"
        )),
    );
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    let release = &settlement_of(&pair, &leaf)["Release"];
    assert!(
        release["baseline_red"].is_null(),
        "a green base holds nothing: {release}"
    );
    let candidate = &release["failure"]["candidate"];
    assert_eq!(candidate["head_sha"], HEAD, "{release}");
    let owner_task = pair.owner_task(&task);
    assert!(
        !owner_task["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == BASELINE_RED_HOLD_EVENT),
        "{owner_task:#}"
    );

    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), task, "the task is pulled again");
    assert_resumes_candidate(&pair, &next, "red base turned green");
}

/// [ORB-14617] A claimed leaf whose delivery push the forge kept refusing
/// ends held, not failed. [ORB-14634] It held its claim while its push
/// retried inside the push's window, so once that window closes the
/// settlement releases the task as `transient` naming the reviewed head it
/// kept and why. The forge refused it, not this host or the crew: the drain
/// keeps offering the crew, and the owner hands the task straight back to it
/// with the kept candidate, which only this host has.
fn a_forge_held_leaf_releases_the_claim_naming_its_head_and_keeps_the_crew() {
    let (pair, drain, leaf, task) = claimed_sol();
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
    end_leaf_at(
        &pair,
        &leaf,
        (0, "diagnostic"),
        JobRunState::Held,
        Some(FORGE_UNAVAILABLE_ERROR_CODE),
        hold.text("step `push` held: the forge refused the push"),
    );

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
    assert_resumes_candidate(&pair, &next, "forge hold");
}

/// A held leaf's persisted hold outranks whatever its step message carries;
/// the scenario above, with no persisted hold, reads the legacy message.
fn a_persisted_forge_hold_outranks_the_hold_in_a_held_message() {
    let (pair, drain, leaf, _task) = claimed_sol();
    leaf_completed(&pair, &leaf, prepared());
    let now = Utc::now();
    let persisted = ForgeUnavailableHold {
        target_ref: format!("refs/heads/{BRANCH}"),
        head_sha: HEAD.into(),
        attempts: 30,
        waited_ms: 7_190_000,
        diagnostic: "! [remote rejected] (Internal Server Error)".into(),
        step_id: "push".into(),
        held_at: now,
        held_since: now - chrono::TimeDelta::hours(2),
    };
    let message = ForgeUnavailableHold {
        head_sha: "legacy-message-head".into(),
        attempts: 2,
        ..persisted.clone()
    }
    .text("step `push` held: the forge refused the push");
    let mut state = pair
        .follower
        .read_run_state(&leaf)
        .unwrap()
        .expect("the leaf's pipeline state");
    state.pull_drain.forge_hold = Some(persisted);
    pair.follower.write_run_state(&leaf, &state).unwrap();
    end_leaf_at(
        &pair,
        &leaf,
        (0, "diagnostic"),
        JobRunState::Held,
        Some(FORGE_UNAVAILABLE_ERROR_CODE),
        message,
    );

    settle_only(&pair, &drain);

    let release = &settlement_of(&pair, &leaf)["Release"];
    let reason = release["failure"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains(HEAD) && reason.contains("30 times"),
        "{release}"
    );
    assert!(!reason.contains("legacy-message-head"), "{release}");
    assert_eq!(release["forge_hold"]["head_sha"], HEAD, "{release}");
}

/// The candidate's own failure, and a task its final recovery judged, still
/// block the task — typed, so `orbit run show` says which.
fn a_candidate_failure_still_blocks_the_task() {
    let (pair, drain, leaf, task) = claimed_sol();
    let pass = fail_and_settle(&pair, &drain, &leaf, "cargo test: 3 tests failed");
    assert_eq!(pair.owner_status(&task), "blocked", "{pass}");
    let failed = &settlement_of(&pair, &leaf)["Fail"];
    assert_eq!(failed["failure"]["class"], "candidate", "{failed}");
    assert!(failed["failure"]["review_decision_pending"].is_null());
    let legacy_failure: orbit_store::contracts::ClaimFailure =
        serde_json::from_value(failed["failure"].clone()).unwrap();
    assert!(!legacy_failure.review_decision_pending);
    assert_eq!(pair.owner_claim_for_leaf(&leaf)["claim"]["phase"], "failed");
    let owner_record = &pair.wire.owner.leaf_settlements(None, false).unwrap()[0];
    assert_eq!(
        serde_json::to_value(owner_record).unwrap()["review_decision_pending"],
        false,
        "the settlements JSON must distinguish markerless candidate failures"
    );
    let legacy_record: orbit_store::contracts::ClaimSettlementRecord =
        serde_json::from_value(json!({
            "kind": "fail", "evidence": "failure", "failure_class": "candidate",
            "reason": "cargo test: 3 tests failed", "settled_at": owner_record.settled_at,
        }))
        .expect("settlements written before the pending-decision flag remain readable");
    assert!(!legacy_record.review_decision_pending);
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
    // [ORB-21840] A markerless candidate failure stays a failure.
    assert_eq!(
        serde_json::to_value(owner_record).unwrap()["accounting"],
        json!({"outcome": "failure", "counts_as_failure": true, "reason": "candidate_failure"}),
    );

    // A claim settled before the owner recorded settlements keeps no typed
    // evidence: it stays explicit as unrecorded and still counts.
    let rewritten = pair
        .wire
        .owner
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE task_coordination_rows \
             SET payload_json = json_remove(payload_json, '$.settlement') \
             WHERE kind = 'distributed-claim-lifecycle-v1' \
             AND json_extract(payload_json, '$.settlement') IS NOT NULL",
            (),
        )
        .unwrap();
    assert_eq!(rewritten, 1, "one settled claim");
    let legacy = &pair.wire.owner.leaf_settlements(None, false).unwrap()[0];
    assert_eq!((legacy.kind, legacy.evidence), (None, "unrecorded"));
    assert_eq!(
        serde_json::to_value(legacy).unwrap()["accounting"],
        json!({"outcome": "unrecorded", "counts_as_failure": true, "reason": "unrecorded"}),
    );
    assert_eq!(pair.owner_status(&task), "blocked");
}

/// [ORB-21840] Only a failed candidate settlement can be a before-landing
/// review's designed wait: a pending-decision flag on any other settlement
/// is malformed evidence and stays a failure, with that reason.
fn a_pending_decision_flag_on_a_release_stays_a_failure() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let claim = pair.admission(&leaf).receipt.unwrap().claim.unwrap();
    let settlement = ClaimMutation::Release(ClaimEvidence {
        summary: Some("released by follower drain".into()),
        failure: Some(orbit_store::contracts::ClaimFailure {
            class: ClaimFailureClass::Transient,
            review_decision_pending: true,
            reason: "the pull request stays open and unmerged until a recorded decision lands it"
                .into(),
            crew: Some("sol".into()),
            candidate: None,
            provider_limit: false,
            provider_refusal: false,
            owner_busy: false,
        }),
        ..ClaimEvidence::default()
    });
    pair.wire
        .call(
            "",
            "orbit.drain.claim.settle",
            json!({"claim_id": claim.claim_id, "run_id": leaf, "settlement": settlement}),
        )
        .unwrap();
    let [record] = &pair.wire.owner.leaf_settlements(None, false).unwrap()[..] else {
        panic!("one settlement");
    };
    assert!(record.review_decision_pending, "{record:?}");
    assert_eq!(
        serde_json::to_value(record).unwrap()["accounting"],
        json!({
            "outcome": "failure",
            "counts_as_failure": true,
            "reason": "review_decision_pending_without_candidate_failure",
        }),
        "the reason text alone never makes a hold"
    );
}

/// [ORB-16793] The owner's task history says why a follower's leaf blocked
/// the task: its `claim_failed` entry names the leaf run, the host, the
/// settlement's evidence and failure classes and the step that failed, with
/// the reason redacted, so neither an operator nor the codebase analytics
/// need the claim settlement to read it.
fn a_failed_leaf_names_its_cause_in_owner_history() {
    let (pair, drain, leaf, task) = claimed_sol();
    let mut synchronized = prepared();
    synchronized["sync_base"] =
        json!({"head": BRANCH, "head_sha": HEAD, "base": "main", "base_sha": "b0"});
    leaf_completed(&pair, &leaf, synchronized);
    let pass = fail_and_settle(
        &pair,
        &drain,
        &leaf,
        "cargo test: 3 tests failed; request sent Bearer fixture-secret-token",
    );
    assert_eq!(pair.owner_status(&task), "blocked", "{pass}");
    let failure = &settlement_of(&pair, &leaf)["Fail"]["failure"];
    assert_eq!(failure["class"], "candidate", "{failure}");
    let failed_step = failure["candidate"]["failed_step_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the kept candidate names its failed step: {failure}"));
    let executed_on = &pair.owner_claim_for_leaf(&leaf)["claim"]["executed_on"];
    let host = executed_on["machine_name"]
        .as_str()
        .or(executed_on["machine_id"].as_str())
        .expect("the claim names its host");
    let owner = pair.owner_task(&task);
    let entry = owner["history"]
        .as_array()
        .and_then(|history| {
            history
                .iter()
                .rev()
                .find(|entry| entry["event"] == "claim_failed")
        })
        .unwrap_or_else(|| panic!("an owner claim_failed entry: {owner:#}"));
    assert_eq!(entry["to_status"], "blocked", "{entry}");
    assert_eq!(
        entry["note"].as_str(),
        Some(
            format!(
                "claimed leaf failed: host={host}, run_id={leaf}, evidence=failure, \
                 failure_class=candidate, failed_step={failed_step}, error=cargo test: 3 tests \
                 failed; request sent Bearer [REDACTED_AUTH]"
            )
            .as_str()
        ),
        "the note carries the settlement's cause, redacted: {entry}"
    );

    // The Blocked ranking reads the cause off the note, and off the claim's
    // settlement for an entry written before the note existed.
    let run_failures = || {
        let view = pair
            .wire
            .owner
            .run_tool(
                "orbit.codebase.analytics",
                json!({"window": "all", "min_support": 1}),
            )
            .expect("codebase analytics");
        view["blocked"]["causes"]
            .as_array()
            .and_then(|causes| causes.iter().find(|cause| cause["cause"] == "run_failure"))
            .map(|cause| (cause["tasks"].clone(), view["blocked"]["causes"].clone()))
            .expect("a run failure bucket")
    };
    let (tasks, causes) = run_failures();
    assert_eq!(tasks, 1, "a candidate failure is the work's: {causes}");
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &pair.wire.owner.global_root(),
        ),
    )
    .unwrap();
    let events = registry
        .find_task_binding(&task)
        .unwrap()
        .expect("owner task binding")
        .canonical_path
        .join(orbit_types::task::TASK_EVENTS_FILE_NAME);
    let rows = std::fs::read_to_string(&events)
        .unwrap()
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            if row["type"] == "claim_failed" {
                row.as_object_mut().unwrap().remove("note");
            }
            format!("{row}\n")
        })
        .collect::<String>();
    std::fs::write(&events, rows).unwrap();
    assert!(
        pair.owner_task(&task)["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["event"] == "claim_failed" && entry["note"].is_null())
    );
    let (tasks, causes) = run_failures();
    assert_eq!(
        tasks, 1,
        "the settlement explains a note-less entry: {causes}"
    );
}

/// Cancelling one launched claimed leaf, as `orbit run cancel <leaf>` and the
/// dashboard's cancel do, gives its claim back with the operator's reason
/// and candidate. Only that task is suppressed for the drain window; the
/// crew stays usable for unrelated work and mandatory before-landing review.
fn cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason() {
    let pair = Pair::with_owner_config(
        "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"sol\"\n",
        &[Some("sol"), Some("sol")],
    );
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);
    leaf_completed(&pair, &leaf, prepared());

    cancel_leaf(&pair, &leaf, "wrong crew for this task", false);
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
    // [ORB-21840] A cancellation keeps its own accounting outcome.
    let listed = pair.wire.owner.leaf_settlements(None, false).unwrap();
    assert_eq!(
        serde_json::to_value(&listed[0]).unwrap()["accounting"],
        json!({"outcome": "operator_cancel", "counts_as_failure": false, "reason": "operator_cancel"}),
    );
    assert_eq!(release["failure"]["candidate"]["branch"], BRANCH);
    assert_eq!(release["failure"]["candidate"]["head_sha"], HEAD);
    assert_eq!(
        pair.owner_claim_for_leaf(&leaf)["claim"]["phase"],
        "revoked"
    );
    assert_eq!(sol_exclusion(&pair, &drain), None);
    assert_eq!(window(&pair, &drain).host_suppressed, None);

    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), pair.tasks[1]);
    let ship = pair.admission(&next).request.ship;
    assert!(ship.before_landing, "the owner's mandatory review survives");
    assert_eq!(ship.review.unwrap().crew.as_deref(), Some("sol"));

    // Replay a request built before the cancel, bypassing the follower's
    // window projection: the owner itself must suppress the original task.
    let same = replay_pull(&pair, "built-before-the-operator-cancel", false);
    assert!(same["receipt"]["claim"].is_null(), "{same:#}");
    assert!(
        held_entry(&same, "crew_unavailable", &task)
            .is_some_and(|held| held["reason"].as_str().unwrap().contains("operator_cancel")),
        "the original task stays withheld from this window: {same:#}"
    );
    assert_eq!(pair.owner_claims().len(), 2, "no immediate repull");

    // A later window can resume the candidate, with the release still in
    // owner history; the budget scenario below verifies the cancel counts.
    let later = replay_pull(&pair, "after-the-operator-cancel-window", true);
    assert_eq!(later["receipt"]["claim"]["task_id"], task.as_str());
    let resumed = &later["receipt"]["task"]["resume_candidate"];
    assert_eq!(resumed["branch"], BRANCH, "{later:#}");
    assert_eq!(resumed["head_sha"], HEAD, "{later:#}");
}

/// An operator who cancels a launched claimed leaf with `--block` asks for the
/// task to stay blocked: the cancel is still typed, but it fails the claim.
fn cancelling_a_launched_claimed_leaf_with_block_blocks_its_task() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    cancel_leaf(&pair, &leaf, "needs a human look", true);
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
fn a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason() {
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.run_drain();
    let reasons = [
        "rebase onto main conflicted in src/f0.rs: first",
        "rebase onto main conflicted in src/f0.rs: second",
    ];

    // An operator's cancel first: released to the backlog, and counted.
    let (cancelled, worker) = pair.running_leaf_with_worker(&drain, 1);
    cancel_leaf(&pair, &cancelled, "rebalancing hosts", false);
    assert!(worker.stopped());
    assert_eq!(pair.owner_status(&task), "backlog");

    for (n, reason) in reasons.iter().enumerate() {
        // A cancel suppresses its task only for the drain that admitted it;
        // later drains can retry the task while the task-wide budget persists.
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        assert_eq!(
            pair.claimed_task(&leaf),
            task,
            "the same task is pulled again"
        );
        leaf_completed(&pair, &leaf, prepared());
        let pass = fail_and_settle(&pair, &drain, &leaf, reason);
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
    let count = |wanted: &str| phases.iter().filter(|phase| *phase == wanted).count();
    assert_eq!(count("revoked"), 2, "{phases:?}");
    assert_eq!(count("failed"), 1, "{phases:?}");
}

/// [ORB-14260] A claimed PR leaf whose committed candidate could not be
/// synchronized onto a base that moved under it releases its claim as a
/// `base_conflict`, keeping the candidate's branch and head rather than
/// blocking the task. The task's next claim carries that candidate into its
/// leaf, whose `resume_candidate` step continues it; a spec change retires
/// it, and the claim after implements fresh.
fn a_base_conflict_keeps_the_candidate_and_the_next_claim_resumes_it() {
    let (pair, drain, leaf, task) = claimed_on(&[None]);
    leaf_completed(&pair, &leaf, prepared());
    let pass = fail_and_settle(
        &pair,
        &drain,
        &leaf,
        "sync_base: rebase onto origin/main conflicted in src/f0.rs; conflict recovery could \
         not resolve it",
    );
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
    assert_resumes_candidate(&pair, &next, "base conflict");
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
    fail_and_settle(
        &pair,
        &drain,
        &next,
        "[transient_failure] the network dropped",
    );
    pair.wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "description": "A different change than the candidate made."}),
        )
        .expect("change the task's spec");
    // A new drain: this one's window may have excluded the crew the
    // transient release ran on.
    let fresh = pair.running_leaf(&pair.run_drain(), 1);
    assert_eq!(pair.claimed_task(&fresh), task);
    assert_eq!(
        kept_candidate(&pair, &fresh),
        None,
        "a spec change retires the kept candidate"
    );
}

/// An operator's `orbit task update --discard-candidate` retires a kept
/// candidate: the task's next claim implements fresh.
fn a_discarded_candidate_is_not_resumed() {
    let (pair, drain, leaf, task) = claimed_on(&[None]);
    leaf_completed(&pair, &leaf, prepared());
    fail_and_settle(
        &pair,
        &drain,
        &leaf,
        "rebase onto main conflicted in src/f0.rs",
    );
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
    assert_eq!(
        kept_candidate(&pair, &next),
        None,
        "a discarded candidate is not resumed"
    );
}

/// [ORB-14257] A worker step whose call to the owner never reached it fails
/// typed as an owner-route failure. Its `owner_route` release evidence
/// holds the task for an operator without blaming the candidate.
fn a_lost_owner_route_fails_the_step_as_owner_route() {
    struct Unreachable;
    impl OwnerCoordinator for Unreachable {
        fn call(
            &self,
            _name: &str,
            _input: Value,
            _session: ToolSessionContext,
        ) -> Result<Value, OrbitError> {
            Err(OrbitError::UnreachableDestination(
                "ssh: connect to host owner port 22: Connection timed out".into(),
            ))
        }
    }
    let (pair, drain, leaf, task) = claimed_sol();
    let bound = bound_to(&pair.follower, &pair, &leaf, Arc::new(Unreachable));

    let error = bound
        .run_tool("orbit.task.show", json!({"id": task}))
        .expect_err("the owner is unreachable");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "the variant is kept: {error:?}"
    );
    let message = error.to_string();
    assert!(
        !message.contains("[owner_route_unavailable]"),
        "no bracketed marker is stamped into new messages: {message}"
    );
    let code = error
        .step_failure_code()
        .expect("a typed step-failure code");
    assert_eq!(
        code,
        orbit_types::workflow::StepFailureCode::OwnerRouteUnavailable,
        "{message}"
    );
    assert_eq!(
        ClaimFailureClass::of_step_failure_code(Some(&code), Some(&message)),
        Some(ClaimFailureClass::OwnerRoute),
        "{message}"
    );

    pair.leaf_fails_with_code(&leaf, &message, &code);
    settle_only(&pair, &drain);
    let failure = &settlement_of(&pair, &leaf)["Release"]["failure"];
    assert_eq!(failure["class"], "owner_route", "{failure}");
    assert_eq!(pair.owner_status(&task), "blocked");
}

/// [ORB-15088] An owner that times out waiting on a task lock answers
/// `lock_busy`. A bound worker's owner read — the task load behind every agent
/// envelope, a final recovery's included — retries it a bounded number of
/// times before failing; a step it still fails settles as `transient`, never
/// as a base conflict, even when the leaf stopped synchronizing its committed
/// candidate onto the base.
#[test]
fn an_owner_lock_timeout_is_retried_and_never_settles_as_a_base_conflict() {
    if !isolated(
        module_path!(),
        "an_owner_lock_timeout_is_retried_and_never_settles_as_a_base_conflict",
    ) {
        return;
    }
    /// Answers `busy` calls with the owner's lock timeout, then forwards.
    struct BusyOwner {
        owner: super::claimed_review::ToOwner,
        busy: AtomicUsize,
        calls: AtomicUsize,
    }
    impl OwnerCoordinator for BusyOwner {
        fn call(
            &self,
            name: &str,
            input: Value,
            session: ToolSessionContext,
        ) -> Result<Value, OrbitError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let busy = self.busy.load(Ordering::SeqCst);
            if busy > 0 {
                self.busy.store(busy - 1, Ordering::SeqCst);
                let message = "owner: timed out after 30000ms acquiring task commit boundary: \
                               recovery at crates/orbit-store/src/task.rs:1 lock \
                               '/state/tasks/workspaces/ws/.task-commit.lock'";
                return Err(OrbitError::RemoteTool {
                    code: orbit_common::LOCK_BUSY_ERROR_CODE.into(),
                    message: message.into(),
                    payload: json!({"code": orbit_common::LOCK_BUSY_ERROR_CODE, "message": message}),
                });
            }
            self.owner.call(name, input, session)
        }
    }

    let (pair, drain, leaf, task) = claimed_sol();
    let owner = Arc::new(BusyOwner {
        owner: super::claimed_review::ToOwner(pair.wire.owner.clone()),
        busy: AtomicUsize::new(2),
        calls: AtomicUsize::new(0),
    });
    let bound = bound_to(&pair.follower, &pair, &leaf, owner.clone());
    let retried_read = || {
        orbit_core::test_support::with_owner_read_retry_wait(
            |_| std::thread::sleep(Duration::from_millis(1)),
            || bound.get_task(&task),
        )
    };

    // Two busy answers are retried through: the envelope's task load succeeds.
    let loaded = retried_read().expect("retried past the busy owner");
    assert_eq!(loaded.id, task);
    assert_eq!(
        owner.calls.load(Ordering::SeqCst),
        3,
        "two retries, then the read"
    );

    // A lock that stays held exhausts the bounded retries.
    owner.calls.store(0, Ordering::SeqCst);
    owner.busy.store(usize::MAX, Ordering::SeqCst);
    let error = retried_read().expect_err("the owner stays busy");
    assert_eq!(
        owner.calls.load(Ordering::SeqCst),
        3,
        "the retries are bounded"
    );
    assert!(error.is_lock_busy(), "the code is kept: {error:?}");
    let message = error.to_string();
    assert_eq!(
        ClaimFailureClass::of_step_failure(None, Some(&message)),
        Some(ClaimFailureClass::Transient),
        "{message}"
    );

    // The leaf stopped at synchronizing its committed candidate, where an
    // untyped failure settles as a base conflict.
    leaf_completed(&pair, &leaf, prepared());
    fail_and_settle(&pair, &drain, &leaf, &message);
    let failure = &settlement_of(&pair, &leaf)["Release"]["failure"];
    assert_eq!(failure["class"], "transient", "{failure}");
    assert_eq!(pair.owner_status(&task), "backlog");
}

/// [ORB-14695] A claimed leaf whose provider account hit its usage limit
/// releases the claim as a `provider` failure without spending the task's
/// release budget: three in a day leave the task in the backlog, where the
/// third release of any other kind blocks it. [ORB-14697] The release itself
/// excludes no crew: the follower's provider-limit store holds the reading
/// that does, and only until it lapses (`provider_limit/gate.rs`).
fn a_provider_limit_releases_the_claim_outside_the_release_budget() {
    let config = "\
[workflow]
default_crew = \"sol\"

[crews.sol]
provider = \"codex\"
model = \"gpt-sol\"

[crews.luna]
provider = \"codex\"
model = \"gpt-luna\"

[crews.opus]
provider = \"claude\"
model = \"claude-opus\"
";
    let pair = Pair::with_configs(config, config, &[Some("sol")]);
    let task = pair.tasks[0].clone();
    let limit = "[provider_limit] provider=codex resets_at=2026-10-09T22:42:00Z cli subprocess \
         exited with code 1: codex provider reported a usage limit: You've hit your usage limit. \
         Upgrade to Pro or try again at 3:42 PM.";
    for n in 1..=3 {
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        assert_eq!(pair.claimed_task(&leaf), task, "release {n}: pulled again");
        let pass = fail_and_settle(&pair, &drain, &leaf, limit);
        assert_eq!(pair.owner_status(&task), "backlog", "release {n}: {pass}");

        let failure = &settlement_of(&pair, &leaf)["Release"]["failure"];
        assert_eq!(failure["class"], "provider", "{failure}");
        assert_eq!(failure["provider_limit"], true, "{failure}");
        assert!(
            failure["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("You've hit your usage limit")
                    && !reason.contains("[provider_limit]")),
            "the reason quotes the provider without Orbit's marker: {failure}"
        );

        let excluded = window(&pair, &drain).excluded;
        assert!(
            excluded.is_empty(),
            "release {n}: a limit release excludes no crew for the window: {excluded:?}"
        );
    }

    let owner = pair.owner_task(&task);
    assert!(
        !comments_of(&owner).contains("released this claim 2 times"),
        "no release budget was spent: {owner:#}"
    );
    assert!(
        pair.owner_claims()
            .iter()
            .all(|claim| claim["claim"]["phase"] == "revoked"),
        "every claim was released, none failed: {:#?}",
        pair.owner_claims()
    );
}

/// [ORB-15573] A claimed leaf whose provider's content policy refused the
/// task did not judge the work. Its claim releases as a `provider` failure
/// marked as a refusal, which excludes no crew from the drain's window: the
/// provider works on this host. The owner instead returns the task to the
/// backlog under a provider hold excluding every crew of the refusing
/// provider, with the refusal in its history. Its next admission draws a
/// crew of another provider; a follower that would run it as a held crew is
/// refused it with the hold as the reason, and an owner with no other
/// provider defers it with the same reason instead of looping.
#[test]
fn a_provider_refusal_releases_the_claim_under_an_owner_provider_hold() {
    if !isolated(
        module_path!(),
        "a_provider_refusal_releases_the_claim_under_an_owner_provider_hold",
    ) {
        return;
    }
    const CODEX: &str = "\
[crews.sol]
provider = \"codex\"
model = \"gpt-sol\"

[crews.luna]
provider = \"codex\"
model = \"gpt-luna\"
";
    let mixed = format!(
        "[workflow]\ndefault_crew = \"opus\"\n\n{CODEX}\n[crews.opus]\nprovider = \"claude\"\n\
         model = \"claude-opus\"\n"
    );
    let codex_only = format!("[workflow]\ndefault_crew = \"sol\"\n\n{CODEX}");
    let refusal = "[provider_refusal] provider=codex cli subprocess exited with code 1: codex \
         provider refused the request: This content was flagged for possible cybersecurity risk.";
    for (config, another_provider) in [(mixed.as_str(), true), (codex_only.as_str(), false)] {
        let pair = Pair::with_configs(config, config, &[Some("sol")]);
        let task = pair.tasks[0].clone();
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let pass = fail_and_settle(&pair, &drain, &leaf, refusal);
        assert_eq!(pair.owner_status(&task), "backlog", "not blocked: {pass}");

        let release = &settlement_of(&pair, &leaf)["Release"];
        assert_eq!(release["failure"]["class"], "provider", "{release}");
        assert_eq!(release["failure"]["provider_refusal"], true, "{release}");
        assert!(
            release["provider_unavailable"].is_null(),
            "a refusal is no unavailability: {release}"
        );
        assert_eq!(
            sol_exclusion(&pair, &drain),
            None,
            "the provider works here, so the window keeps the crew"
        );

        let history = pair.wire.owner.get_task_history(&task).unwrap();
        let entry = history.last().expect("history");
        assert_eq!(entry.event, PROVIDER_FAILURE_HOLD_EVENT, "{entry:?}");
        assert_eq!(entry.to_status, Some(TaskStatus::Backlog), "{entry:?}");
        let note = entry.note.as_deref().unwrap_or_default();
        assert!(
            note.contains("flagged for possible cybersecurity risk"),
            "the history carries the refusal: {note}"
        );
        let hold = ProviderFailureHold::from_text(note).expect("the note carries the hold");
        assert_eq!(hold.class, ProviderFailureClass::Refusal);
        assert_eq!(hold.provider.as_deref(), Some("codex"));
        assert_eq!(hold.run_id, leaf);
        assert!(
            hold.excludes("sol") && hold.excludes("luna") && !hold.excludes("opus"),
            "every Codex crew: {hold:?}"
        );

        // A follower drain that would run the task as `sol` is refused it.
        let pulled = replay_pull(&pair, "after-the-refusal", true);
        assert!(pulled["receipt"]["claim"].is_null(), "{pulled:#}");
        let deferred = held_entry(&pulled, "deferred_conflicts", &task)
            .unwrap_or_else(|| panic!("the hold keeps the task from `sol`: {pulled:#}"));
        assert!(
            deferred["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("provider hold") && reason.contains(&leaf)),
            "{deferred}"
        );

        // The owner's own admission draws another provider, or waits.
        let backlog = pair
            .wire
            .owner
            .run_deterministic(
                "list_backlog_tasks",
                &json!({}),
                &json!({}),
                ToolContext::default(),
            )
            .unwrap();
        let admitted = backlog["task_ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == task.as_str()));
        assert_eq!(admitted, another_provider, "{backlog:#}");
        if !another_provider {
            let excluded = backlog["excluded"]
                .as_array()
                .and_then(|excluded| excluded.iter().find(|entry| entry["id"] == task.as_str()))
                .unwrap_or_else(|| panic!("the task waits out the hold: {backlog:#}"));
            assert_eq!(excluded["reason"], "provider_backoff", "{excluded}");
            assert!(
                excluded["detail"].as_str().is_some_and(|detail| detail
                    .contains("provider_refusal")
                    && detail.contains(&leaf)),
                "the deferral names the refusal and its run: {excluded}"
            );
        }
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    handoff_fetch_timeout_releases_the_owner_but_missing_ref_still_blocks_scenarios,
    handoff_fetch_timeout_releases_the_owner_but_missing_ref_still_blocks,
    crew_failures_release_the_claim_and_exclude_the_crew_for_the_window,
    an_upgrade_refused_agent_releases_the_claim_and_the_next_claim_resumes_it,
    an_upgrade_quiesced_leaf_settles_outside_the_failure_count,
    the_owner_lists_a_follower_leaf_release_with_its_evidence_class,
    the_owner_hydrates_a_legacy_leaf_settlement_from_its_follower_once,
    hydration_observes_stored_runs_without_reconciliation,
    hydration_continuation_visits_every_selected_claim_at_limit_one,
    a_settlement_naming_another_leaf_run_records_no_evidence,
    host_failures_hold_the_task_and_suppress_the_host_for_the_window,
    a_baseline_red_failure_releases_the_claim_without_excluding_the_crew
);

crate::scenario_table!(
    a_forge_held_leaf_releases_the_claim_naming_its_head_and_keeps_the_crew_scenarios,
    a_forge_held_leaf_releases_the_claim_naming_its_head_and_keeps_the_crew,
    a_persisted_forge_hold_outranks_the_hold_in_a_held_message,
    a_candidate_failure_still_blocks_the_task,
    a_pending_decision_flag_on_a_release_stays_a_failure,
    a_failed_leaf_names_its_cause_in_owner_history,
    cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason,
    cancelling_a_launched_claimed_leaf_with_block_blocks_its_task,
    a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason,
    a_base_conflict_keeps_the_candidate_and_the_next_claim_resumes_it,
    a_discarded_candidate_is_not_resumed,
    a_lost_owner_route_fails_the_step_as_owner_route,
    a_provider_limit_releases_the_claim_outside_the_release_budget
);
