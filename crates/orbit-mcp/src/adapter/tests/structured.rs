use super::super::structured::mcp_structured_content;
use serde_json::json;

#[test]
fn mcp_structured_content_preserves_existing_objects() {
    let value = json!({ "ok": true });
    assert_eq!(mcp_structured_content(value.clone()), value);
}

mod artifact_image_content {
    use rmcp::model::RawContent;
    use serde_json::json;

    use super::super::super::structured::mcp_tool_call_result;

    fn artifact_payload(presentation: &str, media_type: &str) -> serde_json::Value {
        json!({
            "id": "ORB-00042",
            "path": "diagrams/flow.png",
            "media_type": media_type,
            "size": 12,
            "presentation": presentation,
            "encoding": "base64",
            "content_base64": "iVBORw0KGgo=",
        })
    }

    #[test]
    fn an_image_artifact_read_carries_a_protocol_image_block_beside_its_structured_payload() {
        let result = mcp_tool_call_result(artifact_payload("image", "image/png"));

        // structuredContent still carries the full record for a client that
        // wants the metadata or the raw bytes.
        let structured = result.structured_content.expect("structured payload");
        assert_eq!(structured["path"], "diagrams/flow.png");

        // The image block is what actually reaches a multimodal client's vision
        // input; base64 in a JSON string would only ever be read as text.
        assert_eq!(result.content.len(), 2);
        match &result.content[1].raw {
            RawContent::Image(image) => {
                assert_eq!(image.mime_type, "image/png");
                assert_eq!(image.data, "iVBORw0KGgo=");
            }
            other => panic!("expected an image content block, got {other:?}"),
        }

        // The accompanying text is a summary, not a second copy of the payload.
        match &result.content[0].raw {
            RawContent::Text(text) => {
                assert!(text.text.contains("diagrams/flow.png"), "{}", text.text);
                assert!(
                    !text.text.contains("iVBORw0KGgo="),
                    "the base64 payload must not be spent twice: {}",
                    text.text
                );
            }
            other => panic!("expected a summary text block, got {other:?}"),
        }
    }

    #[test]
    fn an_opaque_artifact_read_never_becomes_an_image_block() {
        // SVG is the case that matters: it is an image format the transport
        // must still refuse to present as a viewable image.
        let result = mcp_tool_call_result(artifact_payload("opaque", "image/svg+xml"));
        assert!(
            !result
                .content
                .iter()
                .any(|content| matches!(content.raw, RawContent::Image(_))),
            "opaque payloads must not reach a renderer"
        );
        assert!(result.structured_content.is_some());
    }

    #[test]
    fn ordinary_tool_results_are_unchanged() {
        let result = mcp_tool_call_result(json!({ "id": "ORB-00042", "status": "done" }));
        // Unchanged from the default: one text mirror of the JSON payload.
        assert_eq!(result.content.len(), 1);
        assert!(matches!(result.content[0].raw, RawContent::Text(_)));
        assert_eq!(
            result.structured_content.expect("structured payload")["status"],
            "done"
        );
    }
}
