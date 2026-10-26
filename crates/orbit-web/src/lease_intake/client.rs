//! The listener's blocking client for the lease intake socket.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use orbit_core::OrbitError;
use serde::{Deserialize, Serialize};

use super::{
    LEASE_INTAKE_MAX_FRAME_BYTES, LEASE_INTAKE_PROTOCOL_VERSION, LeaseIntakeHello,
    LeaseIntakeRequest, LeaseIntakeResponse, version_mismatch_message,
};

/// The listener's end of the intake socket. Blocking, for the lease
/// backend's blocking-pool methods.
pub struct LeaseIntakeClient {
    stream: UnixStream,
}

impl LeaseIntakeClient {
    /// Connect to the socket at `path` and complete the handshake. `timeout`
    /// bounds each read and write.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, OrbitError> {
        let stream = UnixStream::connect(path).map_err(|error| {
            OrbitError::Io(format!(
                "connect to the lease intake socket {}: {error}",
                path.display()
            ))
        })?;
        stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)))
            .map_err(|error| OrbitError::Io(format!("lease intake socket: {error}")))?;
        let mut client = Self { stream };
        client.send(&LeaseIntakeHello {
            protocol_version: LEASE_INTAKE_PROTOCOL_VERSION,
            error: None,
        })?;
        let reply: LeaseIntakeHello = client.receive()?;
        if let Some(error) = reply.error {
            return Err(OrbitError::Execution(format!(
                "Orbit Web refused the lease intake connection ({}): {}",
                error.code, error.message
            )));
        }
        if reply.protocol_version != LEASE_INTAKE_PROTOCOL_VERSION {
            return Err(OrbitError::Execution(version_mismatch_message(
                "this lease listener",
                LEASE_INTAKE_PROTOCOL_VERSION,
                "Orbit Web",
                reply.protocol_version,
            )));
        }
        Ok(client)
    }

    /// Send one operation and read its answer.
    pub fn call(
        &mut self,
        request: &LeaseIntakeRequest,
    ) -> Result<LeaseIntakeResponse, OrbitError> {
        self.send(request)?;
        self.receive()
    }

    fn send(&mut self, frame: &impl Serialize) -> Result<(), OrbitError> {
        let body = serde_json::to_vec(frame)
            .map_err(|error| OrbitError::Execution(format!("encode intake frame: {error}")))?;
        if body.len() > LEASE_INTAKE_MAX_FRAME_BYTES as usize {
            return Err(OrbitError::InvalidInput(format!(
                "lease intake frame of {} bytes exceeds the {LEASE_INTAKE_MAX_FRAME_BYTES}-byte limit",
                body.len()
            )));
        }
        let header = u32::try_from(body.len()).unwrap_or(u32::MAX).to_be_bytes();
        self.stream
            .write_all(&header)
            .and_then(|()| self.stream.write_all(&body))
            .and_then(|()| self.stream.flush())
            .map_err(|error| OrbitError::Io(format!("write to the lease intake socket: {error}")))
    }

    fn receive<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T, OrbitError> {
        let mut header = [0u8; 4];
        self.stream.read_exact(&mut header).map_err(|error| {
            OrbitError::Io(format!("read from the lease intake socket: {error}"))
        })?;
        let declared = u32::from_be_bytes(header);
        if declared > LEASE_INTAKE_MAX_FRAME_BYTES {
            return Err(OrbitError::Execution(format!(
                "Orbit Web sent a {declared}-byte intake frame, over the \
                 {LEASE_INTAKE_MAX_FRAME_BYTES}-byte limit"
            )));
        }
        let mut body = vec![0u8; declared as usize];
        self.stream.read_exact(&mut body).map_err(|error| {
            OrbitError::Io(format!("read from the lease intake socket: {error}"))
        })?;
        serde_json::from_slice(&body)
            .map_err(|error| OrbitError::Execution(format!("malformed intake frame: {error}")))
    }
}
