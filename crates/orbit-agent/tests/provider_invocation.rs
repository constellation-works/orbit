//! Public provider boundaries: subprocess input and HTTP wire contracts.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "provider_invocation/loop_policy.rs"]
mod loop_policy;
mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_agent::loop_engine::{
    AgentLoop, AgentLoopConfig, AgentLoopError, CacheHint, ContentBlock, LoopTransport, Message,
    NullSink, Session, TurnRequest,
};
use orbit_agent::providers::{
    anthropic::AnthropicMessagesTransport, gemini_http::GeminiHttpTransport,
    openai_compat::OpenAiCompatTransport,
};
use orbit_agent::{Agent, AgentConfig, AgentRequest};
use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_tools::{Tool, ToolContext, ToolRegistry};
use orbit_types::identity::ReasoningEffort;
use orbit_types::tool::ToolSchema;
use orbit_types::workflow::Provider;
use serde_json::{Value, json};
use support::{ChildGuard, HOSTILE_ENV, Server, WAIT, isolated, scratch};

struct CliCase {
    provider: &'static str,
    binary: &'static str,
    model: &'static str,
    emitted_model: &'static str,
    model_flag: &'static str,
    effort_flag: Option<&'static str>,
}

const CLI_CASES: &[CliCase] = &[
    CliCase {
        provider: "claude",
        binary: "claude",
        model: "opus-4.8",
        emitted_model: "claude-opus-4-8",
        model_flag: "--model",
        effort_flag: Some("--effort"),
    },
    CliCase {
        provider: "codex",
        binary: "codex",
        model: "fixture-model",
        emitted_model: "fixture-model",
        model_flag: "--model",
        effort_flag: Some("--config"),
    },
    CliCase {
        provider: "gemini",
        binary: "gemini",
        model: "fixture-model",
        emitted_model: "fixture-model",
        model_flag: "-m",
        effort_flag: None,
    },
    CliCase {
        provider: "grok",
        binary: "grok",
        model: "grok-4.7",
        emitted_model: "grok-4.7",
        model_flag: "--model",
        effort_flag: Some("--reasoning-effort"),
    },
    CliCase {
        provider: "copilot",
        binary: "copilot",
        model: "fixture-model",
        emitted_model: "fixture-model",
        model_flag: "--model",
        effort_flag: None,
    },
    CliCase {
        provider: "cursor",
        binary: "cursor-agent",
        model: "fixture-model",
        emitted_model: "fixture-model",
        model_flag: "--model",
        effort_flag: None,
    },
    CliCase {
        provider: "ollama",
        binary: "ollama",
        model: "fixture-model",
        emitted_model: "fixture-model",
        model_flag: "run",
        effort_flag: None,
    },
    CliCase {
        provider: "pi",
        binary: "pi",
        model: "vendor/fixture-model",
        emitted_model: "vendor/fixture-model",
        model_flag: "--model",
        effort_flag: Some("--thinking"),
    },
    CliCase {
        provider: "antigravity",
        binary: "agy",
        model: "gemini-3.8-flash-high",
        emitted_model: "gemini-3.8-flash-high",
        model_flag: "--model",
        effort_flag: Some("--effort"),
    },
    CliCase {
        provider: "opencode",
        binary: "opencode",
        model: "vendor/fixture-model",
        emitted_model: "vendor/fixture-model",
        model_flag: "--model",
        effort_flag: Some("--variant"),
    },
    CliCase {
        provider: "mock",
        binary: "mock-agent",
        model: "",
        emitted_model: "",
        model_flag: "",
        effort_flag: None,
    },
];

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

#[test]
fn cli_registry_adapters_deliver_stdin_flags_and_safe_environment() {
    isolated(
        "cli_registry_adapters_deliver_stdin_flags_and_safe_environment",
        || {
            // Provider::ALL is the public registry. A newly shipped provider must
            // acquire a CLI fixture, or be explicitly exercised by the HTTP case.
            for provider in Provider::ALL {
                if provider == Provider::OpenaiCompat {
                    continue; // exercised by http_transports_keep_keys_in_headers
                }
                assert!(
                    CLI_CASES
                        .iter()
                        .any(|case| case.provider == provider.as_str()),
                    "provider {} has no recording mock fixture",
                    provider.as_str()
                );
            }

            let fixture = scratch();
            for case in CLI_CASES {
                let dir = fixture.path().join(case.binary);
                fs::create_dir(&dir).expect("provider fixture directory");
                let program = dir.join(case.binary);
                // Byte-separated argv preserves spaces and empty arguments. All
                // output paths are relative to the disposable provider cwd.
                fs::write(&program, b"#!/bin/sh\nprintf '%s\\0' \"$@\" > argv\n/usr/bin/env > environment\n/bin/cat > stdin\nprintf '%s' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n")
                .expect("recording mock executable");
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700))
                    .expect("executable mode");

                for configured in [false, true] {
                    let model = (configured || case.provider == "ollama")
                        .then_some(case.model)
                        .filter(|value| !value.is_empty());
                    let cfg = AgentConfig::cli(program.to_string_lossy())
                        .expect("resolve provider")
                        .with_model(model);
                    if case.effort_flag.is_none() {
                        assert!(
                            cfg.clone()
                                .with_reasoning_effort(Some(ReasoningEffort::High))
                                .is_err(),
                            "{} must reject unsupported effort",
                            case.provider
                        );
                    }
                    let effort =
                        (configured && case.effort_flag.is_some()).then_some(ReasoningEffort::High);
                    let cfg = cfg.with_reasoning_effort(effort).expect("supported effort");
                    let envelope = json!({"prompt": "private-prompt-sentinel with spaces\nand a second line", "input": {"provider": case.provider}});
                    let (spec, _) = Agent::new(&cfg)
                        .expect("public provider adapter")
                        .invoke(AgentRequest::activity(
                            "fixture",
                            serde_json::to_vec(&envelope).unwrap(),
                        ))
                        .expect("invocation");
                    let pass = vec![
                        "EXPLICIT_PROVIDER_SETTING".to_string(),
                        "ORBIT_OPERATOR".to_string(),
                    ];
                    let mut extras = spec.required_env_vars.to_vec();
                    extras.push("ORBIT_WORKSPACE_CLAIM_TOKEN");
                    let env = allowlisted_child_env(&pass, &extras);
                    let mut child = ChildGuard(
                        Command::new(&spec.program)
                            .args(&spec.args)
                            .current_dir(&dir)
                            .env_clear()
                            .envs(env)
                            .envs(spec.fixed_env.iter().copied())
                            .stdin(Stdio::piped())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                            .expect("spawn recording provider"),
                    );
                    child
                        .0
                        .stdin
                        .take()
                        .expect("provider stdin")
                        .write_all(&spec.stdin)
                        .expect("send prompt then close pipe");
                    assert!(
                        child.wait(WAIT).success(),
                        "{} mock exited unsuccessfully",
                        case.provider
                    );

                    let args: Vec<String> = fs::read(dir.join("argv"))
                        .unwrap()
                        .split(|b| *b == 0)
                        .filter(|arg| !arg.is_empty())
                        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
                        .collect();
                    assert!(
                        args.iter()
                            .all(|arg| !arg.contains("private-prompt-sentinel")),
                        "{} exposed prompt in argv: {args:?}",
                        case.provider
                    );
                    let stdin = fs::read_to_string(dir.join("stdin")).unwrap();
                    let prompt = if case.provider == "antigravity" {
                        let event: Value =
                            serde_json::from_str(&stdin).expect("Antigravity user event");
                        assert_eq!(event["event"], "user");
                        event["message"]["content"].as_str().unwrap().to_string()
                    } else {
                        stdin
                    };
                    let received: Value =
                        serde_json::from_str(prompt.lines().last().expect("stdin envelope"))
                            .expect("prompt contains intact JSON envelope");
                    assert_eq!(
                        received, envelope,
                        "{} lost or changed stdin prompt",
                        case.provider
                    );

                    let environment = fs::read_to_string(dir.join("environment")).unwrap();
                    let vars: Vec<_> = environment
                        .lines()
                        .filter_map(|line| line.split_once('='))
                        .collect();
                    for name in HOSTILE_ENV {
                        assert!(
                            !vars.iter().any(|(key, _)| key == name),
                            "{} leaked ambient {name} to provider",
                            case.provider
                        );
                    }
                    for (key, value) in [
                        ("ORBIT_RUN_ID", "fixture-run"),
                        ("EXPLICIT_PROVIDER_SETTING", "opted-in"),
                    ] {
                        assert!(
                            vars.contains(&(key, value)),
                            "{} dropped admitted {key}",
                            case.provider
                        );
                    }
                    if case.provider == "claude" {
                        assert!(
                            vars.contains(&("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")),
                            "Claude background task disable must reach subprocess (ORB-13664)"
                        );
                    }
                    if let Some(model) = model {
                        assert!(
                            has_pair(&args, case.model_flag, case.emitted_model),
                            "{} model {model} mapped incorrectly: {args:?}",
                            case.provider
                        );
                    } else if !case.model_flag.is_empty() {
                        assert!(
                            !args.iter().any(|arg| arg == case.model_flag),
                            "{} emitted unconfigured model",
                            case.provider
                        );
                    }
                    if let Some(flag) = case.effort_flag {
                        let value = if case.provider == "codex" {
                            "model_reasoning_effort=\"high\""
                        } else {
                            "high"
                        };
                        if configured {
                            assert!(
                                has_pair(&args, flag, value),
                                "{} effort mapped incorrectly: {args:?}",
                                case.provider
                            );
                        } else {
                            assert!(
                                !args.iter().any(|arg| arg == value),
                                "{} emitted unconfigured effort",
                                case.provider
                            );
                        }
                    }
                }
            }
        },
    );
}

const API_KEY: &str = "synthetic-http-key";

fn assert_header_key(request: &support::RecordedRequest, header: &str, value: &str) {
    assert_eq!(
        request.header(header),
        Some(value),
        "API key must reach authentication header"
    );
    assert!(
        !request.target.contains('?') && !request.target.contains(API_KEY),
        "API key must never enter request URL: {}",
        request.target
    );
    assert!(
        !request.body.to_string().contains(API_KEY),
        "API key must never enter request body"
    );
}

#[test]
fn http_transports_keep_keys_in_headers() {
    isolated("http_transports_keep_keys_in_headers", || {
        let messages = vec![Message::user_text("wire prompt")];
        let request = TurnRequest {
            system: Some("system instruction"),
            messages: &messages,
            tools: &[],
            cache_hint: CacheHint::None,
            max_response_tokens: 32,
        };
        let server = Server::new(vec![
            json!({"content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn"}),
        ]);
        let transport = AnthropicMessagesTransport::new(API_KEY, "fixture-model")
            .unwrap()
            .with_endpoint(format!("{}/v1/messages", server.base_url))
            .with_timeout(WAIT)
            .unwrap();
        let response = transport
            .send_turn(&request)
            .expect("Anthropic wire response");
        assert!(matches!(&response.content[..], [ContentBlock::Text { text }] if text == "answer"));
        let recorded = server.request();
        assert_header_key(&recorded, "x-api-key", API_KEY);
        assert_eq!(recorded.body["model"], "fixture-model");
        assert_eq!(
            recorded.body["messages"][0]["content"][0]["text"],
            "wire prompt"
        );
        server.finish();

        let server = Server::new(vec![
            json!({"choices":[{"message":{"role":"assistant","content":"answer"},"finish_reason":"stop"}]}),
        ]);
        let transport =
            OpenAiCompatTransport::new(&server.base_url, API_KEY, "fixture-model", vec![])
                .unwrap()
                .with_timeout(WAIT)
                .unwrap();
        let response = transport
            .send_turn(&request)
            .expect("OpenAI-compatible wire response");
        assert!(matches!(&response.content[..], [ContentBlock::Text { text }] if text == "answer"));
        let recorded = server.request();
        assert_header_key(&recorded, "authorization", &format!("Bearer {API_KEY}"));
        assert_eq!(recorded.body["model"], "fixture-model");
        assert_eq!(recorded.body["messages"][1]["content"], "wire prompt");
        server.finish();

        // Gemini authenticates both cachedContents and generateContent. Cover
        // uncached generation too, so the cache path cannot mask a leak.
        let history = vec![
            Message::user_text("cache me"),
            Message::assistant(vec![ContentBlock::Text {
                text: "history".into(),
            }]),
            Message::user_text("wire prompt"),
        ];
        let gemini_request = TurnRequest {
            messages: &history,
            ..request
        };
        for cached in [false, true] {
            let answer = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"answer"}]},"finishReason":"STOP"}]});
            let responses = if cached {
                vec![json!({"name":"cachedContents/fixture"}), answer]
            } else {
                vec![answer]
            };
            let server = Server::new(responses);
            let transport = GeminiHttpTransport::new(API_KEY, "fixture-model", cached.then_some(2))
                .unwrap()
                .with_base_url(&server.base_url)
                .with_timeout(WAIT)
                .unwrap();
            let response = transport
                .send_turn(&gemini_request)
                .expect("Gemini wire response");
            assert!(
                matches!(&response.content[..], [ContentBlock::Text { text }] if text == "answer")
            );
            if cached {
                let recorded = server.request();
                assert_header_key(&recorded, "x-goog-api-key", API_KEY);
                assert_eq!(recorded.target, "POST /v1beta/cachedContents HTTP/1.1");
                assert_eq!(recorded.body["contents"][0]["parts"][0]["text"], "cache me");
            }
            let recorded = server.request();
            assert_header_key(&recorded, "x-goog-api-key", API_KEY);
            assert_eq!(
                recorded.target,
                "POST /v1beta/models/fixture-model:generateContent HTTP/1.1"
            );
            if cached {
                assert_eq!(recorded.body["cachedContent"], "cachedContents/fixture");
                assert_eq!(
                    recorded.body["contents"][0]["parts"][0]["text"],
                    "wire prompt"
                );
            } else {
                assert_eq!(
                    recorded.body["contents"][2]["parts"][0]["text"],
                    "wire prompt"
                );
            }
            server.finish();
        }
    });
}

struct SlowTool(Arc<Mutex<Vec<Value>>>);

impl Tool for SlowTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "fixture.pause".into(),
            description: "Record a dispatch and exhaust its budget".into(),
            parameters: vec![],
            builtin: false,
        }
    }

    fn execute(&self, _ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        self.0.lock().unwrap().push(input.clone());
        std::thread::sleep(Duration::from_millis(1100));
        Ok(input)
    }
}

#[test]
fn expired_budget_stops_later_dispatch_and_pairs_skipped_results() {
    isolated(
        "expired_budget_stops_later_dispatch_and_pairs_skipped_results",
        || {
            // ORB-13486: a running tool can finish, but no later tool or turn may
            // start after expiry, even when the provider calls this its final turn.
            for (finish_reason, count) in [("tool_calls", 2), ("stop", 2), ("stop", 1)] {
                let calls: Vec<_> = (0..count)
                    .map(|index| {
                        json!({"id":format!("call-{index}"),"type":"function",
                "function":{"name":"fixture.pause","arguments":format!("{{\"index\":{index}}}")}})
                    })
                    .collect();
                let server = Server::new(vec![
                    json!({"choices":[{"message":{"role":"assistant","tool_calls":calls},"finish_reason":finish_reason}]}),
                ]);
                let transport =
                    OpenAiCompatTransport::new(&server.base_url, API_KEY, "fixture-model", vec![])
                        .unwrap()
                        .with_timeout(WAIT)
                        .unwrap();
                let executed = Arc::new(Mutex::new(Vec::new()));
                let mut registry = ToolRegistry::new();
                registry.register(SlowTool(Arc::clone(&executed)));
                let mut session = Session::new("openai_compat", "fixture-model", "", None);
                let cfg = AgentLoopConfig::new_for_run("fixture-run")
                    .with_allowlist(vec!["fixture.pause".into()])
                    .with_wall_clock_timeout(Duration::from_secs(1));
                let ctx = ToolContext {
                    allowed_tools: vec!["fixture.pause".into()],
                    ..Default::default()
                };
                let result = AgentLoop::run(
                    &mut session,
                    &cfg,
                    &transport,
                    &registry,
                    &ctx,
                    &NullSink,
                    "exhaust budget",
                );
                assert_eq!(
                    *executed.lock().unwrap(),
                    vec![json!({"index":0})],
                    "ORB-13486: no later dispatch after budget expiry"
                );
                assert!(
                    matches!(result, Err(AgentLoopError::Timeout { .. })),
                    "ORB-13486: must return Timeout: {result:?}"
                );
                let results = &session
                    .history()
                    .last()
                    .expect("paired tool results")
                    .content;
                assert_eq!(
                    results.len(),
                    count,
                    "ORB-13486: every tool_use needs a paired result"
                );
                for (index, block) in results.iter().enumerate() {
                    let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    else {
                        panic!("expected tool result")
                    };
                    assert_eq!(tool_use_id, &format!("call-{index}"));
                    assert_eq!(*is_error, index != 0);
                    let payload: Value = serde_json::from_str(content).unwrap();
                    if index == 0 {
                        assert_eq!(payload, json!({"index":0}));
                    } else {
                        assert_eq!(payload["error"]["code"], "wall_clock_timeout");
                    }
                }
                server.request();
                server.finish();
            }
        },
    );
}
