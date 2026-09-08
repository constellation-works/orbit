//! Grant enablement, precedence capture, stop/revoke compare-and-set, and the
//! effective-policy explanation [ORB-11332].

use orbit_common::OrbitError;
use orbit_config::{CompletionPreference, OperationLayer, OperationPreset, ReviewPolicy};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{GrantRights, GrantStatus};

use super::{fixture, seed_task};
use crate::application::operation::{EnableOperationGrantRequest, OperationGrantControlRequest};

const AUTONOMOUS_DONE: &str = "[operation]\npreset = \"autonomous\"\ndelivery_cap = \"done\"\n";
const AUTONOMOUS_CAPPED: &str = "[operation]\npreset = \"autonomous\"\n";

fn rights(prepare: bool, promote: bool, complete: bool) -> GrantRights {
    GrantRights {
        prepare,
        promote,
        complete,
    }
}

fn enable<'a>(
    task_ids: &'a [String],
    window_seconds: u64,
    rights: GrantRights,
    run_layer: OperationLayer,
) -> EnableOperationGrantRequest<'a> {
    EnableOperationGrantRequest {
        task_ids,
        window_seconds,
        rights,
        run_layer,
        actor: "tester",
        source: "unit",
        claim_token: None,
    }
}

fn control<'a>(
    grant_id: Option<&'a str>,
    expected_revision: Option<u32>,
) -> OperationGrantControlRequest<'a> {
    OperationGrantControlRequest {
        grant_id,
        reason: Some("unit"),
        expected_revision,
        actor: "tester",
        source: "unit",
        claim_token: None,
    }
}

#[test]
fn enable_validates_scope_window_rights_and_task_status() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let backlog = seed_task(runtime, "backlog", TaskStatus::Backlog);
    let done = seed_task(runtime, "done", TaskStatus::Done);

    let error = |request: EnableOperationGrantRequest<'_>| {
        runtime
            .enable_operation_grant(request)
            .expect_err("invalid enablement must be refused")
            .to_string()
    };

    assert!(
        error(enable(
            &[],
            3600,
            rights(true, true, false),
            OperationLayer::default()
        ))
        .contains("finite, non-empty task set")
    );
    let ids = vec![backlog.id.clone()];
    assert!(
        error(enable(
            &ids,
            0,
            rights(true, true, false),
            OperationLayer::default()
        ))
        .contains("grant window must be between 1 and 86400 seconds")
    );
    assert!(
        error(enable(
            &ids,
            90_000,
            rights(true, true, false),
            OperationLayer::default()
        ))
        .contains("grant window must be between 1 and 86400 seconds")
    );
    assert!(
        error(enable(
            &ids,
            3600,
            rights(false, false, false),
            OperationLayer::default()
        ))
        .contains("at least one right")
    );
    let done_ids = vec![done.id.clone()];
    assert!(
        error(enable(
            &done_ids,
            3600,
            rights(true, true, false),
            OperationLayer::default()
        ))
        .contains("is done; a grant covers proposed or backlog work only")
    );
    assert!(
        error(enable(
            &["ORB-99999".to_string()],
            3600,
            rights(true, false, false),
            OperationLayer::default()
        ))
        .contains("ORB-99999")
    );
    let too_many = (0..51)
        .map(|index| format!("ORB-{index}"))
        .collect::<Vec<_>>();
    assert!(
        error(enable(
            &too_many,
            3600,
            rights(true, false, false),
            OperationLayer::default()
        ))
        .contains("at most 50 tasks")
    );
    assert!(runtime.active_operation_grant().expect("query").is_none());
}

#[test]
fn enable_refuses_explicit_escalation_past_the_cap() {
    let fixture = fixture(AUTONOMOUS_CAPPED);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "capped", TaskStatus::Backlog);
    let ids = vec![task.id.clone()];

    let complete_right = runtime
        .enable_operation_grant(enable(
            &ids,
            3600,
            rights(true, true, true),
            OperationLayer::default(),
        ))
        .expect_err("complete right exceeds the review cap");
    assert!(
        complete_right
            .to_string()
            .contains("exceeds the repository delivery cap 'review'")
    );

    let explicit_done = runtime
        .enable_operation_grant(enable(
            &ids,
            3600,
            rights(true, true, false),
            OperationLayer {
                completion: Some(CompletionPreference::Done),
                ..OperationLayer::default()
            },
        ))
        .expect_err("explicit done exceeds the review cap");
    assert!(
        explicit_done
            .to_string()
            .contains("explicit completion 'done' exceeds")
    );

    // The capped preference itself is fine: the captured policy discloses the cap.
    let grant = runtime
        .enable_operation_grant(enable(
            &ids,
            3600,
            rights(true, true, false),
            OperationLayer::default(),
        ))
        .expect("capped enablement");
    assert_eq!(grant.policy["completion"]["value"], "done");
    assert_eq!(grant.policy["delivery_cap"]["value"], "review");
    assert_eq!(
        grant.limits.leaf_ceiling, 10,
        "autonomous preset ceiling within the hard limit"
    );
}

#[test]
fn enable_captures_policy_with_run_layer_precedence_and_hard_limits() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "captured", TaskStatus::Proposed);
    let ids = vec![task.id.clone()];

    let grant = runtime
        .enable_operation_grant(enable(
            &ids,
            1800,
            rights(true, true, true),
            OperationLayer {
                leaf_ceiling: Some(3),
                recovery_episodes_per_task: Some(1),
                ..OperationLayer::default()
            },
        ))
        .expect("enable");

    assert_eq!(grant.status, GrantStatus::Active);
    assert_eq!(grant.revision, 1);
    assert_eq!(grant.task_ids, ids);
    assert!(grant.rights.complete);
    assert_eq!(grant.limits.leaf_ceiling, 3);
    assert_eq!(grant.limits.recovery_episodes_per_task, 1);
    assert_eq!(grant.limits.recovery_minutes_per_task, 30);
    assert_eq!(grant.limits.preparation_due_seconds, 300);
    assert_eq!(grant.policy_version, orbit_config::OPERATION_POLICY_VERSION);
    assert_eq!(grant.policy["leaf_ceiling"]["source"]["layer"], "run");
    assert_eq!(grant.policy["completion"]["value"], "done");
    assert_eq!(grant.policy["completion"]["source"]["preset"], "autonomous");
    assert_eq!(grant.policy["completion"]["source"]["layer"], "workspace");
    assert!(grant.remaining_seconds(chrono::Utc::now()) <= 1800);

    // A preference edit after enablement changes nothing about the grant.
    let stored = runtime.operation_grant(&grant.id).expect("reload");
    assert_eq!(stored, grant);
    assert_eq!(
        runtime
            .active_operation_grant()
            .expect("active")
            .map(|grant| grant.id),
        Some(grant.id.clone())
    );
    assert_eq!(runtime.list_operation_grants(10).expect("list").len(), 1);
}

#[test]
fn one_active_grant_then_stop_replacement_compare_and_set_and_revocation() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "lifecycle", TaskStatus::Backlog);
    let ids = vec![task.id.clone()];
    let first = runtime
        .enable_operation_grant(enable(
            &ids,
            3600,
            rights(true, true, false),
            OperationLayer::default(),
        ))
        .expect("first grant");

    let second = runtime
        .enable_operation_grant(enable(
            &ids,
            3600,
            rights(true, true, false),
            OperationLayer::default(),
        ))
        .expect_err("a second active grant is refused");
    assert!(second.to_string().contains(&first.id));

    let conflict = runtime
        .stop_operation_grant(control(Some(&first.id), Some(9)))
        .expect_err("stale revision loses");
    assert!(
        matches!(conflict, OrbitError::JobRunControlConflict(_)),
        "{conflict:?}"
    );

    let stopped = runtime
        .stop_operation_grant(control(None, Some(1)))
        .expect("stop the active grant");
    assert_eq!(stopped.outcome, "stopped");
    assert_eq!(stopped.grant.status, GrantStatus::Stopped);
    assert_eq!(stopped.grant.revision, 2);
    assert!(stopped.coordinators.is_empty());

    let replayed = runtime
        .stop_operation_grant(control(Some(&first.id), None))
        .expect("replayed stop");
    assert_eq!(replayed.outcome, "unchanged");
    assert_eq!(replayed.grant.revision, 2);

    // A stopped workspace admits a replacement window; the old grant keeps
    // its evidence.
    let replacement = runtime
        .enable_operation_grant(enable(
            &ids,
            600,
            rights(true, false, false),
            OperationLayer::default(),
        ))
        .expect("replacement grant");
    assert_ne!(replacement.id, first.id);
    assert_eq!(
        runtime
            .active_operation_grant()
            .expect("active")
            .map(|grant| grant.id),
        Some(replacement.id.clone())
    );

    let revoked = runtime
        .revoke_operation_grant(control(Some(&first.id), None))
        .expect("revoke the stopped grant");
    assert_eq!(revoked.outcome, "revoked");
    assert_eq!(revoked.grant.status, GrantStatus::Revoked);
    assert!(revoked.grant.stopped.is_some() && revoked.grant.revoked.is_some());
    assert!(!revoked.grant.privileged_actions_allowed());

    let missing = runtime
        .stop_operation_grant(control(Some("ogrant-missing"), None))
        .expect_err("unknown grant");
    assert!(missing.to_string().contains("was not found"));
}

#[test]
fn explanation_names_sources_authority_caps_and_limiting_reasons() {
    let fixture = fixture("[operation]\npreset = \"autonomous\"\nreview_policy = \"before-pr\"\n");
    let runtime = &fixture.runtime;
    std::fs::create_dir_all(runtime.shared_root().join("routines")).expect("routines dir");
    std::fs::write(
        runtime.shared_root().join("routines/state-pilot.yaml"),
        "schemaVersion: 1\nname: state-pilot\nenabled: true\nhosts: [hm_test]\ntarget: job:task_pilot_pipeline\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: hm_test\n    branch: main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 90\npolicy:\n  overlap: forbid\n  timeout_minutes: 90\n",
    )
    .expect("write routine");

    let explanation = runtime.explain_operation(None).expect("explain");
    assert_eq!(explanation["preview"], false);
    assert_eq!(explanation["policy"]["preset"]["value"], "autonomous");
    assert_eq!(explanation["policy"]["preset"]["source"], "workspace");
    assert_eq!(
        explanation["policy"]["leaf_ceiling"]["source"],
        "preset:autonomous@workspace"
    );
    assert_eq!(explanation["authority"]["admission"], "none");
    assert_eq!(
        explanation["authority"]["reason"],
        "scoped_authorization_required"
    );
    assert_eq!(explanation["delivery"]["effective_completion"], "review");
    assert_eq!(explanation["delivery"]["cap"], "delivery_cap_review");
    assert_eq!(explanation["review"]["policy"], "before-pr");
    assert_eq!(explanation["review"]["gates_pr"], true);
    assert_eq!(explanation["review"]["reason"], "review_crew_unconfigured");
    assert_eq!(explanation["review"]["budget"]["reviewer_starts"], 2);
    assert_eq!(
        explanation["preparation"]["cadence_owners"][0]["routine"],
        "state-pilot"
    );
    assert_eq!(
        explanation["preparation"]["reason"],
        serde_json::Value::Null
    );
    assert_eq!(
        explanation["recovery"]["reason"],
        "no_enabled_triage_routine"
    );
    let reasons = explanation["limiting_reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .filter_map(|reason| reason.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        reasons,
        vec![
            "delivery_cap_review",
            "scoped_authorization_required",
            "no_enabled_triage_routine",
            "review_crew_unconfigured",
        ]
    );

    // A run-layer preview resets the preset-managed fields without touching
    // the independent review policy.
    let preview = runtime
        .explain_operation(Some(&OperationLayer {
            preset: Some(OperationPreset::Supervised),
            ..OperationLayer::default()
        }))
        .expect("preview");
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["policy"]["preset"]["source"], "run");
    assert_eq!(preview["policy"]["leaf_ceiling"]["value"], 5);
    assert_eq!(preview["policy"]["review_policy"]["value"], "before-pr");
    assert_eq!(preview["delivery"]["cap"], serde_json::Value::Null);

    // With a grant, the authority block reports it and the grant reason goes away.
    let task = seed_task(runtime, "explained", TaskStatus::Backlog);
    let grant = runtime
        .enable_operation_grant(enable(
            std::slice::from_ref(&task.id),
            3600,
            rights(true, true, false),
            OperationLayer {
                review_policy: Some(ReviewPolicy::None),
                ..OperationLayer::default()
            },
        ))
        .expect("enable");
    let explained = runtime.explain_operation(None).expect("explain with grant");
    assert_eq!(explained["preview"], false);
    assert_eq!(explained["authority"]["grant_id"], grant.id);
    assert_eq!(explained["authority"]["admission"], "open");
    assert_eq!(explained["delivery"]["grant_complete_right"], false);
    assert_eq!(
        explained["authority"]["policy"]["review_policy"]["value"],
        "none"
    );
    assert_eq!(explained["review"]["policy"], "none");
    assert_eq!(explained["policy"]["review_policy"]["value"], "before-pr");
    assert!(
        !explained["limiting_reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason == "scoped_authorization_required")
    );
}

/// [ORB-11507] Retuning `[operation]` after enablement must not rewrite the
/// live grant projection; current preferences remain visible as future-grant
/// defaults, including under a run-layer preview.
#[test]
fn explanation_keeps_captured_grant_policy_after_preference_retune() {
    let fixture = fixture(AUTONOMOUS_DONE);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "retuned", TaskStatus::Backlog);
    let grant = runtime
        .enable_operation_grant(enable(
            std::slice::from_ref(&task.id),
            3600,
            rights(true, true, true),
            OperationLayer::default(),
        ))
        .expect("enable");

    std::fs::write(
        runtime.shared_root().join("config.toml"),
        "[operation]\npreset = \"supervised\"\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\n",
    )
    .expect("retune workspace operation preferences");
    let retuned = crate::OrbitRuntime::from_roots(&runtime.global_root(), &runtime.shared_root())
        .expect("reopen runtime on retuned config")
        .with_automation_machine_identity(Some(super::MACHINE.to_string()));

    let explained = retuned
        .explain_operation(None)
        .expect("explain after retune");
    assert_eq!(explained["preview"], false);
    assert_eq!(explained["authority"]["grant_id"], grant.id);
    assert_eq!(explained["authority"]["admission"], "open");

    assert_eq!(
        explained["authority"]["policy"]["preset"]["value"],
        "autonomous"
    );
    assert_eq!(
        explained["authority"]["policy"]["completion"]["value"],
        "done"
    );
    assert_eq!(
        explained["authority"]["policy"]["delivery_cap"]["value"],
        "done"
    );
    assert_eq!(
        explained["authority"]["policy"]["preparation"]["value"],
        "automatic"
    );
    assert_eq!(
        explained["authority"]["policy"]["recovery"]["value"],
        "scheduled"
    );
    assert_eq!(
        explained["authority"]["policy"]["review_policy"]["value"],
        "none"
    );
    assert_eq!(
        explained["authority"]["policy"]["leaf_ceiling"]["value"],
        10
    );

    assert_eq!(explained["delivery"]["completion_preference"], "done");
    assert_eq!(explained["delivery"]["delivery_cap"], "done");
    assert_eq!(explained["delivery"]["effective_completion"], "done");
    assert_eq!(explained["delivery"]["cap"], serde_json::Value::Null);
    assert_eq!(explained["delivery"]["grant_complete_right"], true);
    assert_eq!(explained["preparation"]["preference"], "automatic");
    assert_eq!(explained["recovery"]["preference"], "scheduled");
    assert_eq!(explained["review"]["policy"], "none");
    assert_eq!(explained["review"]["reason"], serde_json::Value::Null);
    assert_eq!(explained["limits"]["leaf_ceiling"]["preference"], 10);
    assert_eq!(
        explained["preparation"]["reason"],
        "no_enabled_preparation_routine"
    );
    assert_eq!(explained["recovery"]["reason"], "no_enabled_triage_routine");

    assert_eq!(explained["policy"]["preset"]["value"], "supervised");
    assert_eq!(explained["policy"]["completion"]["value"], "review");
    assert_eq!(explained["policy"]["preparation"]["value"], "manual");
    assert_eq!(explained["policy"]["recovery"]["value"], "existing");
    assert_eq!(explained["policy"]["leaf_ceiling"]["value"], 5);
    assert_eq!(explained["policy"]["review_policy"]["value"], "before-pr");
    assert_eq!(explained["policy"]["review_crew"]["value"], "reviewers");
    assert_eq!(explained["policy"]["delivery_cap"]["value"], "review");

    let preview = retuned
        .explain_operation(Some(&OperationLayer {
            leaf_ceiling: Some(1),
            review_policy: Some(ReviewPolicy::AfterLanding),
            ..OperationLayer::default()
        }))
        .expect("preview must not rewrite live grant behavior");
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["policy"]["leaf_ceiling"]["value"], 1);
    assert_eq!(preview["policy"]["review_policy"]["value"], "after-landing");
    assert_eq!(preview["authority"]["policy"]["leaf_ceiling"]["value"], 10);
    assert_eq!(
        preview["authority"]["policy"]["review_policy"]["value"],
        "none"
    );
    assert_eq!(preview["limits"]["leaf_ceiling"]["preference"], 10);
    assert_eq!(preview["review"]["policy"], "none");
    assert_eq!(preview["delivery"]["effective_completion"], "done");
    assert_eq!(preview["delivery"]["completion_preference"], "done");
}

/// [ORB-11333] `before-pr` is an ordinary captured review timing: enablement
/// accepts it and the grant's policy snapshot carries it for every admission.
#[test]
fn enable_captures_a_before_pr_review_policy_with_its_budget() {
    let fixture = fixture(
        "[operation]\npreset = \"autonomous\"\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\nreview_reviewer_starts = 3\n",
    );
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "gated", TaskStatus::Backlog);

    let grant = runtime
        .enable_operation_grant(enable(
            std::slice::from_ref(&task.id),
            3600,
            rights(true, true, false),
            OperationLayer::default(),
        ))
        .expect("before-pr is captured, not refused");
    assert_eq!(grant.policy["review_policy"]["value"], "before-pr");
    assert_eq!(grant.policy["review_crew"]["value"], "reviewers");
    assert_eq!(grant.policy["review_reviewer_starts"]["value"], 3);
    assert_eq!(grant.policy_version, orbit_config::OPERATION_POLICY_VERSION);
}
