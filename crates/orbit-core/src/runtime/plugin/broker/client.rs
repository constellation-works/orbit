//! Client used by the nested CLI and MCP server before their local audit
//! boundary. The broker owns authorization, backend execution, and the row.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::protocol::{FrameError, MAX_REQUEST_BYTES, SCHEMA_VERSION, read_frame, write_frame};

const UNAVAILABLE: &str = "plugin_broker_unavailable";
const RESPONSE_BYTES: u32 = 4 * 1024 * 1024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(310);

fn unavailable(message: impl Into<String>) -> OrbitError {
    let message = message.into();
    OrbitError::RemoteTool {
        code: UNAVAILABLE.to_string(),
        payload: json!({"code": UNAVAILABLE, "message": message, "retryable": false}),
        message,
    }
}

/// Refuse a plugin call that has no broker to go to from inside a masked agent
/// sandbox (design §6.3). The mask hides the backend's state and the secret
/// store reads as unavailable, so the nested process never runs the call
/// itself; it is refused before anything is spawned.
pub(crate) fn refuse_unbrokered_call(global_root: &Path, tool: &str) -> Result<(), OrbitError> {
    if crate::runtime::plugin::sandbox_mask::plugin_trees_masked(global_root) {
        return Err(unavailable(format!(
            "plugin tool '{tool}' runs only through this run's plugin broker: the agent sandbox \
             hides plugin state and secrets, and ORBIT_PLUGIN_BROKER is not set"
        )));
    }
    Ok(())
}

/// Send one plugin call. A failed connection never falls back to execution in
/// the nested process, since its sandbox cannot read the plugin's secrets.
pub(crate) fn forward_call(
    socket: &Path,
    tool: &str,
    input: Value,
    cwd: &Path,
    workspace: Option<&str>,
    entry_point: &str,
) -> Result<Value, OrbitError> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| unavailable(format!("connect to plugin broker: {error}")))?;
    verify_server_uid(&stream)
        .map_err(|error| unavailable(format!("verify plugin broker server uid: {error}")))?;
    stream
        .set_read_timeout(Some(RESPONSE_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(RESPONSE_TIMEOUT)))
        .map_err(|error| unavailable(format!("set plugin broker timeout: {error}")))?;

    let request = json!({
        "schema_version": SCHEMA_VERSION,
        "tool": tool,
        "input": input,
        "cwd": cwd,
        "workspace": workspace,
        "entry_point": entry_point,
        "dry_run": false,
    });
    let body = serde_json::to_vec(&request)
        .map_err(|error| OrbitError::InvalidInput(format!("serialize broker request: {error}")))?;
    if body.len() > MAX_REQUEST_BYTES as usize {
        return Err(OrbitError::InvalidInput(format!(
            "plugin broker request exceeds {MAX_REQUEST_BYTES} bytes"
        )));
    }
    // A full broker can answer `busy` and close without reading our request.
    // A concurrent write may see EPIPE after that answer is already queued;
    // read it before treating the connection as unavailable.
    let sent = write_frame(&mut stream, &body);
    let body = read_frame(&mut stream, RESPONSE_BYTES).map_err(|error| match error {
        FrameError::TooLarge { declared } => unavailable(format!(
            "plugin broker response declares {declared} bytes, over {RESPONSE_BYTES}"
        )),
        FrameError::Io(error) => match sent {
            Ok(()) => unavailable(format!("read plugin broker response: {error}")),
            Err(send_error) => unavailable(format!(
                "send plugin broker request: {send_error}; read response: {error}"
            )),
        },
    })?;
    let response: Value = serde_json::from_slice(&body)
        .map_err(|error| unavailable(format!("invalid plugin broker response: {error}")))?;
    if response["schema_version"].as_u64() != Some(SCHEMA_VERSION) {
        return Err(unavailable("unsupported plugin broker response version"));
    }
    match response["ok"].as_bool() {
        Some(true) => response
            .get("output")
            .cloned()
            .ok_or_else(|| unavailable("plugin broker response omitted output")),
        Some(false) => {
            let error = &response["error"];
            let code = error["code"]
                .as_str()
                .filter(|code| !code.is_empty())
                .ok_or_else(|| unavailable("plugin broker response omitted error code"))?;
            let message = error["message"]
                .as_str()
                .filter(|message| !message.is_empty())
                .ok_or_else(|| unavailable("plugin broker response omitted error message"))?;
            let retryable = error["retryable"]
                .as_bool()
                .ok_or_else(|| unavailable("plugin broker response omitted retryability"))?;
            let mut payload = json!({"code": code, "message": message, "retryable": retryable});
            if let Some(detail) = error.get("detail").filter(|detail| !detail.is_null()) {
                payload["detail"] = detail.clone();
            }
            Err(OrbitError::RemoteTool {
                code: code.to_string(),
                message: format!("plugin tool '{tool}' failed: {message}"),
                payload,
            })
        }
        None => Err(unavailable("plugin broker response omitted status")),
    }
}

#[cfg(target_os = "linux")]
fn verify_server_uid(stream: &UnixStream) -> io::Result<()> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: both pointers address a live `ucred` and its length for this call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut size,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid server credentials",
        ));
    }
    // SAFETY: `geteuid` reads only this process's credentials.
    require_same_uid(credentials.uid, unsafe { libc::geteuid() })
}

#[cfg(target_os = "macos")]
fn verify_server_uid(stream: &UnixStream) -> io::Result<()> {
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: `getpeereid` fills the two live output integers.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `geteuid` reads only this process's credentials.
    require_same_uid(uid, unsafe { libc::geteuid() })
}

pub(super) fn require_same_uid(server: libc::uid_t, caller: libc::uid_t) -> io::Result<()> {
    if server == caller {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "server UID differs from caller",
        ))
    }
}
