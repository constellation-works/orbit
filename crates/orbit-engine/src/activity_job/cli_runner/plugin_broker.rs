//! The per-run plugin broker around one sandboxed provider launch.
//!
//! The host binds the broker before the provider is spawned, anchors its peer
//! authentication to the spawned sandbox, and tears it down when the provider
//! exits (`docs/design/plugins/2_agent_call_broker.md` §3). A broker that
//! cannot be bound costs the run its brokered plugin calls, never the step.

use orbit_types::workflow::ExecutorSandboxKind;

use super::super::dispatcher::ResolvedSandbox;
use crate::context::{PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun, RuntimeHost};

/// This launch's broker, if one bound. Dropping it stops the listener and
/// cancels admitted calls, reclaims backend sessions and removes the socket,
/// so every return path out of the step tears it down.
pub(super) struct RunPluginBroker {
    handle: Option<Box<dyn PluginBrokerHandle>>,
    run_id: String,
}

impl RunPluginBroker {
    /// Start the host's broker when the provider runs under an agent sandbox,
    /// serving the run `run` describes. An unsandboxed launch gets none
    /// (design §7). A bind failure, or a run with no worktree to confine a
    /// brokered backend to, is a warning that names the cause.
    pub(super) fn start(
        host: &dyn RuntimeHost,
        run_id: &str,
        run: Option<&PluginBrokerRun>,
        sandbox: Option<&ResolvedSandbox>,
    ) -> Self {
        let sandboxed = sandbox.is_some_and(|sandbox| {
            matches!(
                sandbox.kind,
                ExecutorSandboxKind::LinuxBwrap | ExecutorSandboxKind::MacosSandboxExec
            )
        });
        let started = match run {
            _ if !sandboxed => Ok(None),
            Some(run) => host.start_plugin_broker(run),
            None => Err(orbit_common::OrbitError::Execution(
                "the run has no worktree to confine a brokered plugin backend to".to_string(),
            )),
        };
        let handle = started.unwrap_or_else(|error| {
            tracing::warn!(
                target: "orbit.plugin_broker",
                run_id,
                cause = %error,
                "plugin broker unavailable; the step runs without {PLUGIN_BROKER_ENV}"
            );
            None
        });
        Self {
            handle,
            run_id: run_id.to_string(),
        }
    }

    /// The `ORBIT_PLUGIN_BROKER` entry for the agent, present only when the
    /// socket bound.
    pub(super) fn env(&self) -> Option<(String, String)> {
        self.handle.as_ref().map(|handle| {
            (
                PLUGIN_BROKER_ENV.to_string(),
                handle.socket_path().display().to_string(),
            )
        })
    }

    /// Anchor peer authentication to the spawned sandbox. A failure leaves the
    /// broker refusing every connection, so the agent's plugin calls fail
    /// closed while the step itself continues.
    pub(super) fn bind_sandbox(&self, sandbox_pid: u32) {
        let Some(handle) = self.handle.as_ref() else {
            return;
        };
        if let Err(error) = handle.bind_sandbox(sandbox_pid) {
            tracing::warn!(
                target: "orbit.plugin_broker",
                run_id = %self.run_id,
                sandbox_pid,
                cause = %error,
                "plugin broker could not identify this run's sandbox; it refuses every connection"
            );
        }
    }
}
