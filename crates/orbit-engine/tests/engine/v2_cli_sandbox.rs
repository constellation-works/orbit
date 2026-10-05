//! Missing trusted macOS wrapper admission through the public dispatch boundary.
//! Runs on Linux as well, without changing PATH or removing a host executable.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_common::OrbitError;
use orbit_engine::{
    DispatchError, PluginBrokerHandle, PluginBrokerRun, ResolvedCliExecutor, ResolvedSandbox,
    RuntimeHost, V2AuditWriter, V2DispatchInput, dispatch_v2_activity,
};
use orbit_store::Store;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::workflow::ExecutorSandboxKind;
use orbit_types::workflow::activity_job::{ActivityV2Spec, V2AuditEventKind};
use serde_json::Value;

#[test]
fn missing_macos_wrapper_preserves_provider_confinement_and_honest_audit() {
    if orbit_exec::sandbox_exec_available() {
        // This regression exercises a missing trusted wrapper, not the macOS
        // kernel's enforced path. Never remove a host binary to force it.
        return;
    }

    for allow_fallback in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let command = temp.path().join("codex");
        fs::write(
            &command,
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" > \"$0.args\"\n\
             printf '%s' \"${ORBIT_PLUGIN_BROKER-}\" > \"$0.broker\"\n\
             cat > /dev/null\n\
             printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
        )
        .unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let host = MissingWrapperHost {
            command: command.clone(),
            allow_fallback,
            broker_starts: AtomicUsize::new(0),
        };
        let audit = V2AuditWriter::with_disk_sinks(
            &temp.path().join("audit"),
            Arc::new(Store::open_in_memory().unwrap()),
            "ws-sandbox",
            "missing-wrapper",
            "codex".to_string(),
            None,
        )
        .unwrap();
        let spec = ActivityV2Spec::AgentLoop(
            serde_json::from_value(serde_json::json!({
                "instruction": "sandbox admission fixture",
                "provider": "codex",
                "tools": [],
                "wall_clock_timeout_seconds": 10
            }))
            .unwrap(),
        );
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "sandbox_fixture",
            spec: &spec,
            fs_profile: None,
            input: serde_json::json!({"prompt": "check admission"}),
            audit: audit.clone(),
            run_id: "missing-wrapper",
            host: Some(&host),
        });
        assert_eq!(
            host.broker_starts.load(Ordering::SeqCst),
            0,
            "an unavailable outer wrapper must never authorize a plugin broker"
        );
        let events = audit.events_snapshot().unwrap();
        let started = events.iter().find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted {
                argv_redacted,
                sandbox_backend,
                sandbox_write_enforcement,
                sandbox_read_enforcement,
                ..
            } => Some((
                argv_redacted,
                sandbox_backend,
                sandbox_write_enforcement,
                sandbox_read_enforcement,
            )),
            _ => None,
        });
        if allow_fallback {
            assert!(outcome.unwrap().success);
            let (argv, backend, writes, reads) = started.expect("bare invocation is audited");
            assert_eq!(backend.as_deref(), Some("bare-fallback"));
            assert_eq!(writes.as_deref(), Some("write_delegated"));
            assert_eq!(reads.as_deref(), Some("read_delegated"));
            let actual_args = fs::read_to_string(temp.path().join("codex.args")).unwrap();
            let actual_argv = std::iter::once(command.to_str().unwrap())
                .chain(actual_args.lines())
                .collect::<Vec<_>>();
            assert_eq!(
                argv, &actual_argv,
                "audit must describe the bare process argv"
            );
            assert!(
                argv.windows(2)
                    .any(|pair| pair == ["--sandbox", "read-only"]),
                "bare fallback must retain the provider's native write confinement"
            );
            assert_eq!(
                fs::read_to_string(temp.path().join("codex.broker")).unwrap(),
                "",
                "bare fallback must receive no plugin broker capability"
            );
        } else {
            assert!(matches!(
                outcome,
                Err(DispatchError::CliInvocationPermanent(_))
            ));
            assert!(
                started.is_none(),
                "refused admission must not audit a launch"
            );
            assert!(
                !temp.path().join("codex.args").exists(),
                "refused admission must not run the provider"
            );
        }
    }
}

struct MissingWrapperHost {
    command: PathBuf,
    allow_fallback: bool,
    broker_starts: AtomicUsize,
}

impl RuntimeHost for MissingWrapperHost {
    fn run_deterministic(
        &self,
        _action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            "unused".into(),
        ))
    }

    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.display().to_string(),
            args: Vec::new(),
        })
    }

    fn provider_cli_config(&self, _provider: &str) -> HashMap<String, String> {
        HashMap::from([("sandbox".into(), "read-only".into())])
    }

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(Some(ResolvedSandbox {
            kind: ExecutorSandboxKind::MacosSandboxExec,
            fs_profile: ResolvedFsProfile {
                name: "fixture".into(),
                read: vec!["**".into()],
                modify: vec![format!("{}/**", self.command.parent().unwrap().display())],
            },
            allow_fallback: self.allow_fallback,
            managed_worktree: false,
            runtime_write_authority: Vec::new(),
            mask: None,
        }))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: Some(self.command.parent().unwrap().to_path_buf()),
            ..Default::default()
        }
    }

    fn start_plugin_broker(
        &self,
        _run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        self.broker_starts.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}
