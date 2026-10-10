//! Public provider boundaries: CLI invocation and retained audit contracts.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "provider_invocation/audit.rs"]
mod audit;
mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use orbit_agent::{Agent, AgentConfig, AgentRequest};
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_types::identity::ReasoningEffort;
use orbit_types::workflow::Provider;
use serde_json::{Value, json};
use support::{ChildGuard, HOSTILE_ENV, WAIT, isolated, scratch};

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
            // acquire a CLI fixture, or fail structurally as an unsupported provider.
            for provider in Provider::ALL {
                if provider == Provider::OpenaiCompat {
                    assert!(matches!(
                        AgentConfig::cli(provider.as_str()),
                        Err(orbit_common::OrbitError::UnsupportedAgentProvider(key))
                            if key == provider.as_str()
                    ));
                    continue;
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
                        assert!(
                            vars.contains(&("CLAUDE_CODE_DISABLE_CRON", "1")),
                            "Claude cron disable must reach subprocess; without it a scheduled \
                             wake-up turn emits a second result over the envelope (ORB-14815)"
                        );
                        assert!(
                            vars.contains(&("BASH_MAX_TIMEOUT_MS", "3600000")),
                            "Claude must be able to wait on a long gate in the foreground; at \
                             the 10-minute default it detaches the gate and ends (ORB-15130)"
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
