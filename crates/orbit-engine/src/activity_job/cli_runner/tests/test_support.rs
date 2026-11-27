use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use orbit_agent::loop_engine::audit::BlobStore;
use orbit_common::{OrbitError, StorageLayer};
use orbit_types::workflow::activity_job::{AgentLoopSpec, Provider};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::dispatcher::{DispatchError, ResolvedCliExecutor, ResolvedSandbox};
use crate::context::ResolvedActivityTools;

/// The registry [`TestHost::resolve_activity_tool_denials`] resolves against.
const TEST_REGISTERED_TOOLS: &[&str] = &[
    "orbit.task.show",
    "orbit.search",
    "orbit.workflow.ship",
    "proc.spawn",
    "github.run.list",
];

/// A persistence-refresh failure a test forces on the post-run path, by the
/// error class the real store reports for it.
pub(in crate::activity_job::cli_runner) enum RefreshFailure {
    /// A store refusal that no retry clears.
    Store(String),
    /// A store refused as read-only or access-denied.
    ReadOnly(String),
    /// SQLite lock contention that outlasted the busy timeout.
    LockBusy(String),
}

impl RefreshFailure {
    fn error(&self) -> OrbitError {
        match self {
            Self::Store(message) => OrbitError::Store(message.clone()),
            Self::ReadOnly(message) => OrbitError::StorageAccessDenied {
                layer: StorageLayer::Store,
                message: message.clone(),
            },
            Self::LockBusy(message) => OrbitError::StorageLockBusy {
                layer: StorageLayer::Store,
                message: message.clone(),
            },
        }
    }
}

/// A host that launches `command` as the provider CLI and otherwise keeps the
/// trait defaults (no task context), except that deny-mode tool policies
/// resolve against a fixed registry. Sandbox and a
/// persistence-refresh failure are optional so a test can force one post-run
/// check without standing up the rest of the runtime.
#[derive(Default)]
pub(in crate::activity_job::cli_runner) struct TestHost {
    command: String,
    sandbox: Option<ResolvedSandbox>,
    refresh_failure: Option<RefreshFailure>,
    /// The first this many refreshes fail; `None` fails every refresh.
    refresh_failing_calls: Option<u32>,
    refresh_calls: AtomicU32,
}

impl TestHost {
    pub(in crate::activity_job::cli_runner) fn with_command(command: String) -> Self {
        Self {
            command,
            ..Self::default()
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

    pub(in crate::activity_job::cli_runner) fn fail_persistence_refresh(
        mut self,
        failure: RefreshFailure,
    ) -> Self {
        self.refresh_failure = Some(failure);
        self
    }

    /// Fail only the first `calls` refreshes, so a bounded retry can clear it.
    pub(in crate::activity_job::cli_runner) fn fail_persistence_refresh_for(
        mut self,
        failure: RefreshFailure,
        calls: u32,
    ) -> Self {
        self.refresh_failure = Some(failure);
        self.refresh_failing_calls = Some(calls);
        self
    }

    /// How many persistence refreshes the post-run path requested.
    pub(in crate::activity_job::cli_runner) fn persistence_refresh_calls(&self) -> u32 {
        self.refresh_calls.load(Ordering::SeqCst)
    }
}

impl crate::test_support::NullRuntimeHost for TestHost {
    fn resolve_cli_executor(&self, _provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Ok(ResolvedCliExecutor {
            command: self.command.clone(),
            args: Vec::new(),
            inspection_args: None,
            image_input: None,
        })
    }

    /// Deny mode over the fixed [`TEST_REGISTERED_TOOLS`] registry.
    fn resolve_activity_tool_denials(
        &self,
        _run_id: Option<&str>,
        _task_ids: &[String],
        _activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        Ok(ResolvedActivityTools {
            effective_tools: orbit_types::workflow::tools_allowed_by_disallow_list(
                disallow_list,
                TEST_REGISTERED_TOOLS.iter().copied(),
            ),
            ..Default::default()
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

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        let call = self.refresh_calls.fetch_add(1, Ordering::SeqCst) + 1;
        match &self.refresh_failure {
            Some(failure) if self.refresh_failing_calls.is_none_or(|calls| call <= calls) => {
                Err(failure.error())
            }
            _ => Ok(()),
        }
    }
}

pub(in crate::activity_job::cli_runner) fn sh_args(script: &str) -> Vec<String> {
    vec!["-c".to_string(), script.to_string()]
}

/// An audit writer persisting envelopes and blobs to an in-memory store and
/// `audit_root/blobs`, so tests read back what a real run would keep.
pub(in crate::activity_job::cli_runner) fn persisted_writer(
    audit_root: &Path,
    run_id: &str,
    agent_identity: &str,
) -> Arc<V2AuditWriter> {
    V2AuditWriter::with_disk_sinks(
        audit_root,
        orbit_store::Store::open_in_memory().expect("open sqlite store"),
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
    AgentLoopSpec::for_provider(String::new(), provider, timeout.as_secs())
}

#[cfg(unix)]
pub(in crate::activity_job::cli_runner) fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script permissions");
}
