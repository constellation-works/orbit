#![allow(missing_docs)]

use orbit_types::identity::ReasoningEffort;

use super::super::antigravity_cli::AntigravityCliTransport;
use super::super::antigravity_runtime::AntigravityRuntime;
use crate::runtime::AgentRuntime;
use crate::types::{AgentOperation, AgentRequest};

#[test]
fn args_pass_model_effort_and_generated_schema() {
    let transport = AntigravityCliTransport::new(
        Some("gemini-3.8-flash-high".to_string()),
        Some(ReasoningEffort::High),
    );
    let args = transport.args();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--model", "gemini-3.8-flash-high"])
    );
    assert!(args.windows(2).any(|pair| pair == ["--effort", "high"]));
    assert!(args.windows(1).any(|pair| pair == ["--json-schema"]));
}

#[test]
fn prompt_travels_as_a_stream_json_user_event_and_never_through_argv() {
    let envelope = br#"{"schemaVersion":1,"input":{"secret_path":"/srv/tenant-42"}}"#;
    let transport = AntigravityCliTransport::new(Some("gemini-3.8-flash-high".to_string()), None);

    let stdin = String::from_utf8(transport.stdin(envelope)).expect("utf8 stdin");
    let event: serde_json::Value = serde_json::from_str(stdin.trim()).expect("user event json");
    assert_eq!(event["event"], "user");
    let content = event["message"]["content"].as_str().expect("content");
    assert!(content.contains("/srv/tenant-42"));
    assert!(content.contains("Execution envelope:"));

    let argv = transport.args().join(" ");
    assert!(!argv.contains("/srv/tenant-42"));
    assert!(!argv.contains("Execution envelope:"));
}

#[test]
fn runtime_requires_state_context_but_never_implicitly_forwards_credentials() {
    let runtime = AntigravityRuntime::new(
        "agy".to_string(),
        Some("gemini-3.8-flash-high".to_string()),
        None,
        "agy",
        &["HOME", "PATH"],
    );
    let (invocation, _) = runtime
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "agy-auth-env".to_string(),
            },
            envelope_json: br#"{"schemaVersion":1,"input":{}}"#.to_vec(),
            verbose: false,
        })
        .expect("render invocation");

    assert_eq!(invocation.required_env_vars, &["HOME", "PATH"]);
    assert!(!invocation.args.iter().any(|arg| arg == "--login"));
    assert!(!invocation.program.contains("gemini"));
}
