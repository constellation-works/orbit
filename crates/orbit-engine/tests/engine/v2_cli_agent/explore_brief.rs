//! An agent dispatch whose activity declares an explore-brief instruction
//! asks its host for the task's brief and records the outcome whatever it
//! is. Only a delivered brief rides in the task snapshot as `explore_brief`
//! with the instruction appended; an activity that declares no instruction
//! (review, recovery) never asks.

use std::sync::Mutex;

use orbit_types::workflow::activity_job::V2AuditEventKind;
use orbit_types::workflow::{
    ExploreBriefAdmission, ExploreBriefDropReason, ExploreBriefOutcome, ExploreBriefRecord,
};
use serde_json::json;

use super::*;

const INSTRUCTION: &str = "Start from the explore brief.";

struct BriefHost {
    cli: ScriptHost,
    admission: Option<ExploreBriefAdmission>,
    asked: Mutex<Vec<String>>,
}

impl orbit_engine::test_support::NullRuntimeHost for BriefHost {
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

    fn explore_brief_for_activity(
        &self,
        _run_id: &str,
        activity_name: &str,
        _input: &Value,
    ) -> Option<ExploreBriefAdmission> {
        self.asked.lock().unwrap().push(activity_name.to_string());
        self.admission.clone()
    }
}

fn record(
    outcome: ExploreBriefOutcome,
    reason: Option<ExploreBriefDropReason>,
) -> ExploreBriefRecord {
    ExploreBriefRecord {
        outcome,
        reason,
        detail: None,
        sha256: Some("ab".repeat(32)),
        inspection_revision: Some("1".repeat(40)),
        base_sha: Some("2".repeat(40)),
        drifted_paths: Vec::new(),
        delivered: false,
    }
}

/// Dispatch `activity_name` through a fake provider that saves its prompt;
/// return the envelope it received, the recorded brief outcomes and the host.
fn dispatch(
    activity_name: &str,
    instruction: Option<&str>,
    admission: Option<ExploreBriefAdmission>,
) -> (Value, Vec<ExploreBriefRecord>, BriefHost) {
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
    let (writer, store) = build_writer(temp.path(), "explore-brief").unwrap();
    let host = BriefHost {
        cli: ScriptHost::new(fake.cli_path()),
        admission,
        asked: Mutex::new(Vec::new()),
    };
    let mut spec = cli_agent_loop_spec(None);
    spec.explore_brief_instruction = instruction.map(str::to_string);
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name,
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "task_id": "fixture-task", "prompt": "hello" }),
        audit: writer,
        run_id: "explore-brief",
        host: Some(&host),
    });
    assert!(outcome.unwrap().success);
    let prompt = fs::read_to_string(prompt).expect("the provider received a prompt");
    let envelope = prompt
        .split_once("Execution envelope:\n")
        .expect("the prompt carries an execution envelope")
        .1;
    let records = events_snapshot(&store, "explore-brief")
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            V2AuditEventKind::ExploreBriefApplied { brief, task_id, .. } => {
                assert_eq!(task_id, "fixture-task");
                Some(brief)
            }
            _ => None,
        })
        .collect();
    (
        serde_json::from_str(envelope.trim()).expect("the envelope is JSON"),
        records,
        host,
    )
}

#[test]
fn a_delivered_brief_rides_in_the_task_snapshot_with_the_instruction() {
    let brief = json!({ "status": "fresh", "brief": { "change": [] } });
    let (envelope, records, _) = dispatch(
        "agent_implement",
        Some(INSTRUCTION),
        Some(ExploreBriefAdmission {
            record: record(ExploreBriefOutcome::Fresh, None),
            brief: Some(brief.clone()),
        }),
    );
    assert_eq!(envelope["task"]["explore_brief"], brief);
    assert_eq!(
        envelope["instruction"],
        format!("cli smoke\n\n{INSTRUCTION}"),
        "the instruction follows the built-in one, once"
    );
    assert_eq!(records.len(), 1, "one audit row per dispatch");
    assert_eq!(records[0].label(), "fresh");
    assert!(records[0].delivered);
}

#[test]
fn an_undelivered_brief_is_recorded_but_never_reaches_the_prompt() {
    for (record, brief) in [
        // `deliver = false`: the host classifies but gives no brief.
        (record(ExploreBriefOutcome::Fresh, None), None),
        (
            record(
                ExploreBriefOutcome::Dropped,
                Some(ExploreBriefDropReason::MaterialChanged),
            ),
            Some(json!({ "status": "dropped" })),
        ),
        (ExploreBriefRecord::absent("no brief is stored"), None),
    ] {
        let label = record.label();
        let (envelope, records, _) = dispatch(
            "agent_implement",
            Some(INSTRUCTION),
            Some(ExploreBriefAdmission { record, brief }),
        );
        assert!(
            envelope["task"].get("explore_brief").is_none(),
            "{label}: no brief in the task snapshot"
        );
        assert_eq!(envelope["instruction"], "cli smoke", "{label}");
        assert_eq!(records.len(), 1, "{label}: the outcome is still recorded");
        assert_eq!(records[0].label(), label);
        assert!(!records[0].delivered, "{label}");
    }
}

#[test]
fn an_activity_without_the_instruction_never_asks_for_a_brief() {
    let (envelope, records, host) = dispatch(
        "agent_review_repair",
        None,
        Some(ExploreBriefAdmission {
            record: record(ExploreBriefOutcome::Fresh, None),
            brief: Some(json!({ "status": "fresh" })),
        }),
    );
    assert!(host.asked.lock().unwrap().is_empty());
    assert!(envelope["task"].get("explore_brief").is_none());
    assert!(records.is_empty());
}
