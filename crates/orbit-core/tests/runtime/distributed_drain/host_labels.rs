//! A task's `requires_labels` follow the follower's declared labels through
//! owner pull admission.
//!
//! The follower's OS and labels are composed, so the same pair withholds the
//! task and then claims it whatever machine runs the test.

use orbit_types::task::HostOs;

use super::*;

/// A follower without `gpu` in its `machine.labels` sends no `labels` field
/// and is not handed the task requiring `gpu`: it claims the unlabelled task,
/// and its idle pass names `host_labels_missing` and the missing label. Once
/// it declares `gpu`, the next drain sends it and claims the labelled task.
fn labelled_claims_go_only_to_a_follower_that_carries_every_label() {
    // The owner carries `gpu`, so the requirement is servable at write.
    let mut pair = Pair::with_owner(2, |owner| {
        owner.with_machine_labels(vec!["gpu".to_string()])
    });
    let (labelled, plain) = (pair.tasks[0].clone(), pair.tasks[1].clone());
    pair.wire
        .owner
        .run_tool(
            "orbit.task.update",
            json!({"id": labelled, "requires_labels": ["gpu"], "model": "codex"}),
        )
        .expect("label owner task");
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Linux));

    let drain = pair.start_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), plain);
    pair.leaf_fails_with(&leaf, "candidate validation failed");
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");
    assert_eq!(pair.owner_status(&labelled), "backlog");
    assert!(
        !pair.owner_has_claim_for_task(&labelled),
        "{:#?}",
        pair.owner_claims()
    );
    let pulls = pair.wire.calls("orbit.task.pull");
    assert!(
        pulls.iter().all(|pull| pull.get("labels").is_none()),
        "{pulls:?}"
    );
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the idle pass is recorded");
    let wait = recorded
        .excluded
        .iter()
        .find(|entry| entry.task_id == labelled)
        .unwrap_or_else(|| panic!("{labelled} is excluded: {recorded:#?}"));
    assert_eq!(wait.reason.as_deref(), Some("host_labels_missing"));
    assert_eq!(
        wait.detail.as_deref(),
        Some("waits for a host labelled `gpu` (requires_labels); the executor lacks `gpu`"),
        "{wait:?}"
    );

    pair.follower = pair
        .follower
        .clone()
        .with_machine_labels(vec!["gpu".to_string()]);
    let labelled_drain = pair.start_drain();
    let labelled_leaf = pair.running_leaf(&labelled_drain, 1);
    assert_eq!(pair.claimed_task(&labelled_leaf), labelled);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").last().unwrap()["labels"],
        json!(["gpu"])
    );
}

crate::scenario_table!(
    labelled_claims_go_only_to_a_follower_that_carries_every_label_scenarios,
    labelled_claims_go_only_to_a_follower_that_carries_every_label
);
