#![allow(missing_docs)]

use super::super::claude_runtime::ClaudeRuntime;
use crate::runtime::AgentRuntime;
use crate::types::{AgentOperation, AgentRequest};

/// [ORB-13664] A headless leaf that backgrounds its validation gates is forced
/// to report before they finish, so every claude invocation disables
/// background tasks at the process level rather than through prompt text.
#[test]
fn every_invocation_disables_background_tasks() {
    let runtime = ClaudeRuntime::new(
        "claude".to_string(),
        Some("opus".to_string()),
        None,
        "claude",
        &["HOME", "PATH"],
    );
    let (invocation, _trace) = runtime
        .invoke(AgentRequest {
            operation: AgentOperation::Activity {
                activity_id: "claude-background-env".to_string(),
            },
            envelope_json: br#"{"schemaVersion":1,"input":{}}"#.to_vec(),
            verbose: false,
        })
        .expect("render invocation");

    assert_eq!(
        invocation.fixed_env,
        &[("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")]
    );
}
