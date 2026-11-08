#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, TaskArtifact, TaskStatus,
};

use crate::isolated_cli_fixture::Fixture;

#[test]
fn decision_cli_warns_and_completes_when_a_task_artifact_is_missing() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_cli_warns_and_completes_when_a_task_artifact_is_missing",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let id = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(
            &fixture.root,
            Path::new(shown["orbit_root"].as_str().unwrap()),
        )
        .unwrap();
        let task = runtime
            .add_task(TaskAddParams {
                title: "Task with dangling evidence".into(),
                ..Default::default()
            })
            .unwrap();
        runtime
            .update_task_with_identity(
                &task.id,
                orbit_core::application::task::TaskUpdateParams {
                    upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                        "checks/owner-gates-after.log",
                        "passed",
                    )],
                    ..Default::default()
                },
                None,
                Some("codex".into()),
            )
            .unwrap();
        let file = runtime
            .get_task_artifact_manifest(&task.id)
            .unwrap()
            .remove(0);
        let bundle = fixture
            .root
            .join("tasks/workspaces")
            .join(runtime.workspace_id().unwrap())
            .join(&task.id);
        std::fs::remove_file(bundle.join("artifacts").join(file.blob)).unwrap();
        task.id
    };
    let listed = fixture.json(&["decision", "list", "--json"]);
    assert_eq!(listed["count"], 0);
    assert!(
        listed["warnings"][0].as_str().unwrap().contains(&id),
        "{listed}"
    );
    let output = fixture
        .command(&["decision", "list", "--format", "plain"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("warning:") && text.contains(&id), "{text}");
    fixture
        .command(&["task", "show", &id, "--json"])
        .assert()
        .failure();
}

/// Missing requests carry the same stable refusal code on both CLI verbs.
#[test]
fn decision_cli_missing_request_has_structured_code() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_cli_missing_request_has_structured_code",
    ) {
        return;
    }
    let fixture = Fixture::new();
    for args in [
        vec!["decision", "show", "missing-request", "--json"],
        vec!["decision", "answer", "missing-request", "approve", "--json"],
    ] {
        let output = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .failure()
            .get_output()
            .clone();
        let payload: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(payload["code"], "request_not_found", "{payload}");
    }
}

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
                    risk_tier: Default::default(),
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                    option: None,
                },
                DecisionAnswer {
                    risk_tier: Default::default(),
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                    option: None,
                },
            ],
            DecisionBinding::BlockEpisode {
                episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        let request_id = request.id.clone();
        let mut entry = request.history_entry("system");
        let mut legacy: serde_json::Value =
            serde_json::from_str(entry.note.as_ref().unwrap()).unwrap();
        legacy.as_object_mut().unwrap().remove("risk_tier");
        legacy.as_object_mut().unwrap().remove("category");
        for answer in legacy["answers"].as_array_mut().unwrap() {
            answer.as_object_mut().unwrap().remove("risk_tier");
        }
        entry.note = Some(legacy.to_string());
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    append_history: vec![entry],
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
    assert_eq!(shown["risk_tier"], "unknown");
    assert_eq!(shown["answers"][0]["risk_tier"], "unknown");
    assert!(shown["category"].is_null());
    assert_eq!(shown["answers"][0]["label"], "Drop criterion 2");

    // A process that declares an agent identity is refused before any
    // write: the request stays open and the task stays blocked.
    let before = fixture.json(&["task", "show", &task_id, "--json"]);
    fixture
        .command(&["decision", "answer", &request_id, "1", "--json"])
        .env("ORBIT_AGENT_NAME", "codex")
        .env("ORBIT_AGENT_MODEL", "gpt-6.1-sol")
        .assert()
        .failure();
    let still_open = fixture.json(&["decision", "show", &request_id, "--json"]);
    assert_eq!(still_open["state"], "open", "{still_open}");
    assert!(
        still_open["resolution"].is_null(),
        "a refused answer records no resolution: {still_open}"
    );
    let still_blocked = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(still_blocked["status"], "blocked", "{still_blocked}");
    assert_eq!(
        still_blocked["comments"], before["comments"],
        "a refused answer writes no comment"
    );

    // The operator (a scripted one uses the audited override) answers.
    let output = fixture
        .command(&[
            "decision",
            "answer",
            &request_id,
            "1",
            "--note",
            "Criterion 2 goes.",
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let answered: serde_json::Value = serde_json::from_slice(&output).unwrap();
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

/// An agent raises a multiple-choice request on a human-crew task through
/// the tool, the CLI lists it by topic, an agent identity cannot answer it,
/// and the operator's answer completes the task.
#[test]
fn decision_raise_cli_raises_lists_and_completes_on_answer() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_raise_cli_raises_lists_and_completes_on_answer",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let config_path = workspace_root.join("config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap_or_default();
    config.push_str("\n[crews.dk]\nkind = \"human\"\n");
    std::fs::write(&config_path, config).unwrap();

    let task_id = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("decision-fixture"));
        runtime
            .add_task(TaskAddParams {
                title: "Sign off the price".into(),
                crew: Some("dk".into()),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };

    let input = serde_json::json!({
        "model": "codex",
        "task_id": task_id,
        "question": "Which price do we ship?",
        "topic": "pricing/sign-off",
        "options": [
            {"label": "Keep it", "consequence": "No change.", "risk_tier": "routine"},
            {"label": "Raise 10%", "consequence": "Edit the price table.", "recommended": true, "risk_tier": "reversible"},
        ],
    })
    .to_string();
    let raised = fixture.json(&["tool", "run", "orbit.decision.raise", "--input", &input]);
    assert_eq!(raised["gate"], "agent_request", "{raised}");
    assert_eq!(raised["answers"][0]["label"], "Raise 10%", "{raised}");
    assert_eq!(raised["recommended"], "1", "{raised}");
    assert_eq!(raised["risk_tier"], "reversible", "{raised}");
    assert!(
        raised["category"]
            .as_str()
            .unwrap()
            .ends_with("/agent_request/pricing")
    );
    let request_id = raised["id"].as_str().unwrap().to_string();

    let listed = fixture.json(&["decision", "list", "--topic", "pricing/sign-off", "--json"]);
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["requests"][0]["risk_tier"], "reversible");
    let other = fixture.json(&["decision", "list", "--topic", "pricing", "--json"]);
    assert_eq!(other["count"], 0, "{other}");

    fixture
        .command(&["decision", "answer", &request_id, "1", "--json"])
        .env("ORBIT_AGENT_NAME", "codex")
        .env("ORBIT_AGENT_MODEL", "gpt-6.1-sol")
        .assert()
        .failure();
    let still_open = fixture.json(&["decision", "show", &request_id, "--json"]);
    assert_eq!(still_open["state"], "open", "{still_open}");
    assert_eq!(still_open["risk_tier"], "reversible");
    let open = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(open["open_decisions"][0]["risk_tier"], "reversible");
    assert_eq!(open["open_decisions"][0]["category"], raised["category"]);

    fixture
        .command(&["decision", "answer", &request_id, "1", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success();
    let task = fixture.json(&["task", "show", &task_id, "--json"]);
    assert_eq!(task["status"], "done", "{task}");
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

    let (request_id, token, fresh_request_id, fresh_token, fresh_task_id) = {
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
                    risk_tier: Default::default(),
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                    option: None,
                },
                DecisionAnswer {
                    risk_tier: Default::default(),
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                    option: None,
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
        runtime
            .register_decision_channel("email", "human:operator", "decision-mail", vec![])
            .unwrap();
        let token = runtime.mint_decision_token("email", &request_id).unwrap();

        // A second, still-outstanding request on its own blocked task:
        // the revoke check below submits its fresh token, so only a
        // working revocation (not the first token's consumption) can
        // refuse it.
        let fresh_task = runtime
            .add_task(TaskAddParams {
                title: "Blocked awaiting a revoked channel".into(),
                description: "decision channel revoke fixture".into(),
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            })
            .unwrap();
        let fresh_history = runtime.get_task_history(&fresh_task.id).unwrap();
        let fresh_blocked_at = fresh_history
            .iter()
            .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .expect("seeded blocked task records its block")
            .at;
        let fresh_request = DecisionRequest::new(
            fresh_task.id.to_string(),
            DecisionGate::BlockTriage,
            vec![
                DecisionAnswer {
                    risk_tier: Default::default(),
                    id: "1".to_string(),
                    label: "Drop criterion 2".to_string(),
                    consequence: "the old API stays retired".to_string(),
                    option: None,
                },
                DecisionAnswer {
                    risk_tier: Default::default(),
                    id: "dismiss".to_string(),
                    label: "Dismiss".to_string(),
                    consequence: "Leave blocked, stop asking for this episode.".to_string(),
                    option: None,
                },
            ],
            DecisionBinding::BlockEpisode {
                episode: fresh_blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
            },
            "block_triage".to_string(),
            chrono::Utc::now(),
        );
        let fresh_request_id = fresh_request.id.clone();
        runtime
            .apply_task_automation_update(
                &fresh_task.id,
                TaskAutomationUpdate {
                    append_history: vec![fresh_request.history_entry("system")],
                    ..Default::default()
                },
            )
            .expect("record the fresh decision request");
        let fresh_token = runtime
            .mint_decision_token("email", &fresh_request_id)
            .unwrap();
        (
            request_id,
            token,
            fresh_request_id,
            fresh_token,
            fresh_task.id.to_string(),
        )
    };

    let channels = fixture.json(&["decision", "channel", "list", "--json"]);
    assert_eq!(channels[0]["channel"], "email", "{channels}");
    assert_eq!(channels[0]["human"], "human:operator");
    assert_eq!(channels[0]["epoch"], 1);
    assert_eq!(
        channels[0]["require_dkim_verification"], false,
        "{channels}"
    );
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
        "--require-dkim-verification",
        "--json",
    ]);
    assert_eq!(registered["channel"], "pager", "{registered}");
    assert_eq!(
        registered["require_dkim_verification"], true,
        "{registered}"
    );
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

    // A malformed token is a terminal credential refusal, distinguishable
    // from a missing request or a retryable verifier outage.
    let invalid = fixture
        .command(&[
            "decision",
            "submit",
            &request_id,
            "1",
            "--channel",
            "email",
            "--token",
            "invalid-token",
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let invalid: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(invalid["code"], "token_invalid", "{invalid}");

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

    let replay = fixture
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
        .failure()
        .get_output()
        .clone();
    let replay: serde_json::Value = serde_json::from_slice(&replay.stderr).unwrap();
    assert_eq!(replay["code"], "token_consumed", "{replay}");

    // Revocation refuses the next submission through the channel. Use
    // the fresh outstanding request: its token was never consumed, so
    // only a working CLI revoke can refuse it, as `channel_revoked`,
    // leaving its task and request untouched.
    let before_task = fixture.json(&["task", "show", &fresh_task_id, "--json"]);
    assert_eq!(before_task["status"], "blocked", "{before_task}");
    fixture.json(&["decision", "channel", "revoke", "email", "--json"]);
    let revoked = fixture
        .command(&[
            "decision",
            "submit",
            &fresh_request_id,
            "dismiss",
            "--channel",
            "email",
            "--token",
            &fresh_token,
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let revoked: serde_json::Value = serde_json::from_slice(&revoked.stderr).unwrap();
    assert_eq!(revoked["code"], "channel_revoked", "{revoked}");
    assert_eq!(
        fixture.json(&["task", "show", &fresh_task_id, "--json"])["status"],
        "blocked",
        "a revoked submission leaves its task unchanged"
    );
    let fresh_shown = fixture.json(&["decision", "show", &fresh_request_id, "--json"]);
    assert_eq!(fresh_shown["state"], "open", "{fresh_shown}");
}

/// `orbit decision digest` over the real CLI: the operator's call raises
/// the proposed task's approval request through the governed tool, a
/// second call raises nothing new, and an agent-declared process is
/// refused before anything is raised.
#[test]
fn decision_digest_cli_raises_and_refuses_agents() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_digest_cli_raises_and_refuses_agents",
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

    let task_id = {
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
                title: "Proposed awaiting the digest".into(),
                description: "decision digest CLI fixture".into(),
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .unwrap();
        runtime
            .register_decision_channel("email", "human:operator", "decision-mail", vec![])
            .unwrap();
        task.id.to_string()
    };

    // An agent-declared process is refused and raises nothing.
    fixture
        .command(&["decision", "digest", "--channel", "email", "--json"])
        .env("ORBIT_AGENT_NAME", "codex")
        .env("ORBIT_AGENT_MODEL", "gpt-6.1-sol")
        .assert()
        .failure();
    let listed = fixture.json(&["decision", "list", "--json"]);
    assert_eq!(
        listed["count"], 0,
        "the refused digest raised nothing: {listed}"
    );

    let output = fixture
        .command(&["decision", "digest", "--channel", "email", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let digest: serde_json::Value = serde_json::from_slice(&output).unwrap();
    // The fixture workspace may carry its own proposed seed task, so
    // assert on this task's request rather than a workspace count.
    let raised = digest["raised"]
        .as_array()
        .unwrap()
        .iter()
        .find(|request| request["task_id"] == task_id)
        .expect("the digest raised this task's approval request")
        .clone();
    assert_eq!(raised["gate"], "proposed_approval");
    assert_eq!(raised["raised_by"], "digest");
    assert_eq!(raised["state"], "open");
    assert_eq!(raised["answers"][0]["id"], "approve");
    assert_eq!(raised["answers"][1]["id"], "reject");
    let request_id = raised["id"].as_str().unwrap().to_string();
    assert!(
        digest["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["id"] == request_id),
        "the digest carries the raised request: {digest}"
    );

    let again: serde_json::Value = serde_json::from_slice(
        &fixture
            .command(&["decision", "digest", "--channel", "email", "--json"])
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert_eq!(again["raised"].as_array().unwrap().len(), 0, "{again}");
    assert!(
        again["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["id"] == request_id),
        "the second digest returns the same request id: {again}"
    );
}

/// A refused governed operation reports once: stderr carries only the CLI
/// error (plain or `--json`), never the raw `orbit.authorization` tracing
/// record, and an operator-override `--json` run leaves stderr clean while
/// stdout JSON is unchanged [ORB-18450].
#[test]
fn governed_denial_and_override_stay_off_stderr() {
    if crate::isolated_test::run_in_child(
        "decision_cli::governed_denial_and_override_stay_off_stderr",
    ) {
        return;
    }

    let fixture = Fixture::new();

    // Refused `--json`: stdout stays empty and stderr is exactly the JSON
    // error document.
    let denied = fixture
        .command(&["decision", "answer", "nope", "approve", "--json"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        denied.stdout.is_empty(),
        "a refused --json run writes no stdout: {}",
        String::from_utf8_lossy(&denied.stdout)
    );
    let stderr = String::from_utf8_lossy(&denied.stderr);
    assert!(
        !stderr.contains("orbit.authorization"),
        "no raw tracing record on stderr: {stderr}"
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&denied.stderr).expect("stderr is the JSON error");
    assert_eq!(payload["code"], "capability_denied", "{payload}");

    // Refused plain output: the same single human-readable error.
    let plain = fixture
        .command(&["decision", "digest", "--channel", "nope"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let plain_stderr = String::from_utf8_lossy(&plain.stderr);
    assert!(
        !plain_stderr.contains("orbit.authorization"),
        "no raw tracing record on stderr: {plain_stderr}"
    );
    assert!(
        plain_stderr.starts_with("error: capability denied:"),
        "the denial is shown once as the CLI error: {plain_stderr}"
    );

    // Operator override with `--json`: stdout JSON unchanged, stderr clean.
    let allowed = fixture
        .command(&["decision", "digest", "--channel", "nope", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .clone();
    let allowed_stderr = String::from_utf8_lossy(&allowed.stderr);
    assert!(
        !allowed_stderr.contains("orbit.authorization"),
        "no raw tracing record on override stderr: {allowed_stderr}"
    );
    assert!(
        allowed_stderr.trim().is_empty(),
        "override stderr stays clean at the default level: {allowed_stderr}"
    );
    let digest: serde_json::Value = serde_json::from_slice(&allowed.stdout).unwrap();
    assert_eq!(digest["count"], 0, "{digest}");
}

/// `orbit decision show` on a `script_grant` request prints what an answer
/// consents to (script, digests, refusal, capabilities and the diff), and
/// `orbit decision answer <id> grant` writes the grant bound to that digest.
#[test]
fn decision_cli_shows_and_answers_a_script_grant_request() {
    if crate::isolated_test::run_in_child(
        "decision_cli::decision_cli_shows_and_answers_a_script_grant_request",
    ) {
        return;
    }
    let fixture = Fixture::new();
    // Run the script job hermetically: `orbit init` seeds `default_crew`
    // from the host's detected CLIs (opus here), which CI runners leave
    // disabled, so `run_job_v2` would refuse before reaching the grant gate.
    crate::fixture_crew::configure_sol(&fixture.root);
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap()).to_path_buf();
    let repo = fixture.repo.clone();
    std::fs::create_dir_all(repo.join("tools")).unwrap();
    std::fs::write(repo.join("tools/report.sh"), "#!/bin/sh\necho '{}'\n").unwrap();
    // This fixture's orbit root sits outside the repo: the catalog reads the
    // root's manifests and a step snapshot the checkout's, so write both.
    for scripts in [
        workspace_root.join("resources/scripts"),
        repo.join(".orbit/resources/scripts"),
    ] {
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("report.yaml"),
            "schemaVersion: 1\nkind: Script\nmetadata:\n  name: report\nspec:\n  description: Fixture report.\n  entry: tools/report.sh\n",
        )
        .unwrap();
    }
    std::fs::write(
        repo.join("sweep.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: sweep\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: run\n      target: script:report\n",
    )
    .unwrap();
    let runtime = || {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        OrbitRuntime::from_roots(&fixture.root, &workspace_root).unwrap()
    };
    let refused = format!(
        "{:?}",
        runtime().run_job_v2_from_yaml(&repo.join("sweep.yaml"), serde_json::json!({}))
    );
    assert!(refused.contains("grant_missing"), "{refused}");

    let listed = fixture.json(&["decision", "list", "--json"]);
    let request = &listed["requests"][0];
    assert_eq!(request["gate"], "script_grant", "{listed}");
    let request_id = request["id"].as_str().unwrap().to_string();
    let digest = request["script_grant"]["digest"]
        .as_str()
        .unwrap()
        .to_string();

    let output = fixture
        .command(&["decision", "show", &request_id])
        .assert()
        .success();
    let plain = String::from_utf8_lossy(&output.get_output().stdout).to_string();
    for expected in [
        "script: report (entry tools/report.sh)".to_string(),
        format!("digest: {digest} (granted: none)"),
        "refusal: grant_missing".to_string(),
        "+echo '{}'".to_string(),
    ] {
        assert!(plain.contains(&expected), "want {expected:?}: {plain}");
    }

    fixture
        .command(&["decision", "answer", &request_id, "grant", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success();
    let grant = runtime()
        .verified_script_grant("report")
        .unwrap()
        .expect("the answer wrote the grant");
    assert_eq!(grant.digest, digest);
    assert_eq!(grant.provenance, "cli");
}

/// An envelope has no description/criteria. Checking a review request against
/// that envelope incorrectly supersedes its full-spec revision binding.
#[test]
fn attention_preserves_review_decisions_bound_to_nonempty_requirements() {
    if crate::isolated_test::run_in_child(
        "decision_cli::attention_preserves_review_decisions_bound_to_nonempty_requirements",
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
                title: "Review with requirements".into(),
                description: "The completed candidate still needs a human decision.".into(),
                acceptance_criteria: vec!["Keep the recorded requirements in the binding".into()],
                status: Some(TaskStatus::Review),
                ..Default::default()
            })
            .unwrap();
        let request = DecisionRequest::new(
            task.id.clone(),
            DecisionGate::ReviewApproval,
            DecisionGate::ReviewApproval.approval_answers(),
            DecisionBinding::for_task(&task, None),
            "digest".into(),
            chrono::Utc::now(),
        );
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    append_history: vec![request.history_entry("system")],
                    ..Default::default()
                },
            )
            .unwrap();
        (task.id, request.id)
    };
    let listed = fixture.json(&["decision", "list", "--json"]);
    assert!(
        listed["requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|request| request["id"] == request_id)
    );
    let output = fixture
        .command(&[
            "attention",
            "--kind",
            "decision_open",
            "--task",
            &task_id,
            "--json",
        ])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let attention: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        attention["counts"]["returned"], 1,
        "attention must check effective review decisions against the full requirements, as decision list does: {attention}"
    );
    assert_eq!(attention["items"][0]["refs"]["decision_id"], request_id);
    assert_eq!(attention["items"][0]["next"]["tool"], "orbit.decision.show");
}

/// An unidentified CLI caller (no TTY, no agent declaration, no override) is
/// not refused on `orbit resident grant`: its widening raises the
/// `resident_authority` request the reference documents and changes no
/// authority, and the operator override still applies a widening at once.
#[test]
fn resident_grant_from_an_unidentified_cli_caller_raises_a_request() {
    if crate::isolated_test::run_in_child(
        "decision_cli::resident_grant_from_an_unidentified_cli_caller_raises_a_request",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let task_id = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("decision-fixture"));
        let id = runtime
            .add_task(TaskAddParams {
                title: "[orchestration]: resident grant fixture".into(),
                tags: vec!["resident".into()],
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        let mandate = serde_json::json!({
            "schema": "resident.mandate/v1",
            "goal": "Exercise the grant request path",
            "exit_criteria": ["The request is answered"],
            "deadline": "2099-01-01T00:00:00Z",
            "workspaces": [runtime.workspace_id().unwrap()],
            "crew": "sol",
            "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
        });
        runtime
            .update_task_as_human(
                &id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        "resident/mandate.json",
                        mandate.to_string(),
                    )],
                    ..Default::default()
                },
                "human:decision-fixture".into(),
            )
            .unwrap();
        id
    };

    let requested = fixture.json(&[
        "resident",
        "grant",
        &task_id,
        "--add",
        "merge",
        "--quote",
        "Merge reviewed release pull requests.",
        "--json",
    ]);
    assert_eq!(requested["outcome"], "requested", "{requested}");
    let held = |value: &serde_json::Value| value["authority"]["rights"].clone();
    assert_eq!(
        held(&requested),
        serde_json::json!([]),
        "a request grants nothing: {requested}"
    );
    let listed = fixture.json(&["decision", "list", "--gate", "resident_authority", "--json"]);
    assert!(
        listed.to_string().contains(&task_id),
        "the request is open for the human: {listed}"
    );

    // The operator override still widens at once.
    let granted = fixture
        .command(&["resident", "grant", &task_id, "--add", "merge", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let granted: serde_json::Value = serde_json::from_slice(&granted).unwrap();
    assert_eq!(granted["outcome"], "granted", "{granted}");
}
