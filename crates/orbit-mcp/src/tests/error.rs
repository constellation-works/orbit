use super::*;
use orbit_common::{ArtifactOrigin, ArtifactOriginMode};

fn federated_origin() -> ArtifactOrigin {
    ArtifactOrigin {
        mode: ArtifactOriginMode::Federated,
        worktree_root: "/safe/sibling".to_string(),
        branch: Some("orbit/ORB-10294".to_string()),
    }
}

#[test]
fn adr_artifact_errors_keep_stable_codes_and_safe_origin() {
    for (error, expected_code) in [
        (
            OrbitError::remote_artifact_unavailable(
                NotFoundKind::Adr,
                "ADR-0234",
                federated_origin(),
            ),
            "remote_artifact_unavailable",
        ),
        (
            OrbitError::artifact_not_local(NotFoundKind::Adr, "ADR-0234", federated_origin()),
            "artifact_not_local",
        ),
    ] {
        let payload = error_payload(&error);
        assert_eq!(payload["code"], expected_code);
        assert_eq!(payload["artifact_origin"]["mode"], "federated");
        assert_eq!(payload["artifact_origin"]["worktree_root"], "/safe/sibling");
        assert_eq!(payload["artifact_origin"]["branch"], "orbit/ORB-10294");
        assert!(payload["artifact_origin"].get("body_path").is_none());

        let result = tool_error_result(&error);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.expect("structured error payload")["code"],
            expected_code
        );
    }
}

#[test]
fn owner_transport_errors_keep_stable_codes_and_call_identity() {
    let unavailable = error_payload(&OrbitError::OwnerUnavailable("offline".to_string()));
    assert_eq!(unavailable["code"], "owner_unavailable");

    let negotiation = error_payload(&OrbitError::OwnerNegotiation("digest drift".to_string()));
    assert_eq!(negotiation["code"], "owner_negotiation");

    let unknown = error_payload(&OrbitError::OutcomeUnknown {
        mcp_call_id: "mcall-exact".to_string(),
        message: "EOF after handoff".to_string(),
    });
    assert_eq!(unknown["code"], "outcome_unknown");
    assert!(
        unknown["message"]
            .as_str()
            .is_some_and(|message| message.contains("mcall-exact"))
    );

    let remote = error_payload(&OrbitError::RemoteTool {
        code: "invalid_input".to_string(),
        message: "definitive".to_string(),
        payload: json!({"code": "invalid_input", "message": "definitive", "detail": 7}),
    });
    assert_eq!(remote["code"], "invalid_input");
    assert_eq!(remote["detail"], 7);
}

#[test]
fn federated_routing_errors_keep_distinct_stable_codes() {
    for (error, expected_code) in [
        (
            OrbitError::UnknownSelector("bad selector".to_string()),
            "unknown_selector",
        ),
        (
            OrbitError::AmbiguousDestination("hm_duplicate".to_string()),
            "ambiguous_destination",
        ),
        (
            OrbitError::UnreachableDestination("hm_offline".to_string()),
            "unreachable_destination",
        ),
        (
            OrbitError::StaleRoute("hm_host/ws_missing".to_string()),
            "stale_route",
        ),
        (
            OrbitError::UnhealthyCheckout("hm_host/ws_broken".to_string()),
            "unhealthy_checkout",
        ),
        (
            OrbitError::ToolNotOnThisHost("orbit_job_run_show".to_string()),
            "tool_not_on_this_host",
        ),
        (
            OrbitError::CapabilityRefused("control_plane".to_string()),
            "capability_refused",
        ),
    ] {
        let payload = error_payload(&error);
        assert_eq!(payload["code"], expected_code);
        assert_ne!(payload["code"], "invalid_input");
        assert_ne!(payload["code"], "capability_denied");

        let result = tool_error_result(&error);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.expect("structured error payload")["code"],
            expected_code
        );
    }
}

#[test]
fn task_bundle_corruption_has_a_stable_code_and_structured_context() {
    let payload = error_payload(&OrbitError::TaskBundleCorrupt {
        task_id: "ORB-00123".to_string(),
        path: "/safe/tasks/ORB-00123".to_string(),
        reason: "missing description.md".to_string(),
    });

    assert_eq!(payload["code"], "task_bundle_corrupt");
    assert_eq!(payload["task_id"], "ORB-00123");
    assert_eq!(payload["path"], "/safe/tasks/ORB-00123");
    assert_eq!(payload["reason"], "missing description.md");
}

/// ORB-10544: the shared ship submission path refuses a duplicate dispatch with
/// a typed conflict; the MCP projection of it must name the contended task and
/// the run holding it under the same `ship_run_in_flight` code the dashboard's
/// 409 body carries, so a tool caller can wait on or cancel that run without
/// parsing the message.
#[test]
fn ship_run_in_flight_has_a_stable_code_and_names_both_ids() {
    let error = OrbitError::ShipRunInFlight {
        task_id: "TST-00001".to_string(),
        run_id: "jrun-in-flight".to_string(),
    };
    let payload = error_payload(&error);

    assert_eq!(payload["code"], "ship_run_in_flight");
    assert_eq!(payload["task_id"], "TST-00001");
    assert_eq!(payload["run_id"], "jrun-in-flight");
    assert!(
        payload["message"].as_str().is_some_and(
            |message| message.contains("TST-00001") && message.contains("jrun-in-flight")
        )
    );

    let result = tool_error_result(&error);
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.expect("structured error payload")["code"],
        "ship_run_in_flight"
    );
}

#[test]
fn friction_not_local_has_a_stable_code_and_names_owners() {
    let error = OrbitError::friction_not_local(
        "F2026-08-001",
        "ORB-00001",
        "ws_task",
        vec!["ws_friction".to_string()],
    );
    let payload = error_payload(&error);

    assert_eq!(payload["code"], "friction_not_local");
    assert_eq!(payload["friction_id"], "F2026-08-001");
    assert_eq!(payload["task_id"], "ORB-00001");
    assert_eq!(payload["workspace_id"], "ws_task");
    assert_eq!(payload["found_in"], json!(["ws_friction"]));
    assert!(payload["message"].as_str().is_some_and(|message| {
        message.contains("F2026-08-001")
            && message.contains("ws_task")
            && message.contains("ws_friction")
    }));
}

#[test]
fn control_conflict_has_stable_conflict_code_and_names_object() {
    for (error, expected_object, expected_hint) in [
        (
            OrbitError::JobRunControlConflict(
                "operation grant 'ogrant-123' is at revision 2, not the expected 1; reread and retry"
                    .to_string(),
            ),
            "operation grant 'ogrant-123'",
            "reread and retry",
        ),
        (
            OrbitError::JobRunControlConflict(
                "worker ceiling of job run 'jrun-456' is at revision 2, not 1; re-read it and decide again"
                    .to_string(),
            ),
            "worker ceiling of job run 'jrun-456'",
            "re-read it and decide again",
        ),
    ] {
        let payload = error_payload(&error);
        assert_eq!(payload["code"], "conflict");
        let message = payload["message"].as_str().expect("error message");
        assert!(!message.starts_with("job run control conflict:"));
        assert!(message.contains(expected_object), "missing object in message: {message}");
        assert!(message.contains(expected_hint), "missing hint in message: {message}");

        let result = tool_error_result(&error);
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.expect("structured error payload")["code"],
            "conflict"
        );
    }
}
