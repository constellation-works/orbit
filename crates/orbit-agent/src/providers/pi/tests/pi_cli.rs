#![allow(missing_docs)]

use orbit_types::identity::ReasoningEffort;

use super::super::pi_cli::PiCliTransport;
use super::super::pi_runtime::PiRuntime;
use crate::runtime::AgentRuntime;
use crate::types::{AgentOperation, AgentRequest};

#[test]
fn args_pass_explicit_model_and_thinking_level() {
    let transport = PiCliTransport::new(Some("sonnet".to_string()), Some(ReasoningEffort::High));

    assert_eq!(
        transport.args(),
        vec!["--model", "sonnet", "--thinking", "high"]
    );
}

#[test]
fn args_are_empty_without_a_model_or_effort() {
    assert!(PiCliTransport::new(None, None).args().is_empty());
}

#[test]
fn model_pattern_may_carry_a_vendor_prefix_without_a_separate_provider_flag() {
    let transport = PiCliTransport::new(Some("openai/gpt-4o".to_string()), None);

    assert_eq!(transport.args(), vec!["--model", "openai/gpt-4o"]);
    assert!(!transport.args().iter().any(|arg| arg == "--provider"));
}

#[test]
fn every_crew_effort_renders_a_thinking_level_pi_documents() {
    // Pi's `--thinking` validates against a closed set and errors on anything
    // outside it, so an Orbit effort that did not appear there would fail the
    // invocation rather than be ignored. This pins the whole crew vocabulary
    // against the documented Pi levels. [ORB-11296]
    const PI_THINKING_LEVELS: &[&str] =
        &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
    ] {
        let args = PiCliTransport::new(None, Some(effort)).args();
        assert_eq!(args[0], "--thinking");
        assert!(
            PI_THINKING_LEVELS.contains(&args[1].as_str()),
            "effort '{effort}' must render a documented Pi thinking level",
        );
        assert!(
            effort.validate_for_provider_model("pi", None).is_ok(),
            "crew effort '{effort}' must be admitted for pi",
        );
    }
}

#[test]
fn prompt_travels_on_stdin_and_never_through_argv() {
    let envelope = br#"{"schemaVersion":1,"input":{"secret_path":"/srv/tenant-42"}}"#;
    let transport = PiCliTransport::new(Some("sonnet".to_string()), Some(ReasoningEffort::Max));

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
        PiCliTransport::new(Some("sonnet".to_string()), None).model_name(),
        Some("sonnet")
    );
    assert_eq!(PiCliTransport::new(None, None).model_name(), None);
}

#[test]
fn runtime_requires_state_context_but_never_implicitly_forwards_an_api_key() {
    let runtime = PiRuntime::new(
        "pi".to_string(),
        Some("sonnet".to_string()),
        Some(ReasoningEffort::Medium),
        "pi",
        &["HOME", "PATH"],
    );
    let (invocation, trace) = runtime
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "pi-auth-env".to_string(),
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
    assert!(!invocation.args.iter().any(|arg| arg == "--api-key"));
    // Usage is not fabricated from a stream Orbit reduces away.
    assert_eq!(trace, orbit_types::telemetry::InvocationTrace::default());
}
