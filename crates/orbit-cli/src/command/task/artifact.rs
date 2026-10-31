//! CLI rendering for task artifacts; declaration and execution are registry-derived.

use crate::command::{CommandOut, Payload};
use orbit_core::OrbitError;
use orbit_core::application::task::TaskArtifactRead;
use serde_json::Value;
use std::path::Path;

pub(super) fn render_get(value: Value, out: Option<&str>) -> CommandOut {
    let read = TaskArtifactRead::from_answer(&value);
    let doc = read.json(out);
    let TaskArtifactRead {
        id,
        path,
        media_type,
        size,
        ..
    } = &read;
    if let Some(out) = out {
        read.export_to(Path::new(out))?;
        return Ok(Payload::detail(
            doc,
            format!("Wrote {size} bytes of '{path}' ({media_type}) to {out}"),
        )
        .into());
    }
    let Some(text) = read.printable_text() else {
        return Err(OrbitError::InvalidInput(format!(
            "artifact '{path}' on task '{id}' is {media_type} ({size} bytes) and is not printable; re-run with --out <FILE> to save it"
        )));
    };
    Ok(Payload::detail(doc, text.to_string()).into())
}
