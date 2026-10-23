//! Read-only delegation at dispatch [ORB-16814]: an implementing activity's
//! `explore_instruction` reaches the agent only while its host offers
//! delegation, an explorer's unwrapped Codex runs in its read-only inner
//! sandbox, and an explorer whose provider has no read-only mode is refused
//! before spawn when Orbit's executor sandbox does not wrap it.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use orbit_engine::{DispatchOutcome, ResolvedSandbox};
use orbit_store::Store;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{EXPLORE_ACTIVITY, V2AuditEvent, V2AuditEventKind};
use serde_json::json;

use super::*;

const INSTRUCTION: &str = "Delegate read-only questions to an explorer.";

struct ExploreHost {
    cli: ScriptHost,
    offered: bool,
    sandbox: Option<ResolvedSandbox>,
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

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(self.sandbox.clone())
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
        sandbox: None,
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

/// The audited argv of one Codex dispatch of `activity_name`.
fn codex_argv(activity_name: &str, sandbox: Option<ResolvedSandbox>) -> Vec<String> {
    let temp = tempfile::tempdir().unwrap();
    let fake = fake_cli(
        "codex",
        "#!/bin/sh\ncat > /dev/null\necho '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    )
    .unwrap();
    let (writer, store) = build_writer(temp.path(), "explore-sandbox").unwrap();
    let mut spec = cli_agent_loop_spec(Some(Provider::Codex));
    spec.model = None;
    let host = ExploreHost {
        cli: ScriptHost::new_with_args(fake.cli_path(), vec!["exec".into(), "--json".into()]),
        offered: false,
        sandbox,
    };
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
        sandbox_mode(&codex_argv(EXPLORE_ACTIVITY, None)),
        Some("read-only"),
        "an explorer outside Orbit's executor sandbox must keep Codex's read-only inner \
         sandbox: it is the explorer's write boundary"
    );
    assert_eq!(
        sandbox_mode(&codex_argv(EXPLORE_ACTIVITY, Some(executor_sandbox_off()))),
        Some("read-only"),
        "an explicit executor opt-out still leaves Codex's read-only sandbox as the \
         explorer's write boundary"
    );
    assert_ne!(
        sandbox_mode(&codex_argv("agent_implement", None)),
        Some("read-only"),
        "an implementer keeps its configured Codex sandbox"
    );
    assert_eq!(
        sandbox_mode(&codex_argv("agent_implement", Some(executor_sandbox_off()))),
        Some("danger-full-access"),
        "an implementer's explicit sandbox opt-out still neutralizes Codex's inner sandbox"
    );
}

/// An executor that declares `sandbox: off`: preparation selects no wrapper.
fn executor_sandbox_off() -> ResolvedSandbox {
    ResolvedSandbox {
        kind: ExecutorSandboxKind::Off,
        fs_profile: ResolvedFsProfile {
            name: "off".to_string(),
            read: Vec::new(),
            modify: Vec::new(),
        },
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: Vec::new(),
        mask: None,
    }
}

/// A scratch worktree and the file a fake provider will try to create in it.
fn scratch_worktree(parent: &Path) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::Builder::new()
        .prefix("explore-")
        .tempdir_in(parent)
        .unwrap();
    let worktree = temp.path().join("worktree");
    fs::create_dir_all(worktree.join("protected")).unwrap();
    let worktree = worktree.canonicalize().unwrap();
    let marker = worktree.join("protected").join("mutated");
    (temp, worktree, marker)
}

/// Fake Claude that consumes its prompt and tries to create `marker`.
///
/// A denied write does not fail the process, so a wrapped explorer can still
/// return its envelope. An unwrapped process creates the file.
fn claude_that_attempts_write(marker: &Path) -> String {
    format!(
        "#!/bin/sh\ncat > /dev/null\nprintf mutated > '{marker}' 2>/dev/null || true\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
        marker = marker.display()
    )
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn audit_events(store: &Store, run_id: &str) -> Vec<V2AuditEvent> {
    events_snapshot(store, run_id).expect("audit events")
}

fn provider_started(events: &[V2AuditEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event.kind, V2AuditEventKind::CliInvocationStarted { .. }))
}

fn dispatch_claude(
    activity_name: &str,
    worktree: &Path,
    command: &Path,
    sandbox: Option<ResolvedSandbox>,
    run_id: &str,
    audit_root: &Path,
) -> (
    Result<DispatchOutcome, DispatchError>,
    std::sync::Arc<Store>,
) {
    let (writer, store) = build_writer(audit_root, run_id).unwrap();
    let host = ExploreHost {
        cli: ScriptHost::new(command),
        offered: false,
        sandbox,
    };
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name,
        spec: &ActivityV2Spec::AgentLoop(cli_agent_loop_spec(Some(Provider::Claude))),
        fs_profile: None,
        input: json!({
            "prompt": "where is the retry budget enforced",
            "workspace_path": worktree,
        }),
        audit: writer,
        run_id,
        host: Some(&host),
    });
    (outcome, store)
}

#[test]
fn an_unwrapped_explorer_without_a_read_only_mode_is_refused_before_spawn() {
    for (label, sandbox) in [("absent", None), ("off", Some(executor_sandbox_off()))] {
        let (temp, worktree, marker) = scratch_worktree(std::env::temp_dir().as_path());
        let command = temp.path().join("claude");
        write_executable(&command, &claude_that_attempts_write(&marker));
        let run_id = "explore-refuse";
        let (outcome, store) = dispatch_claude(
            EXPLORE_ACTIVITY,
            &worktree,
            &command,
            sandbox,
            run_id,
            temp.path(),
        );
        let error = outcome.expect_err("an unwrapped Claude explorer must be refused");
        let DispatchError::CliInvocationPermanent(message) = &error else {
            panic!(
                "{label}: the refusal is permanent, so a retry cannot spawn the provider: {error}"
            );
        };
        assert!(
            message.contains("no enforced read-only sandbox") && message.contains("claude"),
            "{label}: the refusal names the missing boundary and the provider: {message}"
        );
        assert!(
            !marker.exists(),
            "{label}: the fake provider must not modify the caller's worktree"
        );
        let events = audit_events(&store, run_id);
        assert!(
            events.iter().any(|event| {
                matches!(
                    event.kind,
                    V2AuditEventKind::ToolAllowlistHarnessDelegated { .. }
                )
            }),
            "{label}: dispatch reached provider preparation before refusing"
        );
        assert!(
            !provider_started(&events),
            "{label}: the provider is refused before spawn"
        );
    }
}

/// Outer sandbox this host can enforce, or `None` after reporting why the
/// wrapped path cannot run here.
fn enforced_outer_sandbox(worktree: &Path) -> Option<ResolvedSandbox> {
    let test = "v2_cli_agent::explore::a_wrapped_explorer_still_dispatches";
    let kind = {
        #[cfg(target_os = "linux")]
        {
            let probe = orbit_exec::probe_bwrap();
            if !probe.available {
                orbit_exec::report_bwrap_deferral(test, &probe.detail);
                return None;
            }
            ExecutorSandboxKind::LinuxBwrap
        }
        #[cfg(not(target_os = "linux"))]
        {
            if !orbit_exec::macos_sandbox_test_guard(test) {
                return None;
            }
            ExecutorSandboxKind::MacosSandboxExec
        }
    };
    let protected = worktree.join("protected");
    Some(ResolvedSandbox {
        kind,
        fs_profile: ResolvedFsProfile {
            name: "explorer-wrapped".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![
                format!("{}/**", worktree.display()),
                format!("!{}/**", protected.display()),
            ],
        },
        allow_fallback: false,
        managed_worktree: false,
        runtime_write_authority: Vec::new(),
        mask: None,
    })
}

#[test]
fn a_wrapped_explorer_still_dispatches() {
    // Bubblewrap replaces /tmp with a private tmpfs, so the worktree and the
    // fake provider have to live on the checkout filesystem, which the
    // read-only bind of `/` still shows. `/var/tmp` is not writable from
    // every host sandbox that runs this fixture.
    let parent = orbit_common::fs::path::ensure_orbit_scratch_dir(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )
    .expect("orbit scratch dir");
    let (temp, worktree, marker) = scratch_worktree(&parent);
    let Some(sandbox) = enforced_outer_sandbox(&worktree) else {
        return;
    };
    let command = worktree.join("claude");
    write_executable(&command, &claude_that_attempts_write(&marker));
    let run_id = "explore-wrapped";
    let (outcome, store) = dispatch_claude(
        EXPLORE_ACTIVITY,
        &worktree,
        &command,
        Some(sandbox),
        run_id,
        temp.path(),
    );
    let outcome = outcome.expect("a wrapped explorer still dispatches");
    assert!(
        outcome.success,
        "the wrapped fake provider returns its envelope"
    );
    assert!(
        !marker.exists(),
        "the wrapper denies the scratch-worktree write the fake provider attempts"
    );
    assert!(
        provider_started(&audit_events(&store, run_id)),
        "the wrapped provider is spawned, not refused"
    );
}

#[test]
fn an_implementer_with_the_executor_sandbox_off_still_runs() {
    let (temp, worktree, marker) = scratch_worktree(std::env::temp_dir().as_path());
    let command = temp.path().join("claude");
    write_executable(&command, &claude_that_attempts_write(&marker));
    let run_id = "implement-off";
    let (outcome, store) = dispatch_claude(
        "agent_implement",
        &worktree,
        &command,
        Some(executor_sandbox_off()),
        run_id,
        temp.path(),
    );
    let outcome = outcome.expect("an implementer's sandbox opt-out still dispatches");
    assert!(
        outcome.success,
        "the unsandboxed implementer returns its envelope"
    );
    assert!(
        marker.exists(),
        "an implementer's explicit sandbox opt-out still lets the provider write"
    );
    assert!(
        provider_started(&audit_events(&store, run_id)),
        "the implementer provider is spawned"
    );
}
