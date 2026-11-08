//! [ORB-22426] A reply that says `success` and carries an `error` object stays
//! a parser refusal. A read-only activity that declares `response_correction`
//! gets a bounded fresh invocation, within its own wall-clock budget and under
//! the same authority, and only a newly valid reply succeeds.
//!
//! [ORB-22427] So does a clean turn that ends in prose with no envelope. The
//! prose never becomes a result, and a turn the provider itself ended abnormally
//! keeps its own failure.
//!
//! The provider is a scripted `claude` that numbers its invocations and saves
//! each prompt and argv.

use std::{cell::Cell, sync::Mutex};

use orbit_types::workflow::activity_job::ResponseCorrectionPolicy;
use serde_json::json;

use super::*;

const SIGNATURE: &str = "success status must not carry error object";
const CONTRADICTORY: &str = r#"{"schemaVersion":1,"status":"success","result":{"summary":"the answer"},"error":{"code":"none","message":"nothing went wrong"}}"#;
const VALID: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"summary":"the answer"},"error":null}"#;
const DECLARED_FAILED: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"blocked","message":"the checkout was unreadable"}}"#;

/// Records the sandbox each invocation asked the host for.
struct RecordingHost {
    cli: ScriptHost,
    sandbox_requests: Mutex<Vec<(String, Option<String>)>>,
    superseded_traces: Mutex<Vec<orbit_engine::DispatchInvocationTrace>>,
    /// Tools a task's `required_tools` would add to the baseline, as the
    /// production host's `resolve_activity_tools` admits them.
    required_tools: Vec<String>,
}

impl orbit_engine::test_support::NullRuntimeHost for RecordingHost {
    fn persist_invocation_trace(
        &self,
        _job_run_id: &str,
        _activity_id: &str,
        _input: &Value,
        invocation: &orbit_engine::DispatchInvocationTrace,
    ) -> Result<(), DispatchError> {
        self.superseded_traces
            .lock()
            .unwrap()
            .push(invocation.clone());
        Ok(())
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        RuntimeHost::agent_subprocess_environment(&self.cli, required_env_vars)
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

    fn resolve_activity_tools(
        &self,
        _task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<orbit_engine::ResolvedActivityTools, DispatchError> {
        let mut effective_tools = baseline_tools.to_vec();
        for tool in &self.required_tools {
            if !effective_tools.contains(tool) {
                effective_tools.push(tool.clone());
            }
        }
        Ok(orbit_engine::ResolvedActivityTools {
            requested_tools: self.required_tools.clone(),
            effective_tools,
            omitted_requirement_notes: Vec::new(),
            granted_tools: Vec::new(),
            suppressed_tools: Vec::new(),
        })
    }

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<orbit_engine::ResolvedSandbox>, DispatchError> {
        self.sandbox_requests
            .lock()
            .unwrap()
            .push((provider.to_string(), fs_profile.map(str::to_string)));
        Ok(None)
    }
}

struct Fixture {
    temp: TempDir,
    fake: FakeCli,
}

struct Dispatched {
    outcome: orbit_engine::DispatchOutcome,
    sandbox_requests: Vec<(String, Option<String>)>,
    superseded_traces: Vec<orbit_engine::DispatchInvocationTrace>,
    omitted_requirement_notes: Vec<String>,
}

impl Fixture {
    /// Invocation 1 runs `first` and each later one runs `later`, both shell.
    fn new(first: &str, later: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let script = format!(
            "#!/bin/sh\n\
             state='{state}'\n\
             n=$(cat \"$state/count\" 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > \"$state/count\"\n\
             cat > \"$state/prompt-$n\"\n\
             printf '%s\\n' \"$@\" > \"$state/argv-$n\"\n\
             printf '%s' \"$ORBIT_ACTIVITY_TOOLS\" > \"$state/tools-$n\"\n\
             if [ \"$n\" = 1 ]; then\n\
             {first}\n\
             else\n\
             {later}\n\
             fi\n",
            state = temp.path().display(),
        );
        let fake = fake_cli("claude", &script).unwrap();
        Self { temp, fake }
    }

    fn invocations(&self) -> usize {
        fs::read_to_string(self.temp.path().join("count"))
            .map(|count| count.trim().parse().unwrap())
            .unwrap_or(0)
    }

    fn first_reply_ready(&self) -> bool {
        self.temp.path().join("first-reply-ready").exists()
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

    fn argv(&self, invocation: usize) -> String {
        fs::read_to_string(self.temp.path().join(format!("argv-{invocation}"))).unwrap()
    }

    /// The `ORBIT_ACTIVITY_TOOLS` grant the invocation's child received.
    fn tools(&self, invocation: usize) -> String {
        fs::read_to_string(self.temp.path().join(format!("tools-{invocation}"))).unwrap()
    }

    fn dispatch(
        &self,
        policy: Option<ResponseCorrectionPolicy>,
        wall_clock_timeout_seconds: u64,
    ) -> Dispatched {
        self.dispatch_with_required_tools(policy, wall_clock_timeout_seconds, Vec::new())
    }

    fn dispatch_with_required_tools(
        &self,
        policy: Option<ResponseCorrectionPolicy>,
        wall_clock_timeout_seconds: u64,
        required_tools: Vec<String>,
    ) -> Dispatched {
        let run_id = "response-correction";
        let (writer, _store) = build_writer(self.temp.path(), run_id).unwrap();
        let host = RecordingHost {
            cli: ScriptHost::new(self.fake.cli_path()),
            sandbox_requests: Mutex::new(Vec::new()),
            superseded_traces: Mutex::new(Vec::new()),
            required_tools,
        };
        let mut spec = cli_agent_loop_spec(None);
        spec.require_response_envelope = true;
        spec.wall_clock_timeout_seconds = wall_clock_timeout_seconds;
        spec.response_correction = policy;
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "read_only_fixture",
            spec: &ActivityV2Spec::AgentLoop(spec),
            fs_profile: Some("reviewer"),
            input: json!({ "question": "where is it?" }),
            audit: writer.clone(),
            run_id,
            host: Some(&host),
        })
        .expect("the dispatch returns an outcome");
        let omitted_requirement_notes = writer
            .events_snapshot()
            .unwrap()
            .into_iter()
            .flat_map(|event| match event.kind {
                orbit_types::workflow::activity_job::V2AuditEventKind::ToolAllowlistHarnessDelegated {
                    omitted_requirement_notes,
                    ..
                } => omitted_requirement_notes,
                _ => Vec::new(),
            })
            .collect();
        Dispatched {
            outcome,
            sandbox_requests: host.sandbox_requests.into_inner().unwrap(),
            superseded_traces: host.superseded_traces.into_inner().unwrap(),
            omitted_requirement_notes,
        }
    }

    /// Hold the supervision clock at the activity deadline's start until the
    /// scripted provider has written its first reply, then move past that
    /// deadline. This keeps process startup out of the timeout assertion.
    fn dispatch_after_first_reply(
        &self,
        policy: Option<ResponseCorrectionPolicy>,
        wall_clock_timeout_seconds: u64,
    ) -> Dispatched {
        let origin = Instant::now();
        let first_clock_read = Cell::new(true);
        let ready = self.temp.path().join("first-reply-ready");
        let startup_limit = origin + Duration::from_secs(30);
        orbit_engine::test_support::with_cli_supervision_clock(
            move || {
                if first_clock_read.replace(false) {
                    origin
                } else if ready.exists() || Instant::now() >= startup_limit {
                    origin + Duration::from_secs(wall_clock_timeout_seconds + 1)
                } else {
                    origin
                }
            },
            || self.dispatch(policy, wall_clock_timeout_seconds),
        )
    }
}

fn admitted(max_rounds: u32) -> Option<ResponseCorrectionPolicy> {
    Some(ResponseCorrectionPolicy {
        max_rounds,
        min_remaining_seconds: 1,
    })
}

fn reply(envelope: &str) -> String {
    format!("echo '{envelope}'")
}

#[test]
fn a_contradictory_reply_without_a_policy_fails_on_its_first_reply() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), &reply(VALID));
    let outcome = fixture.dispatch(None, 30).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1, "no correction is started");
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains(SIGNATURE), "{message}");
    assert!(!message.contains("correction"), "{message}");
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert!(outcome.output.get("response_correction").is_none());
}

#[test]
fn a_contradictory_reply_is_replaced_by_a_newly_valid_one_under_the_same_authority() {
    let later = r#"echo '{"schemaVersion":1,"status":"success","result":{"summary":"corrected"},"error":null}'"#;
    let fixture = Fixture::new(&reply(CONTRADICTORY), later);
    let Dispatched {
        outcome,
        sandbox_requests,
        ..
    } = fixture.dispatch(admitted(1), 30);

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(fixture.invocations(), 2, "exactly one correction");
    assert_eq!(
        outcome.output["summary"], "corrected",
        "the result is the new reply's, not the rejected one's"
    );
    assert_eq!(outcome.output["response_envelope_valid"], true);
    assert!(
        outcome.output.get("response_correction_request").is_none(),
        "the rejected reply does not become step output"
    );

    // The original diagnostics survive the recovery.
    let record = &outcome.output["response_correction"];
    assert_eq!(record["rounds"], 1);
    assert_eq!(record["max_rounds"], 1);
    assert_eq!(record["refusal"], Value::Null);
    assert!(
        record["original_error"]
            .as_str()
            .unwrap()
            .contains(SIGNATURE),
        "{record}"
    );
    assert!(
        record["original_stdout_blob_ref"].is_string(),
        "the rejected capture stays addressable: {record}"
    );

    // The correction sees what was rejected, as a field of its input.
    let first = fixture.envelope(1);
    let second = fixture.envelope(2);
    assert!(first["input"].get("response_correction").is_none());
    let correction = &second["input"]["response_correction"];
    assert_eq!(correction["round"], 1);
    assert_eq!(correction["rejected_reply_truncated"], false);
    assert_eq!(
        serde_json::from_str::<Value>(correction["rejected_reply"].as_str().unwrap()).unwrap(),
        serde_json::from_str::<Value>(CONTRADICTORY).unwrap(),
    );
    assert!(
        correction["parser_error"]
            .as_str()
            .unwrap()
            .contains(SIGNATURE)
    );

    // Same authority: same instruction, tool grant, input and provider argv,
    // and the same sandbox requested for the same read-only profile.
    for key in ["instruction", "tools", "task", "model"] {
        assert_eq!(first.get(key), second.get(key), "{key}");
    }
    assert_eq!(first["input"]["question"], second["input"]["question"]);
    assert_eq!(fixture.argv(1), fixture.argv(2), "provider argv");
    assert_eq!(
        sandbox_requests,
        vec![("claude".to_string(), Some("reviewer".to_string())); 2],
    );

    // Same budget: the correction's deadline is what the first reply left.
    let remaining = correction["remaining_budget_seconds"].as_u64().unwrap();
    assert!(remaining < 30, "{remaining}");
    assert_eq!(
        outcome.output["effective_deadline_ms"],
        json!(remaining * 1000),
        "the correction's own kill deadline is the unspent budget"
    );
}

#[test]
fn repeated_contradictory_replies_exhaust_the_correction_rounds_and_fail() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), &reply(CONTRADICTORY));
    let outcome = fixture.dispatch(admitted(2), 30).outcome;

    assert!(!outcome.success, "an invalid reply earns no success");
    assert_eq!(fixture.invocations(), 3, "two corrections, then stop");
    let message = outcome.message.as_deref().unwrap();
    assert!(
        message.contains("after 2 response correction(s) (correction refused: rounds_exhausted)"),
        "{message}"
    );
    assert!(
        message.contains("original error before correction:"),
        "{message}"
    );
    assert!(message.contains(SIGNATURE), "{message}");
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert_eq!(outcome.output["response_correction"]["rounds"], 2);
    assert!(outcome.output.get("response_correction_request").is_none());
}

#[test]
fn a_correction_that_declares_failure_stays_failed() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), &reply(DECLARED_FAILED));
    let outcome = fixture.dispatch(admitted(3), 30).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 2, "a declared failure is final");
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains("the checkout was unreadable"), "{message}");
    assert!(message.contains(SIGNATURE), "{message}");
}

#[test]
fn a_spent_wall_clock_budget_refuses_correction() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), &reply(VALID));
    // The whole budget must remain: any time the first invocation spent leaves
    // less, so the refusal does not depend on how long it ran.
    let policy = Some(ResponseCorrectionPolicy {
        max_rounds: 1,
        min_remaining_seconds: 30,
    });
    let outcome = fixture.dispatch(policy, 30).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    let record = &outcome.output["response_correction"];
    assert_eq!(record["rounds"], 0);
    assert_eq!(record["refusal"], "wall_clock_exhausted");
    assert!(
        outcome.message.as_deref().unwrap().contains(SIGNATURE),
        "{:?}",
        outcome.message
    );
}

#[test]
fn a_correction_is_killed_at_the_end_of_the_original_budget() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), "exec sleep 60");
    let started = Instant::now();
    let outcome = fixture.dispatch(admitted(1), 4).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 2);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the correction outlived the 4s budget: {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.output["timed_out"], true);
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains("wall-clock timeout"), "{message}");
    assert!(message.contains(SIGNATURE), "{message}");
}

#[test]
fn an_activity_timeout_is_not_a_correction_request() {
    let fixture = Fixture::new(
        &format!(
            "{}; touch \"$state/first-reply-ready\"; exec sleep 60",
            reply(CONTRADICTORY)
        ),
        &reply(VALID),
    );
    let outcome = fixture.dispatch_after_first_reply(admitted(1), 1).outcome;

    assert!(!outcome.success);
    assert!(
        fixture.first_reply_ready(),
        "the invocation count is meaningful only after the provider wrote its first reply"
    );
    assert_eq!(fixture.invocations(), 1, "one started provider invocation");
    assert_eq!(outcome.output["timed_out"], true);
    assert!(outcome.output.get("response_correction").is_none());
}

#[test]
fn other_failures_do_not_start_a_correction() {
    let terminal = |frame: &str| format!("echo '{CONTRADICTORY}'; echo '{frame}'");
    let cases = [
        ("an explicit failed reply", reply(DECLARED_FAILED)),
        (
            "an explicit timeout reply",
            reply(
                r#"{"schemaVersion":1,"status":"timeout","result":{},"error":{"code":"late","message":"ran out of time"}}"#,
            ),
        ),
        (
            "a success without an error but no object result",
            reply(r#"{"schemaVersion":1,"status":"success","result":"text","error":null}"#),
        ),
        (
            "a malformed error object",
            reply(r#"{"schemaVersion":1,"status":"success","result":{},"error":{"code":7}}"#),
        ),
        (
            "prose that quotes the refusal in a failed reply",
            reply(&format!(
                r#"{{"schemaVersion":1,"status":"failed","result":{{}},"error":{{"code":"x","message":"{SIGNATURE}"}}}}"#
            )),
        ),
        (
            "a nonzero provider exit",
            format!("{}; exit 3", reply(CONTRADICTORY)),
        ),
        (
            "a provider authentication failure",
            terminal(
                r#"{"type":"result","is_error":true,"api_error_status":401,"result":"Failed to authenticate. API Error: 401 authentication_error"}"#,
            ),
        ),
        (
            "a provider usage limit",
            terminal(
                r#"{"type":"result","is_error":true,"result":"You\u0027ve hit your limit, resets 3pm"}"#,
            ),
        ),
        (
            "a provider capacity failure",
            terminal(
                r#"{"type":"result","is_error":true,"result":"Selected model is at capacity. Please try a different model."}"#,
            ),
        ),
        (
            "an unrecognized provider error",
            terminal(r#"{"type":"result","is_error":true,"result":"something else broke"}"#),
        ),
        (
            "a provider policy refusal",
            terminal(
                r#"{"type":"result","subtype":"success","is_error":false,"stop_reason":"refusal","result":"I cannot help with that."}"#,
            ),
        ),
    ];
    for (case, first) in cases {
        let fixture = Fixture::new(&first, &reply(VALID));
        let outcome = fixture.dispatch(admitted(3), 30).outcome;
        assert!(!outcome.success, "{case}: stays failed");
        assert_eq!(fixture.invocations(), 1, "{case}: no correction");
        assert!(
            outcome.output.get("response_correction").is_none(),
            "{case}: nothing was requested"
        );
        assert!(
            !outcome.message.as_deref().unwrap().contains("correction"),
            "{case}: {:?}",
            outcome.message
        );
    }
}

const MISSING: &str = "stdout does not contain an Orbit response envelope";
const PROSE: &str = "The retry budget lives in the supervisor.";

/// What a clean Claude turn that answered in prose prints.
fn prose_wrapper() -> String {
    reply(&format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":"{PROSE}"}}"#
    ))
}

#[test]
fn a_prose_ending_without_a_policy_fails_with_the_missing_envelope_signature() {
    let fixture = Fixture::new(&prose_wrapper(), &reply(VALID));
    let outcome = fixture.dispatch(None, 30).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1, "no correction is started");
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains(MISSING), "{message}");
    assert!(!message.contains("correction"), "{message}");
}

#[test]
fn a_prose_ending_is_replaced_by_a_newly_valid_reply_under_the_same_authority() {
    let later = r#"echo '{"schemaVersion":1,"status":"success","result":{"summary":"corrected"},"error":null}'"#;
    let fixture = Fixture::new(&prose_wrapper(), later);
    let Dispatched {
        outcome,
        sandbox_requests,
        ..
    } = fixture.dispatch(admitted(1), 30);

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(fixture.invocations(), 2, "exactly one correction");
    assert_eq!(
        outcome.output["summary"], "corrected",
        "the result is the new reply's, never read out of the prose"
    );
    assert_eq!(outcome.output["response_envelope_valid"], true);

    let record = &outcome.output["response_correction"];
    assert_eq!(record["rounds"], 1);
    assert!(
        record["original_error"].as_str().unwrap().contains(MISSING),
        "{record}"
    );
    assert!(record["original_stdout_blob_ref"].is_string(), "{record}");

    let first = fixture.envelope(1);
    let second = fixture.envelope(2);
    let correction = &second["input"]["response_correction"];
    assert_eq!(correction["rejection"], "missing_envelope");
    assert_eq!(correction["rejected_reply"], PROSE);
    assert_eq!(correction["rejected_reply_truncated"], false);
    assert!(
        correction["parser_error"]
            .as_str()
            .unwrap()
            .contains(MISSING),
        "{correction}"
    );
    for key in ["instruction", "tools", "task", "model"] {
        assert_eq!(first.get(key), second.get(key), "{key}");
    }
    assert_eq!(fixture.argv(1), fixture.argv(2), "provider argv");
    assert_eq!(
        sandbox_requests,
        vec![("claude".to_string(), Some("reviewer".to_string())); 2],
    );
    let remaining = correction["remaining_budget_seconds"].as_u64().unwrap();
    assert!(remaining < 30, "{remaining}");
    assert_eq!(
        outcome.output["effective_deadline_ms"],
        json!(remaining * 1000),
        "the correction's own kill deadline is the unspent budget"
    );
}

#[test]
fn repeated_prose_endings_stop_at_the_bound_and_keep_the_original_failure() {
    let fixture = Fixture::new(&prose_wrapper(), &prose_wrapper());
    let outcome = fixture.dispatch(admitted(2), 30).outcome;

    assert!(!outcome.success, "prose earns no success");
    assert_eq!(fixture.invocations(), 3, "two corrections, then stop");
    let message = outcome.message.as_deref().unwrap();
    assert!(
        message.contains("after 2 response correction(s) (correction refused: rounds_exhausted)"),
        "{message}"
    );
    let (_, original) = message
        .split_once("original error before correction:")
        .expect("the original failure is kept");
    assert!(original.contains(MISSING), "{message}");
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert_eq!(outcome.output["response_correction"]["rounds"], 2);
    assert!(
        outcome.output.get("summary").is_none(),
        "prose is no result"
    );
}

#[test]
fn a_spent_budget_refuses_correcting_a_prose_ending() {
    let fixture = Fixture::new(&prose_wrapper(), &reply(VALID));
    let policy = Some(ResponseCorrectionPolicy {
        max_rounds: 1,
        min_remaining_seconds: 30,
    });
    let outcome = fixture.dispatch(policy, 30).outcome;

    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert_eq!(
        outcome.output["response_correction"]["refusal"],
        "wall_clock_exhausted"
    );
    assert!(outcome.message.as_deref().unwrap().contains(MISSING));
}

#[test]
fn a_turn_the_provider_ended_for_its_own_reason_is_not_a_correctable_omission() {
    let prose = format!("echo '{PROSE}'");
    let cases = [
        ("a nonzero provider exit", format!("{prose}; exit 3"), "exited with code 3"),
        (
            "a provider authentication failure",
            r#"echo '{"type":"result","is_error":true,"api_error_status":401,"result":"Failed to authenticate. API Error: 401 authentication_error"}'"#.to_string(),
            "authenticate",
        ),
        (
            "a provider usage limit",
            r#"echo '{"type":"result","is_error":true,"result":"You\u0027ve hit your limit, resets 3pm"}'"#.to_string(),
            "usage limit",
        ),
        (
            "a provider capacity failure",
            r#"echo '{"type":"result","is_error":true,"result":"Selected model is at capacity. Please try a different model."}'"#.to_string(),
            "",
        ),
        (
            "a provider policy refusal",
            r#"echo '{"type":"result","subtype":"success","is_error":false,"stop_reason":"refusal","result":"I cannot help with that."}'"#.to_string(),
            "refused the request",
        ),
        (
            "a turn limit that exits zero",
            r#"echo '{"type":"result","subtype":"error_max_turns","is_error":false,"terminal_reason":"max_turns","result":null}'"#.to_string(),
            "",
        ),
        (
            "an authentication failure only on stderr",
            format!("{prose}; echo 'Failed to authenticate. API Error: 401 authentication_error' >&2"),
            MISSING,
        ),
        (
            "a usage limit only on stderr",
            format!("{prose}; echo \"You've hit your limit, resets 3pm\" >&2"),
            MISSING,
        ),
        ("an empty reply", "true".to_string(), MISSING),
        (
            "a capture truncated past its limit",
            format!("head -c 2000000 /dev/zero | tr '\\0' x; echo; {prose}"),
            MISSING,
        ),
        (
            "a wrapper holding no answer",
            r#"echo '{"type":"result","subtype":"success","is_error":false,"result":""}'"#.to_string(),
            MISSING,
        ),
    ];
    for (case, first, expected) in cases {
        let fixture = Fixture::new(&first, &reply(VALID));
        let outcome = fixture.dispatch(admitted(3), 30).outcome;
        assert!(!outcome.success, "{case}: stays failed");
        assert_eq!(fixture.invocations(), 1, "{case}: no correction");
        assert!(
            outcome.output.get("response_correction").is_none(),
            "{case}: nothing was requested"
        );
        let message = outcome.message.as_deref().unwrap();
        assert!(!message.contains("correction"), "{case}: {message}");
        assert!(message.contains(expected), "{case}: {message}");
    }
}

#[test]
fn a_prose_ending_that_outlives_the_budget_stays_a_timeout() {
    let fixture = Fixture::new(
        &format!("echo '{PROSE}'; touch \"$state/first-reply-ready\"; exec sleep 60"),
        &reply(VALID),
    );
    let outcome = fixture.dispatch_after_first_reply(admitted(1), 1).outcome;

    assert!(!outcome.success);
    assert!(
        fixture.first_reply_ready(),
        "the invocation count is meaningful only after the provider wrote its first reply"
    );
    assert_eq!(fixture.invocations(), 1, "one started provider invocation");
    assert_eq!(outcome.output["timed_out"], true);
    assert!(
        outcome
            .message
            .as_deref()
            .unwrap()
            .contains("wall-clock timeout")
    );
    assert!(outcome.output.get("response_correction").is_none());
}

#[test]
fn only_a_read_only_activity_may_declare_response_correction() {
    const CORRECTION: &str =
        "  response_correction:\n    max_rounds: 1\n    min_remaining_seconds: 30\n";
    const READ_ONLY: &str = "  require_response_envelope: true\n  fsProfile: reviewer\n";
    const NO_MUTATION: &str = "  tools:\n    - orbit.task.show\n";
    let asset = |policy: String| {
        format!(
            "schemaVersion: 2\nkind: Activity\nmetadata:\n  name: fixture\nspec:\n  type: agent_loop\n  description: Crafted policy fixture.\n  instruction: Test.\n{policy}{CORRECTION}"
        )
    };

    orbit_engine::load_activity_asset(&asset(format!("{READ_ONLY}{NO_MUTATION}")))
        .expect("a read-only activity may declare it");
    let cases = [
        (
            "require_response_envelope",
            format!("  fsProfile: reviewer\n{NO_MUTATION}"),
        ),
        (
            "fsProfile: reviewer",
            format!("  require_response_envelope: true\n{NO_MUTATION}"),
        ),
        (
            "fsProfile: reviewer",
            format!("  require_response_envelope: true\n  fsProfile: implementer\n{NO_MUTATION}"),
        ),
        (
            "orbit.task.update",
            format!("{READ_ONLY}  tools:\n    - orbit.task.update\n"),
        ),
        (
            "orbit.task.add",
            format!("{READ_ONLY}  tools:\n    - orbit.task.*\n"),
        ),
        (
            "orbit.auto_task.add",
            format!(
                "{READ_ONLY}  tool_disallow_list:\n    - orbit.search\n  proc_disallowed_programs: []\n"
            ),
        ),
        (
            "incomplete_continuation",
            format!(
                "{READ_ONLY}{NO_MUTATION}  incomplete_continuation:\n    max_rounds: 1\n    min_remaining_seconds: 1\n"
            ),
        ),
    ];
    for (expected, policy) in cases {
        let error = orbit_engine::load_activity_asset(&asset(policy.clone()))
            .err()
            .unwrap_or_else(|| panic!("expected a refusal naming {expected}: {policy}"))
            .to_string();
        assert!(error.contains("response_correction"), "{error}");
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn a_mutating_task_requirement_is_dropped_for_a_response_correction_activity() {
    let fixture = Fixture::new(&reply(CONTRADICTORY), &reply(VALID));
    let dispatched = fixture.dispatch_with_required_tools(
        admitted(1),
        30,
        vec!["orbit.task.update".to_string(), "orbit.search".to_string()],
    );

    assert!(
        dispatched.outcome.success,
        "{:?}",
        dispatched.outcome.message
    );
    assert_eq!(fixture.invocations(), 2, "exactly one correction");
    for invocation in [1, 2] {
        let tools = fixture.tools(invocation);
        assert!(
            !tools.split(',').any(|tool| tool == "orbit.task.update"),
            "invocation {invocation} must not receive the mutating requirement: {tools}"
        );
        assert!(
            tools.split(',').any(|tool| tool == "orbit.search"),
            "invocation {invocation} keeps the non-mutating requirement: {tools}"
        );
    }
    assert!(
        dispatched
            .omitted_requirement_notes
            .iter()
            .any(|note| note.contains("orbit.task.update") && note.contains("response_correction")),
        "the drop is recorded as an omitted requirement: {:?}",
        dispatched.omitted_requirement_notes
    );
}

#[test]
fn a_mutating_task_requirement_is_still_granted_without_response_correction() {
    let fixture = Fixture::new(&reply(VALID), &reply(VALID));
    let dispatched =
        fixture.dispatch_with_required_tools(None, 30, vec!["orbit.task.update".to_string()]);

    assert!(
        dispatched.outcome.success,
        "{:?}",
        dispatched.outcome.message
    );
    assert_eq!(fixture.invocations(), 1);
    let tools = fixture.tools(1);
    assert!(
        tools.split(',').any(|tool| tool == "orbit.task.update"),
        "an activity without response_correction keeps today's grant: {tools}"
    );
    assert!(
        dispatched.omitted_requirement_notes.is_empty(),
        "{:?}",
        dispatched.omitted_requirement_notes
    );
}

// [ORB-22448] These coordination writes used to pass admission even though a
// correction repeats the entire activity under the same tool grant.
fn assert_coordination_mutations_refused(deny_mode: bool) {
    for yaml in [include_str!(
        "../../../../orbit-core/assets/activities/task_pilot.yaml"
    )] {
        orbit_engine::load_activity_asset(yaml).expect("the shipped activity remains admissible");
        let mut registry = orbit_tools::ToolRegistry::new();
        registry.register_builtins();
        let mut tools: Vec<_> = registry
            .schemas()
            .into_iter()
            .filter(|schema| {
                schema.name.starts_with("orbit.")
                    && registry.execution_kind(&schema.name)
                        == Some(orbit_tools::ToolExecutionKind::Mutating)
            })
            .map(|schema| schema.name)
            .collect();
        tools.sort();
        assert!(
            !tools.is_empty(),
            "the registry supplies the mutation surface"
        );
        for tool in &tools {
            let mut asset: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
            let spec = asset["spec"].as_mapping_mut().unwrap();
            let deny_key = serde_yaml::Value::from("tool_disallow_list");
            if deny_mode {
                spec.get_mut(&deny_key)
                    .unwrap()
                    .as_sequence_mut()
                    .unwrap()
                    .retain(|entry| entry.as_str() != Some(tool.as_str()));
            } else {
                spec.remove(&deny_key);
                spec.insert(
                    serde_yaml::Value::from("tools"),
                    serde_yaml::to_value([tool]).unwrap(),
                );
            }
            let yaml = serde_yaml::to_string(&asset).unwrap();
            let error = orbit_engine::load_activity_asset(&yaml)
                .err()
                .unwrap_or_else(|| {
                    panic!("coordination write {tool} was admitted (deny={deny_mode})")
                })
                .to_string();
            assert!(error.contains("response_correction"), "{error}");
            assert!(error.contains(tool.as_str()), "{tool}: {error}");
        }
    }
}

#[test]
fn coordination_mutations_are_refused_in_allow_mode() {
    assert_coordination_mutations_refused(false);
}

#[test]
fn coordination_mutations_are_refused_in_deny_mode() {
    assert_coordination_mutations_refused(true);
}

// [ORB-22663] This terminal is a provider-owned formatting failure, not an
// assistant's JSON tool arguments and not an unrelated workspace-trust warning.
const STRUCTURED_EXHAUSTED: &str = r#"{"type":"result","subtype":"error_max_structured_output_retries","is_error":true,"terminal_reason":"structured_output_retry_exhausted","errors":["Failed to provide valid structured output after 5 attempts"],"usage":{"input_tokens":11,"output_tokens":7}}"#;
const STRUCTURED_SIGNATURE: &str = "structured_output_retry_exhausted";

fn structured_exhaustion() -> String {
    let tool = json!({
        "type": "assistant",
        "message": { "content": [{
            "type": "tool_use", "id": "output-attempt", "name": "StructuredOutput",
            "input": serde_json::from_str::<Value>(VALID).unwrap(),
        }] },
    });
    format!(
        "{}\n{}\necho 'Warning: workspace trust is not configured' >&2\nexit 1",
        reply(&tool.to_string()),
        reply(STRUCTURED_EXHAUSTED),
    )
}

#[test]
fn structured_output_exhaustion_gets_a_fresh_reply_with_the_same_authority_and_budget() {
    let fixture = Fixture::new(&structured_exhaustion(), &reply(VALID));
    let Dispatched {
        outcome,
        sandbox_requests,
        superseded_traces,
        ..
    } = fixture.dispatch(admitted(1), 30);

    assert!(
        outcome.success,
        "invocations={}: {:?}",
        fixture.invocations(),
        outcome.message
    );
    assert_eq!(
        fixture.invocations(),
        2,
        "the old tool arguments cannot supply success"
    );
    assert_eq!(outcome.output["response_envelope_valid"], true);
    assert_eq!(outcome.output["summary"], "the answer");
    let record = &outcome.output["response_correction"];
    assert_eq!(record["rounds"], 1);
    let original = record["original_error"].as_str().unwrap();
    assert!(original.contains("exited with code 1"), "{original}");
    assert!(original.contains(STRUCTURED_SIGNATURE), "{original}");
    assert!(original.contains("after 5 attempts"), "{original}");
    assert!(!original.contains("workspace trust"), "{original}");
    assert!(record["original_stdout_blob_ref"].is_string());

    let first = fixture.envelope(1);
    let second = fixture.envelope(2);
    for key in ["instruction", "tools", "task", "model"] {
        assert_eq!(first.get(key), second.get(key), "{key}");
    }
    assert_eq!(first["input"]["question"], second["input"]["question"]);
    assert_eq!(fixture.argv(1), fixture.argv(2));
    assert_eq!(
        sandbox_requests,
        vec![("claude".to_string(), Some("reviewer".to_string())); 2]
    );
    let correction = &second["input"]["response_correction"];
    assert_eq!(correction["rejection"], STRUCTURED_SIGNATURE);
    assert_eq!(correction["rejected_reply_truncated"], true);
    assert!(
        !correction["rejected_reply"]
            .as_str()
            .unwrap()
            .contains("the answer"),
        "tool-call arguments are not a rejected reply"
    );
    let remaining = correction["remaining_budget_seconds"].as_u64().unwrap();
    assert!(remaining < 30);
    assert_eq!(
        outcome.output["effective_deadline_ms"],
        json!(remaining * 1000)
    );
    assert_eq!(
        superseded_traces.len(),
        1,
        "the failed invocation is persisted"
    );
    assert_eq!(superseded_traces[0].provider, "claude");
    assert_eq!(superseded_traces[0].trace.usage.input, 11);
    assert_eq!(superseded_traces[0].trace.usage.output, 7);
    assert_eq!(superseded_traces[0].trace.tool_calls.len(), 1);
    assert_eq!(
        superseded_traces[0].trace.tool_calls[0].tool_name,
        "StructuredOutput"
    );
    assert!(
        outcome.invocation.is_some(),
        "the final invocation stays on the outcome"
    );
}

#[test]
fn structured_output_exhaustion_without_a_policy_keeps_its_terminal_cause() {
    let fixture = Fixture::new(&structured_exhaustion(), &reply(VALID));
    let outcome = fixture.dispatch(None, 30).outcome;
    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert!(
        outcome.output.get("summary").is_none(),
        "no tool-argument salvage"
    );
    assert!(outcome.output.get("response_correction").is_none());
    let message = outcome.message.as_deref().unwrap();
    assert!(message.contains(STRUCTURED_SIGNATURE), "{message}");
    assert!(message.contains("after 5 attempts"), "{message}");
    assert!(!message.contains("workspace trust"), "{message}");

    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let mut frame: Value = serde_json::from_str(STRUCTURED_EXHAUSTED).unwrap();
    frame["errors"] = json!([format!(
        "Failed to provide valid structured output: {secret}"
    )]);
    let fixture = Fixture::new(
        &format!("{}; exit 1", reply(&frame.to_string())),
        &reply(VALID),
    );
    let outcome = fixture.dispatch(admitted(1), 30).outcome;
    assert!(outcome.success, "{:?}", outcome.message);
    let original = outcome.output["response_correction"]["original_error"]
        .as_str()
        .unwrap();
    assert!(original.contains(STRUCTURED_SIGNATURE), "{original}");
    assert!(
        !original.contains(secret),
        "the terminal cause must be redacted"
    );
    let correction = fixture.envelope(2);
    let diagnostic = correction["input"]["response_correction"]["rejected_reply"]
        .as_str()
        .unwrap();
    assert!(
        !diagnostic.contains(secret),
        "the correction must receive a redacted cause"
    );
}

#[test]
fn structured_output_exhaustion_cannot_accept_an_invalid_fresh_reply() {
    for later in [
        reply(r#"{"schemaVersion":1,"status":"success","result":{"summary":"partial"}"#),
        reply(DECLARED_FAILED),
        format!("{}; exit 3", reply(VALID)),
        structured_exhaustion(),
    ] {
        let fixture = Fixture::new(&structured_exhaustion(), &later);
        let Dispatched {
            outcome,
            superseded_traces,
            ..
        } = fixture.dispatch(admitted(1), 30);
        assert!(!outcome.success, "{later}");
        assert_eq!(fixture.invocations(), 2, "{later}");
        assert_eq!(superseded_traces.len(), 1);
        assert!(
            outcome
                .message
                .as_deref()
                .unwrap()
                .contains(STRUCTURED_SIGNATURE)
        );
        assert_eq!(outcome.output["response_correction"]["rounds"], 1);
        if later == structured_exhaustion() {
            assert_eq!(
                outcome.output["response_correction"]["refusal"],
                "rounds_exhausted"
            );
        }
    }
}

#[test]
fn structured_output_exhaustion_cannot_refill_the_original_budget() {
    let fixture = Fixture::new(&structured_exhaustion(), &reply(VALID));
    let outcome = fixture
        .dispatch(
            Some(ResponseCorrectionPolicy {
                max_rounds: 1,
                min_remaining_seconds: 30,
            }),
            30,
        )
        .outcome;
    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert_eq!(outcome.output["response_correction"]["rounds"], 0);
    assert_eq!(
        outcome.output["response_correction"]["refusal"],
        "wall_clock_exhausted"
    );
    assert!(
        outcome
            .message
            .as_deref()
            .unwrap()
            .contains(STRUCTURED_SIGNATURE)
    );

    let fixture = Fixture::new(&structured_exhaustion(), "exec sleep 60");
    let started = Instant::now();
    let outcome = fixture.dispatch(admitted(1), 4).outcome;
    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 2);
    assert_eq!(outcome.output["timed_out"], true);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        outcome
            .message
            .as_deref()
            .unwrap()
            .contains(STRUCTURED_SIGNATURE)
    );
}

#[test]
fn structured_output_markers_do_not_override_other_failures_or_incomplete_evidence() {
    let terminal = reply(STRUCTURED_EXHAUSTED);
    let changed = |key: &str, value: Value| {
        let mut frame: Value = serde_json::from_str(STRUCTURED_EXHAUSTED).unwrap();
        frame[key] = value;
        format!("{}; exit 1", reply(&frame.to_string()))
    };
    let quoted =
        |kind: &str| reply(&json!({"type": kind, "message": STRUCTURED_EXHAUSTED}).to_string());
    let cases = [
        (
            "auth hidden in errors",
            changed(
                "errors",
                json!([
                    "Failed to provide valid structured output after 5 attempts",
                    "Failed to authenticate"
                ]),
            ),
        ),
        (
            "capacity hidden in errors",
            changed(
                "errors",
                json!([
                    "Failed to provide valid structured output after 5 attempts",
                    "Selected model is at capacity"
                ]),
            ),
        ),
        (
            "refusal hidden in errors",
            changed(
                "errors",
                json!([
                    "Failed to provide valid structured output after 5 attempts",
                    "Request violates our usage policy"
                ]),
            ),
        ),
        (
            "API error hidden in errors",
            changed(
                "errors",
                json!([
                    "Failed to provide valid structured output after 5 attempts",
                    "API Error: 400 tools.0.custom.input_schema rejected"
                ]),
            ),
        ),
        ("wrong exit", format!("{terminal}; exit 2")),
        (
            "clean exit is not the admitted terminal outcome",
            terminal.clone(),
        ),
        ("missing terminal", "exit 1".to_string()),
        (
            "missing terminal reason",
            changed("terminal_reason", Value::Null),
        ),
        (
            "different terminal reason",
            changed("terminal_reason", json!("max_turns")),
        ),
        (
            "different subtype",
            changed("subtype", json!("error_max_turns")),
        ),
        ("missing result type", changed("type", Value::Null)),
        ("not an error", changed("is_error", json!(false))),
        (
            "assistant quote",
            format!("{}; exit 1", quoted("assistant")),
        ),
        ("tool quote", format!("{}; exit 1", quoted("user"))),
        ("stderr quote", format!("{terminal} >&2; exit 1")),
        (
            "truncated terminal",
            format!(
                "{}; exit 1",
                reply(&STRUCTURED_EXHAUSTED[..STRUCTURED_EXHAUSTED.len() - 1])
            ),
        ),
        (
            "garbage after terminal",
            format!("{terminal}; echo '{{'; exit 1"),
        ),
        (
            "terminal is not final",
            format!("{terminal}; {}; exit 1", quoted("assistant")),
        ),
        (
            "capture lost its middle",
            format!("head -c 2000000 /dev/zero | tr '\\0' x; echo; {terminal}; exit 1"),
        ),
        (
            "unknown earlier provider error",
            format!(
                "{}; {terminal}; exit 1",
                reply(r#"{"type":"result","is_error":true,"result":"unknown failure"}"#)
            ),
        ),
        (
            "authentication status",
            changed("api_error_status", json!(401)),
        ),
        (
            "refusal stop reason",
            changed("stop_reason", json!("refusal")),
        ),
        (
            "API rejection in provider message",
            changed(
                "message",
                json!("API Error: 400 tools.0.custom.input_schema rejected"),
            ),
        ),
        (
            "API rejection in provider text",
            changed(
                "text",
                json!("API Error: 400 tools.0.custom.input_schema rejected"),
            ),
        ),
        (
            "API schema rejection",
            changed(
                "result",
                json!("API Error: 400 tools.0.custom.input_schema: schema rejected"),
            ),
        ),
        (
            "authentication on stderr",
            format!(
                "{terminal}; echo 'Failed to authenticate. API Error: 401 authentication_error' >&2; exit 1"
            ),
        ),
        (
            "capacity on stderr",
            format!(
                "{terminal}; echo 'Selected model is at capacity. Please try a different model.' >&2; exit 1"
            ),
        ),
        (
            "usage limit on stderr",
            format!("{terminal}; echo 'Usage limit reached' >&2; exit 1"),
        ),
        (
            "refusal on stderr",
            format!("{terminal}; echo 'Request violates our usage policy' >&2; exit 1"),
        ),
        (
            "API schema rejection on stderr",
            format!(
                "{terminal}; echo 'API Error: 400 tools.0.custom.input_schema: schema rejected' >&2; exit 1"
            ),
        ),
        (
            "schema option failure",
            format!("{terminal}; echo 'unknown option --json-schema' >&2; exit 1"),
        ),
    ];
    for (case, first) in cases {
        let fixture = Fixture::new(&first, &reply(VALID));
        let outcome = fixture.dispatch(admitted(2), 30).outcome;
        assert!(!outcome.success, "{case}: {:?}", outcome.message);
        assert_eq!(fixture.invocations(), 1, "{case}: no correction");
        assert!(
            outcome.output.get("response_correction").is_none(),
            "{case}"
        );
    }

    // Even a complete matching terminal cannot turn an activity kill into a
    // formatting correction. The supervision clock makes this deterministic.
    let fixture = Fixture::new(
        &format!("{terminal}; touch \"$state/first-reply-ready\"; exec sleep 60"),
        &reply(VALID),
    );
    let outcome = fixture.dispatch_after_first_reply(admitted(1), 1).outcome;
    assert!(fixture.first_reply_ready());
    assert!(!outcome.success);
    assert_eq!(fixture.invocations(), 1);
    assert_eq!(outcome.output["timed_out"], true);
    assert!(outcome.output.get("response_correction").is_none());
}
