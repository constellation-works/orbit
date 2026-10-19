//! Read-only delegation at dispatch [ORB-16814]: an implementing activity's
//! `explore_instruction` reaches the agent only while its host offers
//! delegation, and an explorer's unwrapped Codex runs in its read-only inner
//! sandbox.

use serde_json::json;

use orbit_types::workflow::activity_job::{EXPLORE_ACTIVITY, V2AuditEventKind};

use super::*;

const INSTRUCTION: &str = "Delegate read-only questions to an explorer.";

struct ExploreHost {
    cli: ScriptHost,
    offered: bool,
}

impl orbit_engine::test_support::NullRuntimeHost for ExploreHost {
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

    fn explore_delegation_available(&self) -> bool {
        self.offered
    }
}

/// Dispatch `agent_implement` through a fake Claude that saves its prompt,
/// and return the instruction its execution envelope carried.
fn implement_instruction(offered: bool) -> Value {
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
    let (writer, _store) = build_writer(temp.path(), "explore-offer").unwrap();
    let host = ExploreHost {
        cli: ScriptHost::new(fake.cli_path()),
        offered,
    };
    let mut spec = cli_agent_loop_spec(None);
    spec.explore_instruction = Some(INSTRUCTION.to_string());
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "agent_implement",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "prompt": "hello" }),
        audit: writer,
        run_id: "explore-offer",
        host: Some(&host),
    });
    assert!(outcome.unwrap().success);
    let prompt = fs::read_to_string(prompt).expect("the provider received a prompt");
    let envelope: Value = serde_json::from_str(
        prompt
            .split_once("Execution envelope:\n")
            .expect("the prompt carries an execution envelope")
            .1
            .trim(),
    )
    .expect("the envelope is JSON");
    envelope["instruction"].clone()
}

#[test]
fn delegation_is_offered_only_while_the_host_enables_it() {
    assert_eq!(
        implement_instruction(true),
        format!("cli smoke\n\n{INSTRUCTION}"),
        "an enabled host appends the delegation instruction once"
    );
    assert_eq!(
        implement_instruction(false),
        "cli smoke",
        "with exploration off, or no explorer crew, the agent is offered no delegation"
    );
}

/// The audited argv of one unwrapped Codex dispatch of `activity_name`.
fn codex_argv(activity_name: &str) -> Vec<String> {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_cli(
        "codex",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )
    .unwrap();
    let (writer, store) = build_writer(temp.path(), "explore-sandbox").unwrap();
    let mut spec = cli_agent_loop_spec(Some(Provider::Codex));
    spec.model = None;
    let host = ScriptHost::new_with_args(fake.cli_path(), vec!["exec".into(), "--json".into()]);
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name,
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "prompt": "where is the retry budget enforced" }),
        audit: writer,
        run_id: "explore-sandbox",
        host: Some(&host),
    })
    .unwrap();
    assert!(outcome.success, "fake codex exits 0");
    events_snapshot(&store, "explore-sandbox")
        .unwrap()
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("cli.invocation.started event")
}

fn sandbox_mode(argv: &[String]) -> Option<&str> {
    argv.windows(2)
        .find(|pair| pair[0] == "--sandbox" || pair[0] == "-s")
        .map(|pair| pair[1].as_str())
}

#[test]
fn an_unwrapped_explorer_runs_codex_read_only() {
    assert_eq!(
        sandbox_mode(&codex_argv(EXPLORE_ACTIVITY)),
        Some("read-only"),
        "an explorer outside Orbit's executor sandbox must keep Codex's read-only inner \
         sandbox: it is the explorer's write boundary"
    );
    assert_ne!(
        sandbox_mode(&codex_argv("agent_implement")),
        Some("read-only"),
        "an implementer keeps its configured Codex sandbox"
    );
}
