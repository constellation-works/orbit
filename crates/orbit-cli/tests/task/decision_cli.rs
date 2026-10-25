#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, TaskStatus,
};

use crate::isolated_cli_fixture::Fixture;

/// `orbit decision list|show|answer` over a raised block-triage request:
/// the operator's answer lands as a human write, the task returns to
/// backlog, the request reads answered, and a second answer is refused.
/// The read-only `orbit.decision.list` tool sees the same request; the
/// answer verb is not a callable tool.
#[test]
fn decision_cli_lists_shows_and_answers_a_raised_request() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_cli_lists_shows_and_answers_a_raised_request",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let (task_id, request_id) = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("decision-fixture"));
        let task = runtime
            .add_task(TaskAddParams {
                title: "Blocked awaiting a decision".into(),
                description: "decision CLI fixture".into(),
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            })
            .unwrap();
        let history = runtime.get_task_history(&task.id).unwrap();
        let blocked_at = history
            .iter()
            .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .expect("seeded blocked task records its block")
            .at;
        let request = DecisionRequest::new(
            task.id.to_string(),
            DecisionGate::BlockTriage,
            vec![
                DecisionAnswer {
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                },
                DecisionAnswer {
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                },
            ],
            DecisionBinding::BlockEpisode {
                episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        let request_id = request.id.clone();
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    append_history: vec![request.history_entry("system")],
                    ..Default::default()
                },
            )
            .expect("record the decision request");
        // The proposal comment that raised the request carries the option's
        // target and replacement, which an answer applies.
        runtime
            .force_update_task_with_identity(
                &task.id,
                orbit_core::application::task::TaskUpdateParams {
                    comment: Some(
                        "blocked_task_triage disposition=decision outcome=proposed\n\nOption 1: Drop criterion 2\nConsequences: the old API stays retired\nApplies to: acceptance_criteria\nReplacement text:\n- Recovered.\n\nRecommended: option 1 (Drop criterion 2)"
                            .to_string(),
                    ),
                    ..Default::default()
                },
                None,
                None,
            )
            .expect("record the proposal comment");

        (task.id.to_string(), request_id)
    };

    let listed = fixture.json(&["decision", "list", "--json"]);
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["requests"][0]["id"], request_id);
    assert_eq!(listed["requests"][0]["state"], "open");

    let tool_input = serde_json::json!({"model": "codex", "task_id": task_id}).to_string();
    let tool = fixture.json(&["tool", "run", "orbit.decision.list", "--input", &tool_input]);
    assert_eq!(tool["requests"][0]["id"], request_id, "{tool}");

    let shown = fixture.json(&["decision", "show", &request_id, "--json"]);
    assert_eq!(shown["gate"], "block_triage", "{shown}");
    assert_eq!(shown["answers"][0]["label"], "Drop criterion 2");

    let answered = fixture.json(&[
        "decision",
        "answer",
        &request_id,
        "1",
        "--note",
        "Criterion 2 goes.",
        "--json",
    ]);
    assert_eq!(answered["state"], "answered", "{answered}");
    assert_eq!(answered["resolution"]["answer_id"], "1");

    let task = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(task["status"], "backlog", "{task}");
    assert!(
        task["open_decisions"].is_null(),
        "the answered request leaves open_decisions: {task}"
    );

    let listed = fixture.json(&["decision", "list", "--json"]);
    assert_eq!(listed["count"], 0, "no open requests remain: {listed}");
    let all = fixture.json(&["decision", "list", "--all", "--json"]);
    assert_eq!(all["requests"][0]["state"], "answered", "{all}");

    fixture
        .command(&["decision", "answer", &request_id, "1"])
        .assert()
        .failure();
    // Answering is not a tool an agent session can call.
    fixture
        .command(&[
            "tool",
            "run",
            "orbit.decision.answer",
            "--input",
            &serde_json::json!({"id": request_id, "answer": "1"}).to_string(),
        ])
        .assert()
        .failure();
}

/// The delegated-human channel over the real CLI: the operator registers
/// a channel, submits an answer with its per-request token through the
/// governed tool (the path a channel plugin uses), and revocation then
/// refuses further submissions. Channel administration is not a tool.
#[test]
fn decision_channel_cli_registers_submits_and_revokes() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_channel_cli_registers_submits_and_revokes",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let config_path = workspace_root.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap_or_default();
    config.push_str("\n[decision_channel]\nenabled = true\n");
    std::fs::write(&config_path, config).unwrap();

    let (request_id, token) = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("decision-fixture"));
        let task = runtime
            .add_task(TaskAddParams {
                title: "Blocked awaiting a channel decision".into(),
                description: "decision channel CLI fixture".into(),
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            })
            .unwrap();
        let history = runtime.get_task_history(&task.id).unwrap();
        let blocked_at = history
            .iter()
            .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .expect("seeded blocked task records its block")
            .at;
        let request = DecisionRequest::new(
            task.id.to_string(),
            DecisionGate::BlockTriage,
            vec![
                DecisionAnswer {
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                },
                DecisionAnswer {
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                },
            ],
            DecisionBinding::BlockEpisode {
                episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        let request_id = request.id.clone();
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    append_history: vec![request.history_entry("system")],
                    ..Default::default()
                },
            )
            .expect("record the decision request");
        runtime
            .register_decision_channel("email", "human:operator", "decision-mail", vec![])
            .unwrap();
        let token = runtime.mint_decision_token("email", &request_id).unwrap();
        (request_id, token)
    };

    let channels = fixture.json(&["decision", "channel", "list", "--json"]);
    assert_eq!(channels[0]["channel"], "email", "{channels}");
    assert_eq!(channels[0]["human"], "human:operator");
    assert_eq!(channels[0]["epoch"], 1);
    assert!(
        channels[0].get("key_hex").is_none(),
        "listings never carry key material: {channels}"
    );

    // Registering a second channel from the CLI works; administering
    // channels through the tool surface does not.
    let registered = fixture.json(&[
        "decision",
        "channel",
        "register",
        "pager",
        "--human",
        "human:operator",
        "--plugin",
        "decision-mail",
        "--json",
    ]);
    assert_eq!(registered["channel"], "pager", "{registered}");
    // Registering from an agent envelope is refused: no running code can
    // mint itself a human.
    fixture
        .command(&[
            "decision",
            "channel",
            "register",
            "agent-channel",
            "--human",
            "human:operator",
            "--plugin",
            "decision-mail",
        ])
        .env("ORBIT_AGENT_NAME", "muse")
        .assert()
        .failure();
    fixture
        .command(&[
            "tool",
            "run",
            "orbit.decision.channel_register",
            "--input",
            &serde_json::json!({"channel": "sms", "human": "human:operator", "plugin": "decision-mail"})
                .to_string(),
        ])
        .assert()
        .failure();

    // The operator tests the channel through the governed submit; a
    // scripted (non-terminal) operator uses the audited override, like
    // every other governed operation.
    let output = fixture
        .command(&[
            "decision",
            "submit",
            &request_id,
            "1",
            "--channel",
            "email",
            "--token",
            &token,
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let submitted: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(submitted["state"], "answered", "{submitted}");
    assert_eq!(submitted["resolution"]["answered_by"], "human:operator");
    assert!(
        submitted["resolution"]["provenance"]
            .as_str()
            .unwrap()
            .starts_with("decision-channel email (request "),
        "{submitted}"
    );

    // Revocation refuses the next submission through the channel.
    fixture.json(&["decision", "channel", "revoke", "email", "--json"]);
    fixture
        .command(&[
            "decision",
            "submit",
            &request_id,
            "dismiss",
            "--channel",
            "email",
            "--token",
            &token,
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure();
}
