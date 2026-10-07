//! A pull drain records what its owner kept off this host [ORB-14475].

use std::collections::BTreeMap;

use orbit_types::task::HostOs;

use super::admission::local_drain_admission;
use super::*;

/// What a recorded waiting task says, by task id: its reason code and the
/// tasks it waits on.
fn by_task(
    tasks: &[orbit_types::workflow::DrainWaitingTask],
) -> BTreeMap<String, (String, Vec<String>)> {
    tasks
        .iter()
        .map(|task| {
            (
                task.task_id.clone(),
                (
                    task.reason.clone().unwrap_or_default(),
                    task.blocked_by.clone(),
                ),
            )
        })
        .collect()
}

/// An owner whose whole backlog is kept off the follower — one task each held
/// by a protected footprint, an unfinished dependency, an owner-local hold, an
/// `os:` tag and a crew this host cannot run — answers idle, and the drain's
/// last pass records that answer rather than an empty backlog. A pass that
/// sends no request keeps it, with the time the owner gave it.
#[test]
fn an_idle_pull_drain_records_what_its_owner_kept_off_this_host() {
    if !isolated(
        module_path!(),
        "an_idle_pull_drain_records_what_its_owner_kept_off_this_host",
    ) {
        return;
    }
    let mut pair = Pair::with_crews(&[Some("antigravity"), None, None, None, None, None]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Linux));
    let [crewless, other_os, dependent, footprint, held, holder] =
        pair.tasks.clone().try_into().unwrap();
    let owner = &pair.wire.owner;
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": other_os, "tags": ["os:macos"], "model": "codex"}),
        )
        .expect("tag the macos task");
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": holder, "status": "in-progress", "model": "codex"}),
        )
        .expect("the holder is in progress");
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": dependent, "dependencies": [holder], "model": "codex"}),
        )
        .expect("depend on the unfinished task");
    owner
        .update_task_as_human(
            &footprint,
            orbit_core::application::task::TaskUpdateParams {
                context_files: Some(vec!["file:src/f5.rs".into()]),
                ..Default::default()
            },
            "fixture operator".into(),
        )
        .expect("share the holder's footprint");
    local_drain_admission(owner, &held);
    let drain = pair.start_drain();

    let first = pair.pass(&drain);
    assert_eq!(first["admitted"], 0, "{first}");
    assert!(pair.owner_claims().is_empty(), "{:#?}", pair.owner_claims());
    let receipt = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .expect("the pass is recorded");

    assert_eq!(receipt.queue_depth, 3, "{receipt:#?}");
    assert_eq!(recorded.queued, receipt.queue_depth as u64, "{recorded:#?}");
    assert_eq!(
        by_task(&recorded.deferred),
        BTreeMap::from([
            (
                footprint.clone(),
                ("context_lock_conflict".into(), vec![holder.clone()])
            ),
            (held.clone(), ("owner_hold".into(), vec![])),
        ]),
        "{recorded:#?}"
    );
    assert_eq!(
        by_task(&recorded.excluded),
        BTreeMap::from([
            (
                dependent.clone(),
                ("dependency_not_done".into(), vec![holder.clone()])
            ),
            (other_os.clone(), ("host_os_mismatch".into(), vec![])),
            (crewless.clone(), ("crew_unavailable".into(), vec![])),
        ]),
        "{recorded:#?}"
    );
    assert_eq!(recorded.excluded_total, 3, "{recorded:#?}");
    assert_eq!(
        recorded.waiting_by_reason,
        BTreeMap::from(
            [
                ("context_lock_conflict", 1),
                ("owner_hold", 1),
                ("dependency_not_done", 1),
                ("host_os_mismatch", 1),
                ("crew_unavailable", 1),
            ]
            .map(|(reason, count)| (reason.to_string(), count))
        ),
        "{recorded:#?}"
    );
    assert_eq!(recorded.consecutive_idle_passes, 1);
    let answered = recorded
        .waiting_recorded_at
        .expect("the owner's answer is timed");
    // The owner's own sentence rides along for the reasons its code alone
    // does not explain.
    assert!(
        recorded
            .excluded
            .iter()
            .find(|task| task.task_id == other_os)
            .and_then(|task| task.detail.as_deref())
            .is_some_and(|detail| detail.contains("macos")),
        "{recorded:#?}"
    );
    let shown = serde_json::to_value(&recorded).unwrap();
    assert_eq!(shown["queued"], 3, "{shown}");
    assert_eq!(shown["excluded_total"], 3, "{shown}");

    // A pass that sends no request — a closed window, or an unreachable
    // owner — keeps the answer and its age instead of reading as empty.
    let pulls = pair.wire.calls("orbit.task.pull").len();
    let closed = pair.pass_over(&drain, json!({"window_expired": true}));
    assert_eq!(closed["admitting"], false, "{closed}");
    *pair.wire.unreachable.lock().unwrap() = true;
    let outage = pair.pass(&drain);
    assert!(!error_of(&outage).is_empty(), "{outage}");
    *pair.wire.unreachable.lock().unwrap() = false;
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), pulls);
    let kept = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .unwrap();
    assert_eq!(kept.queued, 3, "{kept:#?}");
    assert_eq!(kept.excluded_total, 3, "{kept:#?}");
    assert_eq!(by_task(&kept.deferred), by_task(&recorded.deferred));
    assert_eq!(by_task(&kept.excluded), by_task(&recorded.excluded));
    assert_eq!(kept.waiting_by_reason, recorded.waiting_by_reason);
    assert_eq!(kept.consecutive_idle_passes, 1);
    assert_eq!(kept.waiting_recorded_at, Some(answered));
    assert!(kept.recorded_at > answered, "{kept:#?}");

    // Idle answers that keep finding tasks waiting add up.
    pair.pass(&drain);
    let third = pair.pass(&drain);
    assert_eq!(third["admitted"], 0, "{third}");
    let counted = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .unwrap();
    assert_eq!(counted.consecutive_idle_passes, 3, "{counted:#?}");
    assert!(counted.waiting_recorded_at > Some(answered), "{counted:#?}");
}
