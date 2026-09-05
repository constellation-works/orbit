use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_types::identity::ReasoningEffort;

use crate::agent::{Agent, AgentConfig};
use crate::types::{AgentOperation, AgentRequest};

/// Builds the config the same way real dispatch does: resolve the `agy`
/// factory from the CLI name, then attach crew-resolved effort. [ORB-11320]
fn antigravity_config(model: &str, effort: ReasoningEffort) -> Result<AgentConfig, OrbitError> {
    AgentConfig::from_cli_config("agy", Some(model), &HashMap::new())?
        .with_reasoning_effort(Some(effort))
}

#[test]
fn supported_antigravity_effort_reaches_the_agy_invocation() {
    let cfg = antigravity_config("gemini-3.8-flash-high", ReasoningEffort::High)
        .expect("low/medium/high must be admitted for a *-high model slug");

    let agent = Agent::new(&cfg).expect("agy runtime should build from the admitted config");
    let (invocation, _) = agent
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "agy-effort-regression".to_string(),
            },
            envelope_json: br#"{"schemaVersion":1,"input":{}}"#.to_vec(),
            verbose: false,
        })
        .expect("invoke should render an invocation spec");

    assert!(
        invocation
            .args
            .windows(2)
            .any(|pair| pair == ["--effort", "high"]),
        "expected --effort high in {:?}",
        invocation.args
    );
}

#[test]
fn unsupported_antigravity_effort_is_rejected_at_config_admission() {
    let err = antigravity_config("gemini-3.8-flash-high", ReasoningEffort::Xhigh)
        .expect_err("xhigh is outside the antigravity crew vocabulary");

    assert!(
        matches!(err, OrbitError::InvalidInput(ref message) if message.contains("xhigh")),
        "{err}"
    );
}

#[test]
fn legacy_gemini_cli_model_id_is_rejected_at_config_admission() {
    let err = antigravity_config("gemini-3.8-flash", ReasoningEffort::Low)
        .expect_err("bare gemini CLI model ids are not agy model slugs");

    assert!(
        matches!(err, OrbitError::InvalidInput(ref message) if message.contains("gemini-3.8-flash")),
        "{err}"
    );
}
