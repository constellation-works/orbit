//! Session primitive — a resumable, in-process conversation handle.
//!
//! A `Session` pairs a provider/model identity with a growing message history
//! and an opaque stable identifier. Sessions are process-local only: when the
//! owning process exits they die, and any persistence is a separate layer on
//! top. Messages are replayed in full on each turn; prompt caching (where the
//! provider supports it) is how this stays cheap.

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use orbit_tools::{ToolContext, ToolRegistry};

use super::agent_loop::{AgentLoop, AgentLoopConfig, AgentLoopError, LoopOutcome};
use super::audit::{AuditSink, LoopAuditEvent};
use super::transport::{ContentBlock, LoopTransport, Message, MessageRole};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct Session {
    id: String,
    provider: String,
    model: String,
    system_prompt: String,
    history: Vec<Message>,
    audit_tag: Option<String>,
    spawn_emitted: bool,
}

impl Session {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        system_prompt: impl Into<String>,
        audit_tag: Option<String>,
    ) -> Self {
        let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts_us = Utc::now().timestamp_micros();
        let id = format!("S{ts_us:x}-{counter:x}");
        Self {
            id,
            provider: provider.into(),
            model: model.into(),
            system_prompt: system_prompt.into(),
            history: Vec::new(),
            audit_tag,
            spawn_emitted: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    pub fn audit_tag(&self) -> Option<&str> {
        self.audit_tag.as_deref()
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn append_message(&mut self, msg: Message) {
        self.history.push(msg);
    }

    /// Append a new user prompt. A turn that ended with answered tool calls
    /// (terminal stop reason or policy termination) leaves a trailing
    /// tool-result user message; the prompt joins it so roles keep
    /// alternating and the results stay first in that message.
    pub fn append_user_prompt(&mut self, prompt: &str) {
        if let Some(last) = self.history.last_mut()
            && last.role == MessageRole::User
            && last
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        {
            last.content.push(ContentBlock::Text {
                text: prompt.to_string(),
            });
            return;
        }
        self.history.push(Message::user_text(prompt));
    }

    pub fn ensure_spawn_emitted(
        &mut self,
        run_id: &str,
        task_id: Option<&str>,
        sink: &dyn AuditSink,
    ) {
        if self.spawn_emitted {
            return;
        }
        sink.emit(&LoopAuditEvent::SessionSpawn {
            ts: Utc::now(),
            run_id: run_id.to_string(),
            session_id: self.id.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            task_id: task_id.map(|s| s.to_string()),
            audit_tag: self.audit_tag.clone(),
        });
        self.spawn_emitted = true;
    }

    /// Run one `AgentLoop` turn against this session. Thin wrapper that
    /// threads the runtime dependencies through to [`AgentLoop::run`].
    /// Appends the user prompt, runs the loop, appends the assistant reply;
    /// mutating, per AC contract.
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        cfg: &AgentLoopConfig,
        transport: &dyn LoopTransport,
        registry: &ToolRegistry,
        tool_ctx: &ToolContext,
        sink: &dyn AuditSink,
        user_prompt: &str,
    ) -> Result<LoopOutcome, AgentLoopError> {
        AgentLoop::run(self, cfg, transport, registry, tool_ctx, sink, user_prompt)
    }

    pub fn close(self, run_id: &str, sink: &dyn AuditSink) {
        sink.emit(&LoopAuditEvent::SessionClose {
            ts: Utc::now(),
            run_id: run_id.to_string(),
            session_id: self.id.clone(),
            reason: "caller_closed".to_string(),
        });
    }

    /// Close the session without emitting a SessionClose event. Use when the
    /// loop itself has already recorded the termination condition (e.g. a
    /// guardrail error was the "close" reason).
    pub fn drop_quiet(self) {}
}
