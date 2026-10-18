//! A `ui` task follows the follower's verified browser kit through owner pull
//! admission.
//!
//! The follower's kit home and OS are composed, so the same pair withholds
//! the task and then claims it whatever kit the machine running it has.

use orbit_types::task::HostOs;

use super::*;

/// A Linux follower without a verified kit sends no `browser_kit` field,
/// the request an older follower sends, and is handed neither `ui` task. Its
/// idle pass names `browser_kit_missing` and the install command for the
/// task Linux could run, and `host_os_mismatch` for the `os:macos` one. Once
/// its kit is verified, the next drain declares the kit and claims the `ui`
/// task; the `os:macos` one still waits, since a kit does not change the OS.
fn ui_claims_go_only_to_a_follower_that_reports_a_verified_kit() {
    let mut pair = Pair::new(3);
    let (ui, mac_ui, plain) = (
        pair.tasks[0].clone(),
        pair.tasks[1].clone(),
        pair.tasks[2].clone(),
    );
    for (task, tags) in [(&ui, json!(["UI"])), (&mac_ui, json!(["os:macos", "ui"]))] {
        pair.wire
            .owner
            .run_tool(
                "orbit.task.update",
                json!({"id": task, "tags": tags, "model": "codex"}),
            )
            .expect("tag owner task");
    }
    let kit_home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    pair.follower = pair
        .follower
        .clone()
        .with_host_os(Some(HostOs::Linux))
        .with_browser_kit_home(kit_home.path());

    let drain = pair.start_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), plain);
    pair.leaf_fails_with(&leaf, "candidate validation failed");
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");
    for task in [&ui, &mac_ui] {
        assert_eq!(pair.owner_status(task), "backlog");
        assert!(
            !pair.owner_has_claim_for_task(task),
            "{:#?}",
            pair.owner_claims()
        );
    }
    let pulls = pair.wire.calls("orbit.task.pull");
    assert!(
        pulls.iter().all(|pull| pull.get("browser_kit").is_none()),
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
    let waiting = |task: &str| {
        recorded
            .excluded
            .iter()
            .find(|entry| entry.task_id == task)
            .unwrap_or_else(|| panic!("{task} is excluded: {recorded:#?}"))
    };
    let kit_wait = waiting(&ui);
    assert_eq!(kit_wait.reason.as_deref(), Some("browser_kit_missing"));
    assert!(
        kit_wait
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("`orbit browser-kit install`")),
        "{kit_wait:?}"
    );
    assert_eq!(waiting(&mac_ui).reason.as_deref(), Some("host_os_mismatch"));

    super::super::host_ui_routing::write_kit(kit_home.path(), true);
    let kit_drain = pair.start_drain();
    let kit_leaf = pair.running_leaf(&kit_drain, 1);
    assert_eq!(pair.claimed_task(&kit_leaf), ui);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").last().unwrap()["browser_kit"],
        true
    );
    assert_eq!(pair.owner_status(&mac_ui), "backlog");
    assert!(!pair.owner_has_claim_for_task(&mac_ui));
}

crate::scenario_table!(
    ui_claims_go_only_to_a_follower_that_reports_a_verified_kit_scenarios,
    ui_claims_go_only_to_a_follower_that_reports_a_verified_kit
);
