//! A claimed leaf applies the tag instructions its owner captured in the
//! claim's receipt, not its replica's own `workflow.tag_instructions`. Per-host
//! `.orbit/` configuration is untracked and drifts, so the replica's mapping
//! could otherwise add or drop instructions the owner's runs would not.

use orbit_types::workflow::TagInstructionRole;

use super::claimed_review::ReviewedLeaf;
use super::*;

const OWNER: &str = "[workflow.tag_instructions.no-diff-expected]\nimplement = false\n";
/// A mapping the replica cannot read: consulting it would refuse the task.
const REPLICA: &str = "[workflow.tag_instructions.no-diff-expected]\nimplement = \"missing.md\"\n";

fn implement_tags(host: &OrbitRuntime, task: &str) -> Vec<String> {
    RuntimeHost::tag_instructions_for_activity(
        host,
        "jrun-tag-instructions",
        "agent_implement",
        &json!({ "task_id": task }),
    )
    .expect("tag instructions resolve")
    .into_iter()
    .map(|instruction| instruction.tag)
    .collect()
}

fn a_claimed_leaf_applies_the_owners_captured_tag_instructions() {
    // A claimed task's tags are frozen, so tag it before the leaf claims it.
    let leaf = ReviewedLeaf::admit_launched(OWNER, REPLICA, |pair, drain| {
        pair.wire
            .owner
            .update_task_as_human(
                &pair.tasks[0],
                orbit_core::application::task::TaskUpdateParams {
                    tags: Some(vec!["no-diff-expected".into()]),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
        pair.launched_leaf(drain, 1, std::process::id())
    });
    let captured = leaf
        .pair
        .admission(&leaf.leaf)
        .receipt
        .unwrap()
        .tag_instructions
        .expect("the owner captures its tag instructions");
    assert!(
        captured.instructions.iter().all(|instruction| {
            !(instruction.tag == "no-diff-expected"
                && instruction.role == TagInstructionRole::Implement)
        }),
        "a default the owner disabled is not captured"
    );

    assert!(
        implement_tags(&leaf.bound, &leaf.task).is_empty(),
        "the leaf applies the owner's mapping, not the replica's"
    );
    assert!(
        implement_tags(&leaf.pair.wire.owner, &leaf.task).is_empty(),
        "the owner's own runs apply the same instructions"
    );
}

crate::scenario_table!(
    a_claimed_leaf_applies_the_owners_captured_tag_instructions_scenarios,
    a_claimed_leaf_applies_the_owners_captured_tag_instructions,
);
