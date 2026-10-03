//! The shared admission decision every retained entry point takes.

use super::*;

/// A backlog task that automatic dispatch would actually offer: assessed
/// complexity, so the readiness assertions below are about admission rather
/// than about the task-pilot preparation gate.
fn admissible_task(runtime: &OrbitRuntime, title: &str, context_files: &[&str]) -> String {
    runtime
        .add_task(crate::application::task::TaskAddParams {
            title: title.to_string(),
            description: "shared-admission fixture".to_string(),
            plan: "fixture".to_string(),
            complexity: orbit_types::task::TaskComplexity::Low,
            context_files: context_files
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add admissible task")
        .id
}

/// [ORB-12500] The retained entry points — an explicit ship, an owner drain,
/// and the independent registry-driven sweep — converge on one admission
/// decision. This is the mixed-entry-point fence: a task a live claim is
/// executing cannot be shipped beside itself, whichever surface asks.
#[test]
fn a_live_claim_fences_every_retained_entry_point_from_the_same_task() {
    if !enter_isolated_child(
        "entry::a_live_claim_fences_every_retained_entry_point_from_the_same_task",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let task = admissible_task(&runtime, "claimed by a live attempt", &["src/a.rs"]);
    let lookup = admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("claimed-entry", owner_binary_version()),
        owner_binary_version(),
    );
    let claim = claim_id(&lookup);

    // Explicit shipment: the refusal names the claim rather than dispatching a
    // second attempt at the same work.
    let refused = runtime
        .submit_ship_run(
            ShipMode::Pr,
            None,
            std::slice::from_ref(&task),
            crate::application::workflow::CompletionPolicy::Review,
            &[],
            None,
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("a claimed task is not shipped beside its claim");
    let message = refused.to_string();
    assert!(
        message.contains(&task) && message.contains(&claim),
        "{message}"
    );

    // The shared decision itself, for the surfaces that report rather than
    // raise.
    let decision = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ExplicitShip,
            std::slice::from_ref(&task),
            false,
        )
        .expect("shared admission decision");
    assert_eq!(
        decision.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("claimed_by_execution_claim")
    );

    // And automatic discovery reaches the same answer through the claim's
    // frozen footprint rather than through the task's current status.
    let readiness = runtime
        .workspace_auto_readiness(std::slice::from_ref(&task), None, 10, &[])
        .expect("readiness");
    let entry = readiness["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .cloned()
        .expect("one readiness entry");
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(
        entry["reason"], "not_backlog",
        "admission moved the claimed task out of the backlog: {entry}"
    );
}

/// [ORB-12500] The unattended sweep stands down on a host whose drain slots
/// are occupied — including by a claimed leaf, which the `task_auto_pipeline`
/// history scan it used to run could not see at all. An operator's explicit
/// invocation is not stood down by the same reading: its own leaf definition
/// bounds it.
#[test]
fn claimed_occupancy_stands_the_unattended_sweep_down_but_not_an_explicit_ship() {
    if !enter_isolated_child(
        "entry::claimed_occupancy_stands_the_unattended_sweep_down_but_not_an_explicit_ship",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_claimed_local_pipeline",
            1,
            chrono::Utc::now(),
            None,
            None,
        )
        .expect("live claimed leaf");

    let sweep = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ShipSweep,
            &[],
            true,
        )
        .expect("sweep decision");
    assert_eq!(
        sweep.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("ship_in_flight")
    );
    assert_eq!(sweep.occupancy.occupied, 1);
    assert_eq!(
        sweep.occupancy.for_pipeline("task_claimed_local_pipeline"),
        1
    );

    let explicit = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ExplicitShip,
            &[],
            false,
        )
        .expect("explicit decision");
    assert!(explicit.refusal.is_none());
    assert_eq!(explicit.occupancy.occupied, 1);
}

/// [ORB-12968] With a host reboot scheduled, the unattended sweep stands down
/// and names the schedule; an operator's explicit ship and drain are admitted
/// with the schedule reported beside the decision.
#[test]
fn a_scheduled_host_shutdown_stands_unattended_entry_down_but_not_an_explicit_one() {
    if !enter_isolated_child(
        "entry::a_scheduled_host_shutdown_stands_unattended_entry_down_but_not_an_explicit_one",
    ) {
        return;
    }
    use crate::application::distributed::DrainEntryPoint;
    use crate::runtime::host_signal::{FixedHostSignals, ScheduledShutdown};

    let (_root, runtime, _repo_root) = test_runtime();
    let shutdown = ScheduledShutdown {
        mode: "poweroff".to_string(),
        scheduled_at: chrono::Utc::now() + chrono::Duration::hours(1),
        source: "fixture".to_string(),
    };
    let held = runtime.with_host_signal_probe(std::sync::Arc::new(FixedHostSignals::scheduled(
        shutdown.clone(),
    )));

    let sweep = held
        .drain_entry_admission(DrainEntryPoint::ShipSweep, &[], true)
        .expect("sweep decision");
    assert_eq!(
        sweep.refusal.as_ref().map(DrainEntryRefusal::code),
        Some("host_shutdown_scheduled")
    );
    let reason = sweep.refusal.as_ref().map(DrainEntryRefusal::reason);
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains(&shutdown.describe())),
        "{reason:?}"
    );
    assert!(matches!(
        sweep.into_result(),
        Err(orbit_common::OrbitError::PolicyDenied(_))
    ));

    for entry in [DrainEntryPoint::ExplicitShip, DrainEntryPoint::OwnerDrain] {
        let explicit = held
            .drain_entry_admission(entry, &[], false)
            .expect("explicit decision");
        assert!(explicit.refusal.is_none(), "{}", entry.label());
        assert_eq!(explicit.host_shutdown.as_ref(), Some(&shutdown));
    }
}

/// [ORB-12500] A replica serves no owner coordination from any retained entry
/// point. It executes through pull instead, which is the whole point of the
/// role split.
#[test]
fn a_replica_checkout_refuses_every_retained_entry_point() {
    if !enter_isolated_child("entry::a_replica_checkout_refuses_every_retained_entry_point") {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let replica = runtime.with_coordination_write_owner(Some(OWNER.to_string()));
    for entry in [
        crate::application::distributed::DrainEntryPoint::OwnerDrain,
        crate::application::distributed::DrainEntryPoint::ExplicitShip,
        crate::application::distributed::DrainEntryPoint::ShipSweep,
    ] {
        let decision = replica
            .drain_entry_admission(entry, &[], true)
            .expect("replica decision");
        assert_eq!(
            decision.refusal.as_ref().map(DrainEntryRefusal::code),
            Some("replica_checkout"),
            "{} must refuse owner coordination on a replica",
            entry.label()
        );
        assert!(matches!(
            decision.into_result(),
            Err(orbit_common::OrbitError::CapabilityRefused(_))
        ));
    }
}

/// [ORB-12500] A scheduled invocation carries no completion authority, and the
/// shared decision reports the review policy the claim contract admits so no
/// surface has to look it up for itself.
#[test]
fn the_shared_decision_reports_the_claim_contract_verdict() {
    if !enter_isolated_child("entry::the_shared_decision_reports_the_claim_contract_verdict") {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let decision = runtime
        .drain_entry_admission(
            crate::application::distributed::DrainEntryPoint::ShipSweep,
            &[],
            true,
        )
        .expect("sweep decision");
    assert_eq!(decision.review_policy, "none");
    assert!(
        decision.claim_admission_refusal.is_none(),
        "a `none`-policy owner passes the claim contract's ladder: {:?}",
        decision.claim_admission_refusal
    );
    assert!(decision.refusal.is_none(), "an idle host admits");
}

/// [ORB-12500] The surface a legacy wave recomputes for a claimed task cannot
/// drift from the surface the claim froze, because the claim journal refuses
/// every ordinary mutation of a claimed task — including narrowing its
/// `context_files`. That is why the automatic drain needs no separate reading
/// of the claim ledger: its status-derived holder map is already the frozen
/// footprint, and the paths that reach a claimed task directly consult the
/// ledger themselves.
#[test]
fn a_claimed_tasks_declaration_cannot_drift_from_the_footprint_its_claim_froze() {
    if !enter_isolated_child(
        "entry::a_claimed_tasks_declaration_cannot_drift_from_the_footprint_its_claim_froze",
    ) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let claimed = admissible_task(&runtime, "claimed by a live attempt", &["src/a.rs"]);
    admit(
        &runtime,
        &repo_root,
        FOLLOWER,
        &admission_request("frozen-footprint", owner_binary_version()),
        owner_binary_version(),
    );

    let refused = runtime
        .stores()
        .task_records()
        .update(
            &claimed,
            crate::application::task::TaskRecordUpdateParams {
                actor: "test".to_string(),
                context_files: Some(vec!["src/unrelated.rs".to_string()]),
                ..Default::default()
            },
        )
        .expect_err("a claimed task's declaration is not narrowed out from under its attempt");
    assert!(
        refused.to_string().contains("claim"),
        "the refusal names the claim: {refused}"
    );
    assert_eq!(
        runtime.get_task(&claimed).expect("task").context_files,
        vec!["file:src/a.rs".to_string()],
        "the declaration the claim froze is still the declaration on the task"
    );

    // So an overlapping backlog task is withheld by the ordinary status lock,
    // which is that same surface.
    let overlapping = admissible_task(&runtime, "overlaps the frozen surface", &["src/a.rs"]);
    let readiness = runtime
        .workspace_auto_readiness(std::slice::from_ref(&overlapping), None, 10, &[])
        .expect("readiness");
    let entry = readiness["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .cloned()
        .expect("one readiness entry");
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
    let blockers = entry["conflicts"]
        .as_array()
        .map(|conflicts| {
            conflicts
                .iter()
                .filter_map(|conflict| conflict["locking_task_id"].as_str())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(blockers.contains(&claimed), "{entry}");
}
