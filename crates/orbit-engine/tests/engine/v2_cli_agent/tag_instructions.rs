//! An agent dispatch appends the tag instructions its host resolves after the
//! activity's built-in instruction, as one labelled section, and records what
//! it applied; with none the prompt is unchanged, and a refusal stops the
//! dispatch before the provider runs.

use std::sync::Mutex;

use orbit_types::workflow::activity_job::V2AuditEventKind;
use orbit_types::workflow::{
    TagInstruction, TagInstructionRole, TagInstructionSource, render_tag_instructions,
};
use serde_json::json;

use super::*;

struct TagHost {
    cli: ScriptHost,
    instructions: Result<Vec<TagInstruction>, String>,
    asked: Mutex<Vec<(String, Value)>>,
}

impl orbit_engine::test_support::NullRuntimeHost for TagHost {
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        RuntimeHost::agent_subprocess_environment(&self.cli, required_env_vars)
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        RuntimeHost::run_deterministic(&self.cli, action, config, input, tool_context)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        RuntimeHost::resolve_cli_executor(&self.cli, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        RuntimeHost::tool_context_for_activity(
            &self.cli,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }

    fn tag_instructions_for_activity(
        &self,
        _run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<TagInstruction>, DispatchError> {
        self.asked
            .lock()
            .unwrap()
            .push((activity_name.to_string(), input.clone()));
        self.instructions
            .clone()
            .map_err(DispatchError::CliInvocationPermanent)
    }
}

/// Dispatch `agent_implement` through a fake provider that saves its prompt.
fn dispatch(
    instructions: Result<Vec<TagInstruction>, String>,
) -> (
    Result<orbit_engine::DispatchOutcome, DispatchError>,
    Option<String>,
    Vec<V2AuditEvent>,
    TagHost,
) {
    let temp = tempfile::tempdir().unwrap();
    let prompt = temp.path().join("prompt");
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > '{}'\necho '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            prompt.display()
        ),
    )
    .unwrap();
    let (writer, store) = build_writer(temp.path(), "tag-instructions").unwrap();
    let host = TagHost {
        cli: ScriptHost::new(fake.cli_path()),
        instructions,
        asked: Mutex::new(Vec::new()),
    };
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "agent_implement",
        spec: &ActivityV2Spec::AgentLoop(cli_agent_loop_spec(None)),
        fs_profile: None,
        input: json!({ "task_id": "fixture-task", "prompt": "hello" }),
        audit: writer,
        run_id: "tag-instructions",
        host: Some(&host),
    });
    let events = events_snapshot(&store, "tag-instructions").unwrap();
    (outcome, fs::read_to_string(prompt).ok(), events, host)
}

/// The `instruction` of the execution envelope the provider received.
fn instruction_of(prompt: Option<String>) -> String {
    let prompt = prompt.expect("the provider received a prompt");
    let envelope = prompt
        .split_once("Execution envelope:\n")
        .expect("the prompt carries an execution envelope")
        .1;
    let envelope: Value = serde_json::from_str(envelope.trim()).expect("the envelope is JSON");
    envelope["instruction"].as_str().unwrap().to_string()
}

fn applied(events: &[V2AuditEvent]) -> Vec<&V2AuditEventKind> {
    events
        .iter()
        .map(|event| &event.kind)
        .filter(|kind| matches!(kind, V2AuditEventKind::TagInstructionsApplied { .. }))
        .collect()
}

#[test]
fn dispatch_appends_resolved_tag_instructions_after_the_built_in_instruction() {
    let instruction = TagInstruction::admit(
        "ui",
        TagInstructionRole::Implement,
        TagInstructionSource::Default,
        "fixture",
        b"Attach a screenshot of each changed view.\n",
    )
    .unwrap();
    let (outcome, prompt, events, host) = dispatch(Ok(vec![instruction.clone()]));
    assert!(outcome.unwrap().success);
    let asked = host.asked.lock().unwrap();
    assert_eq!(
        (asked[0].0.as_str(), &asked[0].1["task_id"]),
        ("agent_implement", &json!("fixture-task")),
        "the host resolves for the dispatched activity and its task"
    );
    assert_eq!(
        instruction_of(prompt),
        format!(
            "cli smoke\n\n{}",
            render_tag_instructions(std::slice::from_ref(&instruction))
        ),
        "the section follows the built-in instruction, once"
    );
    let applied = applied(&events);
    assert!(
        matches!(applied.as_slice(), [V2AuditEventKind::TagInstructionsApplied {
            activity_name, task_id, instructions, ..
        }] if activity_name == "agent_implement"
            && task_id == "fixture-task"
            && instructions == &vec![instruction.record()]),
        "the dispatch records what it applied: {applied:?}"
    );
}

#[test]
fn dispatch_without_tag_instructions_leaves_the_prompt_unchanged() {
    let (outcome, prompt, events, _) = dispatch(Ok(Vec::new()));
    assert!(outcome.unwrap().success);
    assert_eq!(
        instruction_of(prompt),
        "cli smoke",
        "no section without instructions"
    );
    assert!(
        applied(&events).is_empty(),
        "nothing applied, nothing recorded"
    );
}

#[test]
fn a_refused_tag_instruction_stops_the_dispatch_before_the_provider_runs() {
    let (outcome, prompt, events, _) =
        dispatch(Err("tag_instruction_too_large: fixture refusal".to_string()));
    let refused = match outcome {
        Err(error) => error.to_string(),
        Ok(outcome) => {
            assert!(!outcome.success, "a refusal never succeeds");
            outcome.message.unwrap_or_default()
        }
    };
    assert!(
        refused.contains("tag_instruction_too_large"),
        "the refusal keeps its name: {refused}"
    );
    assert!(prompt.is_none(), "the provider never ran");
    assert!(applied(&events).is_empty());
}
