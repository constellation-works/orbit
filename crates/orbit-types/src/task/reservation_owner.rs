//! Typed reservation owner metadata: the document persisted in the
//! `task_reservations.owner_metadata_json` TEXT column and carried in
//! process as [`ReservationOwnerMetadata`].
//!
//! The column and its JSON keys are unchanged. Known keys are typed
//! fields; any key this type does not name is preserved in `extra`
//! through the flattened remainder, so rows written by older binaries
//! still deserialize and new rows remain readable by them. The store
//! edge is the single place that encodes/decodes the TEXT column (see
//! [`ReservationOwnerMetadata::parse_owner_metadata`] /
//! [`ReservationOwnerMetadata::encode_owner_metadata`]); every
//! in-process carrier holds the struct.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Owner metadata for a task reservation / reservation owner context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReservationOwnerMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_child_admission: Option<PipelineChildAdmissionMeta>,
    /// Keys this type does not name, preserved verbatim on round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The durable child-pipeline admission recorded in owner metadata:
/// whether a managed activity may submit a child pipeline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineChildAdmissionMeta {
    pub action: String,
    #[serde(default)]
    pub parent_step_id: Option<String>,
    pub blocking: bool,
}

impl ReservationOwnerMetadata {
    /// The single accessor both admission readers use. An admission is
    /// present only as a fully typed value: a stored document whose
    /// `pipeline_child_admission` is missing `action` or `blocking`
    /// fails to parse at the boundary where this struct is built, so
    /// no reader can treat a malformed admission as "no admission".
    pub fn child_admission(&self) -> Option<&PipelineChildAdmissionMeta> {
        self.pipeline_child_admission.as_ref()
    }

    /// Parse the persisted TEXT-column form once, at the store edge.
    pub fn parse_owner_metadata(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    /// Encode to the persisted TEXT-column form, at the store edge.
    pub fn encode_owner_metadata(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
