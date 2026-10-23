//! A claimed implementer cannot write its task's relations, so it returns
//! the culprit of the regression it repaired in its handoff. The owner
//! records that `regression_from` relation when the follower's drain
//! delivers the handoff, keeping the task's other relations [F2026-10-453].
//! The shipped pipeline's half, and the refusal of an unknown culprit, are
//! in `no_diff`'s clean-leaf scenarios.

use super::*;

/// An owner task outside the backlog, so the drain never claims it.
fn culprit(pair: &Pair) -> String {
    let task = pair
        .wire
        .owner
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "Land the change that broke CI",
                "description": "Regression attribution fixture culprit.",
                "acceptance_criteria": ["Landed."],
                "type": "chore",
                "complexity": "low",
                "workspace": pair.owner_repo.to_string_lossy(),
                "model": "codex",
            }),
        )
        .expect("culprit task");
    task["id"].as_str().unwrap().to_string()
}

/// Hand the pair's claimed leaf off attributing its regression to
/// `regression_from`, and return the owner's settlement pass with the
/// handoff and the drain.
fn hand_off_attributing(pair: &Pair, regression_from: &str) -> (Value, TaskHandoff, String) {
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let mut delivered = handoff(&pair.admission(&leaf));
    delivered.regression_from = vec![regression_from.to_string()];
    pair.advance(
        &leaf,
        LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(delivered.clone()))),
    );
    pair.follower_jobs
        .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    (pair.pass(&drain), delivered, drain)
}

fn a_claimed_handoff_records_its_regression_culprit() {
    let pair = Pair::new(1);
    let culprit = culprit(&pair);
    pair.wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({
                "id": pair.tasks[0],
                "relations": [{"type": "related_to", "target": culprit}],
                "model": "codex",
            }),
        )
        .expect("existing relation");

    let (settled, delivered, _) = hand_off_attributing(&pair, &culprit);
    assert!(settled["error"].is_null(), "{settled}");

    let task = pair.owner_task(&delivered.task_id);
    assert_eq!(task["status"], "review", "{task}");
    let relations = task["relations"]
        .as_array()
        .expect("relations")
        .iter()
        .map(|relation| {
            (
                relation["type"].as_str().unwrap().to_string(),
                relation["target"].as_str().unwrap().to_string(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        relations,
        BTreeSet::from([
            ("related_to".to_string(), culprit.clone()),
            ("regression_from".to_string(), culprit.clone()),
        ]),
        "the owner records the attributed culprit without an operator write and keeps the \
         task's existing relations: {task}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_claimed_handoff_records_its_regression_culprit_scenarios,
    a_claimed_handoff_records_its_regression_culprit
);
