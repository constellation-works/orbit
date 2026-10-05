#![allow(missing_docs)]

use crate::providers::normalize_cli_stdout;

use serde_json::json;

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

fn text_block(text: &str) -> serde_json::Value {
    json!({"type": "text", "text": text})
}

fn assistant_message(content: serde_json::Value, stop_reason: &str) -> serde_json::Value {
    json!({
        "role": "assistant",
        "content": content,
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "usage": {"input": 12, "output": 34, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": stop_reason,
        "timestamp": 1_762_000_000_000i64,
    })
}

#[test]
fn a_later_failed_or_malformed_assistant_terminal_frame_invalidates_prior_text() {
    let successful = json!({
        "type":"message_end",
        "message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop"),
    });
    let failed = |stop_reason| {
        json!({
            "type":"message_end",
            "message":assistant_message(json!([text_block("later text")]), stop_reason),
        })
    };

    for terminal in [
        failed("error"),
        failed("aborted"),
        json!({
            "type":"message_end",
            "message":{"role":"assistant","content":[text_block("later text")]},
        }),
        json!({
            "type":"message_end",
            "message":assistant_message(json!([]), "stop"),
        }),
    ] {
        let stdout = format!("{successful}\n{terminal}");
        assert!(
            normalize_cli_stdout("pi", stdout.as_bytes()).is_empty(),
            "later assistant terminal outcome must invalidate prior evidence: {terminal}",
        );
    }
}
