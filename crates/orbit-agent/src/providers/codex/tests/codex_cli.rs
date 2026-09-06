#![allow(missing_docs)]

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::codex_cli::CodexCliTransport;

const IMPLEMENT_ACTIVITY: &str =
    include_str!("../../../../../orbit-core/assets/activities/agent_implement.yaml");
const RESPONSE_SCHEMA: &str =
    r#"{"schemaVersion":1,"status":"success|failed|timeout","result":{...},"error":null}"#;
const BASELINE_IMPLEMENT_BYTES: usize = 11_760;
const MAX_IMPLEMENT_TOKENS: usize = 2_500;

#[derive(Deserialize)]
struct ActivityAsset {
    spec: ActivitySpec,
}

#[derive(Deserialize)]
struct ActivitySpec {
    instruction: String,
    tools: Vec<String>,
    model: Option<String>,
}

fn representative_prompt(activity_yaml: &str, input: Value, task: Option<Value>) -> Vec<u8> {
    let asset: ActivityAsset = serde_yaml::from_str(activity_yaml).expect("parse activity asset");
    let prompt = serde_json::to_string(&input).expect("serialize representative prompt");
    let mut envelope = serde_json::Map::new();
    envelope.insert("schemaVersion".to_string(), json!(1));
    envelope.insert("instruction".to_string(), json!(asset.spec.instruction));
    envelope.insert("prompt".to_string(), json!(prompt));
    envelope.insert("input".to_string(), input);
    envelope.insert("run_id".to_string(), json!("jrun-prompt-budget"));
    envelope.insert("tools".to_string(), json!(asset.spec.tools));
    envelope.insert("model".to_string(), json!(asset.spec.model));
    if let Some(task) = task {
        envelope.insert("task".to_string(), task);
    }

    let transport = CodexCliTransport::new(None, None, "workspace-write".to_string(), None, vec![]);
    transport.stdin(&serde_json::to_vec(&envelope).expect("serialize execution envelope"))
}

#[test]
fn representative_activity_prompts_fit_budget_and_preserve_contracts() {
    let workspace = "/tmp/orbit-worktree";
    let implement_input = json!({
        "task_id": "ORB-00001",
        "workspace_path": workspace,
        "repo_root": workspace,
    });
    let task = json!({
        "id": "ORB-00001",
        "status": "in-progress",
        "terminal": false,
        "title": "Representative implementation task",
        "description": "Implement the requested behavior without broadening scope.",
        "acceptance_criteria": ["The focused regression test passes."],
        "plan": "Inspect, implement, and validate the scoped change.",
        "context_files": ["file:src/lib.rs"],
        "tags": ["test"],
        "external_refs": [],
        "workspace_path": workspace,
        "repo_root": workspace,
    });
    let implement = representative_prompt(IMPLEMENT_ACTIVITY, implement_input, Some(task));
    let tokenizer = tiktoken_rs::cl100k_base_singleton();
    let implement_tokens = tokenizer
        .encode_with_special_tokens(std::str::from_utf8(&implement).expect("utf-8 prompt"))
        .len();
    assert!(
        implement_tokens <= MAX_IMPLEMENT_TOKENS,
        "cl100k token estimate grew beyond budget: implement={implement_tokens} max={MAX_IMPLEMENT_TOKENS}",
    );
    assert!(implement.len() < BASELINE_IMPLEMENT_BYTES);
    let implement_text = std::str::from_utf8(&implement).expect("utf-8 implement prompt");
    assert_eq!(
        implement_text.matches(RESPONSE_SCHEMA).count(),
        1,
        "implementation must receive the exact response schema only from the provider renderer"
    );

    for contract in [
        "task.terminal",
        "before the first edit",
        "before validation",
        "pwd -P",
        "git rev-parse --show-toplevel",
        "context_files",
        "not as a perfect inventory",
        "orbit.task.start",
        "move the task to `review`",
        "EPERM",
        "orbit.friction.add",
        "execution_summary",
    ] {
        assert!(
            implement_text.contains(contract),
            "implementation contract disappeared: {contract}"
        );
    }
}

mod args {
    #![allow(missing_docs)]

    use orbit_common::test_fixtures::TEST_CODEX_MODEL;
    use orbit_types::identity::ReasoningEffort;

    use super::super::super::codex_cli::*;

    #[test]
    fn codex_args_use_exec_compatible_approval_config() {
        let transport = CodexCliTransport::new(
            Some(TEST_CODEX_MODEL.to_string()),
            None,
            "workspace-write".to_string(),
            Some("never".to_string()),
            vec!["/tmp/orbit".to_string()],
        );

        assert_eq!(
            transport.args(),
            vec![
                "--config",
                "approval_policy=\"never\"",
                "--model",
                TEST_CODEX_MODEL,
                "--sandbox",
                "workspace-write",
                "--add-dir",
                "/tmp/orbit",
            ]
        );
    }

    #[test]
    fn codex_args_pass_the_resolved_reasoning_effort_as_a_config_override() {
        let transport = CodexCliTransport::new(
            Some(TEST_CODEX_MODEL.to_string()),
            Some(ReasoningEffort::Xhigh),
            "workspace-write".to_string(),
            None,
            vec![],
        );

        assert_eq!(
            transport.args(),
            vec![
                "--model",
                TEST_CODEX_MODEL,
                "--config",
                "model_reasoning_effort=\"xhigh\"",
                "--sandbox",
                "workspace-write",
            ]
        );
    }
}
