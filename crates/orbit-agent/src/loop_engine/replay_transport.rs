//! Recorded-response `LoopTransport` for offline smoke runs.
//!
//! Feeds a scripted sequence of `TurnResponse`s back in order, so the
//! `AgentLoop` can drive a full turn cycle without an outbound HTTP call or
//! credentials. Used by Phase 2b's tool-denial smoke (T20260418-2052 AC4):
//! the replay emits a `tool_use` for a disallowed tool on the first turn,
//! allowing `EnforcedAuditSink` to observe the dispatch and record the
//! `tool.denied` event.
//!
//! Not a general-purpose recorder — pairs well with a future `RecordingTransport`
//! that would capture real provider turns into the same fixture format.

use std::sync::{Mutex, PoisonError};

use super::transport::{
    ContentBlock, LoopTransport, StopReason, TransportError, TurnRequest, TurnResponse, TurnUsage,
};

/// A single canned turn — what the fake provider returns when asked.
#[derive(Debug, Clone)]
pub struct ReplayTurn {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
}

pub struct ReplayTransport {
    provider: String,
    model: String,
    turns: Mutex<Vec<ReplayTurn>>,
    cursor: Mutex<usize>,
}

impl ReplayTransport {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        turns: Vec<ReplayTurn>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            turns: Mutex::new(turns),
            cursor: Mutex::new(0),
        }
    }
}

impl LoopTransport for ReplayTransport {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn send_turn(&self, _req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
        let turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cursor = self.cursor.lock().unwrap_or_else(PoisonError::into_inner);
        if *cursor >= turns.len() {
            return Err(TransportError::Other(format!(
                "ReplayTransport exhausted: asked for turn {} but only {} scripted",
                *cursor,
                turns.len()
            )));
        }
        let turn = turns[*cursor].clone();
        *cursor += 1;
        Ok(TurnResponse {
            content: turn.content,
            stop_reason: turn.stop_reason,
            usage: TurnUsage::default(),
            raw_request_body: b"{\"replay\":\"request\"}".to_vec(),
            raw_response_body: b"{\"replay\":\"response\"}".to_vec(),
            endpoint: format!("replay://{}/{}", self.provider, self.model),
            http_status: 200,
        })
    }
}
