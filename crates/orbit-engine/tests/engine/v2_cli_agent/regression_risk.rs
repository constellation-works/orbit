//! An agent dispatch whose activity declares a regression-risk instruction
//! asks its host for the task's marks, carries them in the envelope's task
//! snapshot as `regression_risk` and appends the instruction; with nothing
//! marked, or no instruction declared, the envelope is unchanged.

use std::sync::Mutex;

use serde_json::json;

use super::*;

const INSTRUCTION: &str = "Act on each regression-risk mark.";

struct MarksHost {
    cli: ScriptHost,
    marks: Option<Value>,
    asked: Mutex<Vec<(String, String, Value)>>,
}

impl orbit_engine::test_support::NullRuntimeHost for MarksHost {
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

    fn task_context_for_agent_input(
        &self,
        _run_id: &str,
        _step_id: &str,
        _input: &Value,
    ) -> Result<Option<Value>, DispatchError> {
        Ok(Some(json!({ "id": "fixture-task", "title": "Fixture" })))
    }

    fn regression_risk_for_agent_input(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Option<Value> {
        self.asked.lock().unwrap().push((
            run_id.to_string(),
            activity_name.to_string(),
            input.clone(),
        ));
        self.marks.clone()
    }
}

/// Dispatch `agent_implement` through a fake provider that saves its prompt,
/// and return the execution envelope it received.
fn dispatch(instruction: Option<&str>, marks: Option<Value>) -> (Value, MarksHost) {
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
    let (writer, _store) = build_writer(temp.path(), "regression-risk").unwrap();
    let host = MarksHost {
        cli: ScriptHost::new(fake.cli_path()),
        marks,
        asked: Mutex::new(Vec::new()),
    };
    let mut spec = cli_agent_loop_spec(None);
    spec.regression_risk_instruction = instruction.map(str::to_string);
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "agent_implement",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "task_id": "fixture-task", "prompt": "hello" }),
        audit: writer,
        run_id: "regression-risk",
        host: Some(&host),
    });
    assert!(outcome.unwrap().success);
    let prompt = fs::read_to_string(prompt).expect("the provider received a prompt");
    let envelope = prompt
        .split_once("Execution envelope:\n")
        .expect("the prompt carries an execution envelope")
        .1;
    (
        serde_json::from_str(envelope.trim()).expect("the envelope is JSON"),
        host,
    )
}

#[test]
fn marks_ride_in_the_task_snapshot_with_the_declared_instruction() {
    let marks = json!({
        "window_days": 30,
        "marks": [{
            "path": "src/hot.rs",
            "regressed": "3 of 6 touching tasks",
            "nature": ["broke_existing"],
            "escape": ["no_test"],
        }],
    });
    let (envelope, host) = dispatch(Some(INSTRUCTION), Some(marks.clone()));
    assert_eq!(envelope["task"]["regression_risk"], marks);
    assert_eq!(envelope["task"]["id"], "fixture-task");
    assert_eq!(
        envelope["instruction"],
        format!("cli smoke\n\n{INSTRUCTION}"),
        "the instruction follows the built-in one, once"
    );
    let asked = host.asked.lock().unwrap();
    assert_eq!(
        (
            asked[0].0.as_str(),
            asked[0].1.as_str(),
            &asked[0].2["task_id"]
        ),
        ("regression-risk", "agent_implement", &json!("fixture-task")),
        "the host is asked for the dispatched run, activity and task"
    );
}

#[test]
fn nothing_marked_or_no_instruction_leaves_the_envelope_unchanged() {
    let (envelope, host) = dispatch(Some(INSTRUCTION), None);
    assert!(envelope["task"].get("regression_risk").is_none());
    assert_eq!(
        envelope["instruction"], "cli smoke",
        "no instruction without marks"
    );
    assert_eq!(host.asked.lock().unwrap().len(), 1);

    let (envelope, host) = dispatch(None, Some(json!({ "marks": [] })));
    assert!(envelope["task"].get("regression_risk").is_none());
    assert_eq!(envelope["instruction"], "cli smoke");
    assert!(
        host.asked.lock().unwrap().is_empty(),
        "an activity that declares no instruction never asks for marks"
    );
}
