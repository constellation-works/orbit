//! The provider-agnostic agent loop.
//!
//! Drives a conversation: send the replayed history to the model via a
//! [`LoopTransport`], parse the response into content blocks, dispatch any
//! `tool_use` blocks through the shared [`ToolRegistry`], feed results back
//! as `tool_result` blocks on the next user turn, and repeat until the model
//! reports a stop reason or a configured guardrail fires. Guardrails,
//! allowlist enforcement, and audit emission all live here so individual
//! transports stay small and wire-format-focused.

use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_tools::{ToolContext, ToolRegistry};
use orbit_types::workflow::activity_job::{OnDenial, tool_allowed};

use super::audit::{AuditSink, LoopAuditEvent, UsageSnapshot};
use super::session::Session;
use super::tool_dispatch::{build_tool_specs, dispatch};
use super::transport::{
    CacheHint, ContentBlock, LoopTransport, Message, MessageRole, StopReason, TransportError,
    TurnRequest, TurnResponse, TurnUsage,
};

pub struct AgentLoopConfig {
    /// Execution allowlist. Calls must also appear in the active tool specs
    /// advertised to the model. Empty = no tools.
    pub tool_allowlist: Vec<String>,
    /// Tools advertised to the model. When `None`, the advertised set is the
    /// same as `tool_allowlist`. When `Some`, this set is advertised instead —
    /// useful when a caller wants the model to *attempt* a disallowed tool so
    /// the dispatch-time policy check exercises. Only active schemas are
    /// advertised. Their intersection with `tool_allowlist` defines what the
    /// model can execute; other calls trigger `PolicyDenied` when invoked.
    pub advertised_tools: Option<Vec<String>>,
    pub max_iterations: u32,
    pub max_total_tokens: u64,
    pub wall_clock_timeout: Duration,
    pub max_response_tokens: u32,
    pub on_denial: OnDenial,
    pub run_id: String,
    pub task_id: Option<String>,
    pub cache_hint: CacheHint,
}

impl AgentLoopConfig {
    pub fn new_for_run(run_id: impl Into<String>) -> Self {
        Self {
            tool_allowlist: Vec::new(),
            advertised_tools: None,
            max_iterations: 20,
            max_total_tokens: 500_000,
            wall_clock_timeout: Duration::from_secs(600),
            max_response_tokens: 4096,
            on_denial: OnDenial::Terminate,
            run_id: run_id.into(),
            task_id: None,
            cache_hint: CacheHint::SystemAndEarliestHistory,
        }
    }

    pub fn with_allowlist(mut self, allow: Vec<String>) -> Self {
        self.tool_allowlist = allow;
        self
    }

    pub fn with_advertised_tools(mut self, advertised: Vec<String>) -> Self {
        self.advertised_tools = Some(advertised);
        self
    }

    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    pub fn with_max_total_tokens(mut self, n: u64) -> Self {
        self.max_total_tokens = n;
        self
    }

    pub fn with_wall_clock_timeout(mut self, d: Duration) -> Self {
        self.wall_clock_timeout = d;
        self
    }

    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_max_response_tokens(mut self, n: u32) -> Self {
        self.max_response_tokens = n;
        self
    }

    pub fn with_on_denial(mut self, on_denial: OnDenial) -> Self {
        self.on_denial = on_denial;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateReason {
    Stop,
    MaxTokens,
    Other,
}

#[derive(Debug)]
pub struct IterationTrace {
    pub iteration: u32,
    pub stop_reason: StopReason,
    pub tool_calls: Vec<String>,
    pub policy_denials: Vec<String>,
    pub usage: TurnUsage,
}

#[derive(Debug)]
pub struct LoopOutcome {
    pub final_message: String,
    pub usage: TurnUsage,
    pub terminate_reason: TerminateReason,
    pub trace: Vec<IterationTrace>,
}

#[derive(Debug)]
pub enum AgentLoopError {
    MaxIterations { limit: u32, observed: u32 },
    TokenBudget { limit: u64, observed: u64 },
    Timeout { limit_ms: u128, observed_ms: u128 },
    PolicyDenied { tool_name: String, iteration: u32 },
    Transport(TransportError),
    Io(String),
}

impl std::fmt::Display for AgentLoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentLoopError::MaxIterations { limit, observed } => {
                write!(f, "max_iterations exceeded ({observed} > {limit})")
            }
            AgentLoopError::TokenBudget { limit, observed } => {
                write!(f, "max_total_tokens exceeded ({observed} > {limit})")
            }
            AgentLoopError::Timeout {
                limit_ms,
                observed_ms,
            } => {
                write!(
                    f,
                    "wall_clock_timeout exceeded ({observed_ms}ms > {limit_ms}ms)"
                )
            }
            AgentLoopError::PolicyDenied {
                tool_name,
                iteration,
            } => {
                write!(
                    f,
                    "tool '{tool_name}' denied by tool policy at iteration {iteration}"
                )
            }
            AgentLoopError::Transport(err) => write!(f, "transport: {err}"),
            AgentLoopError::Io(msg) => write!(f, "io: {msg}"),
        }
    }
}

impl std::error::Error for AgentLoopError {}

impl From<TransportError> for AgentLoopError {
    fn from(err: TransportError) -> Self {
        AgentLoopError::Transport(err)
    }
}

pub struct AgentLoop;

impl AgentLoop {
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        session: &mut Session,
        cfg: &AgentLoopConfig,
        transport: &dyn LoopTransport,
        registry: &ToolRegistry,
        tool_ctx: &ToolContext,
        sink: &dyn AuditSink,
        user_prompt: &str,
    ) -> Result<LoopOutcome, AgentLoopError> {
        session.ensure_spawn_emitted(&cfg.run_id, cfg.task_id.as_deref(), sink);

        session.append_user_prompt(user_prompt);

        let advertised: Vec<String> = match cfg.advertised_tools.as_ref() {
            Some(set) => set.clone(),
            None => cfg.tool_allowlist.clone(),
        };
        let tool_specs = build_tool_specs(registry, &advertised);

        let started = Instant::now();
        let mut total_tokens_observed: u64 = 0;
        let mut cumulative_usage = TurnUsage::default();
        let mut trace: Vec<IterationTrace> = Vec::new();
        let mut last_text_reply = String::new();
        let mut iteration: u32 = 0;

        loop {
            iteration += 1;
            check_deadline(cfg, started)?;
            if iteration > cfg.max_iterations {
                return Err(AgentLoopError::MaxIterations {
                    limit: cfg.max_iterations,
                    observed: iteration - 1,
                });
            }

            let turn_req = TurnRequest {
                system: if session.system_prompt().is_empty() {
                    None
                } else {
                    Some(session.system_prompt())
                },
                messages: session.history(),
                tools: &tool_specs,
                cache_hint: cfg.cache_hint,
                max_response_tokens: cfg.max_response_tokens,
            };

            let TurnResponse {
                content,
                stop_reason,
                usage,
                raw_request_body,
                raw_response_body,
                endpoint,
                http_status,
            } = invoke_transport(transport, sink, cfg, session.id(), iteration, &turn_req)?;

            total_tokens_observed = total_tokens_observed
                .saturating_add(usage.input_tokens)
                .saturating_add(usage.output_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            accumulate(&mut cumulative_usage, &usage);

            // Minimal debug hook — raw bytes retained in audit, not dropped.
            drop((raw_request_body, raw_response_body, endpoint, http_status));

            if total_tokens_observed > cfg.max_total_tokens {
                return Err(AgentLoopError::TokenBudget {
                    limit: cfg.max_total_tokens,
                    observed: total_tokens_observed,
                });
            }
            check_deadline(cfg, started)?;

            let (text_part, assistant_blocks, tool_calls) = classify_content(&content);
            if !text_part.is_empty() {
                last_text_reply = text_part;
            }

            // Providers can end a turn without content (e.g. a safety stop).
            // Replaying an empty assistant message would invalidate later requests.
            if !assistant_blocks.is_empty() {
                session.append_message(Message::assistant(assistant_blocks));
            }

            let mut iter_tool_names = Vec::new();
            let mut iter_denials = Vec::new();
            let mut user_tool_results: Vec<ContentBlock> = Vec::new();
            let mut deadline_hit: Option<AgentLoopError> = None;
            let mut terminating_denial: Option<String> = None;

            let mut pending_calls = tool_calls.into_iter();
            while let Some((tool_use_id, tool_name, input)) = pending_calls.next() {
                // Each dispatch is a side-effect boundary: once the budget is
                // spent, answer this and every later call with a skipped
                // result so the replayed history still pairs each tool_use.
                if let Err(err) = check_deadline(cfg, started) {
                    user_tool_results.push(skipped_tool_result(tool_use_id));
                    user_tool_results.extend(
                        pending_calls
                            .by_ref()
                            .map(|(id, _, _)| skipped_tool_result(id)),
                    );
                    deadline_hit = Some(err);
                    break;
                }
                iter_tool_names.push(tool_name.clone());

                let denial_reason = if !tool_allowed(&tool_name, &cfg.tool_allowlist) {
                    Some("tool not in allowlist")
                } else if !tool_specs.iter().any(|spec| spec.name == tool_name) {
                    Some("tool not advertised")
                } else {
                    None
                };
                if let Some(reason) = denial_reason {
                    let reason = reason.to_string();
                    let denial_payload = serde_json::json!({
                        "error": {
                            "code": "tool_denied",
                            "message": &reason,
                        },
                        "tool_name": &tool_name,
                        "tool_use_id": &tool_use_id,
                        "reason": &reason,
                        "input": &input,
                    });
                    let _ =
                        sink.write_blob(&serde_json::to_vec(&denial_payload).unwrap_or_default());
                    sink.emit(&LoopAuditEvent::PolicyDenial {
                        ts: Utc::now(),
                        run_id: cfg.run_id.clone(),
                        session_id: session.id().to_string(),
                        iteration,
                        tool_name: tool_name.clone(),
                        reason,
                    });
                    iter_denials.push(tool_name.clone());
                    user_tool_results.push(error_tool_result(tool_use_id, &denial_payload));
                    match cfg.on_denial {
                        OnDenial::Terminate => {
                            terminating_denial = Some(tool_name);
                            break;
                        }
                        OnDenial::Continue => continue,
                    }
                }

                let input_bytes = serde_json::to_vec(&input).unwrap_or_default();
                let input_sha256 = sink.write_blob(&input_bytes);
                sink.emit(&LoopAuditEvent::ToolCallRequested {
                    ts: Utc::now(),
                    run_id: cfg.run_id.clone(),
                    session_id: session.id().to_string(),
                    iteration,
                    tool_name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    input_sha256,
                });

                let outcome = dispatch(registry, tool_ctx, &tool_name, input);
                let output_bytes = serde_json::to_vec(&outcome.output).unwrap_or_default();
                let output_sha256 = sink.write_blob(&output_bytes);
                sink.emit(&LoopAuditEvent::ToolCallResult {
                    ts: Utc::now(),
                    run_id: cfg.run_id.clone(),
                    session_id: session.id().to_string(),
                    iteration,
                    tool_name: tool_name.clone(),
                    tool_use_id: tool_use_id.clone(),
                    outcome: if outcome.is_error {
                        "error".to_string()
                    } else {
                        "ok".to_string()
                    },
                    output_sha256,
                    duration_ms: outcome.duration_ms,
                });

                let tool_text = match serde_json::to_string(&outcome.output) {
                    Ok(s) => s,
                    Err(err) => format!("{{\"error\":\"serialize: {err}\"}}"),
                };
                user_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id,
                    content: tool_text,
                    is_error: outcome.is_error,
                });
            }

            if let Some(denied_tool) = &terminating_denial {
                // Calls after the terminating denial are never executed, but
                // each still needs a result so the session stays resumable.
                for (tool_use_id, tool_name, _) in pending_calls {
                    let skipped_payload = serde_json::json!({
                        "error": {
                            "code": "tool_not_executed",
                            "message": format!(
                                "not executed: turn terminated after tool '{denied_tool}' was denied"
                            ),
                        },
                        "tool_name": &tool_name,
                        "tool_use_id": &tool_use_id,
                    });
                    user_tool_results.push(error_tool_result(tool_use_id, &skipped_payload));
                }
            } else {
                // The last tool may itself outlive the budget; never report
                // success or start another turn after expiry.
                let deadline = match deadline_hit {
                    Some(err) => Err(err),
                    None => check_deadline(cfg, started),
                };
                if let Err(err) = deadline {
                    sink.emit(&LoopAuditEvent::IterationBoundary {
                        ts: Utc::now(),
                        run_id: cfg.run_id.clone(),
                        session_id: session.id().to_string(),
                        iteration,
                        continues: false,
                    });
                    if !user_tool_results.is_empty() {
                        session.append_message(Message::user_blocks(user_tool_results));
                    }
                    return Err(err);
                }
            }

            let continues = terminating_denial.is_none()
                && matches!(stop_reason, StopReason::ToolUse)
                && !user_tool_results.is_empty();
            sink.emit(&LoopAuditEvent::IterationBoundary {
                ts: Utc::now(),
                run_id: cfg.run_id.clone(),
                session_id: session.id().to_string(),
                iteration,
                continues,
            });
            trace.push(IterationTrace {
                iteration,
                stop_reason,
                tool_calls: iter_tool_names,
                policy_denials: iter_denials,
                usage: usage.clone(),
            });

            // Answer every tool_use appended above before any exit, so a later
            // `Session::send` never replays an unmatched tool request.
            if !user_tool_results.is_empty() {
                session.append_message(Message::user_blocks(user_tool_results));
            }

            if let Some(tool_name) = terminating_denial {
                return Err(AgentLoopError::PolicyDenied {
                    tool_name,
                    iteration,
                });
            }

            if !continues {
                let terminate_reason = match stop_reason {
                    StopReason::MaxTokens => TerminateReason::MaxTokens,
                    StopReason::EndTurn => TerminateReason::Stop,
                    _ => TerminateReason::Other,
                };
                return Ok(LoopOutcome {
                    final_message: last_text_reply,
                    usage: cumulative_usage,
                    terminate_reason,
                    trace,
                });
            }
        }
    }
}

fn error_tool_result(tool_use_id: String, payload: &serde_json::Value) -> ContentBlock {
    let content = match serde_json::to_string(payload) {
        Ok(s) => s,
        Err(err) => format!("{{\"error\":\"serialize: {err}\"}}"),
    };
    ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error: true,
    }
}

fn classify_content(
    content: &[ContentBlock],
) -> (
    String,
    Vec<ContentBlock>,
    Vec<(String, String, serde_json::Value)>,
) {
    let mut text = String::new();
    let mut assistant_blocks = Vec::with_capacity(content.len());
    let mut tool_calls = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
                assistant_blocks.push(block.clone());
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push((id.clone(), name.clone(), input.clone()));
                assistant_blocks.push(block.clone());
            }
            ContentBlock::ToolResult { .. } => {
                // Providers should not return ToolResult; if they do, pass it through.
                assistant_blocks.push(block.clone());
            }
        }
    }
    (text, assistant_blocks, tool_calls)
}

fn accumulate(total: &mut TurnUsage, delta: &TurnUsage) {
    total.input_tokens = total.input_tokens.saturating_add(delta.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(delta.output_tokens);
    total.cache_read_input_tokens = total
        .cache_read_input_tokens
        .saturating_add(delta.cache_read_input_tokens);
    total.cache_creation_input_tokens = total
        .cache_creation_input_tokens
        .saturating_add(delta.cache_creation_input_tokens);
}

fn check_deadline(cfg: &AgentLoopConfig, started: Instant) -> Result<(), AgentLoopError> {
    let elapsed = started.elapsed().as_millis();
    let limit = cfg.wall_clock_timeout.as_millis();
    if elapsed > limit {
        return Err(AgentLoopError::Timeout {
            limit_ms: limit,
            observed_ms: elapsed,
        });
    }
    Ok(())
}

fn skipped_tool_result(tool_use_id: String) -> ContentBlock {
    let payload = serde_json::json!({
        "error": {
            "code": "wall_clock_timeout",
            "message": "tool not executed: wall-clock budget expired",
        },
    });
    ContentBlock::ToolResult {
        tool_use_id,
        content: payload.to_string(),
        is_error: true,
    }
}

fn invoke_transport(
    transport: &dyn LoopTransport,
    sink: &dyn AuditSink,
    cfg: &AgentLoopConfig,
    session_id: &str,
    iteration: u32,
    req: &TurnRequest<'_>,
) -> Result<TurnResponse, AgentLoopError> {
    let request_preview = preview_request(req, transport);
    let request_body_sha256 = sink.write_blob(&request_preview);
    sink.emit(&LoopAuditEvent::HttpRequest {
        ts: Utc::now(),
        run_id: cfg.run_id.clone(),
        session_id: session_id.to_string(),
        iteration,
        provider: transport.provider().to_string(),
        model: transport.model().to_string(),
        endpoint: String::new(),
        body_sha256: request_body_sha256,
    });

    let resp = transport.send_turn(req)?;

    let response_body_sha256 = sink.write_blob(&resp.raw_response_body);
    sink.emit(&LoopAuditEvent::HttpResponse {
        ts: Utc::now(),
        run_id: cfg.run_id.clone(),
        session_id: session_id.to_string(),
        iteration,
        http_status: resp.http_status,
        stop_reason: resp.stop_reason.as_str().to_string(),
        usage: UsageSnapshot {
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
            cache_read_input_tokens: resp.usage.cache_read_input_tokens,
            cache_creation_input_tokens: resp.usage.cache_creation_input_tokens,
        },
        body_sha256: response_body_sha256,
    });

    let _ = sink.write_blob(&resp.raw_request_body);

    Ok(resp)
}

fn preview_request(req: &TurnRequest<'_>, transport: &dyn LoopTransport) -> Vec<u8> {
    let value = serde_json::json!({
        "provider": transport.provider(),
        "model": transport.model(),
        "system_len": req.system.map(|s| s.len()).unwrap_or(0),
        "message_count": req.messages.len(),
        "tool_count": req.tools.len(),
        "max_response_tokens": req.max_response_tokens,
        "last_role": req.messages.last().map(|m| match m.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }),
    });
    serde_json::to_vec(&value).unwrap_or_default()
}
