use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::{BrokerDispatch, BrokerRequest};

mod peer;
mod protocol;
mod sandbox;
mod server;
mod socket;

/// Keep broker socket fixtures inside the Unix socket path limit on macOS.
fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("obk")
        .tempdir_in("/tmp")
        .expect("short broker test root")
}

/// The tool [`EchoDispatch`] refuses, as a run's policy would.
const REFUSED_TOOL: &str = "fixture.refused";

/// A dispatch that answers each request with its tool and the peer that sent
/// it, refuses [`REFUSED_TOOL`], and keeps every request it was handed.
#[derive(Default)]
struct EchoDispatch {
    calls: Mutex<Vec<(BrokerRequest, u32)>>,
}

impl EchoDispatch {
    fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn calls(&self) -> Vec<(BrokerRequest, u32)> {
        self.calls.lock().expect("calls").clone()
    }
}

impl BrokerDispatch for EchoDispatch {
    fn call(&self, request: BrokerRequest, peer_pid: u32) -> Result<Value, OrbitError> {
        let tool = request.tool.clone();
        self.calls.lock().expect("calls").push((request, peer_pid));
        if tool == REFUSED_TOOL {
            return Err(OrbitError::PolicyDenied(format!("{tool} is not allowed")));
        }
        Ok(json!({"tool": tool, "peer_pid": peer_pid}))
    }
}
