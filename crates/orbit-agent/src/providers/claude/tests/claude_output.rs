#![allow(missing_docs)]

use crate::providers::project_cli_response;

fn projected(stdout: &str) -> String {
    String::from_utf8(project_cli_response("claude", stdout.as_bytes()).into_owned())
        .expect("utf8 projected response")
}

const ENVELOPE: &str = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

/// [ORB-14696] A stream cut off before its terminal `result` has no answer.
/// The `StructuredOutput` call quotes the envelope, and a tool result can
/// quote anything; neither stands in for the result.
#[test]
fn a_stream_without_a_result_projects_to_nothing() {
    let stdout = format!(
        concat!(
            r#"{{"type":"system","subtype":"init","session_id":"s"}}"#,
            "\n",
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"StructuredOutput","input":{envelope}}}],"usage":{{"input_tokens":2,"output_tokens":9}}}}}}"#,
            "\n",
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":{quoted}}}]}}}}"#,
            "\n",
        ),
        envelope = ENVELOPE,
        quoted = serde_json::to_string(ENVELOPE).unwrap(),
    );

    assert!(projected(&stdout).is_empty());
}

/// The last `result` stands when a later turn follows an earlier one, as the
/// single document of `--output-format json` does.
#[test]
fn the_last_result_of_a_stream_is_the_answer() {
    let first = r#"{"type":"result","subtype":"success","is_error":false,"result":"first"}"#;
    let last = r#"{"type":"result","subtype":"success","is_error":false,"result":"last","result_index":1}"#;
    let stdout = format!(
        "{first}\n{}\n{last}\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"more"}]}}"#
    );

    assert_eq!(projected(&stdout), last);
}

/// Output with no stream messages, such as `--output-format json`'s single
/// result document, is read as it stands.
#[test]
fn a_single_result_document_is_unchanged() {
    let stdout = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

    assert_eq!(projected(stdout), stdout);
}
