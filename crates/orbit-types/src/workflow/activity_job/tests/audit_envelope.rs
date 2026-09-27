use serde_json::json;

use super::super::audit_envelope::*;
use super::super::tool_allowlist::ActivityToolPolicyMode;

#[test]
fn tool_allowlist_audit_records_requested_and_effective_lists_compatibly() {
    let encoded = serde_json::to_value(V2AuditEventKind::ToolAllowlistHarnessDelegated {
        provider: "codex".to_string(),
        task_id: Some("ORB-11069".to_string()),
        task_ids: vec!["ORB-11069".to_string(), "ORB-11070".to_string()],
        requested_tools: vec!["github.run.list".to_string()],
        effective_tools: vec!["orbit.task.show".to_string(), "github.run.list".to_string()],
        tools: vec!["orbit.task.show".to_string(), "github.run.list".to_string()],
        tool_policy: None,
        tool_disallow_list: None,
    })
    .expect("serialize tool allowlist audit");
    assert_eq!(encoded["task_id"], "ORB-11069");
    assert_eq!(encoded["task_ids"], json!(["ORB-11069", "ORB-11070"]));
    assert_eq!(encoded["requested_tools"], json!(["github.run.list"]));
    assert_eq!(
        encoded["effective_tools"],
        json!(["orbit.task.show", "github.run.list"])
    );
    assert_eq!(encoded["tools"], encoded["effective_tools"]);

    let decoded: V2AuditEventKind = serde_json::from_value(json!({
        "body_kind": "tool_allowlist_harness_delegated",
        "provider": "codex",
        "tools": ["orbit.task.show"]
    }))
    .expect("deserialize legacy tool allowlist audit");
    assert!(matches!(
        decoded,
        V2AuditEventKind::ToolAllowlistHarnessDelegated {
            task_id: None,
            task_ids,
            requested_tools,
            effective_tools,
            tools,
            ..
        } if task_ids.is_empty()
            && requested_tools.is_empty()
            && effective_tools.is_empty()
            && tools == ["orbit.task.show"]
    ));
}

#[test]
fn step_finished_error_message_round_trips_and_absence_defaults_to_none() {
    let encoded = serde_json::to_value(V2AuditEventKind::StepFinished {
        step_id: "plan".to_string(),
        outcome: "error".to_string(),
        error_message: Some("dispatch failed".to_string()),
    })
    .expect("serialize step finished");

    assert_eq!(encoded["error_message"], "dispatch failed");
    let decoded: V2AuditEventKind =
        serde_json::from_value(encoded).expect("deserialize step finished");
    assert!(matches!(
        decoded,
        V2AuditEventKind::StepFinished {
            step_id,
            outcome,
            error_message: Some(message)
        } if step_id == "plan" && outcome == "error" && message == "dispatch failed"
    ));

    let decoded: V2AuditEventKind = serde_json::from_value(json!({
        "body_kind": "step_finished",
        "step_id": "plan",
        "outcome": "error"
    }))
    .expect("deserialize legacy step finished");
    assert!(matches!(
        decoded,
        V2AuditEventKind::StepFinished {
            error_message: None,
            ..
        }
    ));
}

#[test]
fn cli_invocation_process_round_trips_with_and_without_identity_token() {
    let encoded = serde_json::to_value(V2AuditEventKind::CliInvocationProcess {
        provider: "codex".to_string(),
        pid: 4242,
        pid_start_time: Some("ps-lstart-utc-v1:Mon Jul 27 02:41:00 2026".to_string()),
    })
    .expect("serialize cli invocation process");

    assert_eq!(encoded["body_kind"], "cli_invocation_process");
    assert_eq!(encoded["pid"], 4242);
    assert_eq!(
        encoded["pid_start_time"],
        "ps-lstart-utc-v1:Mon Jul 27 02:41:00 2026"
    );

    let decoded: V2AuditEventKind =
        serde_json::from_value(encoded).expect("deserialize cli invocation process");
    assert_eq!(decoded.event_type(), "cli.invocation.process");
    assert!(matches!(
        decoded,
        V2AuditEventKind::CliInvocationProcess {
            provider,
            pid: 4242,
            pid_start_time: Some(_)
        } if provider == "codex"
    ));

    // A host that cannot probe process start identity (non-Unix, or a sandbox
    // that blocks `ps`) still records a usable PID.
    let encoded = serde_json::to_value(V2AuditEventKind::CliInvocationProcess {
        provider: "claude".to_string(),
        pid: 7,
        pid_start_time: None,
    })
    .expect("serialize unprobed cli invocation process");
    assert!(encoded.get("pid_start_time").is_none());

    let decoded: V2AuditEventKind = serde_json::from_value(json!({
        "body_kind": "cli_invocation_process",
        "provider": "claude",
        "pid": 7
    }))
    .expect("deserialize cli invocation process without token");
    assert!(matches!(
        decoded,
        V2AuditEventKind::CliInvocationProcess {
            pid: 7,
            pid_start_time: None,
            ..
        }
    ));
}

#[test]
fn run_finished_error_message_round_trips_and_absence_defaults_to_none() {
    let encoded = serde_json::to_value(V2AuditEventKind::RunFinished {
        outcome: "error".to_string(),
        error_message: Some("job failed".to_string()),
    })
    .expect("serialize run finished");

    assert_eq!(encoded["error_message"], "job failed");
    let decoded: V2AuditEventKind =
        serde_json::from_value(encoded).expect("deserialize run finished");
    assert!(matches!(
        decoded,
        V2AuditEventKind::RunFinished {
            outcome,
            error_message: Some(message)
        } if outcome == "error" && message == "job failed"
    ));

    let encoded = serde_json::to_value(V2AuditEventKind::RunFinished {
        outcome: "success".to_string(),
        error_message: None,
    })
    .expect("serialize successful run finished");
    assert!(encoded.get("error_message").is_none());

    let decoded: V2AuditEventKind = serde_json::from_value(json!({
        "body_kind": "run_finished",
        "outcome": "success"
    }))
    .expect("deserialize legacy run finished");
    assert!(matches!(
        decoded,
        V2AuditEventKind::RunFinished {
            error_message: None,
            ..
        }
    ));
}

/// [ORB-13315] The harness event records a deny-mode policy and its list, and
/// both fields stay absent from an allowlist-mode record that omits them.
#[test]
fn tool_allowlist_audit_records_policy_mode_and_disallow_list() {
    let deny = V2AuditEventKind::ToolAllowlistHarnessDelegated {
        provider: "claude".to_string(),
        task_id: None,
        task_ids: Vec::new(),
        requested_tools: Vec::new(),
        effective_tools: vec!["orbit.search".to_string()],
        tools: vec!["orbit.search".to_string()],
        tool_policy: Some(ActivityToolPolicyMode::Deny),
        tool_disallow_list: Some(vec!["orbit.task.*".to_string()]),
    };
    let encoded = serde_json::to_value(deny).expect("serialize deny-mode audit");
    assert_eq!(encoded["tool_policy"], "deny");
    assert_eq!(encoded["tool_disallow_list"], json!(["orbit.task.*"]));
    let decoded: V2AuditEventKind = serde_json::from_value(encoded).expect("round trip");
    assert!(matches!(
        decoded,
        V2AuditEventKind::ToolAllowlistHarnessDelegated {
            tool_policy: Some(ActivityToolPolicyMode::Deny),
            tool_disallow_list: Some(list),
            ..
        } if list == ["orbit.task.*"]
    ));

    let legacy = serde_json::to_value(V2AuditEventKind::ToolAllowlistHarnessDelegated {
        provider: "claude".to_string(),
        task_id: None,
        task_ids: Vec::new(),
        requested_tools: Vec::new(),
        effective_tools: Vec::new(),
        tools: Vec::new(),
        tool_policy: None,
        tool_disallow_list: None,
    })
    .expect("serialize allowlist audit");
    assert!(legacy.get("tool_policy").is_none());
    assert!(legacy.get("tool_disallow_list").is_none());
}
