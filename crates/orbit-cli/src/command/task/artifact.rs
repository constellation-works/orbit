//! CLI rendering for task artifacts; declaration and execution are registry-derived.

use crate::command::{CommandOut, Payload};
use base64::Engine as _;
use orbit_core::OrbitError;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn render_get(value: Value, out: Option<&str>) -> CommandOut {
    let text = |key| value.get(key).and_then(Value::as_str).unwrap_or_default();
    let id = text("id");
    let path = text("path");
    let media_type = text("media_type");
    let size = value
        .get("size")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let doc = json!({ "id": id, "path": path, "media_type": media_type,
        "size": size, "presentation": text("presentation"), "written_to": out });
    if let Some(out) = out {
        let bytes = if let Some(content) = value.get("content").and_then(Value::as_str) {
            content.as_bytes().to_vec()
        } else {
            base64::engine::general_purpose::STANDARD
                .decode(text("content_base64"))
                .map_err(|error| {
                    OrbitError::Execution(format!("invalid artifact bytes: {error}"))
                })?
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "artifact export targets are user-selected files, symlinks or devices; retain direct output semantics"
        )]
        std::fs::write(Path::new(out), &bytes)
            .map_err(|error| OrbitError::Io(format!("write artifact to '{out}': {error}")))?;
        return Ok(Payload::detail(
            doc,
            format!("Wrote {size} bytes of '{path}' ({media_type}) to {out}"),
        )
        .into());
    }
    if text("presentation") != "text" {
        return Err(OrbitError::InvalidInput(format!(
            "artifact '{path}' on task '{id}' is {media_type} ({size} bytes) and is not printable; re-run with --out <FILE> to save it"
        )));
    }
    Ok(Payload::detail(doc, text("content").to_string()).into())
}
