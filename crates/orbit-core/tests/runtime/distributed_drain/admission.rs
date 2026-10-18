//! Pull admission: cancel-state reads, owner negotiation, the failure breaker, the crew window, `os:` tag routing, host throttling and local/claim exclusion.

use orbit_core::application::distributed::{DrainEntryPoint, RESOURCE_THROTTLED};
use orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA;
use orbit_types::task::HostOs;
use orbit_types::workflow::{
    BASELINE_RED_HOLD_EVENT, BaselineRedHold, CONTRACT_REFUSAL_MARKER, PullBreaker,
};

use super::*;

/// A configured follower name survives the refill request, owner admission,
/// claimed leaf insertion, task binding and pulled-by history. Legacy callers
/// without a name keep their machine id and unknown display name.
fn pull_admission_preserves_execution_machine_name() {
    for name in [Some("Daniels-Mac-mini.local"), None] {
        let mut pair = Pair::new(1);
        let location = orbit_types::task::ExecutionLocation {
            machine_id: FOLLOWER.into(),
            machine_name: name.map(ToOwned::to_owned),
        };
        pair.follower = pair
            .follower
            .with_automation_execution_location(Some(location.clone()));
        let drain = pair.run_drain();
        // The existing lost-bind-reply fixture stops after real admission and
        // insertion, before this non-worker test binary would launch a leaf.
        let leaf_id = pair.queued_leaf(&drain, 1);
        let leaf = pair.follower_jobs.get_job_run(&leaf_id).unwrap().unwrap();
        assert_eq!(leaf.executed_on, Some(location.clone()));
        let task = pair.owner_task(&pair.tasks[0]);
        assert_eq!(task["job_run_machine"], json!(location));
        let history = pair.wire.owner.get_task_history(&pair.tasks[0]).unwrap();
        let pulled = history
            .iter()
            .find(|entry| entry.event == "pulled_by")
            .unwrap();
        let evidence: Value = serde_json::from_str(pulled.note.as_deref().unwrap()).unwrap();
        assert_eq!(evidence["machine_id"], FOLLOWER);
        assert_eq!(evidence["run_context"]["machine_name"], json!(name));
        assert_eq!(evidence["run_context"]["run_id"], drain);
    }
}

/// Exercise the owner's actual tool/redaction boundary with the incident's
/// inherited session metadata, then the follower's real refill action.
fn session_metadata_does_not_corrupt_owner_probe_or_pull_pass() {
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
fn redacted_owner_identity_retries_without_protocol_skew() {
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
fn redacted_pull_receipt_reports_unknown_outcome_and_replays_same_request() {
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

/// An unanswered request from before both hosts upgraded must not fail the
/// current drain after its probe matches; receipt lookup releases the old slot.
fn obsolete_persisted_request_skew_does_not_fail_a_matching_drain() {
    let current = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    for (old_fingerprint, schema) in [
        (
            Some("older-request-shape"),
            DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        ),
        (None, 1),
        (Some(current), 1),
    ] {
        let pair = Pair::new(0);
        let previous = pair.run_drain();
        let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
        assert_eq!(probe["protocol_fingerprint"], current);
        let destination = serde_json::from_value(pair.destination.clone()).unwrap();
        let request = serde_json::from_value(json!({
            "request_id": "unanswered-before-upgrade", "caller_version": probe["binary_version"],
            "caller_schema": schema,
            "caller_fingerprint": old_fingerprint,
            "caller_before_pr": false, "review_gate": true, "ship": probe["ship"],
            "run_context": {"run_id": previous, "job_name": "workspace_pull_pipeline"},
        }))
        .unwrap();
        let persisted = pair
            .follower_jobs
            .allocate_pull_request(&destination, &request, 1)
            .unwrap()
            .expect("persist the old unanswered request before sending it");
        assert_eq!(persisted.phase, LocalPullPhase::Requested);
        pair.follower_jobs
            .finalize_job_run(&previous, JobRunState::Failed, Utc::now(), None)
            .unwrap();

        let drain = pair.run_drain();
        if old_fingerprint == Some(current) {
            let failure = pair
                .follower
                .run_deterministic(
                    "pull_refill",
                    &json!({}),
                    &json!({"run_id": drain, "destination": pair.destination, "window_expired": false}),
                    ToolContext::default(),
                )
                .expect_err("skew on a current-build fingerprint remains fatal");
            assert!(
                matches!(failure, orbit_engine::DispatchError::ProtocolSkew(_)),
                "{failure}"
            );
            assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1);
            continue;
        }
        let pass = pair.pass(&drain);
        assert!(pass["error"].is_null(), "{old_fingerprint:?}: {pass}");
        assert_eq!(pass["degraded"], false, "{pass}");
        assert_eq!(pass["done"], false, "{pass}");
        assert_eq!(pair.run_state(&drain), JobRunState::Running);
        let records = pair.follower_jobs.local_pull_admissions().unwrap();
        let closed = records
            .iter()
            .find(|record| record.request.request_id == request.request_id)
            .unwrap();
        assert_eq!(closed.phase, LocalPullPhase::Refused);
        assert_eq!(
            closed.request, request,
            "the persisted request remains immutable"
        );
        assert!(closed.refusal.is_some());
        assert!(closed.receipt.is_none());
        assert!(!closed.holds_capacity());
        assert_eq!(pair.wire.calls("orbit.drain.receipt.lookup").len(), 1);
        let pulls = pair.wire.calls("orbit.task.pull");
        assert_eq!(pulls.len(), 2, "the same pass can send a fresh request");
        assert_eq!(pulls[0]["request_id"], request.request_id);
        assert_eq!(pulls[1]["caller_fingerprint"], current);
        assert_ne!(pulls[1]["request_id"], request.request_id);
        assert!(records.iter().any(|record| {
            record.request.run_context.run_id == drain && record.phase == LocalPullPhase::Idle
        }));
        assert!(pair.owner_claims().is_empty());
        assert!(pair.leaf_runs().is_empty());
    }
}

/// Owner admission hands selector-free implementation work to a follower on
/// its first pass with an empty lock footprint; tagged no-diff work is
/// claimable by the follower too [ORB-14474].
fn owner_pull_admits_empty_context_without_locks_and_claims_no_diff_work() {
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
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_eq!(
        claims[0]["claim"]["task_id"], pair.tasks[0],
        "the tagged task is the oldest candidate and the follower claims it: {claims:#?}"
    );
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
        receipt
            .deferred_conflicts
            .iter()
            .all(|entry| !entry.reason.contains("no-diff-expected")),
        "an executor with the NoDiff handoff is not refused tagged work: {receipt:#?}"
    );
}

/// A replica that declares no `workflow.required_validation_commands` starts
/// its pull drain: an empty list means no required check, as it does for an
/// owner's own delivery, so submission notes it rather than refusing. This
/// fixture deploys no job asset, so a submission past every preflight — the
/// owner probe included — reaches the job catalog and stops there.
fn a_follower_without_required_validation_commands_starts_a_pull_drain() {
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
fn unreadable_cancel_state_fails_visibly_without_admission() {
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
                "UPDATE job_run_states SET pipeline_state_json = '{' WHERE workspace_id = ?1 AND run_id = ?2",
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
fn an_older_owner_is_refused_before_a_newer_request_is_sent() {
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
}

/// A pre-fingerprint follower pulls with an older `caller_schema` and no
/// fingerprint. Its build closes a request only on `invalid_input`, so the
/// owner must keep that code for it; a fingerprinted older build is skew.
fn fingerprintless_older_revision_pull_keeps_the_invalid_input_refusal() {
    let pair = Pair::new(1);
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let pull = |request_id: &str, caller_fingerprint: Option<&str>| {
        let mut request = json!({"request_id": request_id, "caller_version": probe["binary_version"],
            "caller_schema": 1, "caller_review_policy": "none", "ship": probe["ship"],
            "run_context": {"run_id": "old-drain", "job_name": "workspace_pull_pipeline"}});
        if let Some(fingerprint) = caller_fingerprint {
            request["caller_fingerprint"] = json!(fingerprint);
        }
        pair.wire.call("", "orbit.task.pull", request).unwrap_err()
    };

    let legacy = pull("old-request", None);
    assert!(
        matches!(
            &legacy,
            OrbitError::InvalidInput(message)
                if message == &format!(
                    "protocol_mismatch: caller revision 1; owner revision {DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA}"
                )
        ),
        "a fingerprint-less older revision must keep the invalid_input refusal: {legacy}"
    );

    let current = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    let fingerprinted = pull("old-fingerprinted-request", Some(current));
    assert!(
        matches!(fingerprinted, OrbitError::ProtocolSkew(_)),
        "a fingerprinted older revision is a different build and stays skew: {fingerprinted}"
    );
    assert!(pair.owner_claims().is_empty());
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
}

/// The incident's same-version builds must differ as soon as an admission
/// field changes, including a nested field. This checks the live probe against
/// the generated request shape instead of pinning a manually bumped hash.
fn probe_fingerprint_tracks_request_types_and_refuses_changed_shapes() {
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
fn owner_checks_fingerprint_before_deserializing_an_incompatible_pull() {
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
fn same_version_skew_ends_the_pull_job_failed_before_any_pull() {
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
    pair.follower = calm_host(
        OrbitRuntime::from_roots(
            &pair.follower.global_root(),
            &pair.follower_repo.join(".orbit"),
        )
        .unwrap()
        .with_automation_machine_identity(Some(FOLLOWER.into()))
        .with_coordination_write_owner(Some(OWNER.into()))
        .with_drain_owner_transport(pair.wire.clone()),
    );
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
            .pull_drain
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
fn repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it() {
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
        let health = state.pull_drain.drain_last_pass.unwrap();
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
fn cancel_recorded_during_orphan_reconciliation_admits_nothing() {
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
fn readable_state_without_cancel_permits_refill() {
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
fn three_failed_claims_open_the_breaker_and_a_new_drain_resets_it() {
    let pair = Pair::new(4);
    let drain = pair.start_drain();

    for failed in 1..=3 {
        let leaf = pair.running_leaf(&drain, 1);
        pair.leaf_fails_with(&leaf, "candidate validation failed");
        let pass = pair.pass_with(&drain, 0);
        assert!(error_of(&pass).is_empty(), "{pass}");
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
    // [ORB-15637] The open breaker is recorded on the pass, so run show and
    // readiness name it instead of reading like an idle owner.
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the pass is recorded");
    assert!(
        recorded
            .admission_refusal
            .as_deref()
            .is_some_and(|refusal| refusal.starts_with("circuit_open")),
        "{recorded:?}"
    );
    assert_eq!(recorded.resource_throttle, None, "{recorded:?}");
    pair.follower_jobs
        .mark_job_run_running(&drain, Utc::now(), std::process::id())
        .expect("running drain");
    let readiness = pair
        .follower
        .workspace_auto_readiness(&[], None, 50, &[])
        .expect("follower readiness");
    assert_eq!(readiness["capacity"]["pull_drain_run_id"], drain.as_str());
    assert!(
        readiness["capacity"]["pull_drain_refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("circuit_open")),
        "{}",
        readiness["capacity"]
    );
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
    assert_eq!(
        reset["consecutive_failures"], 0,
        "launch failures release instead of counting toward the breaker: {reset}"
    );
    let cleared = pair
        .follower
        .read_run_state(&restarted)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the pass is recorded");
    assert_eq!(cleared.admission_refusal, None, "{cleared:?}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 4);
    assert_eq!(backlog(&pair), 1);
    assert_eq!(pair.owner_claims().len(), 4);
}

/// The drain's recorded failure breaker, which a pass leaves open or
/// half-open.
fn breaker(pair: &Pair, drain: &str) -> PullBreaker {
    pair.follower
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .pull_breaker
        .expect("the breaker is recorded")
}

/// Let the breaker's cooldown pass, as the clock would.
fn cooldown_passes(pair: &Pair, drain: &str) {
    let mut state = pair.follower.read_run_state(drain).unwrap().unwrap();
    let breaker = state
        .pull_drain
        .pull_breaker
        .as_mut()
        .expect("an open breaker");
    breaker.half_open_at = Utc::now() - chrono::Duration::seconds(1);
    pair.follower.write_run_state(drain, &state).unwrap();
}

/// [ORB-15639] An open breaker does not latch for the drain's life: once its
/// cooldown has passed it half-opens and admits a single probe claim, and no
/// other while that probe runs, however many slots are free. A probe that
/// fails opens it again with twice the wait; one that hands off closes it,
/// and the drain admits again.
fn an_open_breaker_half_opens_for_one_probe_and_reopens_or_closes_on_its_outcome() {
    let pair = Pair::new(6);
    let drain = pair.start_drain();
    let pulls = |pair: &Pair| pair.wire.calls("orbit.task.pull").len();
    for _ in 0..3 {
        let leaf = pair.running_leaf(&drain, 1);
        pair.leaf_fails_with(&leaf, "candidate validation failed");
        pair.pass_with(&drain, 0);
    }
    let opened = pair.pass_with(&drain, 4);
    assert_eq!(opened["breaker_state"], "open", "{opened}");
    assert_eq!(opened["admitting"], false, "{opened}");
    assert_eq!(pulls(&pair), 3);
    let first = breaker(&pair, &drain);
    assert_eq!(first.failures, 3);
    assert_eq!(first.failure_classes, vec!["candidate"; 3]);
    assert_eq!(
        first.half_open_at - first.opened_at,
        chrono::Duration::minutes(15),
        "the default cooldown"
    );
    assert!(
        opened["refusal"].as_str().is_some_and(
            |refusal| refusal.starts_with("circuit_open") && refusal.contains("half-opens at")
        ),
        "{opened}"
    );

    cooldown_passes(&pair, &drain);
    let probe = pair.running_leaf(&drain, 4);
    assert_eq!(pulls(&pair), 4, "the half-open breaker admits a probe");
    let waiting = pair.pass_with(&drain, 4);
    assert_eq!(waiting["breaker_state"], "half_open", "{waiting}");
    assert_eq!(waiting["admitted"], 0, "{waiting}");
    assert!(
        waiting["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("circuit_half_open")),
        "{waiting}"
    );
    assert_eq!(pulls(&pair), 4, "no second claim while the probe runs");

    pair.leaf_fails_with(&probe, "candidate validation failed");
    let reopened = pair.pass_with(&drain, 4);
    assert_eq!(reopened["breaker_state"], "open", "{reopened}");
    assert_eq!(reopened["consecutive_failures"], 4, "{reopened}");
    assert_eq!(pulls(&pair), 4);
    let second = breaker(&pair, &drain);
    assert_eq!(second.failures, 4);
    assert_eq!(
        second.half_open_at - second.opened_at,
        chrono::Duration::minutes(30),
        "a failed probe doubles the wait"
    );

    cooldown_passes(&pair, &drain);
    let probe = pair.running_leaf(&drain, 4);
    assert_eq!(pulls(&pair), 5);
    pair.leaf_hands_off(&probe);
    let closed = pair.pass_with(&drain, 0);
    assert_eq!(closed["breaker_state"], "closed", "{closed}");
    assert_eq!(closed["consecutive_failures"], 0, "{closed}");
    assert!(closed["refusal"].is_null(), "{closed}");
    let state = pair.follower.read_run_state(&drain).unwrap().unwrap();
    assert_eq!(state.pull_drain.pull_breaker, None);
    pair.running_leaf(&drain, 4);
    assert_eq!(pulls(&pair), 6, "a closed breaker admits again");
}

/// [ORB-15639] Orbit's refusal of an implementer's handoff (a deferral
/// naming a command outside the owner's gate list, say) blocks the task for
/// an operator to requeue, but says nothing about the host: it neither
/// extends nor resets the breaker's streak. Three genuine failures in a row
/// still open it.
fn contract_refusals_do_not_trip_the_breaker_but_genuine_failures_still_do() {
    let pair = Pair::new(6);
    let drain = pair.start_drain();
    let refusal = format!(
        "{CONTRACT_REFUSAL_MARKER}: `deferred_sandbox_validation` names the gate summary \
         `.orbit/tmp/gate.json`, which names `make ci-test-affected`, which is not one of the \
         owner's validation gates"
    );
    let mut refused_task = None;
    for (error, streak) in [
        (refusal.as_str(), 0),
        ("candidate validation failed", 1),
        (refusal.as_str(), 1),
        ("candidate validation failed", 2),
        (refusal.as_str(), 2),
        ("candidate validation failed", 3),
    ] {
        let leaf = pair.running_leaf(&drain, 1);
        refused_task.get_or_insert_with(|| pair.claimed_task(&leaf));
        pair.leaf_fails_with(&leaf, error);
        let pass = pair.pass_with(&drain, 0);
        assert!(error_of(&pass).is_empty(), "{pass}");
        assert_eq!(pass["consecutive_failures"], streak, "{error}: {pass}");
    }
    assert_eq!(
        pair.owner_status(&refused_task.unwrap()),
        "blocked",
        "a refused handoff still blocks its task"
    );
    let opened = pair.pass(&drain);
    assert_eq!(opened["breaker_state"], "open", "{opened}");
    assert_eq!(breaker(&pair, &drain).failure_classes, vec!["candidate"; 3]);
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

/// Required tools must be satisfiable before a follower creates any claim or
/// leaf (F2026-10-352/353/354). Provenance never changes that authority.
fn unsatisfiable_claimed_requirements_stay_in_backlog_before_any_leaf() {
    let pair = Pair::new(0);
    let owner = &pair.wire.owner;
    let mut held = Vec::new();
    for tool in ["orbit.task.update", "orbit.search", "orbit.task.list"] {
        for automated in [false, true] {
            let file = format!("src/requirement{}.rs", held.len());
            let task = if automated {
                std::fs::write(pair.owner_repo.join(&file), "fn work() {}\n").unwrap();
                let definition = owner
                    .auto_task_add(orbit_core::AutoTaskAddParams {
                        name: format!("tool-routing-{}", held.len()),
                        description: "Preclaim requirement regression".into(),
                        schedule: orbit_core::AutoTaskSchedule::Interval { every_minutes: 60 },
                        template: serde_json::from_value(json!({
                            "title": "Automated tool requirement",
                            "description": "Same requirements as hand-authored work.",
                            "acceptance_criteria": ["Requirements remain satisfiable."],
                            "required_tools": [tool],
                            "context_files": [format!("file:{file}")],
                            "complexity": "low", "status": "backlog",
                        }))
                        .unwrap(),
                        dedupe: orbit_core::DedupePolicy::SkipIfOpen,
                    })
                    .unwrap();
                let task = owner.auto_task_mint(&definition.name).unwrap();
                owner.run_tool("orbit.task.update", json!({
                    "id": task.id, "plan": "1. Implement the requirement.", "model": "codex",
                })).unwrap();
                task.id
            } else {
                backlog_task_with_input(
                    owner,
                    &pair.owner_repo,
                    &file,
                    None,
                    json!({"required_tools": [tool]}),
                )
            };
            held.push((task, tool));
        }
    }
    let local = owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    for (task, _) in &held {
        assert!(
            local["task_ids"].as_array().unwrap().contains(&json!(task)),
            "owner-local dispatch remains eligible: {local}"
        );
    }
    let drain = pair.start_drain();
    let idle = pair.pass(&drain);
    assert!(error_of(&idle).is_empty(), "{idle}");
    assert_eq!(idle["admitted"], 0, "{idle}");
    assert!(
        pair.owner_claims().is_empty(),
        "no claim for unsatisfiable tools"
    );
    assert!(
        pair.leaf_runs().is_empty(),
        "no worktree setup or failed leaf"
    );
    let request = pair.wire.calls("orbit.task.pull").pop().unwrap();
    let receipt = pair
        .wire
        .call("", "orbit.task.pull", request.clone())
        .unwrap();
    assert!(receipt["receipt"]["claim"].is_null(), "{receipt}");
    assert_eq!(
        pair.wire
            .call("", "orbit.task.pull", request.clone())
            .unwrap(),
        receipt,
        "idle replay preserves the original exclusions"
    );
    let exclusions = receipt["receipt"]["deferred_conflicts"].as_array().unwrap();
    assert_eq!(exclusions.len(), held.len(), "{receipt}");
    for (task, tool) in &held {
        let reason = exclusions
            .iter()
            .find(|entry| entry["task_id"] == *task)
            .unwrap()["reason"]
            .as_str()
            .unwrap();
        assert!(reason.contains(tool), "{reason}");
        assert!(
            reason.contains(if *tool == "orbit.task.update" {
                "denied in claimed mode"
            } else {
                "outside the closed claimed-owner bridge"
            }),
            "{reason}"
        );
        assert_eq!(pair.owner_status(task), "backlog");
        assert_eq!(pair.owner_task(task)["required_tools"], json!([tool]));
        let mutation = owner.run_tool(
            "orbit.task.update",
            json!({
                "id": task, "required_tools": [], "model": "codex",
            }),
        );
        assert!(mutation.is_err(), "requirements remain immutable");
        // The owner-local implementer keeps its existing activity admission.
        let grant = owner
            .resolve_activity_tool_denials(std::slice::from_ref(task), "agent_implement", &[])
            .unwrap();
        assert_eq!(grant.requested_tools, vec![tool.to_string()]);
        assert!(grant.effective_tools.iter().any(|name| name == tool));
    }
    for peers in held.as_chunks::<2>().0 {
        let reason = |task: &str| {
            exclusions
                .iter()
                .find(|entry| entry["task_id"] == task)
                .unwrap()["reason"]
                .clone()
        };
        assert_eq!(
            reason(&peers[0].0),
            reason(&peers[1].0),
            "hand-authored and auto-task records obey the same policy"
        );
    }

    let supported = backlog_task_with_input(
        owner,
        &pair.owner_repo,
        "src/supported.rs",
        None,
        json!({"required_tools": [
            "orbit.task.show", "orbit.task.add", "orbit.friction.add",
            "orbit.task.artifact.get", "orbit.task.artifact.put", "proc.spawn",
        ]}),
    );
    assert_eq!(
        pair.wire.call("", "orbit.task.pull", request).unwrap(),
        receipt,
        "adding eligible work does not alter a stored idle receipt"
    );
    let leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), supported);
    assert_eq!(pair.owner_claims().len(), 1);
    assert_eq!(pair.leaf_runs(), vec![leaf]);
    for (task, tool) in &held {
        assert!(!pair.owner_has_claim_for_task(task));
        assert_eq!(pair.owner_task(task)["required_tools"], json!([tool]));
    }
}

/// A follower whose window preflight cannot find crew `antigravity`'s CLI is
/// never handed an `antigravity` task [ORB-13941]. The owner admits the next
/// task it can run instead and, once only the unrunnable one is left, answers
/// idle and names it, so the task stays in the backlog for the owner or
/// another follower rather than being claimed and failed here.
fn a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run() {
    let pair = Pair::with_crews(&[Some("antigravity"), Some("sol")]);
    pair.follower_cli("antigravity", "orbit-test-no-such-provider-cli");
    let (unrunnable, runnable) = (&pair.tasks[0], &pair.tasks[1]);
    let drain = pair.start_drain();

    let leaf = pair.running_leaf(&drain, 1);
    let first = pair.pass(&drain);
    let exclusion = excluded(&first, "antigravity");
    assert_eq!(exclusion["source"], "preflight", "{first}");
    assert!(
        exclusion["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("orbit-test-no-such-provider-cli")),
        "{first}"
    );
    pair.leaf_fails_with(&leaf, "candidate validation failed");
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
fn a_follower_is_handed_only_tasks_its_os_satisfies() {
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

    for _ in 0..2 {
        let leaf = pair.running_leaf(&drain, 1);
        pair.leaf_fails_with(&leaf, "candidate validation failed");
    }
    pair.pass(&drain);
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
fn a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
    a_provider_failure_releases_the_claim(
        "[provider_unavailable] claude provider authentication failure (HTTP 401): \
         Failed to authenticate: OAuth token revoked. Please log in again or contact your administrator.",
        "OAuth token revoked",
    );
}

/// [ORB-14149] Likewise a claimed leaf whose provider said its selected model
/// was at capacity: recovery could not change that and an immediate rerun
/// would use the same model, so the claim goes back to the owner's backlog.
fn a_provider_capacity_failure_releases_the_claim_and_excludes_the_crew_for_the_window() {
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
fn a_provider_auth_failure_excludes_every_crew_of_that_provider() {
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
    // launch fails and the owner releases that task. The claim phase is the
    // launch failure, which is how an admitted crew shows up here.
    assert!(
        launch_refused(&pass),
        "sol was admitted far enough to launch: {pass}"
    );
    assert_eq!(
        phase(&sol),
        Some("revoked"),
        "the other provider is admitted, then its launch fails: {claims:#?}"
    );
    let settles = pair.wire.calls("orbit.drain.claim.settle");
    assert!(
        settles.iter().any(|call| {
            call["settlement"]["Release"]["failure"]["reason"]
                .as_str()
                .is_some_and(|summary| summary.starts_with("leaf launch failed"))
        }),
        "sol's admission reached launch: {settles:?}"
    );
    assert_eq!(pair.owner_status(&sonnet), "backlog");
    assert_eq!(pair.owner_status(&opus), "backlog");
    assert_eq!(
        pair.owner_status(&sol),
        "backlog",
        "the launch refusal releases the admitted task"
    );

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    let runnable = window.runnable.as_ref().expect("preflight ran");
    assert!(
        window.host_suppressed.is_some(),
        "launch failure suppresses the follower: {window:#?}"
    );
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
/// offers it once the owner's clock tick records that the required command
/// passes on the new base. Pull admission never re-checks it [ORB-14739].
fn a_red_base_failure_releases_the_claim_until_the_command_passes() {
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
        selection: None,
        failures: Default::default(),
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
            selection: None,
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
    let refresh = pair.wire.owner.refresh_baseline_holds(None).unwrap();
    assert_eq!(refresh.held, vec![task.clone()], "{refresh:?}");
    let moved_red = pair.pass(&drain);
    assert_eq!(
        moved_red["admitted"], 0,
        "the new tip is still red: {moved_red}"
    );
    assert_eq!(pair.leaf_runs(), leaves, "a still-red base is not pulled");

    std::fs::write(repo.join("Makefile"), "ci-lint:\n\t@echo lint-ok\n").unwrap();
    git(repo, &["add", "Makefile"]);
    git(repo, &["commit", "-q", "-m", "fix lint"]);
    let unchecked = pair.pass(&drain);
    assert_eq!(
        unchecked["admitted"], 0,
        "admission waits for the tick's verdict on the green tip: {unchecked}"
    );
    assert_eq!(pair.leaf_runs(), leaves);
    let refresh = pair.wire.owner.refresh_baseline_holds(None).unwrap();
    assert_eq!(refresh.lifted, vec![task.clone()], "{refresh:?}");
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
fn a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume() {
    let mut pair = Pair::new(2);
    let probe = crate::dispatch_admission::PressureProbe::calm();
    pair.follower = pair
        .follower
        .clone()
        .with_host_resource_probe(probe.clone());
    let drain = pair.start_drain();

    // A launched candidate fails and its settlement reply is lost, so
    // the drain owes the owner that settlement.
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_fails_with(&leaf, "candidate validation failed");
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
        .pull_drain
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

/// [ORB-15637] A pull claim runs on the follower, so only the follower's own
/// host pressure gates it. With the owner held on CPU, the owner's own
/// unattended discovery stands down, yet a calm follower still claims. With
/// the follower held on CPU and the owner calm, the follower requests nothing
/// and reports its own throttle, not an admission refusal.
fn a_pull_claim_is_gated_by_the_follower_host_never_the_owner() {
    let owner_probe = crate::dispatch_admission::PressureProbe::calm();
    let mut pair = Pair::with_owner(2, |owner| {
        owner.with_host_resource_probe(owner_probe.clone())
    });
    let follower_probe = crate::dispatch_admission::PressureProbe::calm();
    pair.follower = pair
        .follower
        .clone()
        .with_host_resource_probe(follower_probe.clone());
    let drain = pair.start_drain();

    owner_probe.sustain(&pair.wire.owner, |probe| probe.cpu(Some(231.0)));
    let owner_hold = pair
        .wire
        .owner
        .resource_admission()
        .throttle
        .expect("the owner is throttled on CPU");
    assert_eq!(owner_hold.resources[0].resource, "cpu", "{owner_hold:?}");
    let sweep = pair
        .wire
        .owner
        .drain_entry_admission(DrainEntryPoint::ShipSweep, &[], true)
        .unwrap();
    assert_eq!(
        sweep.refusal.map(|refusal| refusal.code()),
        Some(RESOURCE_THROTTLED),
        "the owner's own unattended discovery stands down"
    );

    // The fixture cannot launch the claimed leaf, so the pass ends at that
    // launch: the claim itself is what the owner's throttle never stopped.
    let claimed = pair.pass(&drain);
    assert!(launch_refused(&claimed), "{claimed}");
    assert_eq!(claimed["resource_throttle"], Value::Null, "{claimed}");
    assert!(
        !claimed.to_string().contains(RESOURCE_THROTTLED),
        "no throttle held the claim: {claimed}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "the calm follower requests a claim while the owner is throttled"
    );
    assert_eq!(pair.owner_claims().len(), 1, "the owner admitted it");

    // The owner calms; the follower is now the one under pressure. A new
    // drain, because the refused launch suppressed this host for the first
    // drain's window.
    owner_probe.cpu(Some(10.0));
    owner_probe.memory(10.0, Utc::now());
    assert!(pair.wire.owner.resource_admission().throttle.is_none());
    let drain = pair.start_drain();
    follower_probe.sustain(&pair.follower, |probe| probe.cpu(Some(195.0)));
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false, "{held}");
    assert_eq!(held["admitted"], 0, "{held}");
    assert_eq!(
        held["resource_throttle"]["resources"][0]["resource"], "cpu",
        "{held}"
    );
    assert_eq!(held["refusal"], Value::Null, "{held}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "no new claim");
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the pass is recorded");
    assert_eq!(
        recorded
            .resource_throttle
            .as_ref()
            .map(|throttle| throttle.resources[0].resource.as_str()),
        Some("cpu"),
        "the follower's own throttle is what held it: {recorded:?}"
    );
    assert_eq!(recorded.admission_refusal, None, "{recorded:?}");
}

/// A local drain's admission of `task` on the owner, as its gate leaves it
/// while waiting for context locks: the wrapper and gate carry the task, and
/// the task is still `backlog` with nothing reserved.
pub(super) fn local_drain_admission(owner: &OrbitRuntime, task: &str) -> Vec<String> {
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
fn a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends() {
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
fn a_task_under_a_live_claim_is_never_admitted_by_the_local_drain() {
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
    assert_eq!(pair.only_owner_claim()["claim"]["phase"], "claimed");

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

/// [ORB-15514] The owner's drain admits its wave through the pull admission
/// section, so a follower pulling after the wave, but before the wave's gate
/// has reserved anything, is handed neither the admitted task nor a task
/// overlapping its footprint.
fn a_pull_after_an_owner_wave_cannot_take_the_footprint_it_admitted() {
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    let admitted = pair.tasks[0].clone();
    let overlapping = backlog_task(owner, &pair.owner_repo, "src/f0.rs", None);

    let wave = owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([admitted]), "{wave}");

    // Nothing has reserved the footprint or carries the task in a live run
    // yet: only the wave's admission stands between the pull and a claim.
    let drain = pair.start_drain();
    let pass = pair.pass(&drain);
    assert!(error_of(&pass).is_empty(), "{pass}");
    assert!(pair.owner_claims().is_empty(), "{:#?}", pair.owner_claims());
    assert_eq!(pair.owner_status(&admitted), "backlog");
    assert_eq!(pair.owner_status(&overlapping), "backlog");
}

/// [ORB-15514] A follower's live claim on a footprint is named wherever the
/// owner waits on it: the drain's recorded hold reason, readiness, and a gate
/// whose wait budget runs out, which defers its task to the backlog and ends
/// its run successfully instead of failing it.
fn a_wait_on_a_claimed_footprint_names_the_claim_and_a_starved_gate_defers() {
    let pair = Pair::new(1);
    let claimed = pair.tasks[0].clone();
    let drain = pair.start_drain();
    // The owner commits the claim; the lost reply leaves it unbound and live.
    pair.wire.lose_next_reply("orbit.task.pull");
    let lost = pair.pass(&drain);
    assert!(error_of(&lost).contains("dropped"), "{lost}");
    let claim = pair.only_owner_claim();
    let claim_id = claim["claim"]["claim_id"].as_str().unwrap().to_string();
    let owner = &pair.wire.owner;
    let waiting = backlog_task(owner, &pair.owner_repo, "src/f0.rs", None);

    // The drain's hold reason.
    let owner_jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let local_drain = owner_jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    owner
        .write_run_state(
            &local_drain.run_id,
            &orbit_types::workflow::PipelineState::new(
                local_drain.run_id.clone(),
                local_drain.job_id,
                json!({}),
            ),
        )
        .unwrap();
    let local_drain = local_drain.run_id;
    let wave = owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": local_drain, "max_active_leaf_runs": 4}),
            ToolContext::default(),
        )
        .expect("classify");
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave}");
    let pass = owner
        .read_run_state(&local_drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("last pass");
    let held = pass
        .excluded
        .iter()
        .chain(&pass.deferred)
        .find(|task| task.task_id == waiting)
        .unwrap_or_else(|| panic!("the pass records the waiting task: {pass:#?}"));
    assert_eq!(held.blocked_by, vec![claimed.clone()], "{held:?}");
    let detail = held.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains(&claim_id) && detail.contains(FOLLOWER),
        "the hold names the claim and its machine: {held:?}"
    );

    // Readiness.
    let readiness = owner.workspace_auto_readiness(&[], None, 50, &[]).unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == waiting.as_str())
        .unwrap_or_else(|| panic!("readiness lists the waiting task: {readiness:#}"));
    assert_eq!(entry["eligible"], false, "{entry}");
    let claims = entry["blocking_claims"]
        .as_array()
        .expect("blocking claims");
    assert_eq!(claims.len(), 1, "{entry}");
    assert_eq!(claims[0]["task_id"], claimed.as_str(), "{entry}");
    assert_eq!(claims[0]["claim_id"], claim_id.as_str(), "{entry}");
    assert_eq!(claims[0]["machine_id"], FOLLOWER, "{entry}");

    // A gate whose wait budget runs out while the claim holds the footprint,
    // run from the shipped job and activities.
    orbit_core::bootstrap::init::init_workspace_at_root(
        &owner.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("seed shipped gate job and activities");
    let owner = calm_host(
        OrbitRuntime::from_roots(&owner.global_root(), &pair.owner_repo.join(".orbit"))
            .unwrap()
            .with_automation_machine_identity(Some(OWNER.into())),
    );
    let gate_job = owner.show_job_catalog_entry("task_gate_pipeline").unwrap();
    let gate = owner
        .run_job_v2_from_yaml(
            &gate_job.path,
            json!({
                "task_ids": [waiting],
                "mode": "local",
                "max_wait_seconds": 0,
                "poll_interval_seconds": 0,
            }),
        )
        .expect("gate run");
    assert!(
        gate.success,
        "a starved gate defers, never fails: {gate:#?}"
    );
    let deferral = &gate.pipeline["starvation_check"];
    assert_eq!(deferral["deferred"], true, "{deferral}");
    assert_eq!(
        deferral["blocking_task_ids"],
        json!([claimed]),
        "{deferral}"
    );
    assert_eq!(
        deferral["blocking_claims"][0]["claim_id"],
        claim_id.as_str(),
        "{deferral}"
    );
    let dispatch = &gate.pipeline["dispatch_child"];
    assert_eq!(dispatch["skipped"], true, "{dispatch}");
    assert_eq!(dispatch["status"], "success", "{dispatch}");
    assert_eq!(dispatch["deferral"], *deferral, "{dispatch}");
    assert!(
        owner_jobs
            .list_job_runs("task_local_pipeline")
            .unwrap()
            .is_empty(),
        "a deferred gate dispatches no delivery run"
    );
    assert_eq!(pair.owner_status(&waiting), "backlog");
    assert_eq!(pair.only_owner_claim()["claim"]["phase"], "claimed");
}

/// A task that keeps an admissible status but is refused at dispatch (a pilot
/// hold, or a pilot receipt that cannot be decoded) stops the gate as a failed
/// child whose reason and audit record carry the admission error, never a
/// claim that its status changed. A status that really left the admissible set
/// still reports a withdrawal.
fn a_held_backlog_task_reports_the_hold_at_dispatch_not_a_status_change() {
    let pair = Pair::new(3);
    let owner = &pair.wire.owner;
    let gate = |task: &str| {
        owner
            .run_deterministic(
                "invoke_and_wait",
                &json!({"run_id": "parent-run"}),
                &json!({
                    "job_name": "task_pr_pipeline",
                    "run_input": {"task_ids": [task]},
                    "admission_task_ids": [task],
                    "admission_workflow": "worktree_setup",
                    "timeout_seconds": 5,
                }),
                ToolContext::default(),
            )
            .expect("gate dispatch")
    };
    let audit_reason = |command: &str| {
        let events = owner
            .list_audit_events(None, None, None, None, 1000)
            .expect("audit events");
        let event = events
            .iter()
            .find(|event| event.command == command)
            .unwrap_or_else(|| panic!("no {command} audit event"));
        let payload: Value =
            serde_json::from_str(event.arguments_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload["outcome"], command.trim_start_matches("gate."));
        payload["reason"].as_str().unwrap().to_string()
    };

    // A host-operational pilot hold recorded after the gate was admitted.
    let held = pair.tasks[0].clone();
    super::pilot::apply_assessment(
        &pair,
        "The owner-side repair is observable.",
        json!({
            "disposition": "host_operational", "context_files_after": [],
            "evidence": "Only the operator can repair the owner-side definition.",
        }),
    );
    let output = gate(&held);
    assert_eq!(output["skipped"], true, "{output}");
    assert_eq!(output["status"], "failed", "{output}");
    let error = output["error"].as_str().unwrap();
    assert!(
        error.contains("Only the operator can repair the owner-side definition."),
        "{output}"
    );
    assert!(!error.contains("no longer admissible"), "{output}");
    assert!(!error.contains("status changed"), "{output}");
    assert_eq!(output["task_statuses"][0]["status"], "backlog", "{output}");
    assert!(
        audit_reason("gate.admission_refused")
            .contains("Only the operator can repair the owner-side definition.")
    );

    // A malformed pilot receipt is surfaced instead of discarded.
    let malformed = pair.tasks[1].clone();
    // The write persists the comment, then its own admission read refuses it.
    let _ = owner.update_task_as_human(
        &malformed,
        orbit_core::application::task::TaskUpdateParams {
            comment: Some("operation_id=fixture\n{not json".into()),
            ..Default::default()
        },
        "task-pilot".into(),
    );
    assert_eq!(
        owner.get_task_comments(&malformed).unwrap().len(),
        1,
        "the malformed receipt must be recorded"
    );
    let output = gate(&malformed);
    assert_eq!(output["status"], "failed", "{output}");
    let error = output["error"].as_str().unwrap();
    assert!(error.contains("decode pilot assessment"), "{output}");
    assert!(!error.contains("no longer admissible"), "{output}");

    // A task a human moved out of the admissible set is still a withdrawal.
    let withdrawn = pair.tasks[2].clone();
    owner
        .update_task_as_human(
            &withdrawn,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(orbit_types::task::TaskStatus::Someday),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let output = gate(&withdrawn);
    assert_eq!(output["status"], "failed", "{output}");
    assert!(
        output["error"]
            .as_str()
            .unwrap()
            .contains("no longer admissible"),
        "{output}"
    );
    assert!(audit_reason("gate.withdrawn").contains("someday"));
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
fn review_settings_never_refuse_a_pull_and_the_owner_captures_before_pr() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();

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

    // The review crew is one the owner defines: a load refuses one it does
    // not [ORB-15195].
    for config in [
        "[review]\nbefore_pr = true\n[operation]\nreview_crew = \"sol\"\n",
        "[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"sol\"\n",
    ] {
        let owner = owner_with(&root.path().join(config.len().to_string()), config, false);
        let probe = probe_owner(&owner, false);
        assert_eq!(probe["review"]["before_pr"]["enabled"], true, "{config}");
        assert_eq!(probe["ship"]["before_pr"], true, "{config}");
        assert_eq!(probe["ship"]["review"]["crew"], "sol", "{config}");
        assert_eq!(probe["admits"], true, "{config}: {probe}");
    }
}

/// A host whose Git protection would refuse every sandboxed leaf must not
/// turn the tasks it claims into `blocked` one by one [ORB-14928]. The drain
/// stops at submission with the scan's diagnostic: no owner call, no run, no
/// task touched.
#[cfg(unix)]
#[test]
fn a_pull_drain_on_a_host_that_fails_the_git_scan_claims_nothing() {
    if !isolated(
        module_path!(),
        "a_pull_drain_on_a_host_that_fails_the_git_scan_claims_nothing",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let sandbox = if cfg!(target_os = "linux") {
        orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap
    } else {
        orbit_types::workflow::ExecutorSandboxKind::MacosSandboxExec
    };
    pair.follower
        .upsert_executor_def(&ExecutorDef {
            name: "codex".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some("codex".to_string()),
            args: vec![],
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: Some(sandbox),
            allow_fallback: false,
            host_os: None,
            created_at: None,
            updated_at: None,
        })
        .expect("sandboxed executor");
    let outside = pair.follower_repo.parent().unwrap().join("outside-hook");
    std::fs::write(&outside, "#!/bin/sh\n").unwrap();
    let hook = pair.follower_repo.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::hard_link(&outside, &hook).unwrap();
    let before: Vec<String> = pair.tasks.iter().map(|id| pair.owner_status(id)).collect();
    let selector = pair.destination["selector"].as_str().unwrap().to_string();

    let error = pair
        .follower
        .submit_workspace_pull_run(
            orbit_core::WorkspacePullRequest {
                selector: &selector,
                for_seconds: Some(60),
                max_active_leaf_runs: Some(1),
                allowed_crews: &[],
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("a failing scan refuses admission");

    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::PolicyDenied(_))
            && message.contains(&hook.display().to_string()),
        "the scan's diagnostic names the entry: {message}"
    );
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.leaf_runs().is_empty());
    let after: Vec<String> = pair.tasks.iter().map(|id| pair.owner_status(id)).collect();
    assert_eq!(before, after, "no task changed status");
}

/// A reboot inside the hold horizon requests nothing and records why. One
/// strictly later requests a claim. Equality with the horizon holds, so the
/// near side is scheduled exactly `hold_minutes` out and the far side one
/// minute past that, measured at the pass.
fn a_shutdown_inside_the_hold_horizon_refuses_pull_claims_and_one_beyond_it_requests_one() {
    let mut pair = Pair::new(1);
    let drain = pair.start_drain();
    let schedule = |minutes: i64| orbit_core::runtime::host_signal::ScheduledShutdown {
        mode: "reboot".into(),
        scheduled_at: Utc::now() + chrono::Duration::minutes(minutes),
        source: "fixture".into(),
    };
    let install = |pair: &mut Pair, minutes: i64| {
        pair.follower = pair
            .follower
            .clone()
            .with_host_shutdown_hold_minutes(30)
            .with_host_signal_probe(Arc::new(
                orbit_core::runtime::host_signal::FixedHostSignals::scheduled(schedule(minutes)),
            ));
    };

    install(&mut pair, 30);
    let held = pair.pass(&drain);
    assert_eq!(held["admitting"], false, "{held}");
    assert_eq!(held["admitted"], 0, "{held}");
    assert!(
        held["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("host_shutdown_scheduled")),
        "{held}"
    );
    assert!(
        held["host_shutdown"]
            .as_str()
            .is_some_and(|text| text.contains("reboot")),
        "{held}"
    );
    assert!(
        pair.wire.calls("orbit.task.pull").is_empty(),
        "a hold must not request a claim: {held}"
    );
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the pass is recorded");
    assert!(
        recorded
            .admission_refusal
            .as_deref()
            .is_some_and(|refusal| refusal.starts_with("host_shutdown_scheduled")),
        "{recorded:?}"
    );

    install(&mut pair, 31);
    let _leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        1,
        "a schedule past the horizon is admitted"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    pull_admission_preserves_execution_machine_name_scenarios,
    pull_admission_preserves_execution_machine_name,
    session_metadata_does_not_corrupt_owner_probe_or_pull_pass,
    redacted_owner_identity_retries_without_protocol_skew,
    redacted_pull_receipt_reports_unknown_outcome_and_replays_same_request,
    obsolete_persisted_request_skew_does_not_fail_a_matching_drain,
    owner_pull_admits_empty_context_without_locks_and_claims_no_diff_work,
    a_follower_without_required_validation_commands_starts_a_pull_drain,
    unreadable_cancel_state_fails_visibly_without_admission,
    an_older_owner_is_refused_before_a_newer_request_is_sent,
    fingerprintless_older_revision_pull_keeps_the_invalid_input_refusal,
    probe_fingerprint_tracks_request_types_and_refuses_changed_shapes,
    owner_checks_fingerprint_before_deserializing_an_incompatible_pull
);

crate::scenario_table!(
    same_version_skew_ends_the_pull_job_failed_before_any_pull_scenarios,
    same_version_skew_ends_the_pull_job_failed_before_any_pull,
    repeated_pass_failures_degrade_the_drain_and_a_new_drain_resets_it,
    cancel_recorded_during_orphan_reconciliation_admits_nothing,
    readable_state_without_cancel_permits_refill,
    three_failed_claims_open_the_breaker_and_a_new_drain_resets_it,
    an_open_breaker_half_opens_for_one_probe_and_reopens_or_closes_on_its_outcome,
    contract_refusals_do_not_trip_the_breaker_but_genuine_failures_still_do,
    a_follower_never_receives_a_claim_for_a_crew_its_window_cannot_run,
    unsatisfiable_claimed_requirements_stay_in_backlog_before_any_leaf,
    a_follower_is_handed_only_tasks_its_os_satisfies,
    a_provider_auth_failure_releases_the_claim_and_excludes_the_crew_for_the_window
);

crate::scenario_table!(
    a_provider_capacity_failure_releases_the_claim_and_excludes_the_crew_for_the_window_scenarios,
    a_provider_capacity_failure_releases_the_claim_and_excludes_the_crew_for_the_window,
    a_provider_auth_failure_excludes_every_crew_of_that_provider,
    a_red_base_failure_releases_the_claim_until_the_command_passes,
    a_throttled_pull_drain_keeps_settling_and_pulls_again_below_resume,
    a_pull_claim_is_gated_by_the_follower_host_never_the_owner,
    a_task_a_local_drain_admitted_is_not_pulled_until_that_admission_ends,
    a_task_under_a_live_claim_is_never_admitted_by_the_local_drain,
    a_pull_after_an_owner_wave_cannot_take_the_footprint_it_admitted,
    a_wait_on_a_claimed_footprint_names_the_claim_and_a_starved_gate_defers,
    a_held_backlog_task_reports_the_hold_at_dispatch_not_a_status_change
);

crate::scenario_table!(
    review_settings_never_refuse_a_pull_and_the_owner_captures_before_pr_scenarios,
    review_settings_never_refuse_a_pull_and_the_owner_captures_before_pr,
    a_shutdown_inside_the_hold_horizon_refuses_pull_claims_and_one_beyond_it_requests_one
);
