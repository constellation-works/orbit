//! A task's `requires_labels` follow the follower's declared labels through
//! owner pull admission.
//!
//! The follower's OS and labels are composed, so the same pair withholds the
//! task and then claims it whatever machine runs the test.

use orbit_core::application::task::TaskUpdateParams;
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

/// Grants used to reach claimed implementers despite their closed owner
/// bridge. Even a grant for an ordinary process tool must stay owner-local.
fn granted_tasks_stay_on_the_owner_before_any_claim_or_leaf() {
    let pair = Pair::new(2);
    let owner = &pair.wire.owner;
    let grants = [
        vec![
            "orbit.auto_task.update".to_string(),
            "proc.spawn".to_string(),
        ],
        vec!["proc.spawn".to_string()],
    ];
    for (task, granted) in pair.tasks.iter().zip(&grants) {
        owner
            .update_task_as_human(
                task,
                TaskUpdateParams {
                    granted_tools: Some(granted.clone()),
                    ..Default::default()
                },
                "human:fixture".to_string(),
            )
            .unwrap();
    }
    let local = owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    for task in &pair.tasks {
        assert!(
            local["task_ids"].as_array().unwrap().contains(&json!(task)),
            "{local}"
        );
    }
    let drain = pair.start_drain();
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "grants must stay on the owner: {idle}");
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    let request = pair.wire.calls("orbit.task.pull").pop().unwrap();
    let receipt = pair.wire.call("", "orbit.task.pull", request).unwrap();
    for (task, granted) in pair.tasks.iter().zip(&grants) {
        let reason = receipt["receipt"]["deferred_conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["task_id"] == *task)
            .unwrap()["reason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("owner host"), "{reason}");
        for tool in granted {
            assert!(reason.contains(tool), "{reason}");
        }
        assert_eq!(pair.owner_status(task), "backlog");
        assert_eq!(owner.get_task(task).unwrap().granted_tools, *granted);
    }
    owner
        .update_task_as_human(
            &pair.tasks[0],
            TaskUpdateParams {
                granted_tools: Some(Vec::new()),
                ..Default::default()
            },
            "human:fixture".to_string(),
        )
        .unwrap();
    let leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), pair.tasks[0]);
    assert!(!pair.owner_has_claim_for_task(&pair.tasks[1]));
}

/// A live labelled follower cannot satisfy an owner's grant/label conflict.
/// Readiness, dispatch and pull must name the intersection and keep ownership.
fn grants_and_labels_require_the_owner_to_carry_every_label() {
    let mut pair = Pair::new(0);
    pair.follower = pair
        .follower
        .clone()
        .with_machine_labels(vec!["gpu".to_string()]);
    let drain = pair.start_drain();
    assert_eq!(pair.pass(&drain)["admitted"], 0);
    let owner = &pair.wire.owner;
    let task = backlog_task_with_input(
        owner,
        &pair.owner_repo,
        "src/granted.rs",
        None,
        json!({"requires_labels": ["gpu"]}),
    );
    owner
        .update_task_as_human(
            &task,
            TaskUpdateParams {
                granted_tools: Some(vec!["orbit.auto_task.update".to_string()]),
                ..Default::default()
            },
            "human:fixture".to_string(),
        )
        .unwrap();

    let shown = owner
        .run_tool("orbit.task.show", json!({"id": task}))
        .unwrap();
    let wait = shown["placement_wait"]
        .as_str()
        .expect("the owner cannot satisfy both constraints");
    for detail in [
        "owner host",
        "orbit.auto_task.update",
        "gpu",
        "requires_labels",
    ] {
        assert!(wait.contains(detail), "{wait}");
    }
    let local = owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(local["task_ids"].as_array().unwrap().is_empty(), "{local}");
    let exclusion = local["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .unwrap();
    assert_eq!(exclusion["reason"], "host_labels_missing");
    assert_eq!(exclusion["detail"], wait);
    let ready = owner.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let row = ready["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap();
    assert_eq!(row["eligible"], false, "{ready}");
    assert_eq!(row["detail"], wait);
    assert_eq!(row["placement_wait"], wait);

    let idle = pair.pass(&drain);
    assert_eq!(
        idle["admitted"], 0,
        "labels must never override ownership: {idle}"
    );
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    let request = pair.wire.calls("orbit.task.pull").pop().unwrap();
    let receipt = pair.wire.call("", "orbit.task.pull", request).unwrap();
    let held = receipt["receipt"]["deferred_conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap();
    assert_eq!(held["reason"], wait, "{receipt}");

    let matching_owner = owner.clone().with_machine_labels(vec!["gpu".to_string()]);
    assert!(
        matching_owner
            .task_placement_wait(&owner.get_task(&task).unwrap())
            .is_none()
    );
    let eligible = matching_owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert!(
        eligible["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task)),
        "{eligible}"
    );
    assert_eq!(pair.owner_status(&task), "backlog");
    assert_eq!(owner.get_task(&task).unwrap().requires_labels, vec!["gpu"]);
}

/// An operator grant arriving after the initial hold snapshot still fences
/// the transaction that would otherwise create a follower claim.
fn a_grant_added_during_admission_is_rechecked_before_claim() {
    let pair = Pair::new(1);
    let owner = pair.wire.owner.clone();
    let task = pair.tasks[0].clone();
    let drain = pair.start_drain();
    let idle = orbit_core::test_support::with_pull_admission_checkpoint(
        move || {
            owner
                .update_task_as_human(
                    &task,
                    TaskUpdateParams {
                        granted_tools: Some(vec!["proc.spawn".to_string()]),
                        ..Default::default()
                    },
                    "human:fixture".to_string(),
                )
                .unwrap();
        },
        || pair.pass(&drain),
    );
    assert_eq!(
        idle["admitted"], 0,
        "the transaction must recheck the grant: {idle}"
    );
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

crate::scenario_table!(
    granted_tasks_stay_on_the_owner_scenarios,
    granted_tasks_stay_on_the_owner_before_any_claim_or_leaf,
    grants_and_labels_require_the_owner_to_carry_every_label,
    a_grant_added_during_admission_is_rechecked_before_claim
);
