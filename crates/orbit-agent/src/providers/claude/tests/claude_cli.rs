#![allow(missing_docs)]

use serde_json::Value;

use super::super::claude_cli::ClaudeCliTransport;

/// Both packaged executor definitions Orbit ships for claude. The transport's
/// per-request flags and these static arg lists are compiled and read from
/// different places, which is exactly how they drift.
const PACKAGED_EXECUTOR: &str =
    include_str!("../../../../../orbit-core/assets/executors/claude.yaml");
const WORKSPACE_EXECUTOR: &str =
    include_str!("../../../../../../.orbit/resources/executors/claude.yaml");

fn schema_arg(args: &[String]) -> Value {
    let index = args
        .iter()
        .position(|arg| arg == "--json-schema")
        .expect("transport emits --json-schema");
    serde_json::from_str(&args[index + 1]).expect("schema argument is valid JSON")
}

fn static_args(executor_yaml: &str) -> Vec<String> {
    let asset: Value = serde_yaml::from_str(executor_yaml).expect("parse executor asset");
    asset["spec"]["args"]
        .as_array()
        .expect("executor declares args")
        .iter()
        .map(|arg| arg.as_str().expect("arg is a string").to_string())
        .collect()
}

/// [ORB-10746] The prevention layer. Prompt text is guidance a model may
/// ignore — `jrun-20260812-0312-9` ignored it after 89 turns and $3.17 — so
/// the envelope frame has to reach the CLI as a machine constraint.
#[test]
fn transport_constrains_every_invocation_with_the_protocol_schema() {
    let args = ClaudeCliTransport::new(Some("sonnet".to_string()), None).args(false);
    let schema = schema_arg(&args);

    assert_eq!(schema["properties"]["schemaVersion"]["const"], 1);
    assert_eq!(
        schema["properties"]["status"]["enum"],
        serde_json::json!(["success", "failed", "timeout"])
    );
    assert_eq!(schema["properties"]["result"]["type"], "object");
    assert_eq!(schema["properties"]["error"]["type"], "object");
    assert_eq!(
        schema["properties"]["error"]["required"],
        serde_json::json!(["code", "message"])
    );
    assert_eq!(
        schema["required"],
        serde_json::json!(["schemaVersion", "status", "result"])
    );
}

/// The schema must stay inside Anthropic's structured-output subset. A
/// top-level `oneOf`/`allOf`/`anyOf` is rejected with
/// `input_schema does not support oneOf, allOf, or anyOf at the top level`,
/// and that rejection only surfaces mid-run, after the request reaches the
/// API — the most expensive place to learn about a schema mistake.
#[test]
fn protocol_schema_avoids_the_conditional_subschema_keywords_the_provider_rejects() {
    let args = ClaudeCliTransport::new(None, None).args(false);
    let schema = schema_arg(&args);
    let object = schema.as_object().expect("schema is an object");

    for keyword in ["oneOf", "allOf", "anyOf", "if", "then", "else", "not"] {
        assert!(
            !object.contains_key(keyword),
            "top-level `{keyword}` is outside the provider's schema subset"
        );
    }
}

/// The status/error correlation is deliberately absent from the schema and
/// lives in `parse_json_envelope` instead. Pinning that here keeps a future
/// reader from "fixing" the omission and reintroducing the 400.
#[test]
fn protocol_schema_leaves_the_status_error_correlation_to_the_rust_parser() {
    let args = ClaudeCliTransport::new(None, None).args(false);
    let schema = schema_arg(&args);

    assert_eq!(
        schema["properties"]["error"]["type"], "object",
        "error is an object when present; omit it on success rather than emitting null"
    );
    assert!(
        !schema["required"]
            .as_array()
            .expect("required is an array")
            .iter()
            .any(|field| field == "error"),
        "requiring `error` unconditionally would break every success envelope"
    );
}

/// The flag rides on the per-request transport precisely so it cannot drift
/// between the two static definitions. If someone later moves it into one
/// YAML, this fails and points at the other copy.
#[test]
fn neither_packaged_executor_copy_declares_the_flag_the_transport_owns() {
    let packaged = static_args(PACKAGED_EXECUTOR);
    let workspace = static_args(WORKSPACE_EXECUTOR);

    assert_eq!(
        packaged, workspace,
        "the packaged asset and the workspace resource must declare identical static args"
    );
    for args in [&packaged, &workspace] {
        assert!(
            !args.iter().any(|arg| arg == "--json-schema"),
            "--json-schema is emitted per-request by the transport; declaring it statically \
             creates a second copy that can drift"
        );
    }
    // The static args the schema depends on: structured output is only
    // meaningful when the wrapper itself is JSON.
    assert!(packaged.iter().any(|arg| arg == "--output-format"));
    assert!(packaged.iter().any(|arg| arg == "json"));
}

#[test]
fn per_request_toggles_still_compose_with_the_schema_flag() {
    let args = ClaudeCliTransport::new(Some("opus-5".to_string()), None).args(true);

    assert!(args.iter().any(|arg| arg == "--json-schema"));
    assert!(args.iter().any(|arg| arg == "--verbose"));
    let model_index = args
        .iter()
        .position(|arg| arg == "--model")
        .expect("model flag");
    assert_eq!(args[model_index + 1], "claude-opus-5");
}

/// Crew configs write the short `<family>-<version>` form for every Claude
/// tier; the transport expands each to the CLI's `claude-<family>-<version>`
/// id, so the newest tier must not silently pass through unexpanded.
#[test]
fn short_model_forms_expand_to_claude_cli_ids_for_every_tier() {
    for (short, expanded) in [
        ("opus-5", "claude-opus-5"),
        ("sonnet-4.6", "claude-sonnet-4-6"),
        ("fable-5.1", "claude-fable-5-1"),
        ("fable", "fable"),
        ("claude-fable-5-1", "claude-fable-5-1"),
    ] {
        let args = ClaudeCliTransport::new(Some(short.to_string()), None).args(false);
        let model_index = args
            .iter()
            .position(|arg| arg == "--model")
            .expect("model flag");
        assert_eq!(args[model_index + 1], expanded, "{short}");
    }
}

#[test]
fn effort_reaches_claude_cli_for_every_supported_crew_value() {
    use orbit_types::identity::ReasoningEffort;

    for (effort, expected) in [
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Xhigh, "xhigh"),
        (ReasoningEffort::Max, "max"),
    ] {
        let args = ClaudeCliTransport::new(Some("opus".to_string()), Some(effort)).args(false);
        assert_eq!(args[args.len() - 2..], ["--effort", expected], "{expected}");
    }
}
