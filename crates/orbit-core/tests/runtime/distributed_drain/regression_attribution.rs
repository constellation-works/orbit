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

/// QA sweeps and whole-review area chores file classified findings through
/// the same claim-scoped public tool boundary as delivery review.
fn claimed_qa_and_area_reviews_file_only_workspace_regressions() {
    for tags in [
        vec!["auto-task:qa-sweep"],
        vec!["auto-task:qa-full-sweep"],
        vec!["full-code-review", "no-diff-expected"],
        vec!["qa-full-sweep", "no-diff-expected"],
    ] {
        let leaf = super::claimed_review::ReviewedLeaf::admit_launched("", "", |pair, drain| {
            pair.wire
                .owner
                .update_task_as_human(
                    &pair.tasks[0],
                    orbit_core::application::task::TaskUpdateParams {
                        tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
                        ..Default::default()
                    },
                    "fixture operator".into(),
                )
                .unwrap();
            pair.launched_leaf(drain, 1, std::process::id())
        });
        let owner = &leaf.pair.wire.owner;
        let culprit = culprit(&leaf.pair);
        let foreign_repo = leaf.pair._root.path().join(OWNER).join("other-repo");
        std::fs::create_dir_all(foreign_repo.join(".orbit")).unwrap();
        let foreign_runtime = OrbitRuntime::from_roots(
            &leaf.pair._root.path().join(OWNER).join("global"),
            &foreign_repo.join(".orbit"),
        )
        .unwrap();
        let foreign = foreign_runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "A task in another owner workspace",
                    "description": "This is not a culprit in the claimed workspace.",
                    "complexity": "low", "model": "codex",
                    "workspace": foreign_repo,
                }),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let relations = |target: &str| {
            json!([
                {"type": "spawned_from", "target": leaf.task},
                {"type": "regression_from", "target": target,
                 "regression": {"nature": "broke_existing", "escape": "no_test"}},
            ])
        };
        let file = |target: &str| {
            leaf.bound.run_tool(
                "orbit.task.add",
                json!({
                    "title": "A confirmed QA regression",
                    "description": "The culprit commit broke the observed behavior.",
                    "type": "bug", "complexity": "low", "model": "codex",
                    "relations": relations(target),
                }),
            )
        };
        let filed = file(&culprit).unwrap_or_else(|error| panic!("{tags:?}: {error}"));
        assert_eq!(
            leaf.pair.owner_task(filed["id"].as_str().unwrap())["relations"],
            relations(&culprit)
        );
        let before = owner.list_tasks().unwrap().len();
        let refused = file(&foreign).unwrap_err().to_string();
        assert!(
            refused.contains("not a task in the claimed task's workspace"),
            "{tags:?}: {refused}"
        );
        assert_eq!(owner.list_tasks().unwrap().len(), before);
        let (_, worker) = leaf.claim();
        owner
            .mutate_execution_claim(
                Some(&worker),
                "release",
                &ClaimMutation::Release(ClaimEvidence {
                    summary: Some("The QA worker returned its claim.".into()),
                    ..Default::default()
                }),
            )
            .unwrap();
        let stale = file(&culprit).unwrap_err().to_string();
        assert!(stale.contains("stale_claim"), "{stale}");
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_claimed_handoff_records_its_regression_culprit_scenarios,
    a_claimed_handoff_records_its_regression_culprit,
    claimed_qa_and_area_reviews_file_only_workspace_regressions
);
