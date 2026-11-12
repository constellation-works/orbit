//! [ORB-24115] A follower's late report of the effort of leaves it settled
//! without one, and the owner's recording of it against the settlement.
//! [ORB-24150] The same report carries the implementing model, and is
//! accepted for a claim the owner holds no settlement record for.

use super::*;
use orbit_core::application::agents::ClaimedEffortOutcome;
use orbit_types::identity::ReasoningEffort;

/// A pulled attempt whose follower settled it (an operator cancel releases
/// it) without reporting an effort, as a follower that predates the report
/// did, or one whose launch record was not yet written. Returns the leaf and
/// its claim.
fn settled_without_effort(pair: &Pair) -> (String, String) {
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    cancel_without_effort(pair, leaf)
}

/// [`settled_without_effort`] for a leaf already running on the follower.
fn cancel_without_effort(pair: &Pair, leaf: String) -> (String, String) {
    let prior_settles = pair.wire.calls("orbit.drain.claim.settle").len();
    let task = pair.claimed_task(&leaf);
    let claim = pair
        .admission(&leaf)
        .receipt
        .and_then(|receipt| receipt.claim)
        .unwrap()
        .claim_id;
    // Keep this fixture from signalling the test process used as the fake
    // worker, as the operator cancel scenario does.
    let mut run = pair.follower_jobs.get_job_run(&leaf).unwrap().unwrap();
    run.pid = None;
    run.pid_start_time = None;
    let workspace = pair.follower.workspace_id().unwrap();
    pair.follower
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace, &run, None)
        .unwrap();
    pair.follower
        .cancel_job_run_with_options_and_policy(&leaf, "operator", "cli", None, false, false)
        .expect("cancel claimed leaf");
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(
        settles.len(),
        prior_settles + 1,
        "one new settlement: {settles:?}"
    );
    let settle = &settles[prior_settles];
    assert!(
        settle.get("reasoning_effort").is_none(),
        "the fixture settles without an effort: {:#}",
        settle
    );
    assert_eq!(pair.owner_status(&task), "backlog");
    (leaf, claim)
}

/// Record on the follower a launch of the leaf's implementing agent at
/// `effort`, as `orbit migrate --backfill-dispatch-effort` or a dispatch-time
/// launch record leaves it.
fn record_follower_launch(pair: &Pair, leaf: &str, effort: ReasoningEffort) {
    let run = pair.follower_jobs.get_job_run(leaf).unwrap().unwrap();
    pair.follower
        .sqlite_store()
        .unwrap()
        .begin_agent_execution(
            &pair.follower.workspace_id().unwrap(),
            leaf,
            "implement_one",
            "fixture",
            run.crew_model.as_deref(),
            &orbit_types::telemetry::AgentExecutionIdentity {
                crew: Some("sol".into()),
                reasoning_effort: Some(effort),
            },
        )
        .unwrap();
}

/// The model the follower's leaf run recorded, which its launch record and
/// late report carry.
fn follower_model(pair: &Pair, leaf: &str) -> Option<String> {
    pair.follower_jobs
        .get_job_run(leaf)
        .unwrap()
        .unwrap()
        .crew_model
}

/// [`record_follower_launch`] on `model`.
fn record_follower_launch_on(pair: &Pair, leaf: &str, effort: ReasoningEffort, model: &str) {
    pair.follower
        .sqlite_store()
        .unwrap()
        .begin_agent_execution(
            &pair.follower.workspace_id().unwrap(),
            leaf,
            "implement_one",
            "fixture",
            Some(model),
            &orbit_types::telemetry::AgentExecutionIdentity {
                crew: Some("sol".into()),
                reasoning_effort: Some(effort),
            },
        )
        .unwrap();
}

/// The owner's implementer scorecard model and effort of each row.
fn owner_models_and_efforts(pair: &Pair) -> Vec<(Value, Value)> {
    let card = pair
        .wire
        .owner
        .run_tool("orbit.agents.scorecard", json!({"role": "implementers"}))
        .expect("owner scorecard");
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["model"].clone(), row["reasoning_effort"].clone()))
        .collect()
}

/// Rewrite the owner's lifecycle row of `claim` as an owner that kept no
/// settlement record wrote it (`field: None`), or with the record's `field`
/// removed, as a follower that predates reporting it settled.
fn forget_owner_settlement(pair: &Pair, claim: &str, field: Option<&str>) {
    let connection =
        rusqlite::Connection::open(pair.wire.owner.global_root().join("orbit.db")).unwrap();
    let payload: String = connection
        .query_row(
            "SELECT payload_json FROM task_coordination_rows \
             WHERE kind = 'distributed-claim-lifecycle-v1' AND row_id = ?1",
            [claim],
            |row| row.get(0),
        )
        .unwrap();
    let mut state: Value = serde_json::from_str(&payload).unwrap();
    let removed = match field {
        None => state.as_object_mut().unwrap().remove("settlement"),
        Some(field) => state["settlement"].as_object_mut().unwrap().remove(field),
    };
    assert!(
        removed.is_some(),
        "the fixture's settlement held it: {state:#}"
    );
    connection
        .execute(
            "UPDATE task_coordination_rows SET payload_json = ?1 \
             WHERE kind = 'distributed-claim-lifecycle-v1' AND row_id = ?2",
            rusqlite::params![state.to_string(), claim],
        )
        .unwrap();
}

/// The owner's implementer scorecard effort of each row.
fn owner_efforts(pair: &Pair) -> Vec<Value> {
    let card = pair
        .wire
        .owner
        .run_tool("orbit.agents.scorecard", json!({"role": "implementers"}))
        .expect("owner scorecard");
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["reasoning_effort"].clone())
        .collect()
}

/// Everything the owner's claim journal holds: every coordination row of its
/// store, by kind and id, with its payload.
fn owner_coordination_rows(pair: &Pair) -> Vec<(String, String, String)> {
    let connection =
        rusqlite::Connection::open(pair.wire.owner.global_root().join("orbit.db")).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT kind, row_id, payload_json FROM task_coordination_rows \
             ORDER BY kind, row_id",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// The owner's stored state a refused report must leave untouched.
fn owner_state(pair: &Pair) -> (Vec<(String, String, String)>, Vec<Value>) {
    (owner_coordination_rows(pair), pair.owner_claims())
}

/// A leaf settled without an effort counts as effort unknown on the owner;
/// once its follower's records hold the implementing launch, the follower's
/// backfill reports it, and the owner's scorecard counts the attempt under
/// it. A second run records nothing and leaves the owner store unchanged.
fn a_followers_late_report_moves_a_settled_attempt_out_of_effort_unknown() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, claim) = settled_without_effort(&pair);
    assert_eq!(owner_efforts(&pair), [json!("unknown")], "before backfill");

    record_follower_launch(&pair, &leaf, ReasoningEffort::High);
    let rows_before = owner_coordination_rows(&pair);
    let report = pair
        .follower
        .backfill_claimed_leaf_effort()
        .expect("follower backfill");
    let added: Vec<_> = owner_coordination_rows(&pair)
        .into_iter()
        .filter(|row| !rows_before.contains(row))
        .collect();
    assert_eq!(added.len(), 1, "one late report row: {added:?}");
    assert_eq!(
        (added[0].0.as_str(), added[0].1.as_str()),
        ("distributed-late-leaf-effort-v1", claim.as_str())
    );
    assert_eq!(
        (report.examined, report.recorded, report.replayed),
        (1, 1, 0),
        "{report:#?}"
    );
    assert_eq!(report.rows[0].claim_id, claim);
    assert_eq!(report.rows[0].reasoning_effort.as_deref(), Some("high"));
    assert_eq!(owner_efforts(&pair), [json!("high")], "after backfill");
    let reports = pair.wire.calls("orbit.drain.claim.effort");
    let mut sent = json!({"claim_id": claim, "run_id": leaf, "reasoning_effort": "high"});
    if let Some(model) = follower_model(&pair, &leaf) {
        sent["crew_model"] = json!(model);
    }
    assert_eq!(reports, [sent]);

    let before = owner_state(&pair);
    let again = pair
        .follower
        .backfill_claimed_leaf_effort()
        .expect("second backfill");
    assert_eq!(
        (again.examined, again.recorded, again.replayed),
        (1, 0, 1),
        "{again:#?}"
    );
    assert_eq!(owner_state(&pair), before, "a rerun changes nothing");
    assert_eq!(owner_efforts(&pair), [json!("high")]);
}

/// A report from a machine that did not run and settle the claim is
/// refused, and the owner store is unchanged.
fn a_late_report_from_another_machine_is_refused() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, claim) = settled_without_effort(&pair);
    let before = owner_state(&pair);
    let wrong_session = ToolSessionContext {
        caller_machine_id: Some("hm_other_follower".into()),
        process_machine_id: Some(OWNER.into()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let error = pair
        .wire
        .owner
        .serve_claim_effort(&wrong_session, &claim, &leaf, Some("high".into()), None)
        .expect_err("another machine must not report this claim's effort");
    assert!(
        error.to_string().contains("not_settling_host"),
        "the refusal names the fence: {error}"
    );
    assert_eq!(
        owner_state(&pair),
        before,
        "a refused report changes nothing"
    );
    assert_eq!(owner_efforts(&pair), [json!("unknown")]);
}

/// A report that disagrees with the effort a claim already holds is refused
/// and the held effort stands, whether the settlement carried it or an
/// earlier late report recorded it. The follower's backfill counts it.
fn a_late_report_disagreeing_with_a_held_effort_is_refused_and_counted() {
    // Held on the settlement itself.
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let claim = pair
        .admission(&leaf)
        .receipt
        .and_then(|receipt| receipt.claim)
        .unwrap()
        .claim_id;
    pair.wire
        .call(
            "",
            "orbit.drain.claim.settle",
            json!({
                "claim_id": claim,
                "run_id": leaf,
                "settlement": ClaimMutation::Fail(ClaimEvidence {
                    summary: Some("leaf failed".into()),
                    ..Default::default()
                }),
                "crew": "sol",
                "reasoning_effort": "high",
            }),
        )
        .expect("the owner accepts the settlement");
    let before = owner_state(&pair);
    let error = pair
        .wire
        .call(
            "",
            "orbit.drain.claim.effort",
            json!({"claim_id": claim, "run_id": leaf, "reasoning_effort": "low"}),
        )
        .expect_err("a disagreeing report is refused");
    assert!(error.to_string().contains("effort_conflict"), "{error}");
    let same = pair
        .wire
        .call(
            "",
            "orbit.drain.claim.effort",
            json!({"claim_id": claim, "run_id": leaf, "reasoning_effort": "high"}),
        )
        .expect("the held effort replays");
    assert_eq!(same["outcome"], "replayed", "{same}");
    assert_eq!(
        owner_state(&pair),
        before,
        "neither report changes the owner"
    );
    assert_eq!(owner_efforts(&pair), [json!("high")]);

    // Held from an earlier late report, then contradicted by a later launch
    // record on the follower.
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, _) = settled_without_effort(&pair);
    record_follower_launch(&pair, &leaf, ReasoningEffort::High);
    let first = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(first.recorded, 1, "{first:#?}");
    record_follower_launch(&pair, &leaf, ReasoningEffort::Low);
    let before = owner_state(&pair);
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.conflicting),
        (1, 0, 1),
        "{report:#?}"
    );
    let detail = report.rows[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("effort_conflict"), "{detail}");
    assert_eq!(owner_state(&pair), before, "the held effort stands");
    assert_eq!(owner_efforts(&pair), [json!("high")]);
}

/// A leaf whose follower holds no matching invocation or launch record is
/// counted with its reason, nothing is sent, and it stays effort unknown.
fn a_leaf_without_a_follower_record_stays_unknown_and_is_counted() {
    let pair = Pair::with_crews(&[Some("sol")]);
    settled_without_effort(&pair);
    let before = owner_state(&pair);
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.effort_unknown),
        (1, 0, 1),
        "{report:#?}"
    );
    let detail = report.rows[0].detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("no implementing invocation or launch record"),
        "{detail}"
    );
    assert!(pair.wire.calls("orbit.drain.claim.effort").is_empty());
    assert_eq!(owner_state(&pair), before);
    assert_eq!(owner_efforts(&pair), [json!("unknown")]);
}

/// An owner whose build predates the late report refuses it unread on its
/// internal route; the follower counts it, and neither store changes.
fn an_owner_that_predates_the_report_refuses_it_without_changing_state() {
    let pair = Pair::with_crews(&[Some("sol"), Some("sol")]);
    let (first_leaf, first_claim) = settled_without_effort(&pair);
    let (second_leaf, second_claim) = settled_without_effort(&pair);
    assert_ne!(first_claim, second_claim, "exercise two owner calls");
    record_follower_launch(&pair, &first_leaf, ReasoningEffort::High);
    record_follower_launch(&pair, &second_leaf, ReasoningEffort::High);
    *pair.wire.predates_late_effort.lock().unwrap() = true;
    let owner_before = owner_state(&pair);
    let follower_before = pair.follower_jobs.local_pull_admissions().unwrap();
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.owner_unsupported),
        (2, 0, 2),
        "{report:#?}"
    );
    assert!(
        report.rows.iter().all(|row| {
            row.outcome == ClaimedEffortOutcome::OwnerUnsupported
                && row.detail.as_deref().is_some_and(|detail| {
                    detail.contains("-32602") && detail.contains("unknown internal drain operation")
                })
        }),
        "the actual legacy JSON-RPC refusal is classified as unsupported: {report:#?}"
    );
    assert_eq!(
        pair.wire.calls("orbit.drain.claim.effort").len(),
        2,
        "an unsupported owner is retried for each claim, not suppressed as unreachable"
    );
    assert_eq!(owner_state(&pair), owner_before);
    assert_eq!(
        pair.follower_jobs.local_pull_admissions().unwrap(),
        follower_before
    );
    assert_eq!(owner_efforts(&pair), [json!("unknown")]);
}

/// Another structured JSON-RPC refusal is a refusal, not owner-unsupported
/// or owner-unreachable, and changes no state.
fn another_rpc_refusal_is_refused_without_changing_state() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, _) = settled_without_effort(&pair);
    record_follower_launch(&pair, &leaf, ReasoningEffort::High);
    *pair.wire.late_effort_rpc_refusal.lock().unwrap() =
        Some((-32603, "internal error".to_string()));
    let owner_before = owner_state(&pair);
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (
            report.examined,
            report.refused,
            report.owner_unsupported,
            report.owner_unreachable
        ),
        (1, 1, 0, 0),
        "{report:#?}"
    );
    assert_eq!(report.rows[0].outcome, ClaimedEffortOutcome::Refused);
    assert_eq!(owner_state(&pair), owner_before);
    assert_eq!(owner_efforts(&pair), [json!("unknown")]);
}

/// A genuine connection failure remains an unreachable owner, with no state
/// change on either host.
fn a_connection_failure_remains_owner_unreachable() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, _) = settled_without_effort(&pair);
    record_follower_launch(&pair, &leaf, ReasoningEffort::High);
    *pair.wire.unreachable.lock().unwrap() = true;
    let owner_before = owner_state(&pair);
    let follower_before = pair.follower_jobs.local_pull_admissions().unwrap();
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.owner_unreachable),
        (1, 0, 1),
        "{report:#?}"
    );
    assert!(
        report.rows[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Connection timed out")),
        "retain the connection failure: {report:#?}"
    );
    assert_eq!(owner_state(&pair), owner_before);
    assert_eq!(
        pair.follower_jobs.local_pull_admissions().unwrap(),
        follower_before
    );
    assert_eq!(owner_efforts(&pair), [json!("unknown")]);
}

/// A claim the owner settled before it kept settlement records refused
/// every late report as `no_settlement_record`. The report is now recorded
/// from the machine that ran the claim, and the owner's scorecard counts the
/// attempt under the reported effort and model; another machine is still
/// refused, and a rerun replays.
fn a_late_report_for_a_claim_without_a_settlement_record_is_recorded() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, claim) = settled_without_effort(&pair);
    forget_owner_settlement(&pair, &claim, None);
    let model = follower_model(&pair, &leaf).unwrap_or_else(|| "model-fixture".into());
    record_follower_launch_on(&pair, &leaf, ReasoningEffort::High, &model);

    let wrong_session = ToolSessionContext {
        caller_machine_id: Some("hm_other_follower".into()),
        process_machine_id: Some(OWNER.into()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let before = owner_state(&pair);
    let error = pair
        .wire
        .owner
        .serve_claim_effort(
            &wrong_session,
            &claim,
            &leaf,
            Some("high".into()),
            Some(model.clone()),
        )
        .expect_err("another machine must not report this claim");
    assert!(error.to_string().contains("not_settling_host"), "{error}");
    assert_eq!(
        owner_state(&pair),
        before,
        "a refused report changes nothing"
    );

    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.refused),
        (1, 1, 0),
        "{report:#?}"
    );
    assert_eq!(report.models_recorded, 1, "{report:#?}");
    assert_eq!(
        owner_models_and_efforts(&pair),
        [(json!(model), json!("high"))],
        "the attempt counts under the reported model and effort"
    );

    let before = owner_state(&pair);
    let again = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (again.recorded, again.replayed, again.models_replayed),
        (0, 1, 1),
        "{again:#?}"
    );
    assert_eq!(owner_state(&pair), before, "a rerun changes nothing");
}

/// A settlement that recorded no model gets the model of the follower's
/// implementing launch record, and a later report naming another model is
/// refused as `model_conflict` with nothing recorded, its effort included.
fn a_late_model_report_fills_an_absent_model_and_never_overwrites_one() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, claim) = settled_without_effort(&pair);
    assert_eq!(
        owner_models_and_efforts(&pair),
        [(json!("model unknown"), json!("unknown"))],
        "the settlement recorded neither"
    );
    record_follower_launch_on(&pair, &leaf, ReasoningEffort::High, "model-fixture");
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.recorded, report.models_recorded),
        (1, 1),
        "{report:#?}"
    );
    assert_eq!(
        report.rows[0].crew_model.as_deref(),
        Some("model-fixture"),
        "{report:#?}"
    );
    assert_eq!(
        owner_models_and_efforts(&pair),
        [(json!("model-fixture"), json!("high"))]
    );

    let before = owner_state(&pair);
    let error = pair
        .wire
        .call(
            "",
            "orbit.drain.claim.effort",
            json!({
                "claim_id": claim,
                "run_id": leaf,
                "reasoning_effort": "high",
                "crew_model": "another-model",
            }),
        )
        .expect_err("a disagreeing model is refused");
    assert!(error.to_string().contains("model_conflict"), "{error}");
    assert_eq!(owner_state(&pair), before, "the held model stands");
}

/// Replaying a pre-crew late report with the same effort and model records the
/// newer follower's crew, so upgrading a follower can repair old attribution.
fn a_replayed_crewless_report_can_be_enriched_with_its_crew() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (leaf, claim) = settled_without_effort(&pair);
    forget_owner_settlement(&pair, &claim, None);
    record_follower_launch_on(&pair, &leaf, ReasoningEffort::High, "model-luna");

    pair.wire
        .call(
            "",
            "orbit.drain.claim.effort",
            json!({
                "claim_id": claim,
                "run_id": leaf,
                "reasoning_effort": "high",
                "crew_model": "model-luna",
            }),
        )
        .expect("the older crewless report is accepted");
    assert_eq!(
        owner_crew_models_and_efforts(&pair),
        [(json!("sol"), json!("model-luna"), json!("high"))],
        "before the upgraded follower reports its crew"
    );

    set_follower_run_crew(&pair, &leaf, "luna");
    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.replayed, report.models_replayed),
        (1, 1),
        "{report:#?}"
    );
    assert_eq!(
        owner_crew_models_and_efforts(&pair),
        [(json!("luna"), json!("model-luna"), json!("high"))],
        "the same effort and model replay while the crew attribution is enriched"
    );
}

/// Give the follower's leaf run `leaf` the crew it actually ran as, as its
/// run record holds it.
fn set_follower_run_crew(pair: &Pair, leaf: &str, crew: &str) {
    let mut run = pair.follower_jobs.get_job_run(leaf).unwrap().unwrap();
    run.resolved_crew = Some(crew.into());
    let workspace = pair.follower.workspace_id().unwrap();
    pair.follower
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&workspace, &run, None)
        .unwrap();
}

/// The owner's implementer scorecard rows as `(crew, model, effort)`.
fn owner_crew_models_and_efforts(pair: &Pair) -> Vec<(Value, Value, Value)> {
    let card = pair
        .wire
        .owner
        .run_tool("orbit.agents.scorecard", json!({"role": "implementers"}))
        .expect("owner scorecard");
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["crew"].clone(),
                row["model"].clone(),
                row["reasoning_effort"].clone(),
            )
        })
        .collect()
}

/// [ORB-24206] A task re-crewed between two pulled claims, each settled
/// without a settlement record and each late-reported with the crew its leaf
/// ran as. The first leaf ran as `luna` although the task began its attempt
/// crewed `sol`; the second ran as `opus` after the re-crew. Each attempt keys
/// to the crew its own report names together with its model, never to the
/// crew history held at its start.
fn a_re_crewed_task_keys_each_late_attempt_to_its_reported_crew() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let (first_leaf, first_claim) = settled_without_effort(&pair);
    let task = pair.claimed_task(&first_leaf);
    forget_owner_settlement(&pair, &first_claim, None);
    set_follower_run_crew(&pair, &first_leaf, "luna");
    record_follower_launch_on(&pair, &first_leaf, ReasoningEffort::High, "model-luna");

    pair.wire
        .owner
        .update_task_as_human(
            &task,
            orbit_core::application::task::TaskUpdateParams {
                crew: Some(Some("opus".into())),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .expect("operator crew update");

    let second_drain = pair.run_drain();
    let second_leaf = pair.running_leaf(&second_drain, 1);
    let (second_leaf, second_claim) = cancel_without_effort(&pair, second_leaf);
    forget_owner_settlement(&pair, &second_claim, None);
    set_follower_run_crew(&pair, &second_leaf, "opus");
    record_follower_launch_on(&pair, &second_leaf, ReasoningEffort::High, "model-opus");

    let report = pair.follower.backfill_claimed_leaf_effort().unwrap();
    assert_eq!(
        (report.examined, report.recorded, report.models_recorded),
        (2, 2, 2),
        "{report:#?}"
    );
    let mut rows = owner_crew_models_and_efforts(&pair);
    rows.retain(|(_, model, _)| *model == json!("model-luna") || *model == json!("model-opus"));
    rows.sort_by_key(|(crew, _, _)| crew.to_string());
    assert_eq!(
        rows,
        [
            (json!("luna"), json!("model-luna"), json!("high")),
            (json!("opus"), json!("model-opus"), json!("high")),
        ],
        "each attempt counts under the crew its own report named"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    late_effort_report_scenarios,
    a_followers_late_report_moves_a_settled_attempt_out_of_effort_unknown,
    a_late_report_from_another_machine_is_refused,
    a_late_report_disagreeing_with_a_held_effort_is_refused_and_counted,
    a_leaf_without_a_follower_record_stays_unknown_and_is_counted,
    an_owner_that_predates_the_report_refuses_it_without_changing_state,
    another_rpc_refusal_is_refused_without_changing_state,
    a_connection_failure_remains_owner_unreachable,
    a_late_report_for_a_claim_without_a_settlement_record_is_recorded,
    a_late_model_report_fills_an_absent_model_and_never_overwrites_one,
    a_replayed_crewless_report_can_be_enriched_with_its_crew,
    a_re_crewed_task_keys_each_late_attempt_to_its_reported_crew,
);
