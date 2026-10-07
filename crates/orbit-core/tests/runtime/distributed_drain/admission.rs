//! Pull admission: cancel-state reads, owner negotiation, the failure breaker, the crew window, `os:` tag routing, host throttling and local/claim exclusion.

use orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA;
use orbit_types::task::HostOs;
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, BaselineRedHold};

use super::*;

/// Exercise the owner's actual tool/redaction boundary with the incident's
/// inherited session metadata, then the follower's real refill action.
#[test]
fn session_metadata_does_not_corrupt_owner_probe_or_pull_pass() {
    if !isolated(
        module_path!(),
        "session_metadata_does_not_corrupt_owner_probe_or_pull_pass",
    ) {
        return;
    }
    let pair = Pair::new(0);
    let drain = pair.run_drain();
    let _env = orbit_common::test_env::scoped([
        ("XDG_SESSION_ID", Some("35912")),
        (
            "DBUS_SESSION_BUS_ADDRESS",
            Some("unix:path=/run/user/1000/bus"),
        ),
        ("SESSION_MANAGER", Some("local/host:@/tmp/.ICE-unix/35912")),
        (
            "TERM_SESSION_ID",
            Some("35912AB0-0000-4000-8000-123456789ABC"),
        ),
        ("MY_SESSION_TOKEN", Some("35912")),
    ]);
    let reply = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    assert_eq!(
        reply["protocol_fingerprint"],
        orbit_store::contracts::distributed_drain_protocol_fingerprint()
    );
    // Also cover the exact incident substring even if the generated request
    // shape changes its fingerprint in a later build.
    let incident = "3531880b5359125003d9ff82a4666593a2fc93756c67758714a336aadcb0a658";
    assert_eq!(
        orbit_common::security::redaction::redact_sensitive_env_text(incident),
        incident
    );
    let pass = pair.pass(&drain);
    assert!(pass["error"].is_null(), "{pass}");
    assert_eq!(pass["degraded"], false, "{pass}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
}

/// Both a current owner's typed redaction refusal and an older owner's
/// corrupted fingerprint must leave the window alive for the next pass.
#[test]
fn redacted_owner_identity_retries_without_protocol_skew() {
    if !isolated(
        module_path!(),
        "redacted_owner_identity_retries_without_protocol_skew",
    ) {
        return;
    }
    let pair = Pair::new(0);
    let fingerprint = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    let drain = pair.run_drain();
    {
        // A realistic mixed hex credential can overlap a legitimate hash.
        // It must remain secret: reject the corrupted reply rather than
        // allowlisting hash-shaped output from redaction.
        let _env = orbit_common::test_env::scoped([("MY_SESSION_TOKEN", Some(&fingerprint[..16]))]);
        let error = pair
            .wire
            .call("", "orbit.drain.probe", json!({}))
            .unwrap_err();
        assert!(matches!(error, OrbitError::OwnerNegotiation(_)), "{error}");
        let pass = pair.pass(&drain);
        assert!(error_of(&pass).contains("redaction artefact"), "{pass}");
        assert_eq!(pass["degraded"], false);
        assert_eq!(pass["done"], false);
    }
    let recovered = pair.pass(&drain);
    assert!(recovered["error"].is_null(), "{recovered}");
    assert_eq!(recovered["consecutive_pass_failures"], 0);

    let pulls_before_corrupted_probe = pair.wire.calls("orbit.task.pull").len();
    *pair.wire.fingerprint.lock().unwrap() = Some(json!(
        "3531880b5[REDACTED_ENV]5003d9ff82a4666593a2fc93756c67758714a336aadcb0a658"
    ));
    let pass = pair.pass(&drain);
    assert!(
        error_of(&pass).contains("owner negotiation failed"),
        "{pass}"
    );
    assert_eq!(pass["degraded"], false);
    assert_eq!(pass["done"], false);
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        pulls_before_corrupted_probe,
        "a corrupted probe must not send a new pull"
    );
    *pair.wire.fingerprint.lock().unwrap() = None;
    let recovered = pair.pass(&drain);
    assert!(recovered["error"].is_null(), "{recovered}");
    assert_eq!(recovered["consecutive_pass_failures"], 0);
}

/// A scrubbed mutation reply must not imply that admission failed: replaying
/// its durable request returns the receipt the owner already stored.
#[test]
fn redacted_pull_receipt_reports_unknown_outcome_and_replays_same_request() {
    if !isolated(
        module_path!(),
        "redacted_pull_receipt_reports_unknown_outcome_and_replays_same_request",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let fingerprint = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    let request = json!({
        "request_id": "redacted-receipt-request", "caller_version": probe["binary_version"],
        "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, "caller_fingerprint": fingerprint,
        "caller_before_pr": false, "review_gate": true, "ship": probe["ship"],
        "run_context": {"run_id": "receipt-drain", "job_name": "workspace_pull_pipeline"},
    });
    {
        let _env = orbit_common::test_env::scoped([("MY_SESSION_TOKEN", Some(&fingerprint[..16]))]);
        let error = pair
            .wire
            .call("", "orbit.task.pull", request.clone())
            .unwrap_err();
        assert!(
            matches!(error, OrbitError::OutcomeUnknown { .. }),
            "{error}"
        );
    }
    let reply = pair.wire.call("", "orbit.task.pull", request).unwrap();
    assert_eq!(
        reply["receipt"]["request"]["request_id"],
        "redacted-receipt-request"
    );
    assert_eq!(
        reply["receipt"]["request"]["caller_fingerprint"],
        fingerprint
    );
    assert_eq!(reply["receipt"]["claim"]["task_id"], pair.tasks[0]);
    assert_eq!(
        pair.owner_claims().len(),
        1,
        "replay must not admit a second claim"
    );
}

/// Owner admission hands selector-free implementation work to a follower on
/// its first pass with an empty lock footprint; tagged no-diff stays on owner.
#[test]
fn owner_pull_admits_empty_context_without_locks_and_keeps_no_diff_work_on_owner() {
    if !isolated(
        module_path!(),
        "owner_pull_admits_empty_context_without_locks_and_keeps_no_diff_work_on_owner",
    ) {
        return;
    }
    let pair = Pair::new(2);
    for (index, id) in pair.tasks.iter().enumerate() {
        pair.wire
            .owner
            .update_task_as_human(
                id,
                orbit_core::application::task::TaskUpdateParams {
                    context_files: Some(vec![]),
                    tags: (index == 0).then(|| vec!["no-diff-expected".into()]),
                    ..Default::default()
                },
                "fixture operator".into(),
            )
            .unwrap();
    }
    let drain = pair.start_drain();
    let first = pair.pass(&drain);
    assert!(launch_refused(&first), "{first}");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], pair.tasks[1]);
    assert_eq!(claims[0]["claim"]["footprint"], json!([]), "{claims:#?}");
    let receipts = pair.follower_jobs.local_pull_admissions().unwrap();
    let receipt = receipts
        .iter()
        .filter_map(|record| record.receipt.as_ref())
        .find(|receipt| receipt.claim.is_some())
        .expect("first pass claimed selector-free work");
    assert!(receipt.invalid_candidates.is_empty(), "{receipt:#?}");
    assert!(receipt.task.as_ref().unwrap().context_files.is_empty());
    assert!(
        receipt.deferred_conflicts.iter().any(|entry| {
            entry.task_id == pair.tasks[0]
                && entry.reason.contains("no-diff-expected")
                && entry.reason.contains("verified handoff")
        }),
        "tag alone cannot establish a verified NoDiff handoff: {receipt:#?}"
    );
}

/// A replica that declares no `workflow.required_validation_commands` starts
/// its pull drain: an empty list means no required check, as it does for an
/// owner's own delivery, so submission notes it rather than refusing. This
/// fixture deploys no job asset, so a submission past every preflight — the
/// owner probe included — reaches the job catalog and stops there.
#[test]
fn a_follower_without_required_validation_commands_starts_a_pull_drain() {
    if !isolated(
        module_path!(),
        "a_follower_without_required_validation_commands_starts_a_pull_drain",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let follower = OrbitRuntime::in_memory()
        .expect("replica runtime")
        .with_automation_machine_identity(Some(FOLLOWER.into()))
        .with_coordination_write_owner(Some(OWNER.into()))
        .with_drain_owner_transport(pair.wire.clone());
    assert!(follower.workflow_required_validation_commands().is_empty());
    assert!(follower.required_validation_note().is_some());
    let logical = follower
        .workspace_runtime_binding()
        .expect("registered replica")
        .logical_workspace_id
        .clone();

    let submitted = follower
        .submit_workspace_pull_run(
            orbit_core::WorkspacePullRequest {
                selector: &format!("{OWNER}/{logical}"),
                for_seconds: Some(60),
                max_active_leaf_runs: Some(1),
                allowed_crews: &[],
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("a fixture without job assets never submits a run");

    assert!(
        matches!(
            submitted,
            OrbitError::NotFound { ref id, .. } if id == "workspace_pull_pipeline"
        ),
        "an empty requirement list must not refuse a pull drain: {submitted}"
    );
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), 2);
}

/// An unreadable persisted cancel request fails the whole pass visibly; it
/// cannot probe, request or launch work without readable control state.
#[test]
fn unreadable_cancel_state_fails_visibly_without_admission() {
    if !isolated(
        module_path!(),
        "unreadable_cancel_state_fails_visibly_without_admission",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    pair.follower
        .cancel_job_run_with_options(&drain, "operator", "cli", None, false)
        .expect("persist graceful cancellation");
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    assert!(state.drain_cancelling());
    let store = pair.follower.sqlite_store().unwrap();
    let workspace = pair.follower.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            let changed = tx.connection().execute(
                "UPDATE job_runs SET pipeline_state_json = '{' WHERE workspace_id = ?1 AND run_id = ?2",
                [workspace.as_str(), drain.as_str()],
            ).unwrap();
            assert_eq!(changed, 1);
            Ok(())
        })
        .unwrap();
    let read_error = pair
        .follower
        .read_run_state(&drain)
        .unwrap_err()
        .to_string();

    for expired in [false, true] {
        let failure = pair
            .follower
            .run_deterministic(
                "pull_refill",
                &json!({}),
                &json!({"run_id": drain, "destination": pair.destination,
                "window_expired": expired}),
                ToolContext::default(),
            )
            .expect_err("unreadable pass health fails visibly, without touching admissions");
        assert!(failure.to_string().contains(&read_error), "{failure}");
    }
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    pair.follower.write_run_state(&drain, &state).unwrap();
    let recovered = pair.pass(&drain);
    assert!(recovered["error"].is_null(), "{recovered}");
    assert_eq!(recovered["cancelling"], true, "{recovered}");
    assert_eq!(recovered["done"], true, "{recovered}");
    assert_eq!(pair.run_state(&drain), JobRunState::Cancelled);
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
}

/// Older owners reject additive fields (`crews`, `os`) despite equal binary
/// versions. Negotiation must stop the newer follower before it sends them.
#[test]
fn an_older_owner_is_refused_before_a_newer_request_is_sent() {
    if !isolated(
        module_path!(),
        "an_older_owner_is_refused_before_a_newer_request_is_sent",
    ) {
        return;
    }
    let pair = Pair::new(1);
    *pair.wire.protocol.lock().unwrap() = Some(1);
    let drain = pair.start_drain();
    let failure = pair
        .follower
        .run_deterministic(
            "pull_refill",
            &json!({}),
            &json!({"run_id": drain, "destination": pair.destination, "window_expired": false}),
            ToolContext::default(),
        )
        .unwrap_err();
    assert!(
        matches!(&failure, orbit_engine::DispatchError::ProtocolSkew(_)),
        "{failure}"
    );
    assert!(
        failure.to_string().contains(&format!(
            "caller revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}; owner revision 1"
        )),
        "{failure}"
    );
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");

    // A newer owner's boundary likewise refuses an older follower by type.
    *pair.wire.protocol.lock().unwrap() = None;
    let probe = pair
        .wire
        .call(
            "",
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": 1, "caller_review_policy": "none",
            }),
        )
        .unwrap();
    assert_eq!(probe["refusal"], "protocol_mismatch");
    assert!(probe["diagnostics"].to_string().contains(&format!(
        "caller revision 1; owner revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}"
    )));
    let request = json!({"request_id": "old-request", "caller_version": probe["binary_version"],
        "caller_schema": 1, "caller_review_policy": "none", "ship": probe["ship"],
        "run_context": {"run_id": "old-drain", "job_name": "workspace_pull_pipeline"}});
    let failure = pair.wire.call("", "orbit.task.pull", request).unwrap_err();
    assert!(
        failure.to_string().contains(&format!(
            "protocol_skew: caller revision 1; owner revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}"
        )),
        "{failure}"
    );
    assert!(pair.owner_claims().is_empty());
}

/// The incident's same-version builds must differ as soon as an admission
/// field changes, including a nested field. This checks the live probe against
/// the generated request shape instead of pinning a manually bumped hash.
#[test]
fn probe_fingerprint_tracks_request_types_and_refuses_changed_shapes() {
    if !isolated(
        module_path!(),
        "probe_fingerprint_tracks_request_types_and_refuses_changed_shapes",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let schema = orbit_store::contracts::admission_request_schema();
    let fingerprint = sha256_hex(schema.to_string().as_bytes());
    assert_eq!(
        probe["protocol_fingerprint"], fingerprint,
        "the failed-pull incident requires the live fingerprint to change with the derived request types"
    );
    for (section, name, field) in [
        ("properties", "future_field", json!({"type": "string"})),
        (
            "definitions",
            "AdmissionRunContext",
            json!({"type": "object", "properties": {"future_field": {"type": "boolean"}}}),
        ),
    ] {
        let mut changed = schema.clone();
        changed[section][name] = field;
        let changed_fingerprint = sha256_hex(changed.to_string().as_bytes());
        assert_ne!(changed_fingerprint, fingerprint);
        let error = pair
            .wire
            .call(
                "",
                "orbit.drain.probe",
                json!({
                    "caller_fingerprint": changed_fingerprint,
                    "caller_version": probe["binary_version"],
                    "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                }),
            )
            .unwrap_err();
        assert!(matches!(error, OrbitError::ProtocolSkew(_)), "{error}");
    }
    assert!(pair.owner_claims().is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// The owner can upgrade after preflight: skew is checked before unknown
/// request fields or malformed nested values can hide it as invalid input.
#[test]
fn owner_checks_fingerprint_before_deserializing_an_incompatible_pull() {
    if !isolated(
        module_path!(),
        "owner_checks_fingerprint_before_deserializing_an_incompatible_pull",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let error = pair
        .wire
        .call(
            "",
            "orbit.task.pull",
            json!({
                "caller_fingerprint": "skewed", "unknown_new_field": true,
                "ship": "cannot deserialize this request on the owner",
            }),
        )
        .unwrap_err();
    assert!(matches!(error, OrbitError::ProtocolSkew(_)), "{error}");
    let matching = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    for input in [
        json!({"caller_fingerprint": matching, "unknown_new_field": true}),
        json!({"caller_fingerprint": matching, "ship": "malformed"}),
    ] {
        let error = pair.wire.call("", "orbit.task.pull", input).unwrap_err();
        assert!(
            matches!(
                error,
                OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. }
            ),
            "{error}"
        );
    }
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// The real pull job ends failed on same-release wire skew, even with an open
/// window; neither stop nor cancellation can turn the failed pass into success.
#[test]
fn same_version_skew_ends_the_pull_job_failed_before_any_pull() {
    if !isolated(
        module_path!(),
        "same_version_skew_ends_the_pull_job_failed_before_any_pull",
    ) {
        return;
    }
    let mut pair = Pair::new(1);
    orbit_core::bootstrap::init::init_workspace_at_root(
        &pair.follower.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .unwrap();
    pair.follower = OrbitRuntime::from_roots(
        &pair.follower.global_root(),
        &pair.follower_repo.join(".orbit"),
    )
    .unwrap()
    .with_automation_machine_identity(Some(FOLLOWER.into()))
    .with_coordination_write_owner(Some(OWNER.into()))
    .with_drain_owner_transport(pair.wire.clone());
    let job = pair
        .follower
        .show_job_catalog_entry("workspace_pull_pipeline")
        .unwrap();
    // No fingerprint covers a legacy owner; a different hash covers wire skew
    // between two builds with the same version and manual revision.
    for fingerprint in [Value::Null, json!("different-request-shape")] {
        *pair.wire.fingerprint.lock().unwrap() = Some(fingerprint);
        let error = pair
            .follower
            .run_job_v2_from_yaml(
                &job.path,
                json!({"for_seconds": 3600, "destination": pair.destination}),
            )
            .unwrap_err();
        assert!(matches!(error, OrbitError::ProtocolSkew(_)), "{error}");
        let runs = pair
            .follower
            .list_job_runs(orbit_core::application::job::JobRunListParams {
                job_id: Some("workspace_pull_pipeline".into()),
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        let run = &runs[0];
        assert_eq!(run.state, JobRunState::Failed);
        assert!(
            run.steps
                .iter()
                .any(|step| step.error_code.as_deref() == Some("protocol_skew")),
            "{run:#?}"
        );
        let pass = pair
            .follower
            .read_run_state(&run.run_id)
            .unwrap()
            .unwrap()
            .drain_last_pass
            .unwrap();
        assert!(pass.degraded);
        assert_eq!(pass.last_pass_error_code.as_deref(), Some("protocol_skew"));
        assert_eq!(pass.consecutive_pass_failures, 1);
    }
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// Transport errors persist, a successful pass resets a transient streak,
/// and three failures latch a warning instead of idling invisibly forever.
#[test]
fn repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it() {
    if !isolated(
        module_path!(),
        "repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it",
    ) {
        return;
    }
    let pair = Pair::new(0);
    let drain = pair.start_drain();
    *pair.wire.unreachable.lock().unwrap() = true;
    assert_eq!(pair.pass(&drain)["consecutive_pass_failures"], 1);
    *pair.wire.unreachable.lock().unwrap() = false;
    let recovered = pair.pass(&drain);
    assert_eq!(recovered["consecutive_pass_failures"], 0);
    assert!(recovered["last_pass_error"].is_null());
    *pair.wire.unreachable.lock().unwrap() = true;
    for count in 1..=3 {
        let pass = pair.pass(&drain);
        assert_eq!(pass["consecutive_pass_failures"], count);
        assert_eq!(pass["degraded"], count == 3);
        let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
        let health = state.drain_last_pass.unwrap();
        assert_eq!(health.consecutive_pass_failures, count);
        assert!(
            health
                .last_pass_error
                .unwrap()
                .contains("Connection timed out")
        );
    }
    *pair.wire.unreachable.lock().unwrap() = false;
    let before = pair.wire.calls("orbit.drain.probe").len();
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false);
    assert_eq!(held["degraded"], true);
    assert!(
        held["refusal"]
            .as_str()
            .unwrap()
            .starts_with("pass_failures:")
    );
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), before);
    let fresh = pair.pass(&pair.start_drain());
    assert_eq!(fresh["degraded"], false);
    assert_eq!(fresh["consecutive_pass_failures"], 0);
}

/// A graceful cancel persisted while orphan reconciliation runs is observed
/// before this pass probes, requests, or launches anything.
#[test]
fn cancel_recorded_during_orphan_reconciliation_admits_nothing() {
    if !isolated(
        module_path!(),
        "cancel_recorded_during_orphan_reconciliation_admits_nothing",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    assert!(
        !pair
            .follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );
    let follower = pair.follower.clone();
    let drain_id = drain.clone();
    pair.follower.install_orphan_reconcile_hook(move || {
        follower
            .cancel_job_run_with_options(&drain_id, "operator", "cli", None, false)
            .expect("persist graceful cancellation during reconciliation");
    });

    let pass = pair.pass(&drain);
    assert_eq!(pass["cancelling"], true, "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert!(pass["error"].is_null(), "{pass}");
    assert!(pair.wire.calls("orbit.drain.probe").is_empty(), "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty(), "{pass}");
    assert!(pair.owner_claims().is_empty());
    assert!(pair.leaf_runs().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert!(
        pair.follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );
}

/// A readable run state with no cancellation still probes and admits work.
#[test]
fn readable_state_without_cancel_permits_refill() {
    if !isolated(
        module_path!(),
        "readable_state_without_cancel_permits_refill",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = pair.start_drain();
    assert!(
        !pair
            .follower
            .read_run_state(&drain)
            .unwrap()
            .unwrap()
            .drain_cancelling()
    );

    let pass = pair.pass(&drain);
    assert!(
        launch_refused(&pass),
        "the admitted leaf reaches launch: {pass}"
    );
    assert_eq!(pass["cancelling"], false, "{pass}");
    assert_eq!(pair.wire.calls("orbit.drain.probe").len(), 2);
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1);
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(pair.leaf_runs().len(), 1);
}

/// Three claims in a row that this drain settled as failures open its
/// breaker: the next pass requests nothing and reports why. A new drain
/// starts with a closed breaker and pulls again.
#[test]
fn three_failed_claims_open_the_breaker_and_a_new_drain_resets_it() {
    if !isolated(
        module_path!(),
        "three_failed_claims_open_the_breaker_and_a_new_drain_resets_it",
    ) {
        return;
    }
    let pair = Pair::new(4);
    let drain = pair.start_drain();

    for failed in 1..=3 {
        let pass = pair.pass(&drain);
        assert!(launch_refused(&pass), "{pass}");
        assert_eq!(pass["consecutive_failures"], failed, "{pass}");
    }
    let opened = pair.pass(&drain);
    assert_eq!(opened["admitting"], false, "{opened}");
    assert!(
        opened["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("circuit_open")),
        "{opened}"
    );
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 3);
    let backlog = |pair: &Pair| {
        pair.tasks
            .iter()
            .filter(|id| pair.owner_status(id) == "backlog")
            .count()
    };
    assert_eq!(backlog(&pair), 1, "the fourth task is left on the owner");

    let restarted = pair.start_drain();
    let reset = pair.pass(&restarted);
    assert!(launch_refused(&reset), "{reset}");
    assert_eq!(reset["consecutive_failures"], 1, "{reset}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 4);
    assert_eq!(backlog(&pair), 0);
    assert_eq!(pair.owner_claims().len(), 4);
}

fn excluded(pass: &Value, crew: &str) -> Value {
    pass["crews"]["excluded"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|exclusion| exclusion["crew"] == crew)
        .cloned()
        .unwrap_or(Value::Null)
}

/// A follower whose window preflight cannot find crew `antigravity`'s CLI is
/// never handed an `antigravity` task [ORB-13941]. The owner admits the next
/// task it can run instead and, once only the unrunnable one is left, answers
/// idle and names it, so the task stays in the backlog for the owner or
/// another follower rather than being claimed and failed here.
#[test]
fn a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run() {
    if !isolated(
        module_path!(),
        "a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("antigravity"), Some("sol")]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    let (unrunnable, runnable) = (&pair.tasks[0], &pair.tasks[1]);
    let drain = pair.start_drain();

    let first = pair.pass(&drain);
    assert!(launch_refused(&first), "{first}");
    let exclusion = excluded(&first, "antigravity");
    assert_eq!(exclusion["source"], "preflight", "{first}");
    assert!(
        exclusion["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit-test-no-such-provider-cli")),
        "{first}"
    );
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");

    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], runnable.as_str());
    assert_eq!(pair.owner_status(unrunnable), "backlog");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert_eq!(pulls.len(), 2, "{pulls:?}");
    for pull in &pulls {
        let runnable = pull["crews"]["runnable"]
            .as_array()
            .expect("declared crews");
        assert!(runnable.iter().any(|crew| crew == "sol"), "{pull}");
        assert!(runnable.iter().all(|crew| crew != "antigravity"), "{pull}");
    }
    let idle_receipt = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .find(|record| record.phase == LocalPullPhase::Idle)
        .and_then(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        idle_receipt
            .crew_unavailable
            .iter()
            .any(|skipped| &skipped.task_id == unrunnable && skipped.reason.contains("antigravity")),
        "{idle_receipt:#?}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(window.checked_at.is_some());
    assert!(
        window
            .excluded
            .iter()
            .any(|exclusion| exclusion.crew == "antigravity"),
        "{window:#?}"
    );
}

/// The owner hands a follower only tasks whose `os:` tags its declared OS
/// satisfies [ORB-14005]. A Linux follower takes a task tagged for both OSes
/// and an untagged one, never the `os:macos` task, which the idle receipt
/// names and which stays in the owner's backlog until a macOS follower pulls
/// it.
#[test]
fn a_follower_is_handed_only_tasks_its_os_satisfies() {
    if !isolated(
        module_path!(),
        "a_follower_is_handed_only_tasks_its_os_satisfies",
    ) {
        return;
    }
    let mut pair = Pair::new(3);
    let (mac, either, anywhere) = (
        pair.tasks[0].clone(),
        pair.tasks[1].clone(),
        pair.tasks[2].clone(),
    );
    for (task, tags) in [
        (&mac, json!(["os:macos"])),
        (&either, json!(["os:linux", "os:macos"])),
    ] {
        pair.wire
            .owner
            .run_tool(
                "orbit.task.update",
                json!({"id": task, "tags": tags, "model": "codex"}),
            )
            .expect("tag owner task");
    }
    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Linux));
    let drain = pair.start_drain();

    for _ in 0..3 {
        pair.pass(&drain);
    }
    let claimed = |pair: &Pair| {
        pair.owner_claims()
            .iter()
            .map(|claim| claim["claim"]["task_id"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        claimed(&pair),
        BTreeSet::from([either.clone(), anywhere.clone()])
    );
    assert_eq!(pair.owner_status(&mac), "backlog");
    let pulls = pair.wire.calls("orbit.task.pull");
    assert!(pulls.iter().all(|pull| pull["os"] == "linux"), "{pulls:?}");
    let idle = pair
        .follower_jobs
        .local_pull_admissions()
        .unwrap()
        .into_iter()
        .rev()
        .filter(|record| record.phase == LocalPullPhase::Idle)
        .find_map(|record| record.receipt)
        .expect("the owner answered idle");
    assert!(
        idle.os_unavailable
            .iter()
            .any(|skipped| skipped.task_id == mac
                && skipped
                    .reason
                    .contains("waits for a macos host (os:macos); the executor runs linux")),
        "{idle:#?}"
    );

    pair.follower = pair.follower.clone().with_host_os(Some(HostOs::Macos));
    let mac_drain = pair.start_drain();
    pair.pass(&mac_drain);
    assert_eq!(claimed(&pair), BTreeSet::from([mac, either, anywhere]));
    assert_eq!(
        pair.wire.calls("orbit.task.pull").last().unwrap()["os"],
        "macos"
    );
}

/// A claimed leaf whose provider refused to authenticate gives its claim
/// back [ORB-13941]: the owner's task returns to the backlog rather than
/// `blocked`, the failure breaker does not count it, and the drain offers
/// that crew no more for the rest of its window, so the same task is not
/// pulled straight back to fail the same way.
#[test]
fn a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
    if !isolated(
        module_path!(),
        "a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window",
    ) {
        return;
    }
    a_provider_failure_releases_the_claim(
        "[provider_unavailable] claude provider authentication failure (HTTP 401): \
         Failed to authenticate: OAuth token revoked. Please log in again or contact your administrator.",
        "OAuth token revoked",
    );
}

/// [ORB-14149] Likewise a claimed leaf whose provider said its selected model
/// was at capacity: recovery could not change that and an immediate rerun
/// would use the same model, so the claim goes back to the owner's backlog.
#[test]
fn a_provider_capacity_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
    if !isolated(
        module_path!(),
        "a_provider_capacity_failure_releases_the_claim_and_excludes_the_crew_for_the_window",
    ) {
        return;
    }
    a_provider_failure_releases_the_claim(
        "[provider_capacity] cli subprocess exited with code 1: codex provider reported the \
         selected model at capacity: Selected model is at capacity. Please try a different model.",
        "Selected model is at capacity",
    );
}

/// The settlement of a claimed `sol` leaf that ended on `diagnostic`, a typed
/// provider failure whose provider text includes `reason`.
fn a_provider_failure_releases_the_claim(diagnostic: &str, reason: &str) {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, diagnostic);

    let pass = pair.pass(&drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(pass["consecutive_failures"], 0, "{pass}");
    assert_eq!(pass["admitted"], 0, "{pass}");
    let exclusion = excluded(&pass, "sol");
    assert_eq!(exclusion["source"], "provider_unavailable", "{pass}");
    assert!(
        exclusion["reason"]
            .as_str()
            .is_some_and(|text| text.contains(task.as_str())
                && text.contains(reason)
                && !text.contains("[provider_")),
        "the reason quotes the provider without Orbit's marker: {pass}"
    );

    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let release = &settles[0]["settlement"]["Release"];
    assert_eq!(
        release["provider_unavailable"]["crew"], "sol",
        "{settles:?}"
    );
    let claims = pair.owner_claims();
    assert_eq!(
        claims.len(),
        1,
        "the released task is not pulled back: {claims:#?}"
    );
    assert_eq!(claims[0]["claim"]["phase"], "revoked");
    assert!(
        comments_of(&pair.owner_task(&task)).contains("could not use the provider of crew `sol`"),
        "{}",
        pair.owner_task(&task)
    );
    let pulls = pair.wire.calls("orbit.task.pull");
    let last = pulls.last().expect("the pass asked again");
    assert!(
        last["crews"]["excluded"]
            .as_array()
            .is_some_and(|excluded| excluded.iter().any(|exclusion| exclusion["crew"] == "sol")),
        "{last}"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(
        window
            .runnable
            .as_ref()
            .is_some_and(|runnable| !runnable.iter().any(|crew| crew == "sol")),
        "{window:#?}"
    );
}

/// [ORB-14262] One provider's authentication failure closes every crew that
/// resolves to it. `sonnet` is configured as the `anthropic` alias of
/// `claude`, so a 401 on `opus` must keep `sonnet` out of the window while
/// `sol`, on codex, is still admitted.
#[test]
fn a_provider_auth_failure_excludes_every_crew_of_that_provider() {
    if !isolated(
        module_path!(),
        "a_provider_auth_failure_excludes_every_crew_of_that_provider",
    ) {
        return;
    }
    let config = "\
[workflow]
default_crew = \"sol\"

[crews.opus]
provider = \"claude\"
model = \"claude-opus\"

[crews.sonnet]
provider = \"anthropic\"
model = \"claude-sonnet\"

[crews.sol]
provider = \"codex\"
model = \"gpt-sol\"
";
    // Oldest first, which is admission order: opus fails, then sonnet would
    // be next if the alias were still runnable, and sol is the other provider.
    let pair = Pair::with_configs(config, config, &[Some("opus"), Some("sonnet"), Some("sol")]);
    let (opus, sonnet, sol) = (
        pair.tasks[0].clone(),
        pair.tasks[1].clone(),
        pair.tasks[2].clone(),
    );
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), opus);

    pair.leaf_fails_with(
        &leaf,
        "[provider_unavailable] claude provider authentication failure (HTTP 401): \
         Failed to authenticate: OAuth token revoked. Please log in again or contact your administrator.",
    );
    let pass = pair.pass(&drain);

    for crew in ["opus", "sonnet"] {
        let exclusion = excluded(&pass, crew);
        assert_eq!(
            exclusion["source"], "provider_unavailable",
            "{crew}: {pass}"
        );
        assert!(
            exclusion["reason"].as_str().is_some_and(
                |text| text.contains(opus.as_str()) && text.contains("OAuth token revoked")
            ),
            "{crew}: {pass}"
        );
    }
    assert!(excluded(&pass, "sol").is_null(), "{pass}");

    let claims = pair.owner_claims();
    let phase = |task: &str| {
        claims.iter().find_map(|claim| {
            (claim["claim"]["task_id"] == task).then(|| claim["claim"]["phase"].as_str().unwrap())
        })
    };
    assert_eq!(phase(&opus), Some("revoked"), "{claims:#?}");
    assert_eq!(
        phase(&sonnet),
        None,
        "the alias crew is not admitted: {claims:#?}"
    );
    // This test binary cannot re-exec a worker, so the admitted sol leaf's
    // launch fails and the owner blocks that task. The claim phase is the
    // launch failure, which is how an admitted crew shows up here.
    assert!(
        launch_refused(&pass),
        "sol was admitted far enough to launch: {pass}"
    );
    assert_eq!(
        phase(&sol),
        Some("failed"),
        "the other provider is admitted, then its launch fails: {claims:#?}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles.iter().any(|call| {
            call["settlement"]["Fail"]["summary"]
                .as_str()
                .is_some_and(|summary| summary.starts_with("leaf launch failed"))
        }),
        "sol's admission reached launch: {settles:?}"
    );
    assert_eq!(pair.owner_status(&sonnet), "backlog");
    assert_eq!(pair.owner_status(&opus), "backlog");
    assert_eq!(
        pair.owner_status(&sol),
        "blocked",
        "the launch refusal blocks the admitted task"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    let runnable = window.runnable.as_ref().expect("preflight ran");
    assert!(runnable.iter().any(|crew| crew == "sol"), "{window:#?}");
    assert!(
        runnable
            .iter()
            .all(|crew| crew != "opus" && crew != "sonnet"),
        "{window:#?}"
    );
}

/// [ORB-14258] A claimed leaf whose required validation failed on its base
/// exactly as on the candidate releases its claim with the hold. The owner's
/// task returns to the backlog under that hold, and the failure breaker does
/// not count it. The owner's admission withholds the task while the base still
/// points at the red commit and after it moves to another failing tip, then
/// offers it once the required command passes on the new base.
#[test]
fn a_red_base_failure_releases_the_claim_until_the_command_passes() {
    if !isolated(
        module_path!(),
        "a_red_base_failure_releases_the_claim_until_the_command_passes",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let repo = &pair.owner_repo;
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    std::fs::write(
        repo.join("Makefile"),
        "ci-lint:\n\t@echo lint is red >&2; exit 2\n",
    )
    .unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "red base"]);
    let red = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    let hold = BaselineRedHold {
        base_ref: "main".into(),
        base_sha: red.clone(),
        command: "make ci-lint".into(),
        run_id: String::new(),
    };
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(
        &leaf,
        &hold.text(&format!(
            "required validation 'make ci-lint' fails on base {red} exactly as on the candidate"
        )),
    );

    let pass = pair.pass(&drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(pass["consecutive_failures"], 0, "{pass}");
    assert_eq!(
        pass["admitted"], 0,
        "the held task is not pulled back: {pass}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert_eq!(settles.len(), 1, "{settles:?}");
    let released = &settles[0]["settlement"]["Release"]["baseline_red"];
    assert_eq!(released["base_sha"], red.as_str(), "{settles:?}");
    assert_eq!(released["command"], "make ci-lint", "{settles:?}");
    assert_eq!(
        settles[0]["settlement"]["Release"]["failure"]["class"], "baseline_red",
        "[ORB-14257] the release is typed: {settles:?}"
    );
    let owner_task = pair.owner_task(&task);
    let latest = owner_task["history"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|entry| !entry["to_status"].is_null())
        .cloned()
        .unwrap();
    assert_eq!(latest["event"], BASELINE_RED_HOLD_EVENT, "{owner_task:#}");
    assert_eq!(
        latest["note"].as_str().and_then(BaselineRedHold::from_text),
        Some(BaselineRedHold {
            run_id: leaf.clone(),
            ..hold.clone()
        }),
        "{owner_task:#}"
    );
    assert!(
        comments_of(&owner_task).contains("no pull request was opened"),
        "{owner_task:#}"
    );

    let leaves = pair.leaf_runs();
    let still_red = pair.pass(&drain);
    assert_eq!(still_red["admitted"], 0, "{still_red}");
    assert_eq!(
        pair.leaf_runs(),
        leaves,
        "no leaf is created for the held task"
    );
    assert_eq!(pair.owner_status(&task), "backlog");

    std::fs::write(
        repo.join("Makefile"),
        "ci-lint:\n\t@echo lint is still red >&2; exit 2\n",
    )
    .unwrap();
    git(repo, &["add", "Makefile"]);
    git(repo, &["commit", "-q", "-m", "still red"]);
    let moved_red = pair.pass(&drain);
    assert_eq!(
        moved_red["admitted"], 0,
        "the new tip is still red: {moved_red}"
    );
    assert_eq!(pair.leaf_runs(), leaves, "a still-red base is not pulled");

    std::fs::write(repo.join("Makefile"), "ci-lint:\n\t@echo lint-ok\n").unwrap();
    git(repo, &["add", "Makefile"]);
    git(repo, &["commit", "-q", "-m", "fix lint"]);
    let next = pair.queued_leaf(&drain, 1);
    assert_eq!(
        pair.claimed_task(&next),
        task,
        "the required command passes on the new base, so the task is offered again"
    );
}

/// [ORB-13901] While sustained host pressure throttles the follower, its
/// drain requests no claim but still delivers a settlement it owes; once
/// memory is back below its resume mark the next pass pulls again.
#[test]
fn a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume() {
    if !isolated(
        module_path!(),
        "a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume",
    ) {
        return;
    }
    let mut pair = Pair::new(2);
    let probe = crate::dispatch_admission::PressureProbe::calm();
    pair.follower = pair
        .follower
        .clone()
        .with_host_resource_probe(probe.clone());
    let drain = pair.start_drain();

    // The first claim fails at launch and its settlement reply is lost, so
    // the drain owes the owner that settlement.
    pair.wire.lose_next_reply("orbit.drain.claim.settle");
    let owed = pair.pass(&drain);
    assert!(error_of(&owed).contains("dropped"), "{owed}");
    assert_eq!(pair.follower.pending_pull_settlements().unwrap().count, 1);

    probe.sustain_memory(&pair.follower, 96.0);
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false, "{held}");
    assert_eq!(held["admitted"], 0, "{held}");
    assert_eq!(
        held["resource_throttle"]["resources"][0]["resource"], "memory",
        "{held}"
    );
    assert_eq!(held["sleep_seconds"], 30, "a throttled drain polls: {held}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "no new claim");
    assert_eq!(
        pair.follower.pending_pull_settlements().unwrap().count,
        0,
        "the owed settlement is delivered while throttled"
    );
    assert_eq!(pair.wire.calls("orbit.drain.claim.settle").len(), 2);
    let unclaimed = pair
        .tasks
        .iter()
        .filter(|id| pair.owner_status(id) == "backlog")
        .count();
    assert_eq!(unclaimed, 1, "the second task stays on the owner");
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .drain_last_pass
        .and_then(|pass| pass.resource_throttle)
        .expect("the pull pass records the throttle");
    assert_eq!(recorded.resources[0].percent, 96.0);

    probe.memory(70.0, Utc::now());
    let resumed = pair.pass(&drain);
    assert!(launch_refused(&resumed), "{resumed}");
    assert_eq!(resumed["resource_throttle"], Value::Null, "{resumed}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 2);
}

/// A local drain's admission of `task` on the owner, as its gate leaves it
/// while waiting for context locks: the wrapper and gate carry the task, and
/// the task is still `backlog` with nothing reserved.
fn local_drain_admission(owner: &OrbitRuntime, task: &str) -> Vec<String> {
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    ["task_auto_pipeline", "task_gate_pipeline"]
        .into_iter()
        .map(|job| {
            jobs.insert_job_run(job, 1, Utc::now(), Some(json!({"task_ids": [task]})), None)
                .expect("local run")
                .run_id
        })
        .collect()
}

/// [ORB-13918] A task the owner's local drain admitted is not pulled while
/// that admission is live, even though its gate has not yet moved it out of
/// `backlog` or reserved its footprint; once the local runs end, it is.
#[test]
fn a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends() {
    if !isolated(
        module_path!(),
        "a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let local = local_drain_admission(&pair.wire.owner, &task);
    for run in &local {
        set_run_state(&pair.wire.owner, run, "retrying");
    }
    let drain = pair.start_drain();

    let held = pair.pass(&drain);
    assert!(error_of(&held).is_empty(), "{held}");
    assert!(pair.owner_claims().is_empty(), "{:#?}", pair.owner_claims());
    assert_eq!(pair.owner_status(&task), "backlog");
    assert!(pair.leaf_runs().is_empty());

    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    for run in &local {
        set_run_state(&pair.wire.owner, run, "running");
        owner_jobs
            .finalize_job_run(
                run,
                orbit_types::workflow::JobRunState::Cancelled,
                Utc::now(),
                None,
            )
            .unwrap();
    }
    let released = pair.pass(&drain);
    assert!(launch_refused(&released), "{released}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["task_id"], task.as_str());
}

/// [ORB-13918] While a follower's claim on a task is live, the owner's local
/// drain neither selects it nor lets a gate that queued it before the claim
/// landed dispatch it: the gate's pre-dispatch admission stops as a no-op and
/// no delivery run starts.
#[test]
fn a_task_under_a_live_claim_is_never_admitted_by_the_local_drain() {
    if !isolated(
        module_path!(),
        "a_task_under_a_live_claim_is_never_admitted_by_the_local_drain",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.start_drain();
    // The owner commits the claim; the lost reply leaves it unbound and live.
    pair.wire.lose_next_reply("orbit.task.pull");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(claims[0]["claim"]["phase"], "claimed");
    let owner = &pair.wire.owner;

    let wave = owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");

    let gate = owner
        .run_deterministic(
            "invoke_and_wait",
            &json!({}),
            &json!({
                "job_name": "task_pr_pipeline",
                "run_input": {"task_ids": [task]},
                "admission_task_ids": [task],
                "admission_workflow": "worktree_setup",
                "timeout_seconds": 5,
            }),
            ToolContext::default(),
        )
        .expect("gate dispatch");
    assert_eq!(gate["skipped"], true, "{gate}");
    assert_eq!(gate["status"], "success", "{gate}");
    assert!(gate.get("error").is_none(), "{gate}");
    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    assert!(
        owner_jobs
            .list_job_runs("task_pr_pipeline")
            .unwrap()
            .is_empty(),
        "no local delivery run starts beside the claim"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "claimed");

    // Replay is a fresh admission too, including older singular task input.
    for input in [json!({"task_ids": [task]}), json!({"task_id": task})] {
        let source = owner_jobs
            .insert_job_run("replay_fixture", 1, Utc::now(), Some(input), None)
            .unwrap();
        let before = owner.list_job_runs(Default::default()).unwrap();
        let foreground = owner.replay_job_run(&source.run_id).unwrap_err();
        let detached = owner
            .submit_replay_run(
                &source.run_id,
                None,
                None,
                orbit_types::workflow::JobRunTrigger::dashboard(),
            )
            .unwrap_err();
        for error in [foreground, detached] {
            assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error:?}");
        }
        assert_eq!(
            owner.list_job_runs(Default::default()).unwrap(),
            before,
            "a replay cannot admit beside the live claim"
        );
    }
}

/// Ask `owner`'s probe, as the routed follower does, whether a pull with
/// `caller_before_pr` would be admitted.
fn probe_owner(owner: &OrbitRuntime, caller_before_pr: bool) -> Value {
    owner
        .run_tool_with_context_and_role(
            "orbit.drain.probe",
            json!({
                "caller_version": orbit_core::application::distributed::owner_binary_version(),
                "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                "caller_before_pr": caller_before_pr,
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    caller_machine_id: Some(FOLLOWER.to_string()),
                    process_machine_id: Some(OWNER.to_string()),
                    transport: Some(McpTransport::SshMcp),
                    effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("probe")
}

/// An owner opened over `config` and, when `after_landing` is set, an
/// enabled `delivery-code-review` auto-task.
fn owner_with(root: &Path, config: &str, after_landing: bool) -> OrbitRuntime {
    let orbit = root.join(OWNER).join("repo/.orbit");
    std::fs::create_dir_all(orbit.join("auto_tasks")).unwrap();
    std::fs::write(orbit.join("config.toml"), config).unwrap();
    if after_landing {
        let seed = include_str!("../../../assets/auto_tasks/delivery-code-review.yaml")
            .replace("__ORBIT_BASE_BRANCH__", "main")
            .replace("enabled: false", "enabled: true")
            .replace("updated_by: system", "updated_by: human:operator");
        std::fs::write(orbit.join("auto_tasks/delivery-code-review.yaml"), seed).unwrap();
    }
    open_runtime(root, OWNER).0
}

/// [ORB-13992] [ORB-13908] Review never refuses a PR-route pull from an
/// executor of this binary. After-landing review — the `delivery-code-review`
/// auto-task, or the deprecated policy value that stands in for it — runs on
/// the owner after landing; the owner's `review.before_pr` is captured on the
/// claim for the leaf's gate; and the executor's own switch decides nothing.
#[test]
fn review_settings_never_refuse_a_pull_and_the_owner_captures_before_pr() {
    if !isolated(
        module_path!(),
        "review_settings_never_refuse_a_pull_and_the_owner_captures_before_pr",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();

    let after_landing = owner_with(&root.path().join("auto-task"), "", true);
    let probe = probe_owner(&after_landing, false);
    assert_eq!(probe["review"]["after_landing"]["enabled"], true);
    assert_eq!(probe["review"]["before_pr"]["enabled"], false);
    assert_eq!(probe["admits"], true, "{probe}");
    assert_eq!(probe["ship"]["before_pr"], false);
    assert!(probe["ship"].get("review").is_none(), "{probe}");

    let legacy_after_landing = owner_with(
        &root.path().join("legacy-after-landing"),
        "[operation]\nreview_policy = \"after-landing\"\n",
        false,
    );
    assert_eq!(probe_owner(&legacy_after_landing, false)["admits"], true);

    let follower_before_pr = probe_owner(&after_landing, true);
    assert_eq!(follower_before_pr["admits"], true, "{follower_before_pr}");
    assert_eq!(follower_before_pr["ship"]["before_pr"], false);

    for config in [
        "[review]\nbefore_pr = true\n[operation]\nreview_crew = \"reviewer\"\n",
        "[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewer\"\n",
    ] {
        let owner = owner_with(&root.path().join(config.len().to_string()), config, false);
        let probe = probe_owner(&owner, false);
        assert_eq!(probe["review"]["before_pr"]["enabled"], true, "{config}");
        assert_eq!(probe["ship"]["before_pr"], true, "{config}");
        assert_eq!(probe["ship"]["review"]["crew"], "reviewer", "{config}");
        assert_eq!(probe["admits"], true, "{config}: {probe}");
    }
}
