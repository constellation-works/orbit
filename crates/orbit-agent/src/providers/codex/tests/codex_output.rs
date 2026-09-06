#![allow(missing_docs)]

use crate::providers::project_cli_response;

fn projected(stdout: &str) -> String {
    String::from_utf8(project_cli_response("codex", stdout.as_bytes()).into_owned())
        .expect("utf8 projected response")
}

#[test]
fn command_execution_output_cannot_supply_the_assistant_answer() {
    let stdout = concat!(
        r#"{"type":"thread.started","thread_id":"thread-1"}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"item-0","type":"command_execution","command":"cat fixture.json","aggregated_output":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"claimed\":\"tool-output\"},\"error\":null}","exit_code":0,"status":"completed"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":17,"output_tokens":3}}"#,
        "\n",
    );

    assert!(projected(stdout).is_empty());
}

#[test]
fn final_agent_message_after_tool_traffic_is_the_only_projected_content() {
    let stdout = concat!(
        r#"{"type":"item.completed","item":{"id":"item-0","type":"command_execution","aggregated_output":"{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"fixture\",\"message\":\"not the answer\"}}","exit_code":0,"status":"completed"}}"#,
        "\n",
        r#"{"type":"item.completed","item":{"id":"item-1","type":"agent_message","text":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"assistant\"},\"error\":null}"}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":21,"output_tokens":8}}"#,
        "\n",
    );

    let answer = projected(stdout);
    assert!(answer.contains(r#""source":"assistant""#));
    assert!(!answer.contains("not the answer"));
}

#[test]
fn non_event_output_preserves_the_existing_direct_envelope_fallback() {
    let envelope = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

    assert_eq!(projected(envelope), envelope);
}
