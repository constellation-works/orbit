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
