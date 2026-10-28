//! Desktop mutation receipts and their legacy note reader.

use serde::{Deserialize, Serialize};

use super::TaskDecision;
use crate::task::TaskHistoryEntry;

/// Durable identity and payload binding of an applied desktop mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopMutationReceipt {
    /// Client request identity, compared exactly on replay.
    pub request_id: String,
    /// Digest of the redacted request payload. Empty only for a legacy
    /// note-only PR closure marker, which recorded no payload binding.
    pub payload_digest: String,
}

/// A desktop history decision cannot be used as an idempotency receipt.
#[derive(Debug, thiserror::Error)]
pub enum DesktopMutationReceiptError {
    /// A different typed decision is authoritative and cannot be read as text.
    #[error("desktop history carries a mismatched decision")]
    MismatchedDecision,
    /// A legacy receipt identifies this request but has no readable digest.
    #[error("request identity reused with a different payload")]
    PayloadMismatch,
}

impl DesktopMutationReceipt {
    /// Read this request's receipt. Typed decisions take precedence even
    /// when their note names another request; a foreign decision never
    /// falls through to legacy text. Callers select the history event.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
        request_id: &str,
    ) -> Result<Option<Self>, DesktopMutationReceiptError> {
        match &entry.decision {
            Some(TaskDecision::DesktopMutation(receipt)) => {
                Ok((receipt.request_id == request_id).then(|| receipt.clone()))
            }
            Some(_) => Err(DesktopMutationReceiptError::MismatchedDecision),
            None => Self::from_legacy_note(entry, request_id),
        }
    }

    /// The single compatibility reader for pre-typed mutation receipts and
    /// PR closure markers. Preserve the original exact note binding: any
    /// suffix other than the expected digest still refuses mutation replay.
    fn from_legacy_note(
        entry: &TaskHistoryEntry,
        request_id: &str,
    ) -> Result<Option<Self>, DesktopMutationReceiptError> {
        let Some(note) = entry.note.as_deref() else {
            return Ok(None);
        };
        let key = format!("desktop_request={request_id}");
        let payload_digest = if entry.event == "desktop_pr_closure" {
            if note != key {
                return Ok(None);
            }
            String::new()
        } else {
            let Some(suffix) = note.strip_prefix(&format!("{key} ")) else {
                return Ok(None);
            };
            suffix
                .strip_prefix("digest=")
                .ok_or(DesktopMutationReceiptError::PayloadMismatch)?
                .to_string()
        };
        Ok(Some(Self {
            request_id: request_id.into(),
            payload_digest,
        }))
    }
}
