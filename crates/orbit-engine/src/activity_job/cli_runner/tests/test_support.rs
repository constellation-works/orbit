use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::BlobStore;
use orbit_common::OrbitError;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, ResolvedCliExecutor, ResolvedSandbox};
use crate::context::{ResolvedActivityTools, RuntimeHost};

pub(in crate::activity_job::cli_runner) fn sh_args(script: &str) -> Vec<String> {
    vec!["-c".to_string(), script.to_string()]
}

/// A host that launches `command` as the provider CLI and otherwise keeps the
/// trait defaults (no task context), except that deny-mode tool policies
/// resolve against a fixed registry. Sandbox, primary checkout, and a
/// persistence-refresh failure are optional so a test can force one post-run
/// check without standing up the rest of the runtime.
pub(in crate::activity_job::cli_runner) struct TestHost {
    command: String,
    sandbox: Option<ResolvedSandbox>,
    workspace_root: Option<PathBuf>,
    refresh_error: Option<String>,
}

impl TestHost {
    pub(in crate::activity_job::cli_runner) fn with_command(command: String) -> Self {
        Self {
            command,
            sandbox: None,
            workspace_root: None,
            refresh_error: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(in crate::activity_job::cli_runner) fn with_sandbox(
        mut self,
        sandbox: ResolvedSandbox,
    ) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub(in crate::activity_job::cli_runner) fn with_workspace_root(
        mut self,
        root: PathBuf,
    ) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub(in crate::activity_job::cli_runner) fn fail_persistence_refresh(
        mut self,
        message: impl Into<String>,
    ) -> Self {
        self.refresh_error = Some(message.into());
        self
    }
}

impl RuntimeHost for TestHost {
    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.clone(),
            args: Vec::new(),
        })
    }

    /// Deny mode over the fixed [`TEST_REGISTERED_TOOLS`] registry.
    fn resolve_activity_tool_denials(
        &self,
        _task_ids: &[String],
        _activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        Ok(ResolvedActivityTools {
            requested_tools: Vec::new(),
            effective_tools: orbit_types::workflow::tools_allowed_by_disallow_list(
                disallow_list,
                TEST_REGISTERED_TOOLS.iter().copied(),
            ),
            omitted_requirement_notes: Vec::new(),
        })
    }

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(self.sandbox.clone())
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: self.workspace_root.clone(),
            ..orbit_tools::ToolContext::default()
        }
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        match &self.refresh_error {
            Some(message) => Err(OrbitError::Store(message.clone())),
            None => Ok(()),
        }
    }
}

/// The registry [`TestHost::resolve_activity_tool_denials`] resolves against.
const TEST_REGISTERED_TOOLS: &[&str] = &[
    "orbit.task.show",
    "orbit.search",
    "orbit.workflow.ship",
    "proc.spawn",
    "github.run.list",
];

/// An audit writer persisting envelopes and blobs to an in-memory store and
/// `audit_root/blobs`, so tests read back what a real run would keep.
pub(in crate::activity_job::cli_runner) fn persisted_writer(
    audit_root: &Path,
    run_id: &str,
    agent_identity: &str,
) -> Arc<V2AuditWriter> {
    V2AuditWriter::with_disk_sinks(
        audit_root,
        Arc::new(orbit_store::Store::open_in_memory().expect("open sqlite store")),
        "ws-test",
        run_id,
        agent_identity,
        None,
    )
    .expect("audit writer")
}

/// The blob store [`persisted_writer`] writes under `audit_root`.
pub(in crate::activity_job::cli_runner) fn persisted_blobs(audit_root: &Path) -> BlobStore {
    BlobStore::new(audit_root.join("blobs"))
}

pub(in crate::activity_job::cli_runner) fn test_agent_loop_spec_for(
    provider: &str,
    timeout: Duration,
) -> AgentLoopSpec {
    let provider = match provider {
        "codex" => Provider::Codex,
        "grok" => Provider::Grok,
        other => panic!("unsupported provider for test: {other}"),
    };
    AgentLoopSpec {
        tool_disallow_list: None,
        instruction: String::new(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider,
        wall_clock_timeout_seconds: timeout.as_secs(),
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    }
}

pub(in crate::activity_job::cli_runner) fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script permissions");
}
