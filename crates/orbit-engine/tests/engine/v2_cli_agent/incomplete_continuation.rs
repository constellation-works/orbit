//! [ORB-21837] An implementer that declares the typed
//! `implementation_incomplete` signal gets a bounded continuation in the same
//! worktree, within its own wall-clock budget; every other declared failure,
//! and every continuation the bounds refuse, keeps its truthful failure and
//! the uncommitted candidate.
//!
//! The provider is a scripted `claude` that numbers its invocations, saves
//! each prompt, and edits a Git worktree the way an implementer would.

use std::process::Command;

use orbit_types::workflow::activity_job::IncompleteContinuationPolicy;
use serde_json::json;

use super::*;

const INCOMPLETE: &str = r#"{"schemaVersion":1,"status":"failed","result":{"remaining_work":["wire the evaluator","add the integration test"]},"error":{"code":"implementation_incomplete","message":"The evaluator and its integration test were not implemented."}}"#;
const SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"summary":"done"},"error":null}"#;
const MISSING_PARTIAL: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"lost_candidate","message":"the partial edit was gone"}}"#;

struct ContinuationHost {
    cli: ScriptHost,
    terminal: bool,
}

impl orbit_engine::test_support::NullRuntimeHost for ContinuationHost {
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
        Ok(Some(json!({
            "id": "fixture-task",
            "title": "Fixture",
            "status": if self.terminal { "done" } else { "in-progress" },
            "terminal": self.terminal,
        })))
    }
}

struct Fixture {
    temp: TempDir,
    _fake: FakeCli,
}

impl Fixture {
    /// A scripted implementer: invocation 1 writes a partial edit and prints
    /// `first`; each later invocation prints `later` (a shell fragment).
    fn new(first: &str, later: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        let worktree = state.join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        git(&worktree, &["init", "-q"]);
        fs::write(worktree.join("feature.txt"), "base\n").unwrap();
        git(&worktree, &["add", "feature.txt"]);
        git(
            &worktree,
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        let script = format!(
            "#!/bin/sh\n\
             state='{state}'\n\
             n=$(cat \"$state/count\" 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > \"$state/count\"\n\
             cat > \"$state/prompt-$n\"\n\
             if [ \"$n\" = 1 ]; then\n\
             echo partial >> \"$state/worktree/feature.txt\"\n\
             echo '{first}'\n\
             else\n\
             {later}\n\
             fi\n",
            state = state.display(),
        );
        let fake = fake_cli("claude", &script).unwrap();
        Self { temp, _fake: fake }
    }

    fn invocations(&self) -> usize {
        fs::read_to_string(self.temp.path().join("count"))
            .map(|count| count.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn envelope(&self, invocation: usize) -> Value {
        let prompt =
            fs::read_to_string(self.temp.path().join(format!("prompt-{invocation}"))).unwrap();
        let envelope = prompt
            .split_once("Execution envelope:\n")
            .expect("the prompt carries an execution envelope")
            .1;
        serde_json::from_str(envelope.trim()).expect("the envelope is JSON")
    }

    fn worktree(&self) -> PathBuf {
        self.temp.path().join("worktree")
    }

    fn feature(&self) -> String {
        fs::read_to_string(self.worktree().join("feature.txt")).unwrap()
    }

    /// The commits on the worktree's branch and its uncommitted paths.
    fn git_state(&self) -> (String, String) {
        (
            git(&self.worktree(), &["rev-list", "--count", "HEAD"]),
            git(&self.worktree(), &["status", "--short"]),
        )
    }

    fn dispatch(
        &self,
        policy: Option<IncompleteContinuationPolicy>,
        terminal: bool,
    ) -> orbit_engine::DispatchOutcome {
        let run_id = "incomplete-continuation";
        let (writer, _store) = build_writer(self.temp.path(), run_id).unwrap();
        let host = ContinuationHost {
            cli: ScriptHost::new(self._fake.cli_path()),
            terminal,
        };
        let mut spec = cli_agent_loop_spec(None);
        spec.wall_clock_timeout_seconds = 30;
        spec.incomplete_continuation = policy;
        dispatch_v2_activity(V2DispatchInput {
            activity_name: "agent_implement",
            spec: &ActivityV2Spec::AgentLoop(spec),
            fs_profile: None,
            input: json!({ "task_id": "fixture-task" }),
            audit: writer,
            run_id,
            host: Some(&host),
        })
        .expect("the dispatch returns an outcome")
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn admitted(max_rounds: u32) -> Option<IncompleteContinuationPolicy> {
    Some(IncompleteContinuationPolicy {
        max_rounds,
        min_remaining_seconds: 5,
    })
}

/// Finish only when the earlier session's partial edit is still there.
fn finish_from_partial() -> String {
    format!(
        "if grep -q partial \"$state/worktree/feature.txt\"; then echo finished >> \"$state/worktree/feature.txt\"; echo '{SUCCESS}'; else echo '{MISSING_PARTIAL}'; fi"
    )
}

#[test]
fn an_incomplete_declaration_without_a_policy_fails_on_its_first_handoff() {
    let fixture = Fixture::new(INCOMPLETE, &finish_from_partial());
    let started = Instant::now();
    let outcome = fixture.dispatch(None, false);

    assert!(!outcome.success, "the declared failure stays a failure");
    assert_eq!(fixture.invocations(), 1, "no continuation is started");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the activity still had admitted budget when it failed"
    );
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains("implementation_incomplete"), "{message}");
    assert!(!message.contains("continuation"), "{message}");
    assert_eq!(
        fixture.feature(),
        "base\npartial\n",
        "the partial candidate"
    );
}

#[test]
fn an_admitted_continuation_finishes_the_same_candidate_without_committing() {
    let fixture = Fixture::new(INCOMPLETE, &finish_from_partial());
    let outcome = fixture.dispatch(admitted(2), false);

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(fixture.invocations(), 2, "exactly one continuation");
    assert_eq!(fixture.feature(), "base\npartial\nfinished\n");
    assert_eq!(
        fixture.git_state(),
        ("1".to_string(), "M feature.txt".to_string()),
        "the candidate stays uncommitted on the worktree's base"
    );
    assert_eq!(outcome.output["incomplete_continuation"]["rounds"], 1);
    assert_eq!(
        outcome.output["incomplete_continuation"]["refusal"],
        Value::Null
    );

    let first = fixture.envelope(1);
    let second = fixture.envelope(2);
    assert!(first["input"].get("implementation_continuation").is_none());
    let continuation = &second["input"]["implementation_continuation"];
    assert_eq!(continuation["round"], 1);
    assert_eq!(continuation["max_rounds"], 2);
    assert_eq!(
        continuation["remaining_work"],
        json!(["wire the evaluator", "add the integration test"])
    );
    assert!(
        continuation["previous_error"]
            .as_str()
            .unwrap()
            .contains("The evaluator and its integration test were not implemented.")
    );
    assert!(continuation["remaining_budget_seconds"].as_u64().unwrap() <= 30);
    // The continuation runs under the same task, instruction and tool grant.
    for key in ["instruction", "task", "tools"] {
        assert_eq!(first.get(key), second.get(key), "{key}");
    }
    assert_eq!(first["input"]["task_id"], second["input"]["task_id"]);
}

#[test]
fn exhausted_continuation_rounds_keep_the_original_failure_and_candidate() {
    let fixture = Fixture::new(INCOMPLETE, &format!("echo '{INCOMPLETE}'"));
    let outcome = fixture.dispatch(admitted(1), false);

    assert!(!outcome.success, "unfinished work earns no success");
    assert_eq!(fixture.invocations(), 2, "one continuation, then stop");
    let message = outcome.message.as_deref().unwrap();
    assert!(
        message.contains(
            "after 1 implementation continuation(s) (continuation refused: rounds_exhausted)"
        ),
        "{message}"
    );
    assert!(
        message.contains("original error before continuation:"),
        "{message}"
    );
    assert_eq!(
        outcome.output["incomplete_continuation"]["refusal"],
        "rounds_exhausted"
    );
    assert_eq!(fixture.feature(), "base\npartial\n");
    assert_eq!(fixture.git_state().0, "1", "nothing was committed");
}

#[test]
fn a_spent_wall_clock_budget_refuses_continuation() {
    let fixture = Fixture::new(INCOMPLETE, &finish_from_partial());
    // The whole 30s budget must remain: any time the first session spent
    // leaves less, so the refusal does not depend on how long it ran.
    let policy = Some(IncompleteContinuationPolicy {
        max_rounds: 2,
        min_remaining_seconds: 30,
    });
    let outcome = fixture.dispatch(policy, false);

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert_eq!(
        outcome.output["incomplete_continuation"],
        json!({ "rounds": 0, "max_rounds": 2, "refusal": "wall_clock_exhausted" })
    );
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains("implementation_incomplete"), "{message}");
    assert_eq!(fixture.feature(), "base\npartial\n");
}

#[test]
fn a_terminal_task_refuses_continuation() {
    let fixture = Fixture::new(INCOMPLETE, &finish_from_partial());
    let outcome = fixture.dispatch(admitted(2), true);

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert_eq!(
        outcome.output["incomplete_continuation"]["refusal"],
        "task_terminal"
    );
    assert_eq!(fixture.feature(), "base\npartial\n");
}

#[test]
fn blockers_and_malformed_declarations_are_not_continuation_requests() {
    let cases = [
        (
            "a declared blocker",
            r#"{"schemaVersion":1,"status":"failed","result":{"remaining_work":["wire the evaluator"],"blocker":{"kind":"missing_dependency","evidence":"crate absent"}},"error":{"code":"implementation_incomplete","message":"blocked"}}"#,
        ),
        (
            "no remaining_work",
            r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"implementation_incomplete","message":"unfinished"}}"#,
        ),
        (
            "an empty remaining_work",
            r#"{"schemaVersion":1,"status":"failed","result":{"remaining_work":[]},"error":{"code":"implementation_incomplete","message":"unfinished"}}"#,
        ),
        (
            "a non-string remaining_work entry",
            r#"{"schemaVersion":1,"status":"failed","result":{"remaining_work":[7]},"error":{"code":"implementation_incomplete","message":"unfinished"}}"#,
        ),
        (
            "another error code",
            r#"{"schemaVersion":1,"status":"failed","result":{"remaining_work":["wire the evaluator"]},"error":{"code":"validation_failed","message":"implementation incomplete"}}"#,
        ),
        (
            "a timeout status",
            r#"{"schemaVersion":1,"status":"timeout","result":{"remaining_work":["wire the evaluator"]},"error":{"code":"implementation_incomplete","message":"unfinished"}}"#,
        ),
        (
            "prose without an envelope",
            "implementation_incomplete: remaining_work wire the evaluator",
        ),
    ];
    for (case, first) in cases {
        let fixture = Fixture::new(first, &finish_from_partial());
        let outcome = fixture.dispatch(admitted(2), false);
        assert!(!outcome.success, "{case}: stays failed");
        assert_eq!(fixture.invocations(), 1, "{case}: no continuation");
        assert!(
            outcome.output.get("incomplete_continuation").is_none(),
            "{case}: nothing was requested"
        );
        assert!(
            !outcome.message.as_deref().unwrap().contains("continuation"),
            "{case}: the original failure is unchanged"
        );
        assert_eq!(
            fixture.feature(),
            "base\npartial\n",
            "{case}: candidate kept"
        );
    }
}

#[test]
fn a_failed_provider_exit_is_not_a_continuation_request() {
    // The typed envelope alone does not admit a continuation when the
    // provider process itself failed.
    let fixture = Fixture::new(
        &format!("{INCOMPLETE}'; exit 3; echo '"),
        &finish_from_partial(),
    );
    let outcome = fixture.dispatch(admitted(2), false);
    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert!(
        outcome
            .message
            .as_deref()
            .unwrap()
            .contains("exited with code 3"),
        "{:?}",
        outcome.message
    );
}
