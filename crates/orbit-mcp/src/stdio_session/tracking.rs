//! Request/response accounting and incoming frame classification.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::BytesMut;
use rmcp::model::{ClientJsonRpcMessage, ClientNotification, ClientRequest};
use rmcp::transport::async_rw::{JsonRpcMessageCodec, JsonRpcMessageCodecError};
use serde_json::Value;
use tokio_util::codec::Decoder as _;

/// Requests forwarded to the service and not yet answered, keyed by their
/// JSON-RPC id's JSON text.
#[derive(Default)]
pub(super) struct Tracker {
    pending: HashSet<String>,
    /// The client's `initialize` params, as sent.
    pub(super) initialize: Option<Value>,
}

impl Tracker {
    pub(super) fn forwarded(&mut self, line: &[u8]) {
        // Match rmcp's incoming message type, including its BOM tolerance.
        // A raw method/id pair is insufficient. Lines that do not decode are
        // not pending: the pump answers an invalid request without forwarding
        // it, and the transport drops broken JSON and compatibility-path
        // notifications without a response.
        let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        let Ok(message) = serde_json::from_slice::<ClientJsonRpcMessage>(line) else {
            return;
        };
        match message {
            ClientJsonRpcMessage::Request(request) => {
                let Ok(id) = serde_json::to_string(&request.id) else {
                    return;
                };
                if matches!(request.request, ClientRequest::InitializeRequest(_)) {
                    self.initialize = serde_json::from_slice::<Value>(line)
                        .ok()
                        .and_then(|message| message.get("params").cloned());
                }
                self.pending.insert(id);
            }
            ClientJsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    notification.notification
                    && let Some(id) = cancelled.params.request_id
                    && let Ok(id) = serde_json::to_string(&id)
                {
                    self.pending.remove(&id);
                }
            }
            _ => {}
        }
    }

    pub(super) fn answered(&mut self, line: &[u8]) {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        for message in messages(message) {
            if message.get("method").is_none()
                && (message.get("result").is_some() || message.get("error").is_some())
                && let Some(id) = message.get("id")
            {
                self.pending.remove(&id.to_string());
            }
        }
    }
}

fn messages(message: Value) -> Vec<Value> {
    match message {
        Value::Array(batch) => batch,
        single => vec![single],
    }
}

/// A line rmcp's `AsyncRwTransport::receive` would answer with an id-less
/// `-32600` after consuming it. Broken JSON and the non-standard
/// notification compatibility path are excluded: rmcp writes nothing for
/// those, and answering them would steal the next read.
///
/// Classification goes through rmcp's public line codec, which already
/// decides the compatibility path. A copied method list would drift.
pub(super) fn rmcp_receive_would_reject(line: &[u8]) -> bool {
    let mut codec = JsonRpcMessageCodec::<ClientJsonRpcMessage>::new();
    let mut buffer = BytesMut::from(line);
    match codec.decode(&mut buffer) {
        Err(JsonRpcMessageCodecError::Serde(error)) => matches!(
            error.classify(),
            serde_json::error::Category::Data | serde_json::error::Category::Io
        ),
        _ => false,
    }
}

pub(super) fn stdio_session_is_idle(tracker: &Mutex<Tracker>, undelivered: &AtomicUsize) -> bool {
    undelivered.load(Ordering::SeqCst) == 0 && lock_stdio_tracker(tracker).pending.is_empty()
}

pub(super) fn lock_stdio_tracker(tracker: &Mutex<Tracker>) -> std::sync::MutexGuard<'_, Tracker> {
    tracker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
