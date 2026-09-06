#![allow(missing_docs)]

use std::time::Duration;

use orbit_types::identity::ReasoningEffort;

use super::super::antigravity_cli::{
    AntigravityCliTransport, PRINT_TIMEOUT_SHUTDOWN_MARGIN, apply_antigravity_print_timeout,
    derived_antigravity_print_timeout, format_antigravity_print_timeout,
};
use super::super::antigravity_runtime::AntigravityRuntime;
use crate::runtime::AgentRuntime;
use crate::types::{AgentOperation, AgentRequest};

fn print_timeout_values(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        if let Some(value) = args[idx].strip_prefix("--print-timeout=") {
            values.push(value.to_string());
            idx += 1;
            continue;
        }
        if args[idx] == "--print-timeout"
            && let Some(value) = args.get(idx + 1)
        {
            values.push(value.clone());
            idx += 2;
            continue;
        }
        idx += 1;
    }
    values
}

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

#[test]
fn long_budget_print_timeout_exceeds_the_documented_five_minute_default() {
    let remaining = Duration::from_secs(3 * 60 * 60);
    let derived = derived_antigravity_print_timeout(remaining);
    assert_eq!(derived, remaining - PRINT_TIMEOUT_SHUTDOWN_MARGIN);
    assert!(derived > Duration::from_secs(5 * 60));
    assert_eq!(format_antigravity_print_timeout(derived), "2h59m30s");

    let mut args = vec![
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--json-schema".to_string(),
        "{}".to_string(),
    ];
    apply_antigravity_print_timeout("antigravity", &mut args, remaining);
    assert_eq!(print_timeout_values(&args), vec!["2h59m30s".to_string()]);
}

#[test]
fn short_budget_uses_the_remaining_deadline_when_the_margin_does_not_fit() {
    let remaining = Duration::from_secs(10);
    let derived = derived_antigravity_print_timeout(remaining);
    assert_eq!(derived, remaining);
    assert_eq!(format_antigravity_print_timeout(derived), "10s");

    let mut args = Vec::new();
    apply_antigravity_print_timeout("antigravity", &mut args, remaining);
    assert_eq!(print_timeout_values(&args), vec!["10s".to_string()]);
}

#[test]
fn minute_scale_budget_subtracts_the_shutdown_margin() {
    let remaining = Duration::from_secs(60);
    let derived = derived_antigravity_print_timeout(remaining);
    assert_eq!(derived, Duration::from_secs(30));
    let mut args = Vec::new();
    apply_antigravity_print_timeout("agy", &mut args, remaining);
    assert_eq!(print_timeout_values(&args), vec!["30s".to_string()]);
}

#[test]
fn shorter_executor_print_timeout_is_preserved_without_a_duplicate_flag() {
    let mut args = vec![
        "--print-timeout".to_string(),
        "2m".to_string(),
        "--model".to_string(),
        "gemini-3.8-flash-high".to_string(),
    ];
    apply_antigravity_print_timeout("antigravity", &mut args, Duration::from_secs(3 * 60 * 60));
    assert_eq!(print_timeout_values(&args), vec!["2m".to_string()]);
    assert_eq!(
        args,
        vec![
            "--print-timeout".to_string(),
            "2m".to_string(),
            "--model".to_string(),
            "gemini-3.8-flash-high".to_string(),
        ]
    );
}

#[test]
fn longer_executor_print_timeout_is_capped_to_the_derived_budget() {
    let mut args = vec![
        "--print-timeout=4h".to_string(),
        "--effort".to_string(),
        "high".to_string(),
    ];
    apply_antigravity_print_timeout("antigravity", &mut args, Duration::from_secs(3 * 60 * 60));
    assert_eq!(args[0], "--print-timeout=2h59m30s");
    assert_eq!(print_timeout_values(&args), vec!["2h59m30s".to_string()]);
}

#[test]
fn duplicate_print_timeout_flags_are_collapsed() {
    let mut args = vec![
        "--print-timeout".to_string(),
        "4h".to_string(),
        "--print-timeout=15m".to_string(),
    ];
    apply_antigravity_print_timeout("antigravity", &mut args, Duration::from_secs(3 * 60 * 60));
    assert_eq!(
        args,
        vec!["--print-timeout".to_string(), "2h59m30s".to_string()]
    );
}

#[test]
fn other_providers_are_not_given_a_print_timeout() {
    let mut args = vec!["--json".to_string()];
    apply_antigravity_print_timeout("claude", &mut args, Duration::from_secs(3600));
    apply_antigravity_print_timeout("gemini", &mut args, Duration::from_secs(3600));
    assert_eq!(args, vec!["--json".to_string()]);
}
