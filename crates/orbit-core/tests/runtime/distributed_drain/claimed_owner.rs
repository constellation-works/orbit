//! A claimed worker files follow-up work on its owner [ORB-14260].
//!
//! A claimed leaf's binding reaches the owner exactly as its run's broker
//! forwards a bridged call. The owner creates a task from it only when every
//! relation the task declares is `spawned_from` the claimed task, and only
//! while the claim is still active; a friction the worker files is recorded
//! during the claimed task whatever the call names.

use super::*;

use super::claimed_review::ReviewedLeaf;

#[test]
fn a_claimed_worker_files_follow_up_work_only_from_its_active_claim() {
    if !isolated(
        module_path!(),
        "a_claimed_worker_files_follow_up_work_only_from_its_active_claim",
    ) {
        return;
    }
    let leaf = ReviewedLeaf::admit();
    let add = |relations: Value| {
        leaf.bound
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Follow up on the claimed work",
                    "description": "Found while implementing the claimed task.",
                    "complexity": "low", "relations": relations, "model": "codex",
                }),
            )
            .map_err(|error| error.to_string())
    };
    let spawned = json!([{"type": "spawned_from", "target": leaf.task}]);

    let created = add(spawned.clone()).expect("a follow-up spawned from the claim");
    let id = created["id"].as_str().expect("the new task's id");
    let stored = leaf
        .pair
        .wire
        .owner
        .run_tool("orbit.task.show", json!({"id": id}))
        .expect("the owner holds the new task");
    assert_eq!(stored["relations"], spawned, "{stored}");

    for (case, relations) in [
        ("no relation", json!([])),
        (
            "another task",
            json!([{"type": "spawned_from", "target": "TSO-999"}]),
        ),
        (
            "an extra relation",
            json!([{"type": "spawned_from", "target": leaf.task},
                   {"type": "blocks", "target": "TSO-999"}]),
        ),
    ] {
        let refused = add(relations).unwrap_err();
        assert!(refused.contains("spawned_from"), "{case}: {refused}");
    }

    let friction = leaf
        .bound
        .run_tool(
            "orbit.friction.add",
            json!({"body": "The owner route was slow.", "model": "codex"}),
        )
        .expect("a friction during the claim");
    assert_eq!(friction["during_task"], leaf.task, "{friction}");

    let (_, worker) = leaf.claim();
    leaf.pair
        .wire
        .owner
        .mutate_execution_claim(
            Some(&worker),
            "release",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("The executor gave the claim back.".into()),
                ..ClaimEvidence::default()
            }),
        )
        .expect("the executor releases its claim");
    let stale = add(spawned).unwrap_err();
    assert!(stale.contains("stale_claim"), "{stale}");
}
