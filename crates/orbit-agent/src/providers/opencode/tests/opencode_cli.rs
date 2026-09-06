#![allow(missing_docs)]

use orbit_types::identity::ReasoningEffort;

use super::super::opencode_cli::OpencodeCliTransport;
use super::super::opencode_runtime::OpencodeRuntime;
use crate::runtime::AgentRuntime;
use crate::types::{AgentOperation, AgentRequest};

#[test]
fn args_pass_explicit_model_and_variant() {
    let transport = OpencodeCliTransport::new(
        Some("anthropic/claude-sonnet-4-5".to_string()),
        Some(ReasoningEffort::High),
    );

    assert_eq!(
        transport.args(),
        vec![
            "--model",
            "anthropic/claude-sonnet-4-5",
            "--variant",
            "high"
        ]
    );
}

#[test]
fn args_are_empty_without_a_model_or_effort() {
    assert!(OpencodeCliTransport::new(None, None).args().is_empty());
}

#[test]
fn model_coordinate_names_the_vendor_inside_the_opencode_lane() {
    // `--model provider/model` selects the *model vendor*, never the Orbit
    // executor. No separate provider flag is rendered, and the vendor half of
    // the coordinate must not leak into a second argument. [ORB-11295]
    let transport = OpencodeCliTransport::new(Some("openai/gpt-5".to_string()), None);

    assert_eq!(transport.args(), vec!["--model", "openai/gpt-5"]);
    assert!(!transport.args().iter().any(|arg| arg == "--provider"));
}

#[test]
fn only_the_documented_variant_vocabulary_is_admitted_for_opencode() {
    // OpenCode forwards `--variant` verbatim to the selected model provider and
    // publishes no provider-independent vocabulary, so admission is restricted
    // to the spellings its own help text names. An unsupported effort must fail
    // at configuration time rather than be dropped or remapped at spawn.
    // [ORB-11295]
    for effort in [ReasoningEffort::High, ReasoningEffort::Max] {
        let args = OpencodeCliTransport::new(None, Some(effort)).args();
        assert_eq!(args, vec!["--variant".to_string(), effort.to_string()]);
        assert!(
            effort.validate_for_provider_model("opencode", None).is_ok(),
            "crew effort '{effort}' must be admitted for opencode",
        );
    }

    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::Xhigh,
    ] {
        let error = effort
            .validate_for_provider_model("opencode", Some("anthropic/claude-sonnet-4-5"))
            .expect_err("unsupported effort must be refused");
        assert!(
            error.contains("not remapped"),
            "rejection must state that values are not remapped: {error}",
        );
    }
}

#[test]
fn prompt_travels_on_stdin_and_never_through_argv() {
    let envelope = br#"{"schemaVersion":1,"input":{"secret_path":"/srv/tenant-42"}}"#;
    let transport = OpencodeCliTransport::new(
        Some("anthropic/claude-sonnet-4-5".to_string()),
        Some(ReasoningEffort::Max),
    );

    let stdin = String::from_utf8(transport.stdin(envelope)).expect("utf8 stdin");
    assert!(stdin.contains("/srv/tenant-42"));
    assert!(stdin.contains("Execution envelope:"));

    let argv = transport.args().join(" ");
    assert!(!argv.contains("/srv/tenant-42"));
    assert!(!argv.contains("schemaVersion"));
}

#[test]
fn model_name_reports_the_selected_model() {
    assert_eq!(
        OpencodeCliTransport::new(Some("openai/gpt-5".to_string()), None).model_name(),
        Some("openai/gpt-5")
    );
    assert_eq!(OpencodeCliTransport::new(None, None).model_name(), None);
}

#[test]
fn runtime_requires_state_context_but_never_implicitly_forwards_an_api_key() {
    let runtime = OpencodeRuntime::new(
        "opencode".to_string(),
        Some("anthropic/claude-sonnet-4-5".to_string()),
        Some(ReasoningEffort::High),
        "opencode",
        &["HOME", "PATH"],
    );
    let (invocation, trace) = runtime
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "opencode-auth-env".to_string(),
            },
            envelope_json: br#"{"schemaVersion":1,"input":{}}"#.to_vec(),
            verbose: false,
        })
        .expect("render invocation");

    assert_eq!(invocation.required_env_vars, &["HOME", "PATH"]);
    assert!(
        !invocation
            .required_env_vars
            .iter()
            .any(|var| var.ends_with("API_KEY"))
    );
    // Usage is not fabricated from a stream Orbit reduces away.
    assert_eq!(trace, orbit_types::telemetry::InvocationTrace::default());
}
