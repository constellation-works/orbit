use rmcp::model::{CallToolResult, Content};
use serde_json::{Value, json};

/// Keep MCP `structuredContent` object-shaped for clients that enforce record
/// results (notably Cursor and VS Code), while preserving non-object payloads.
pub(super) fn mcp_structured_content(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        Value::Array(items) => json!({ "items": items }),
        value => json!({ "value": value }),
    }
}

/// Turn one host tool result into the MCP result the client receives.
///
/// Every result carries `structuredContent`. An artifact read that the artifact
/// policy classified as a safe image *also* carries an `image` content block,
/// because base64 inside a JSON field is a string to a model — only a protocol
/// image block reaches its vision input. That is the whole point of the read
/// surface: a worker should be able to look at a stored reference, not read a
/// description of one.
///
/// The decision is made on the payload's own shape rather than on the tool
/// name, so the transport never has to be taught about individual tools and
/// cannot disagree with the classification the artifact owner already made.
/// Anything not classified `image` — SVG, HTML, an octet-stream, or bytes that
/// contradict their declared media type — stays structured-only and is never
/// handed to a renderer.
pub(super) fn mcp_tool_call_result(value: Value) -> CallToolResult {
    let Some(image) = artifact_image_content(&value) else {
        return CallToolResult::structured(mcp_structured_content(value));
    };
    let summary = artifact_summary_text(&value);
    let structured = mcp_structured_content(value);
    // `CallToolResult::structured` mirrors the whole payload into a text block.
    // For an image read that would spend the base64 twice — once as text the
    // model cannot see anything in, once as the image block it can. Replace the
    // mirror with a one-line summary and keep the bytes in exactly two places:
    // the image block, and `structuredContent` for a text-only client.
    let mut result = CallToolResult::structured(structured);
    result.content = vec![Content::text(summary), image];
    result
}

/// A one-line stand-in for the JSON text mirror of an image read.
fn artifact_summary_text(value: &Value) -> String {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    };
    let size = value
        .get("size")
        .and_then(Value::as_u64)
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "Task artifact {} on {}: {}, {} bytes, attached below as an image.",
        field("path"),
        field("id"),
        field("media_type"),
        size,
    )
}

/// Extract the image block from an artifact read payload, if it is one.
fn artifact_image_content(value: &Value) -> Option<Content> {
    let object = value.as_object()?;
    if object.get("presentation")?.as_str()? != "image" {
        return None;
    }
    if object.get("encoding")?.as_str()? != "base64" {
        return None;
    }
    let data = object.get("content_base64")?.as_str()?;
    let media_type = object.get("media_type")?.as_str()?;
    Some(Content::image(data, media_type))
}
