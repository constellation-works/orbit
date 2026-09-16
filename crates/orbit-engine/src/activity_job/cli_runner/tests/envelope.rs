#![allow(missing_docs)]

use std::collections::HashMap;
use std::time::Duration;

use orbit_agent::ParsedStdout;
use orbit_store::{InvocationInsertParams, InvocationQuery, Store};
use orbit_types::telemetry::TokenUsage;
use serde_json::Value;

use super::super::envelope::{
    cli_agent_envelope_json, parse_cli_invocation_trace_from, task_id_from_input,
    task_ids_from_input, user_prompt_from_input,
};
use super::test_support::{TestHost, test_agent_loop_spec};

#[test]
fn user_prompt_from_object_input_without_prompt_serializes_full_input() {
    let input = serde_json::json!({
        "failed_step_id": "push",
        "activity_name": "git_push",
        "error_message": "network timeout",
        "attempt": 2,
        "max_attempts": 2,
    });

    let prompt = user_prompt_from_input(&input).expect("prompt serializes");
    let parsed: serde_json::Value = serde_json::from_str(&prompt).expect("prompt is json");

    assert_eq!(parsed, input);
}

#[test]
fn user_prompt_from_object_input_prefers_explicit_prompt() {
    let prompt = user_prompt_from_input(&serde_json::json!({
        "prompt": "do only this",
        "failed_step_id": "push",
    }))
    .expect("prompt resolves");

    assert_eq!(prompt, "do only this");
}

#[test]
fn cli_agent_envelope_carries_input_run_id_and_task_context() {
    let host = TestHost {
        command: "codex".to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "id": "TCTX",
            "workspace_path": "/tmp/orbit-worktree",
            "plan": "implement it"
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.instruction = "perform the requested task".to_string();
    let input = serde_json::json!({
        "prompt": "do it",
        "task_id": "TCTX",
        "workspace_path": "/tmp/orbit-worktree"
    });

    let raw = cli_agent_envelope_json(
        &spec,
        "jrun-context",
        &input,
        host.task_context.as_ref(),
        &["github.run.list".to_string()],
        &["orbit.task.show".to_string(), "github.run.list".to_string()],
    )
    .expect("build cli agent envelope");
    let envelope: Value = serde_json::from_slice(&raw).expect("parse envelope json");

    assert_eq!(envelope["schemaVersion"], 1);
    assert_eq!(envelope["prompt"], "do it");
    assert_eq!(envelope["run_id"], "jrun-context");
    assert_eq!(envelope["input"]["task_id"], "TCTX");
    assert_eq!(envelope["input"]["workspace_path"], "/tmp/orbit-worktree");
    assert_eq!(envelope["task"]["id"], "TCTX");
    assert_eq!(envelope["task"]["workspace_path"], "/tmp/orbit-worktree");
    assert_eq!(envelope["instruction"], "perform the requested task");
    assert_eq!(
        envelope["required_tools"],
        serde_json::json!(["github.run.list"])
    );
    assert_eq!(
        envelope["tools"],
        serde_json::json!(["orbit.task.show", "github.run.list"])
    );
    assert!(
        envelope.get("response_schema").is_none(),
        "the provider renderer owns response framing; the embedded task envelope must not duplicate it"
    );
}

#[test]
fn task_id_from_input_reads_common_activity_shapes() {
    assert_eq!(
        task_id_from_input(&serde_json::json!({"task_id": "T1"})),
        Some("T1")
    );
    assert_eq!(
        task_id_from_input(&serde_json::json!({"task": {"id": "T2"}})),
        Some("T2")
    );
    assert_eq!(
        task_id_from_input(&serde_json::json!({"task_ids": ["T3", "T4"]})),
        Some("T3")
    );
    assert_eq!(task_id_from_input(&serde_json::json!({})), None);
}

#[test]
fn task_ids_from_input_reads_batched_activity_shapes_without_duplicates() {
    assert_eq!(
        task_ids_from_input(&serde_json::json!({"task_ids": ["T3", "T4", "T3"]})),
        vec!["T3", "T4"]
    );
    assert_eq!(
        task_ids_from_input(&serde_json::json!({
            "task_ids": [],
            "candidates": [
                {"task_id": "T5"},
                {"task_id": "T6"},
                {"task_id": "T5"}
            ]
        })),
        vec!["T5", "T6"]
    );
    assert_eq!(
        task_ids_from_input(&serde_json::json!({
            "task_id": "T1",
            "task_ids": ["not-selected-by-this-shape"]
        })),
        vec!["T1"]
    );
}

#[test]
fn parse_cli_invocation_trace_extracts_gemini_cli_stats_tokens() {
    let stdout = serde_json::json!({
        "result": {
            "schemaVersion": 1,
            "status": "success",
            "result": {}
        },
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "tokens": {
                        "input": 12,
                        "cached": 3,
                        "candidates": 4,
                        "total": 19
                    },
                    "roles": {
                        "user": {
                            "tokens": {
                                "input": 12,
                                "cached": 3
                            }
                        },
                        "model": {
                            "tokens": {
                                "candidates": 4
                            }
                        }
                    }
                }
            }
        }
    })
    .to_string();

    assert_eq!(
        parse_cli_invocation_trace_from(&ParsedStdout::parse(&stdout), b"", Some(0), 99, true)
            .map(|trace| trace.usage),
        Some(TokenUsage {
            input: 12,
            cache_read: 3,
            cache_create: 0,
            cache_create_1h: 0,
            output: 4,
        })
    );
}

#[test]
fn claude_cache_creation_ttl_split_ingests_at_the_one_hour_rate() {
    // Ground truth from worker run 91d7ef01. This exercises the production
    // Claude CLI wrapper parser and then persists its trace through the
    // invocation store, rather than constructing a pre-split TokenUsage.
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{}}",
        "total_cost_usd": 1.014_018,
        "modelUsage": {
            "claude-opus-4-8[1m]": {
                "costUSD": 1.014_018,
                "canonicalModel": "claude-opus-4-8"
            }
        },
        "usage": {
            "input_tokens": 36,
            "output_tokens": 8_265,
            "cache_read_input_tokens": 858_526,
            "cache_creation_input_tokens": 37_795,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 0,
                "ephemeral_1h_input_tokens": 37_795,
            }
        }
    })
    .to_string();
    let trace =
        parse_cli_invocation_trace_from(&ParsedStdout::parse(&stdout), b"", Some(0), 99, true)
            .expect("Claude CLI envelope parses");
    assert_eq!(trace.provider_model.as_deref(), Some("claude-opus-4-8[1m]"));
    assert_eq!(trace.provider_cost_usd, Some(1.014_018));
    let store = Store::open_in_memory().expect("open store");

    store
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-91d7ef01".to_string(),
            activity_id: "implement_one".to_string(),
            agent: "claude".to_string(),
            model: Some("claude-opus-4-8[1m]".to_string()),
            task_ids: vec!["ORB-10353".to_string()],
            trace,
        })
        .expect("persist parsed trace");

    let records = store
        .list_invocation_records(&InvocationQuery {
            job_run_id: Some("jrun-91d7ef01".to_string()),
            limit: 1,
            ..InvocationQuery::default()
        })
        .expect("read persisted trace");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].cache_create_tokens, 0);
    assert_eq!(records[0].cache_create_1h_tokens, 37_795);
    assert_eq!(records[0].provider_cost_usd, Some(1.014_018));
    let derived = records[0].derived_cost_usd.expect("priced Claude model");
    assert!(
        (derived - 1.014_018).abs() < 1e-6,
        "derived cost was {derived}, expected ~1.014018"
    );
}

#[test]
fn parse_cli_invocation_trace_accepts_grok_json_text_envelope() {
    let stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"pong\":\"grok\"},\"error\":null}",
        "stopReason": "EndTurn",
        "sessionId": "grok-session",
        "requestId": "grok-request"
    })
    .to_string();

    assert!(
        parse_cli_invocation_trace_from(&ParsedStdout::parse(&stdout), b"", Some(0), 99, true)
            .is_some(),
        "grok --output-format json stdout should expose the embedded Orbit envelope"
    );
}
