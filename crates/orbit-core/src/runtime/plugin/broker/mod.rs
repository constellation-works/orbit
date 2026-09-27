//! The per-run host broker for agent-initiated plugin calls
//! (`docs/design/plugins/2_agent_call_broker.md`).
//!
//! The `orbit job run-pipeline-worker` process that runs a sandboxed agent
//! step starts one broker before spawning the provider and drops it when the
//! provider exits. The server owns the socket, kernel peer authentication,
//! the wire protocol and its limits, and teardown; each authenticated,
//! well-formed request is handed to the run's [`BrokerDispatch`], which runs
//! it through the host's audited plugin dispatch under the run's own
//! authority.

mod client;
mod peer;
mod protocol;
mod server;
mod socket;

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::PluginBrokerHandle;
use serde_json::Value;

pub(crate) use client::forward_call;
pub(crate) use peer::PeerAnchor;
pub(crate) use protocol::{BrokerRequest, EntryPoint};
pub(crate) use socket::sweep_orphaned;

use server::{AnchorSlot, BrokerServer};
use socket::RunSocketDir;

/// Executes one authenticated request for the run a broker serves.
///
/// The implementation holds the run's dispatch record, so everything that
/// decides authority — task, job run, activity policy, agent profile — comes
/// from the host, never from `request`. `peer_pid` is the authenticated
/// caller's PID, recorded on the call's audit row.
pub(crate) trait BrokerDispatch: Send + Sync {
    /// Run the call. `Ok` carries the backend's `output` only; an error is
    /// rendered as the §4.3 error response.
    fn call(
        &self,
        request: BrokerRequest,
        peer_pid: u32,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Value, OrbitError>;
}

/// One run's broker. Dropping it stops the listener and removes the socket
/// and its directory.
pub(crate) struct PluginBroker {
    socket: RunSocketDir,
    anchor: Arc<AnchorSlot>,
    server: Option<BrokerServer>,
    run_id: String,
}

impl PluginBroker {
    /// Bind this run's socket under `global_root` and start listening,
    /// answering requests through `dispatch`. Every connection is refused
    /// until [`Self::bind_anchor`] names the sandbox.
    pub(crate) fn start(
        global_root: &Path,
        run_id: &str,
        dispatch: Arc<dyn BrokerDispatch>,
    ) -> Result<Self, OrbitError> {
        let socket = RunSocketDir::create(global_root)?;
        let listener = match UnixListener::bind(socket.socket_path()) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = socket.remove();
                return Err(OrbitError::Execution(format!(
                    "bind plugin broker socket `{}`: {error}",
                    socket.socket_path().display()
                )));
            }
        };
        let anchor = Arc::new(AnchorSlot::default());
        let server = match BrokerServer::spawn(listener, Arc::clone(&anchor), dispatch, run_id) {
            Ok(server) => server,
            Err(error) => {
                let _ = socket.remove();
                return Err(OrbitError::Execution(format!(
                    "start plugin broker listener: {error}"
                )));
            }
        };
        Ok(Self {
            socket,
            anchor,
            server: Some(server),
            run_id: run_id.to_string(),
        })
    }

    /// Accept peers that belong to `anchor` from now on.
    pub(crate) fn bind_anchor(&self, anchor: PeerAnchor) {
        self.anchor.bind(anchor);
    }
}

impl PluginBrokerHandle for PluginBroker {
    fn socket_path(&self) -> &Path {
        self.socket.socket_path()
    }

    fn bind_sandbox(&self, sandbox_pid: u32) -> Result<(), OrbitError> {
        match PeerAnchor::for_sandbox(sandbox_pid) {
            Ok(anchor) => {
                self.bind_anchor(anchor);
                Ok(())
            }
            Err(error) => {
                self.anchor.close();
                Err(error)
            }
        }
    }
}

impl Drop for PluginBroker {
    fn drop(&mut self) {
        self.anchor.close();
        drop(self.server.take());
        if let Err(error) = self.socket.remove() {
            tracing::warn!(
                target: "orbit.plugin_broker",
                run_id = %self.run_id,
                dir = %self.socket.dir().display(),
                error = %error,
                "could not remove the plugin broker socket directory"
            );
        }
    }
}

#[cfg(test)]
mod tests;
