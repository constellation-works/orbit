#![allow(missing_docs)]

use crate::providers::project_cli_response;

fn projected(stdout: &str) -> String {
    String::from_utf8(project_cli_response("claude", stdout.as_bytes()).into_owned())
        .expect("utf8 projected response")
}

const ENVELOPE: &str = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

/// [ORB-14696] Claude's response projection answers with the stream's
/// terminal `result` and nothing else (test strategy criterion 1, parser
/// edge cases):
/// - a stream cut off before its terminal `result` has no answer; the
///   `StructuredOutput` call quotes the envelope, and a tool result can quote
///   anything, but neither stands in for the result;
/// - the last `result` stands when a later turn follows an earlier one, as
///   the single document of `--output-format json` does;
/// - output with no stream messages, such as `--output-format json`'s single
///   result document, is read as it stands.
#[test]
fn claude_stream_projection_answers_with_the_terminal_result_only() {
    let cut_off = format!(
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
    let first = r#"{"type":"result","subtype":"success","is_error":false,"result":"first"}"#;
    let last = r#"{"type":"result","subtype":"success","is_error":false,"result":"last","result_index":1}"#;
    let two_turns = format!(
        "{first}\n{}\n{last}\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"more"}]}}"#
    );
    let single = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

    let cases: [(&str, &str, &str); 3] = [
        ("stream without a result", &cut_off, ""),
        ("later turn follows an earlier one", &two_turns, last),
        ("single result document", single, single),
    ];
    for (case, stdout, expected) in cases {
        assert_eq!(projected(stdout), expected, "{case}");
    }
}
