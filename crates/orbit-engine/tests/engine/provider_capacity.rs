//! [ORB-14149] A provider whose selected model is at capacity, and
//! [ORB-14266] one whose content policy refused the turn, run through
//! `dispatch_v2_activity` and `execute_job_with_resume` against fake provider
//! CLIs.
//!
//! Only text the provider wrote about itself — stderr, or its own failure
//! frames on a failed exit — types the failure. The typed run skips step and
//! final recovery, so the same model is invoked once and the worktree keeps
//! the candidate for the failure handoff; every other failure still reaches
//! recovery and its post-recovery attempt.
//!
//! [ORB-14260] A claimed worker whose owner calls could not reach its run's
//! coordinator declares `owner_route_unavailable`; repair cannot open the
//! route either, so that run takes the same skip.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, JobOutcome,
    ResolvedCliExecutor, RuntimeHost, V2AuditWriter, V2DispatchInput, dispatch_v2_activity,
    execute_job_with_resume,
};
use orbit_store::Store;
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, DeterministicSpec, JobV2, JobV2StepBody, OnDenial,
    Provider, V2AuditEvent, V2AuditEventKind,
};
use orbit_types::workflow::{
    BaselineRedHold, failed_provider, is_baseline_red_failure, is_owner_route_unavailable,
    is_provider_capacity_exhausted, is_provider_failure, is_provider_refusal,
    is_provider_unavailable,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const CAPACITY: &str = "Selected model is at capacity. Please try a different model.";

/// What Codex wrote when `gpt-6.1-sol` was at capacity (run
/// jrun-20261005-0456-c6): its own `error` and `turn.failed` frames after the
/// turn's tool traffic.
fn codex_capacity_frames() -> String {
    format!(
        r#"{{"type":"item.completed","item":{{"id":"item_1","type":"command_execution","command":"cargo test","aggregated_output":"","exit_code":0,"status":"completed"}}}}
{{"type":"error","message":"{CAPACITY}"}}
{{"type":"turn.failed","error":{{"message":"{CAPACITY}"}}}}"#
    )
}

const CONTENT_FILTER: &str = "This content was flagged for possible cybersecurity risk. If this \
     seems wrong, try rephrasing your request. To get authorized for security work, join the \
     Trusted Access for Cyber program: https://chatgpt.com/cyber";

/// What Codex wrote when its cybersecurity content filter ended a review of
/// sandbox code (runs jrun-20261006-0104-c24/c29): its own `error` and
/// `turn.failed` frames after the turn's tool traffic.
fn codex_content_filter_frames() -> String {
    format!(
        r#"{{"type":"item.completed","item":{{"id":"item_1","type":"command_execution","command":"rg sandbox","aggregated_output":"","exit_code":0,"status":"completed"}}}}
{{"type":"error","message":"{CONTENT_FILTER}"}}
{{"type":"turn.failed","error":{{"message":"{CONTENT_FILTER}"}}}}"#
    )
}

const SUCCESS_ENVELOPE: &str = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

/// A fake provider binary named for the provider it stands in for. Each
/// invocation appends a line to `invocations`.
struct FakeProvider {
    dir: TempDir,
    path: PathBuf,
}

impl FakeProvider {
    fn new(binary: &str, stdout: &str, stderr: &str, exit_code: i32, before: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(binary);
        let invocations = dir.path().join("invocations");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\ncat > /dev/null\necho invoked >> '{}'\n{before}\n\
                 cat <<'STDOUT'\n{stdout}\nSTDOUT\ncat >&2 <<'STDERR'\n{stderr}\nSTDERR\n\
                 exit {exit_code}\n",
                invocations.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, path }
    }

    fn invocations(&self) -> usize {
        fs::read_to_string(self.dir.path().join("invocations"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

fn agent_spec(provider: Provider) -> AgentLoopSpec {
    AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "implement the task".to_string(),
        tools: vec![],
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    }
}

fn writer(root: &Path, run_id: &str) -> Arc<V2AuditWriter> {
    let audit = root.join("audit");
    fs::create_dir_all(&audit).unwrap();
    V2AuditWriter::with_disk_sinks(
        &audit,
        Arc::new(Store::open_in_memory().unwrap()),
        "ws_capacity",
        run_id,
        "capacity".to_string(),
        None,
    )
    .unwrap()
}

/// Stands in for Core: resolves the fake provider, records deterministic
/// actions, and admits final recovery so a skip is the executor's own.
struct CapacityHost {
    cli: PathBuf,
    worktree: PathBuf,
    actions: Mutex<Vec<(String, Value)>>,
    final_recovery_admissions: Mutex<usize>,
}

impl CapacityHost {
    fn new(cli: &Path, worktree: &Path) -> Self {
        Self {
            cli: cli.to_path_buf(),
            worktree: worktree.to_path_buf(),
            actions: Mutex::new(Vec::new()),
            final_recovery_admissions: Mutex::new(0),
        }
    }

    fn calls(&self, action: &str) -> Vec<Value> {
        self.actions
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }
}

impl RuntimeHost for CapacityHost {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.actions
            .lock()
            .unwrap()
            .push((action.to_string(), input.clone()));
        if action == "test_stub_red_base_validate" {
            let hold = BaselineRedHold {
                base_ref: "origin/main".to_string(),
                base_sha: "b".repeat(40),
                command: "make ci-lint".to_string(),
                run_id: String::new(),
            };
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: hold.text("required validation 'make ci-lint' fails on the base too"),
            });
        }
        if action == "test_stub_candidate_validate" {
            return Err(DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: "required validation 'make test' did not pass on candidate head"
                    .to_string(),
            });
        }
        Ok(match action {
            "setup" => json!({ "workspace_path": self.worktree, "base_ref": "main" }),
            _ => json!({ "action": action }),
        })
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.cli.to_string_lossy().into_owned(),
            args: Vec::new(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext::default()
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        _request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        *self.final_recovery_admissions.lock().unwrap() += 1;
        Ok(FinalRecoveryAdmission::Admitted)
    }
}

fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": { "type": "deterministic", "description": name, "action": name, "config": {} },
    });
    load_activity_asset(&asset.to_string()).unwrap().spec
}

/// `setup → implement_one`, the implementation step with `step_fix` as its
/// recovery, and the job's `decide` final recovery and `handoff` failure
/// activity.
fn implementation_job(spec: AgentLoopSpec) -> JobV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "provider_capacity_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [
                { "id": "setup", "spec": { "type": "deterministic", "action": "setup", "config": {} } },
                { "id": "implement_one", "spec": ActivityV2Spec::AgentLoop(spec) },
            ],
        },
    });
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    job.steps[1].recovery_activity = Some("step_fix".to_string());
    job.steps[1].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));
    job
}

struct JobRun {
    outcome: Result<JobOutcome, DispatchError>,
    events: Vec<V2AuditEvent>,
}

fn run_job(job: &JobV2, host: &CapacityHost, run_id: &str) -> JobRun {
    let audit = tempfile::tempdir().unwrap();
    let writer = writer(audit.path(), run_id);
    let outcome = execute_job_with_resume(
        job,
        json!({ "prompt": "implement", "task_ids": ["T-1"] }),
        run_id,
        writer.clone(),
        host,
        None,
    );
    JobRun {
        outcome,
        events: writer.events_snapshot().unwrap(),
    }
}

fn count(events: &[V2AuditEvent], matches: impl Fn(&V2AuditEventKind) -> bool) -> usize {
    events.iter().filter(|event| matches(&event.kind)).count()
}

fn failure_message(outcome: &Result<JobOutcome, DispatchError>) -> String {
    match outcome {
        Ok(outcome) => {
            assert!(!outcome.success, "the run must fail: {outcome:?}");
            outcome.message.clone().unwrap_or_default()
        }
        Err(error) => error.to_string(),
    }
}

/// Capacity is typed only from the provider's own failure, never from the
/// agent's transcript or an Orbit envelope, and never on a turn that finished.
#[test]
fn capacity_is_typed_only_from_text_the_failed_provider_wrote() {
    let envelope_answer = format!(
        r#"{{"type":"item.completed","item":{{"type":"agent_message","text":{}}}}}"#,
        serde_json::to_string(SUCCESS_ENVELOPE).unwrap()
    );
    let cases: Vec<(&str, Provider, String, &str, i32, bool)> = vec![
        // Codex's own error and turn.failed frames on a failed exit.
        (
            "codex",
            Provider::Codex,
            codex_capacity_frames(),
            "",
            1,
            true,
        ),
        // The same text on the provider's stderr.
        ("codex", Provider::Codex, String::new(), CAPACITY, 1, true),
        // A claude error result is the provider's own frame too.
        (
            "claude",
            Provider::Claude,
            format!(r#"{{"type":"result","is_error":true,"result":"{CAPACITY}"}}"#),
            "",
            1,
            true,
        ),
        // Reported mid-turn, then the turn finished: not unavailable.
        (
            "codex",
            Provider::Codex,
            format!("{}\n{envelope_answer}", codex_capacity_frames()),
            "",
            0,
            false,
        ),
        // The agent's own answer quotes the text.
        (
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{CAPACITY}"}}}}"#
            ),
            "",
            1,
            false,
        ),
        // A tool the agent ran printed it.
        (
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"command_execution","command":"probe","aggregated_output":"{CAPACITY}","exit_code":1,"status":"failed"}}}}"#
            ),
            "",
            1,
            false,
        ),
        // An Orbit envelope describes the work, not the provider.
        (
            "claude",
            Provider::Claude,
            format!(
                r#"{{"schemaVersion":1,"status":"failed","result":{{}},"error":{{"code":"x","message":"{CAPACITY}"}}}}"#
            ),
            "",
            1,
            false,
        ),
        // An ordinary failed exit.
        ("codex", Provider::Codex, String::new(), "boom", 1, false),
    ];
    for (index, (binary, provider, stdout, stderr, exit_code, capacity)) in
        cases.into_iter().enumerate()
    {
        let fake = FakeProvider::new(binary, &stdout, stderr, exit_code, "");
        let run_id = format!("capacity-{index}");
        let audit = tempfile::tempdir().unwrap();
        let host = CapacityHost::new(&fake.path, audit.path());
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "capacity_fixture",
            spec: &ActivityV2Spec::AgentLoop(agent_spec(provider)),
            fs_profile: None,
            input: json!({ "prompt": "implement" }),
            audit: writer(audit.path(), &run_id),
            run_id: &run_id,
            host: Some(&host),
        })
        .unwrap();
        let message = outcome.message.as_deref();
        assert_eq!(
            is_provider_capacity_exhausted(None, message),
            capacity,
            "case {index} ({binary} exit {exit_code}): {outcome:?}"
        );
        assert_eq!(
            is_provider_unavailable(None, message),
            capacity,
            "capacity is the only unavailability here: case {index}: {outcome:?}"
        );
        if capacity {
            assert!(!outcome.success, "case {index}: {outcome:?}");
            assert_eq!(
                message.and_then(failed_provider),
                Some(binary),
                "the failure names the provider a hold excludes: {message:?}"
            );
            assert!(
                message.is_some_and(|message| message.contains(CAPACITY)),
                "the operator sees the provider's own words: {message:?}"
            );
        }
    }
}

/// The incident run: recovery cannot change capacity and the rerun used the
/// same model. Now the provider is invoked once, neither recovery runs, and
/// the worktree still holds the partial candidate for the failure handoff.
#[test]
fn a_capacity_failure_skips_recovery_and_keeps_the_candidate() {
    let worktree = tempfile::tempdir().unwrap();
    let candidate = worktree.path().join("candidate.rs");
    let fake = FakeProvider::new(
        "codex",
        &codex_capacity_frames(),
        "",
        1,
        &format!("echo partial > '{}'", candidate.display()),
    );
    let host = CapacityHost::new(&fake.path, worktree.path());
    let run = run_job(
        &implementation_job(agent_spec(Provider::Codex)),
        &host,
        "capacity-run",
    );

    let message = failure_message(&run.outcome);
    assert!(
        is_provider_capacity_exhausted(None, Some(&message))
            && is_provider_unavailable(None, Some(&message)),
        "the run keeps the typed marker for pull-drain settlement: {message}"
    );
    assert_eq!(fake.invocations(), 1, "the same model is not rerun");
    assert!(host.calls("step_fix").is_empty(), "no step recovery runs");
    assert_eq!(
        count(&run.events, |kind| matches!(
            kind,
            V2AuditEventKind::StepRecoveryAttempted { .. }
                | V2AuditEventKind::StepPostRecoveryAttempt { .. }
        )),
        0,
        "recovery admission and the post-recovery attempt are skipped"
    );
    assert_eq!(
        *host.final_recovery_admissions.lock().unwrap(),
        0,
        "final recovery is not admitted"
    );
    assert!(host.calls("decide").is_empty());
    assert_eq!(
        count(&run.events, |kind| matches!(
            kind,
            V2AuditEventKind::FinalRecoveryAttempted { outcome, .. } if outcome == "skipped"
        )),
        1,
        "the skip is audited"
    );
    let handoff = host.calls("handoff");
    assert_eq!(handoff.len(), 1, "the failure handoff keeps the candidate");
    assert_eq!(handoff[0]["failed_step_id"], "implement_one");
    assert_eq!(handoff[0]["error_code"], "provider_capacity", "{handoff:?}");
    assert!(
        handoff[0]["error_message"]
            .as_str()
            .is_some_and(|message| is_provider_capacity_exhausted(None, Some(message))),
        "{handoff:?}"
    );
    assert_eq!(
        fs::read_to_string(&candidate).unwrap().trim(),
        "partial",
        "the worktree still holds the partial candidate"
    );
}

/// [ORB-14266] A content-policy refusal is typed only from the provider's own
/// failure: Codex's content-filter frames on a failed exit, or Claude's
/// terminal result stopping for a refusal, which fails the turn even on exit
/// 0. It is a provider failure, but not an unavailability.
#[test]
fn refusal_is_typed_only_from_text_the_failed_provider_wrote() {
    let envelope_answer = format!(
        r#"{{"type":"item.completed","item":{{"type":"agent_message","text":{}}}}}"#,
        serde_json::to_string(SUCCESS_ENVELOPE).unwrap()
    );
    let claude_answer = serde_json::to_string(SUCCESS_ENVELOPE).unwrap();
    let cases: Vec<(&str, Provider, String, &str, i32, bool)> = vec![
        // The incident: Codex's own content-filter frames on a failed exit.
        (
            "codex",
            Provider::Codex,
            codex_content_filter_frames(),
            "",
            1,
            true,
        ),
        // The same text on the provider's stderr.
        ("codex", Provider::Codex, String::new(), CONTENT_FILTER, 1, true),
        // Claude's terminal result stopped for a refusal: no answer stands,
        // whatever the exit code.
        (
            "claude",
            Provider::Claude,
            format!(
                r#"{{"type":"result","subtype":"success","is_error":false,"stop_reason":"refusal","result":{claude_answer}}}"#
            ),
            "",
            0,
            true,
        ),
        // Claude Code's usage-policy error result.
        (
            "claude",
            Provider::Claude,
            r#"{"type":"result","is_error":true,"result":"API Error: Claude Code is unable to respond to this request, which appears to violate our Usage Policy."}"#
                .to_string(),
            "",
            1,
            true,
        ),
        // Flagged mid-turn, then the turn finished: not a refusal.
        (
            "codex",
            Provider::Codex,
            format!("{}\n{envelope_answer}", codex_content_filter_frames()),
            "",
            0,
            false,
        ),
        // The agent's own answer quotes the filter.
        (
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{CONTENT_FILTER}"}}}}"#
            ),
            "",
            1,
            false,
        ),
        // A tool the agent ran printed it.
        (
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"command_execution","command":"probe","aggregated_output":"{CONTENT_FILTER}","exit_code":1,"status":"failed"}}}}"#
            ),
            "",
            1,
            false,
        ),
    ];
    for (index, (binary, provider, stdout, stderr, exit_code, refusal)) in
        cases.into_iter().enumerate()
    {
        let fake = FakeProvider::new(binary, &stdout, stderr, exit_code, "");
        let run_id = format!("refusal-{index}");
        let audit = tempfile::tempdir().unwrap();
        let host = CapacityHost::new(&fake.path, audit.path());
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "refusal_fixture",
            spec: &ActivityV2Spec::AgentLoop(agent_spec(provider)),
            fs_profile: None,
            input: json!({ "prompt": "implement" }),
            audit: writer(audit.path(), &run_id),
            run_id: &run_id,
            host: Some(&host),
        })
        .unwrap();
        let message = outcome.message.as_deref();
        assert_eq!(
            is_provider_refusal(None, message),
            refusal,
            "case {index} ({binary} exit {exit_code}): {outcome:?}"
        );
        assert!(
            !is_provider_unavailable(None, message),
            "a refusal is not an unavailability: case {index}: {outcome:?}"
        );
        if refusal {
            assert!(!outcome.success, "case {index}: {outcome:?}");
            assert_eq!(
                message.and_then(failed_provider),
                Some(binary),
                "{message:?}"
            );
        }
    }
}

/// The incident runs: final recovery escalated a content-filter refusal it
/// could not change. Now neither recovery runs, the provider is invoked once,
/// and the failure handoff gets the typed code with the candidate intact.
#[test]
fn a_content_filter_refusal_skips_recovery_and_keeps_the_candidate() {
    let worktree = tempfile::tempdir().unwrap();
    let candidate = worktree.path().join("candidate.rs");
    let fake = FakeProvider::new(
        "codex",
        &codex_content_filter_frames(),
        "",
        1,
        &format!("echo partial > '{}'", candidate.display()),
    );
    let host = CapacityHost::new(&fake.path, worktree.path());
    let run = run_job(
        &implementation_job(agent_spec(Provider::Codex)),
        &host,
        "refusal-run",
    );

    let message = failure_message(&run.outcome);
    assert!(
        is_provider_refusal(None, Some(&message)) && is_provider_failure(None, Some(&message)),
        "the run keeps the typed marker for run finalization: {message}"
    );
    assert_eq!(fake.invocations(), 1, "the refused task is not resent");
    assert!(host.calls("step_fix").is_empty(), "no step recovery runs");
    assert_eq!(
        *host.final_recovery_admissions.lock().unwrap(),
        0,
        "final recovery is not admitted"
    );
    assert!(host.calls("decide").is_empty(), "no final recovery runs");
    let handoff = host.calls("handoff");
    assert_eq!(handoff.len(), 1, "the failure handoff keeps the candidate");
    assert_eq!(handoff[0]["error_code"], "provider_refusal", "{handoff:?}");
    assert_eq!(
        fs::read_to_string(&candidate).unwrap().trim(),
        "partial",
        "the worktree still holds the partial candidate"
    );
}

/// A declared `owner_route_unavailable` envelope is typed for the pull drain
/// and skips both recoveries; the same envelope with any other code reaches
/// step recovery (the last case of the test below).
#[test]
fn a_declared_owner_route_failure_skips_recovery() {
    let worktree = tempfile::tempdir().unwrap();
    let fake = FakeProvider::new(
        "claude",
        r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"owner_route_unavailable","message":"the coordinator could not be reached"}}"#,
        "",
        0,
        "",
    );
    let host = CapacityHost::new(&fake.path, worktree.path());
    let run = run_job(
        &implementation_job(agent_spec(Provider::Claude)),
        &host,
        "owner-route-run",
    );

    let message = failure_message(&run.outcome);
    assert!(
        is_owner_route_unavailable(None, Some(&message)),
        "the run keeps the typed marker for pull-drain settlement: {message}"
    );
    assert!(!is_provider_failure(None, Some(&message)), "{message}");
    assert_eq!(fake.invocations(), 1, "the step is not rerun");
    assert!(host.calls("step_fix").is_empty(), "no step recovery runs");
    assert_eq!(
        *host.final_recovery_admissions.lock().unwrap(),
        0,
        "final recovery is not admitted"
    );
    assert!(host.calls("decide").is_empty());
    assert_eq!(
        count(&run.events, |kind| matches!(
            kind,
            V2AuditEventKind::FinalRecoveryAttempted { outcome, .. } if outcome == "skipped"
        )),
        1,
        "the skip is audited"
    );
    assert_eq!(host.calls("handoff").len(), 1);
}

/// Every failure that is not provider capacity still gets step recovery and
/// its post-recovery attempt: a failed exit (whose answer may even quote the
/// capacity text), a declared `status: "failed"` envelope, and a timeout.
#[test]
fn other_provider_failures_still_reach_recovery_and_the_post_recovery_attempt() {
    let failed_envelope = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"red","message":"tests failed"}}"#;
    let cases: Vec<(&str, Provider, String, &str, i32, &str, u64)> = vec![
        ("codex", Provider::Codex, String::new(), "boom", 1, "", 30),
        (
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{CAPACITY}"}}}}"#
            ),
            "",
            1,
            "",
            30,
        ),
        (
            "claude",
            Provider::Claude,
            failed_envelope.to_string(),
            "",
            0,
            "",
            30,
        ),
        (
            "claude",
            Provider::Claude,
            String::new(),
            "",
            0,
            "sleep 10",
            1,
        ),
    ];
    for (index, (binary, provider, stdout, stderr, exit_code, before, timeout)) in
        cases.into_iter().enumerate()
    {
        let worktree = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new(binary, &stdout, stderr, exit_code, before);
        let host = CapacityHost::new(&fake.path, worktree.path());
        let mut spec = agent_spec(provider);
        spec.wall_clock_timeout_seconds = timeout;
        let mut job = implementation_job(spec);
        job.final_recovery_activity = None;
        job.resolved_final_recovery_activity = None;
        let run = run_job(&job, &host, &format!("recovered-{index}"));

        let message = failure_message(&run.outcome);
        assert!(
            !is_provider_unavailable(None, Some(&message)),
            "case {index}: {message}"
        );
        assert_eq!(host.calls("step_fix").len(), 1, "case {index}: {message}");
        assert_eq!(
            fake.invocations(),
            2,
            "case {index}: the post-recovery attempt reruns the step"
        );
        assert_eq!(
            count(&run.events, |kind| matches!(
                kind,
                V2AuditEventKind::StepPostRecoveryAttempt { .. }
            )),
            1,
            "case {index}"
        );
    }
}

/// A required validation failure remains a candidate defect, so it still
/// spends step recovery and reruns validation on the repaired candidate.
#[test]
fn a_required_validation_failure_still_reaches_step_recovery_and_post_recovery_attempt() {
    let worktree = tempfile::tempdir().unwrap();
    let fake = FakeProvider::new("codex", "", "", 0, "");
    let host = CapacityHost::new(&fake.path, worktree.path());
    let mut job = implementation_job(agent_spec(Provider::Codex));
    let JobV2StepBody::Target(target) = &mut job.steps[1].body else {
        panic!("the inline validation fixture is a target step");
    };
    target.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: "test_stub_candidate_validate".to_string(),
        config: json!({ "commands": ["make test"] }),
    });
    job.final_recovery_activity = None;
    job.resolved_final_recovery_activity = None;

    let run = run_job(&job, &host, "validation-recovery-run");
    let message = failure_message(&run.outcome);

    assert!(
        message.contains("required validation 'make test' did not pass"),
        "the validation failure remains the terminal failure: {message}"
    );
    assert_eq!(host.calls("test_stub_candidate_validate").len(), 2);
    assert_eq!(host.calls("step_fix").len(), 1);
    assert_eq!(
        count(&run.events, |kind| matches!(
            kind,
            V2AuditEventKind::StepPostRecoveryAttempt { .. }
        )),
        1,
        "validation runs once before and once after recovery"
    );
}

/// [ORB-14258] A required validation failure the base shares is not the
/// candidate's: no step or final recovery is dispatched, so the provider is
/// not invoked again, and the failure handoff receives the typed failure.
#[test]
fn a_red_base_validation_failure_dispatches_no_recovery() {
    let worktree = tempfile::tempdir().unwrap();
    let answer = format!(
        r#"{{"type":"item.completed","item":{{"type":"agent_message","text":{}}}}}"#,
        serde_json::to_string(SUCCESS_ENVELOPE).unwrap()
    );
    let fake = FakeProvider::new("codex", &answer, "", 0, "");
    let host = CapacityHost::new(&fake.path, worktree.path());
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Job",
        "metadata": { "name": "red_base_fixture" },
        "spec": {
            "state": "enabled",
            "kind": "workflow",
            "steps": [
                { "id": "setup", "spec": { "type": "deterministic", "action": "setup", "config": {} } },
                { "id": "implement_one", "spec": ActivityV2Spec::AgentLoop(agent_spec(Provider::Codex)) },
                { "id": "validate", "spec": { "type": "deterministic", "action": "test_stub_red_base_validate", "config": {} } },
            ],
        },
    });
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    job.steps[2].recovery_activity = Some("step_fix".to_string());
    job.steps[2].resolved_recovery_activity = Some(deterministic_activity("step_fix"));
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(deterministic_activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(deterministic_activity("decide"));

    let run = run_job(&job, &host, "red-base-run");

    let message = failure_message(&run.outcome);
    assert!(is_baseline_red_failure(None, Some(&message)), "{message}");
    assert_eq!(
        fake.invocations(),
        1,
        "only the implementation step ran the provider"
    );
    assert_eq!(
        host.calls("test_stub_red_base_validate").len(),
        1,
        "no post-recovery attempt"
    );
    assert!(host.calls("step_fix").is_empty(), "no step recovery runs");
    assert_eq!(*host.final_recovery_admissions.lock().unwrap(), 0);
    assert!(host.calls("decide").is_empty(), "no final recovery runs");
    let handoff = host.calls("handoff");
    assert_eq!(handoff.len(), 1);
    assert_eq!(handoff[0]["failed_step_id"], "validate");
    assert_eq!(handoff[0]["error_code"], "baseline_red", "{handoff:?}");
    assert!(
        handoff[0]["error_message"]
            .as_str()
            .and_then(BaselineRedHold::from_text)
            .is_some(),
        "the handoff can read the hold: {handoff:?}"
    );
}
