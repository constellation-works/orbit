#![allow(missing_docs)]
// Examples are user-facing smoke binaries that print progress and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! Network-free smoke: each of the three loop guardrails trips and returns a
//! distinct structured error variant. Uses an inline mock transport so this
//! runs in `cargo build --examples` and `cargo run --example guardrails_smoke`
//! without credentials.
//!
//! The iteration case calls `noop.tool`. The loop advertises only active
//! registry schemas named by the allowlist, so this fixture registers that
//! tool. An allowlist entry alone does not make an unregistered tool callable,
//! and production denial of a missing or disallowed tool is unchanged.

use std::process::ExitCode;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use orbit_agent::loop_engine::{
    AgentLoop, AgentLoopConfig, AgentLoopError, ContentBlock, InMemorySink, LoopTransport, Session,
    StopReason, TransportError, TurnRequest, TurnResponse, TurnUsage,
};
use orbit_tools::{Tool, ToolContext, ToolExecutionKind, ToolRegistry};
use orbit_types::tool::ToolSchema;

/// In-process tool the iteration case is allowed to call. It returns a fixed
/// JSON object and does not touch Orbit state, the network, or credentials.
struct NoopTool;

impl Tool for NoopTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "noop.tool".to_string(),
            description: "Credential-free no-op for the offline guardrail smoke.".to_string(),
            parameters: Vec::new(),
            builtin: false,
        }
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn execute(
        &self,
        _ctx: &ToolContext,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, orbit_common::OrbitError> {
        Ok(serde_json::json!({ "ok": true }))
    }
}

struct ScriptedTransport {
    model: String,
    behavior: Mutex<Behavior>,
}

enum Behavior {
    /// Always returns tool_use content so the loop keeps iterating.
    AlwaysToolUse,
    /// Reports an enormous input_tokens count on every turn.
    BigTokens,
    /// Sleeps to force a wall-clock timeout.
    SlowResponses { delay: Duration },
}

impl ScriptedTransport {
    fn new(model: impl Into<String>, behavior: Behavior) -> Self {
        Self {
            model: model.into(),
            behavior: Mutex::new(behavior),
        }
    }
}

impl LoopTransport for ScriptedTransport {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn send_turn(&self, _req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
        let behavior = self.behavior.lock().expect("behavior mutex");
        match &*behavior {
            Behavior::AlwaysToolUse => Ok(TurnResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "toolu_mock".to_string(),
                    name: "noop.tool".to_string(),
                    input: serde_json::json!({}),
                }],
                stop_reason: StopReason::ToolUse,
                usage: TurnUsage {
                    input_tokens: 10,
                    output_tokens: 1,
                    ..Default::default()
                },
                raw_request_body: b"{}".to_vec(),
                raw_response_body: b"{\"mock\":\"tool_use\"}".to_vec(),
                endpoint: "mock://scripted".to_string(),
                http_status: 200,
            }),
            Behavior::BigTokens => Ok(TurnResponse {
                content: vec![ContentBlock::Text {
                    text: "done".to_string(),
                }],
                stop_reason: StopReason::EndTurn,
                usage: TurnUsage {
                    input_tokens: 10_000_000,
                    output_tokens: 0,
                    ..Default::default()
                },
                raw_request_body: b"{}".to_vec(),
                raw_response_body: b"{\"mock\":\"big_tokens\"}".to_vec(),
                endpoint: "mock://scripted".to_string(),
                http_status: 200,
            }),
            Behavior::SlowResponses { delay } => {
                sleep(*delay);
                Ok(TurnResponse {
                    content: vec![ContentBlock::Text {
                        text: "late".to_string(),
                    }],
                    stop_reason: StopReason::EndTurn,
                    usage: TurnUsage::default(),
                    raw_request_body: b"{}".to_vec(),
                    raw_response_body: b"{\"mock\":\"slow\"}".to_vec(),
                    endpoint: "mock://scripted".to_string(),
                    http_status: 200,
                })
            }
        }
    }
}

fn main() -> ExitCode {
    let sink = InMemorySink::new(
        std::env::temp_dir()
            .join("orbit-agent-examples")
            .join("guardrails-blobs"),
    );
    // Registration is what advertises the tool. The case below still allowlists
    // the same name; production policy refuses a call that lacks either.
    let mut registry = ToolRegistry::new();
    registry.register(NoopTool);
    let ctx = ToolContext::default();

    // 1. max_iterations
    {
        let transport = ScriptedTransport::new("mock-a", Behavior::AlwaysToolUse);
        let cfg = AgentLoopConfig::new_for_run("guardrails-max-iter")
            .with_allowlist(vec!["noop.tool".to_string()])
            .with_max_iterations(3)
            .with_max_total_tokens(u64::MAX)
            .with_wall_clock_timeout(Duration::from_secs(10));
        let mut session = Session::new("mock", "mock-a", "sys", None);
        let res = AgentLoop::run(&mut session, &cfg, &transport, &registry, &ctx, &sink, "go");
        match res {
            Err(AgentLoopError::MaxIterations { limit, observed }) => {
                println!("ok MaxIterations(limit={limit}, observed={observed})");
            }
            other => {
                eprintln!("expected MaxIterations, got {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    // 2. max_total_tokens
    {
        let transport = ScriptedTransport::new("mock-b", Behavior::BigTokens);
        let cfg = AgentLoopConfig::new_for_run("guardrails-tokens")
            .with_max_iterations(5)
            .with_max_total_tokens(1_000)
            .with_wall_clock_timeout(Duration::from_secs(10));
        let mut session = Session::new("mock", "mock-b", "sys", None);
        let res = AgentLoop::run(&mut session, &cfg, &transport, &registry, &ctx, &sink, "go");
        match res {
            Err(AgentLoopError::TokenBudget { limit, observed }) => {
                println!("ok TokenBudget(limit={limit}, observed={observed})");
            }
            other => {
                eprintln!("expected TokenBudget, got {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    // 3. wall_clock_timeout
    {
        let transport = ScriptedTransport::new(
            "mock-c",
            Behavior::SlowResponses {
                delay: Duration::from_millis(250),
            },
        );
        let cfg = AgentLoopConfig::new_for_run("guardrails-timeout")
            .with_max_iterations(5)
            .with_max_total_tokens(u64::MAX)
            .with_wall_clock_timeout(Duration::from_millis(50));
        let mut session = Session::new("mock", "mock-c", "sys", None);
        let res = AgentLoop::run(&mut session, &cfg, &transport, &registry, &ctx, &sink, "go");
        match res {
            Err(AgentLoopError::Timeout {
                limit_ms,
                observed_ms,
            }) => {
                println!("ok Timeout(limit_ms={limit_ms}, observed_ms={observed_ms})");
            }
            other => {
                eprintln!("expected Timeout, got {other:?}");
                return ExitCode::FAILURE;
            }
        }
    }

    let events = sink.events();
    println!(
        "total audit events emitted across three smokes: {}",
        events.len()
    );
    ExitCode::SUCCESS
}
