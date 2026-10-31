//! Export of an `orbit.task.artifact.get` answer, shared by every CLI view.
//!
//! The answer is the same whether the tool ran in-process or on the host that
//! holds the task, so the local and the routed renderers read it through
//! [`TaskArtifactRead`] and keep only their own wording.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};

use orbit_common::OrbitError;

/// One artifact read as `orbit.task.artifact.get` answered it.
#[derive(Debug, Clone)]
pub struct TaskArtifactRead {
    /// Task that owns the artifact.
    pub id: String,
    /// Artifact path relative to the task's artifacts directory.
    pub path: String,
    /// Stored media type.
    pub media_type: String,
    /// Stored artifact size in bytes, as the owning host reported it.
    pub size: u64,
    /// The owner's classification: `text`, `image` or `opaque`.
    pub presentation: String,
    content: Option<String>,
    content_base64: String,
}

impl TaskArtifactRead {
    /// Read the fields of an `orbit.task.artifact.get` answer.
    pub fn from_answer(answer: &Value) -> Self {
        let text = |key| {
            answer
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Self {
            id: text("id"),
            path: text("path"),
            media_type: text("media_type"),
            size: answer
                .get("size")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            presentation: text("presentation"),
            content: answer
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            content_base64: text("content_base64"),
        }
    }

    /// The payload when the owner classified it as printable text.
    pub fn printable_text(&self) -> Option<&str> {
        (self.presentation == "text")
            .then_some(self.content.as_deref())
            .flatten()
    }

    /// The JSON document a view reports, naming the file written, if any.
    pub fn json(&self, written_to: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "path": self.path,
            "media_type": self.media_type,
            "size": self.size,
            "presentation": self.presentation,
            "written_to": written_to,
        })
    }

    /// The artifact's bytes: the text payload, else the decoded base64 one.
    pub fn bytes(&self) -> Result<Vec<u8>, OrbitError> {
        if let Some(content) = &self.content {
            return Ok(content.as_bytes().to_vec());
        }
        BASE64_STANDARD
            .decode(&self.content_base64)
            .map_err(|error| OrbitError::Execution(format!("invalid artifact bytes: {error}")))
    }

    /// Write the artifact's bytes to the user-selected `out` file.
    pub fn export_to(&self, out: &Path) -> Result<(), OrbitError> {
        let bytes = self.bytes()?;
        #[allow(
            clippy::disallowed_methods,
            reason = "artifact export targets are user-selected files, symlinks or devices; retain direct output semantics"
        )]
        std::fs::write(out, &bytes).map_err(|error| {
            OrbitError::Io(format!("write artifact to '{}': {error}", out.display()))
        })
    }
}
