use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::BlobStore;
use orbit_types::workflow::activity_job::{AgentLoopSpec, OnDenial, Provider};

use orbit_engine::ResolvedActivityTools;
use orbit_engine::V2AuditWriter;
use orbit_engine::{DispatchError, ResolvedCliExecutor, ResolvedSandbox};

/// A host that launches the fixture provider and resolves deny-mode tools.
/// Its optional primary checkout lets the public runner observe checkout drift.
pub(super) struct TestHost {
    command: String,
    sandbox: Option<ResolvedSandbox>,
    workspace_root: Option<PathBuf>,
    registered_tools: Vec<String>,
}

impl TestHost {
    pub(super) fn with_command(command: String) -> Self {
        Self {
            command,
            sandbox: None,
            workspace_root: None,
            registered_tools: TEST_REGISTERED_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
        }
    }

    pub(super) fn with_additional_tools(mut self, tools: &[&str]) -> Self {
        self.registered_tools
            .extend(tools.iter().map(|tool| (*tool).to_string()));
        self
    }

    pub(super) fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }
}

impl orbit_engine::test_support::NullRuntimeHost for TestHost {
    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.clone(),
            args: Vec::new(),
            // As the shipped executors declare: inspection runs reach the
            // checks after the inspection-tools gate.
            inspection_args: Some(Vec::new()),
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
                self.registered_tools.iter().map(String::as_str),
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
pub(super) fn persisted_writer(
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
pub(super) fn persisted_blobs(audit_root: &Path) -> BlobStore {
    BlobStore::new(audit_root.join("blobs"))
}

pub(super) fn test_agent_loop_spec_for(provider: &str, timeout: Duration) -> AgentLoopSpec {
    let provider = match provider {
        "codex" => Provider::Codex,
        "grok" => Provider::Grok,
        other => panic!("unsupported provider for test: {other}"),
    };
    AgentLoopSpec {
        tool_disallow_list: None,
        instruction: String::new(),
        steering_instruction: None,
        regression_risk_instruction: None,
        explore_instruction: None,
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
        crew_env: Vec::new(),
        crew_account: None,
    }
}

pub(super) fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script permissions");
}
