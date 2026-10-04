use super::latest_assistant_message;

#[test]
fn the_newest_assistant_message_is_read_from_each_output_shape() {
    let envelope =
        r#"{"schemaVersion":1,"status":"success","result":{"summary":"ok"},"error":null}"#;
    let codex_stream = [
        r#"{"type":"thread.started","thread_id":"t"}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Reading the config."}}"#,
        r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#,
        r#"{"type":"item.started","item":{"type":"command_execution","command":"ls"}}"#,
    ]
    .join("\n");
    let codex_without_message = r#"{"type":"thread.started","thread_id":"t"}"#;
    let claude_result =
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Done."}"#;
    let claude_structured = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{envelope}}}"#
    );
    let assistant_stream = [
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"First."}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Second"},{"type":"tool_use","name":"Bash"},{"type":"text","text":"part."}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"quoted"}]}}"#,
    ]
    .join("\n");
    let envelope_after_chatter = format!("{{\"type\":\"system\"}}\n{envelope}\n");

    let cases: [(&str, &str, Option<&str>); 9] = [
        ("codex", &codex_stream, Some("Reading the config.")),
        ("codex", codex_without_message, None),
        ("claude", claude_result, Some("Done.")),
        ("claude", &claude_structured, Some(envelope)),
        ("claude", &assistant_stream, Some("Second\npart.")),
        ("claude", &envelope_after_chatter, Some(envelope)),
        ("claude", envelope, Some(envelope)),
        ("claude", "  the host is fine\n", Some("the host is fine")),
        ("claude", "\n", None),
    ];
    for (provider, stdout, expected) in cases {
        assert_eq!(
            latest_assistant_message(provider, stdout.as_bytes()).as_deref(),
            expected,
            "{provider} output: {stdout}"
        );
    }
}
mod http_body;
pub(crate) mod http_fixture;
