//! The host route: the few callbacks a backend child cannot run itself,
//! carried back to the host process that spawned it.
//!
//! A decision channel's digest and submission read core's channel store,
//! which holds every channel's token key and is denied to every plugin
//! child (`docs/design/human-decision-channel/2_design.md` §3.2). A confined
//! backend's nested `orbit` can therefore never answer them. The host that
//! spawned the backend can: it already holds the child's callback session,
//! and it runs outside the plugin sandbox.
//!
//! When a session's ceiling admits one of [`HOST_ROUTED_CALLBACK_TOOLS`], the
//! host creates a datagram socket pair, keeps one end and hands the child the
//! other at [`PLUGIN_HOST_ROUTE_FD`], beside the callback credential. Like the
//! credential it survives a cleared environment, `exec` and `setsid`, and a
//! descendant inherits it. A call is one stream connection: the nested
//! `orbit` creates a stream pair, passes one end over the route as
//! `SCM_RIGHTS`, writes one request frame and reads one response frame, so
//! concurrent descendants never share a stream.
//!
//! **The route carries no identity of its own.** The host answers every
//! request under the session it minted for this child, never one a request
//! names, by resolving that session for the duration of the call
//! ([`routed_callback_record`]). The dispatch then derives the caller, checks
//! the callback allowlist and the session ceiling, and audits the call
//! exactly as it would for a callback the child ran itself. Possession of the
//! route proves only that the caller descends from this backend, which is
//! what the credential descriptor already proves.
//!
//! **The route runs two tools and nothing else.** A confined child must not
//! gain an unconfined host for its other callbacks, so the host refuses any
//! tool outside [`HOST_ROUTED_CALLBACK_TOOLS`] before dispatch.

use std::cell::RefCell;
use std::path::Path;

use orbit_common::{DecisionRefusalCode, OrbitError};
use serde_json::{Value, json};

use super::SessionRecord;

/// The descriptor the host maps the route socket onto in a backend child.
///
/// Fixed, like [`super::PLUGIN_CALLBACK_FD`], so a descendant finds it without
/// being told. A backend that closes it loses only the host route: its digest
/// and submit callbacks are then answered by the child itself, which a
/// confined child cannot do.
pub const PLUGIN_HOST_ROUTE_FD: i32 = 4;

/// The callbacks the host route runs on the host: the two that read the
/// decision-channel key store. The list is closed.
pub const HOST_ROUTED_CALLBACK_TOOLS: [&str; 2] =
    ["orbit.decision.digest", "orbit.decision.submit"];

/// Whether `tool` is a callback the host route runs on the host.
pub fn is_host_routed_callback_tool(tool: &str) -> bool {
    HOST_ROUTED_CALLBACK_TOOLS.contains(&tool)
}

thread_local! {
    /// The session a host-route call runs under, set only by the route's own
    /// server thread for the length of one call.
    static ROUTED_CALLBACK: RefCell<Option<SessionRecord>> = const { RefCell::new(None) };
}

/// The session the current thread is answering a host-route call for.
pub(super) fn routed_callback_record() -> Option<SessionRecord> {
    ROUTED_CALLBACK.with(|routed| routed.borrow().clone())
}

/// What a forwarded call came back with.
#[derive(Debug)]
pub enum HostRouteOutcome {
    /// The host dispatched the call and wrote its audit row.
    Answered(Result<Value, OrbitError>),
    /// The call never reached the host's dispatch: the route was unusable or
    /// the host refused the request first. The caller audits it.
    Unanswered(OrbitError),
}

#[cfg(unix)]
pub(super) use self::unix::HostRoute;
#[cfg(unix)]
pub use self::unix::forward_host_routed_callback;

/// No other platform inherits descriptors, so no call is forwarded.
#[cfg(not(unix))]
pub fn forward_host_routed_callback(
    _global_root: &Path,
    _tool: &str,
    _input: &Value,
) -> Option<HostRouteOutcome> {
    None
}

/// Largest request frame the host reads. A submission's raw message is
/// bounded at 256 KiB before JSON escaping.
const MAX_REQUEST_BYTES: u32 = 4 * 1024 * 1024;
/// Largest response frame a caller reads: a digest lists every open request.
const MAX_RESPONSE_BYTES: u32 = 64 * 1024 * 1024;
const SCHEMA_VERSION: u64 = 1;

/// A host error as the route carries it. The kinds a caller branches on keep
/// their variant, decision refusals their stable code; everything else keeps
/// its rendered message.
fn error_json(error: &OrbitError) -> Value {
    match error {
        OrbitError::DecisionRefused { code, message } => {
            json!({ "kind": "decision_refused", "code": code, "message": message })
        }
        OrbitError::PolicyDenied(message) => json!({ "kind": "policy_denied", "message": message }),
        OrbitError::CapabilityDenied(message) => {
            json!({ "kind": "capability_denied", "message": message })
        }
        OrbitError::InvalidInput(message) => json!({ "kind": "invalid_input", "message": message }),
        OrbitError::Execution(message) => json!({ "kind": "execution", "message": message }),
        other => json!({ "kind": "execution", "message": other.to_string() }),
    }
}

fn error_from_json(value: &Value) -> OrbitError {
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the host route answered with an unreadable error")
        .to_string();
    match value.get("kind").and_then(Value::as_str) {
        Some("decision_refused") => {
            match value
                .get("code")
                .cloned()
                .map(serde_json::from_value::<DecisionRefusalCode>)
            {
                Some(Ok(code)) => OrbitError::DecisionRefused { code, message },
                _ => OrbitError::Execution(message),
            }
        }
        Some("policy_denied") => OrbitError::PolicyDenied(message),
        Some("capability_denied") => OrbitError::CapabilityDenied(message),
        Some("invalid_input") => OrbitError::InvalidInput(message),
        _ => OrbitError::Execution(message),
    }
}

fn answered_response(result: &Result<Value, OrbitError>) -> Value {
    match result {
        Ok(output) => {
            json!({ "schema_version": SCHEMA_VERSION, "answered": true, "ok": true, "output": output })
        }
        Err(error) => json!({
            "schema_version": SCHEMA_VERSION,
            "answered": true,
            "ok": false,
            "error": error_json(error),
        }),
    }
}

fn unanswered_response(error: &OrbitError) -> Value {
    json!({ "schema_version": SCHEMA_VERSION, "answered": false, "error": error_json(error) })
}

fn outcome_from_response(body: &[u8]) -> HostRouteOutcome {
    let Ok(response) = serde_json::from_slice::<Value>(body) else {
        return HostRouteOutcome::Unanswered(OrbitError::Execution(
            "the plugin host route answered with something other than JSON".to_string(),
        ));
    };
    let error = || error_from_json(response.get("error").unwrap_or(&Value::Null));
    if response.get("answered").and_then(Value::as_bool) != Some(true) {
        return HostRouteOutcome::Unanswered(error());
    }
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        HostRouteOutcome::Answered(Ok(response.get("output").cloned().unwrap_or(Value::Null)))
    } else {
        HostRouteOutcome::Answered(Err(error()))
    }
}

fn parse_request(body: &[u8]) -> Result<(String, Value), OrbitError> {
    let request: Value = serde_json::from_slice(body).map_err(|error| {
        OrbitError::InvalidInput(format!("plugin host route request is not JSON: {error}"))
    })?;
    if request.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return Err(OrbitError::InvalidInput(format!(
            "plugin host route request must carry schema_version {SCHEMA_VERSION}"
        )));
    }
    let tool = request
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            OrbitError::InvalidInput("plugin host route request must name a `tool`".to_string())
        })?
        .to_string();
    let input = request
        .get("input")
        .filter(|input| input.is_object())
        .cloned()
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "plugin host route request `input` must be a JSON object".to_string(),
            )
        })?;
    Ok((tool, input))
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Read, Write};
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use super::*;
    use crate::OrbitToolHost;

    /// How often the server looks at its stop flag while no call arrives.
    const POLL: Duration = Duration::from_millis(100);
    /// How long the host waits for a connected caller's request frame, and
    /// for a caller to take its response.
    const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

    /// The host half of one session's route. Dropping it stops the server,
    /// after the call it may be answering.
    pub(in crate::plugin::callback) struct HostRoute {
        child_end: OwnedFd,
        stop: Arc<AtomicBool>,
        server: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for HostRoute {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HostRoute")
                .field("child_end", &self.child_end.as_raw_fd())
                .finish_non_exhaustive()
        }
    }

    impl HostRoute {
        /// Start answering calls for `record` through `host`.
        pub(in crate::plugin::callback) fn start(
            record: SessionRecord,
            host: Arc<dyn OrbitToolHost>,
        ) -> Result<Self, OrbitError> {
            let io =
                |error: io::Error| OrbitError::Io(format!("open the plugin host route: {error}"));
            let (host_end, child_end) = UnixDatagram::pair().map_err(io)?;
            // Clear of both inherited numbers, so mapping the credential onto
            // its number never overwrites this source before it is mapped.
            let child_end =
                lift_above(OwnedFd::from(child_end), PLUGIN_HOST_ROUTE_FD).map_err(io)?;
            host_end.set_read_timeout(Some(POLL)).map_err(io)?;
            let stop = Arc::new(AtomicBool::new(false));
            let server = std::thread::Builder::new()
                .name("orbit-plugin-host-route".to_string())
                .spawn({
                    let stop = Arc::clone(&stop);
                    move || serve(&host_end, &record, host.as_ref(), &stop)
                })
                .map_err(io)?;
            Ok(Self {
                child_end,
                stop,
                server: Some(server),
            })
        }

        /// The descriptor the child must receive as [`PLUGIN_HOST_ROUTE_FD`].
        /// Borrowed: the route owns it and outlives the spawn.
        pub(in crate::plugin::callback) fn child_fd(&self) -> RawFd {
            self.child_end.as_raw_fd()
        }
    }

    impl Drop for HostRoute {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(server) = self.server.take()
                && server.join().is_err()
            {
                tracing::warn!(
                    target: "orbit.tools.plugin",
                    "the plugin host route server panicked"
                );
            }
        }
    }

    /// Duplicate `fd` above `floor`, close-on-exec, when it sits at or below it.
    fn lift_above(fd: OwnedFd, floor: RawFd) -> io::Result<OwnedFd> {
        if fd.as_raw_fd() > floor {
            return Ok(fd);
        }
        // SAFETY: `fd` is open and owned here; `F_DUPFD_CLOEXEC` returns a new
        // descriptor at or above `floor + 1`, which this scope then owns.
        let lifted = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, floor + 1) };
        if lifted < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `lifted` is a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(lifted) })
    }

    fn serve(
        socket: &UnixDatagram,
        record: &SessionRecord,
        host: &dyn OrbitToolHost,
        stop: &AtomicBool,
    ) {
        while !stop.load(Ordering::Relaxed) {
            match receive_connection(socket) {
                Ok(Some(stream)) => answer(stream, record, host),
                Ok(None) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.tools.plugin",
                        plugin = %record.plugin,
                        "the plugin host route stopped: {error}"
                    );
                    return;
                }
            }
        }
    }

    /// Answer one call. Every request is decided under `record`, the session
    /// the host minted for this child; nothing in the request names one.
    fn answer(mut stream: UnixStream, record: &SessionRecord, host: &dyn OrbitToolHost) {
        let _ = stream.set_read_timeout(Some(FRAME_TIMEOUT));
        let _ = stream.set_write_timeout(Some(FRAME_TIMEOUT));
        let response = match read_frame(&mut stream, MAX_REQUEST_BYTES)
            .map_err(|error| {
                OrbitError::InvalidInput(format!("read plugin host route request: {error}"))
            })
            .and_then(|body| parse_request(&body))
        {
            Err(error) => unanswered_response(&error),
            Ok((tool, _)) if !is_host_routed_callback_tool(&tool) => {
                unanswered_response(&OrbitError::PolicyDenied(format!(
                    "the plugin host route runs only {}; '{tool}' runs in the backend's own \
                     sandbox",
                    HOST_ROUTED_CALLBACK_TOOLS.join(" and ")
                )))
            }
            Ok((tool, input)) => {
                let result = run_as(record, || host.execute_host_routed_callback(&tool, input));
                answered_response(&result)
            }
        };
        if let Err(error) = write_frame(&mut stream, response.to_string().as_bytes()) {
            tracing::warn!(
                target: "orbit.tools.plugin",
                plugin = %record.plugin,
                "the plugin host route could not deliver a response: {error}"
            );
        }
    }

    /// Run `call` with this thread resolving `record` as its callback session,
    /// and clear it again however `call` ends.
    fn run_as<T>(record: &SessionRecord, call: impl FnOnce() -> T) -> T {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                ROUTED_CALLBACK.with(|routed| routed.borrow_mut().take());
            }
        }
        ROUTED_CALLBACK.with(|routed| *routed.borrow_mut() = Some(record.clone()));
        let _clear = Clear;
        call()
    }

    /// Forward `tool` to the host that spawned this backend, when this process
    /// is a backend descendant holding both its callback credential and the
    /// host route. `None` means the caller answers the call itself: the tool
    /// is not host-routed, this process holds no route, or it is the host
    /// answering a routed call already.
    pub fn forward_host_routed_callback(
        global_root: &Path,
        tool: &str,
        input: &Value,
    ) -> Option<HostRouteOutcome> {
        if !is_host_routed_callback_tool(tool) || routed_callback_record().is_some() {
            return None;
        }
        if !descriptor_is_datagram_socket(PLUGIN_HOST_ROUTE_FD) {
            return None;
        }
        // Only a process holding a credential the host wrote forwards: an
        // ordinary caller may have any socket on the number.
        crate::plugin::callback::resolution::credential_from_descriptor(
            &crate::plugin::callback::resolution::callback_dir(global_root),
        )?;
        Some(match call_host(tool, input) {
            Ok(body) => outcome_from_response(&body),
            Err(error) => HostRouteOutcome::Unanswered(OrbitError::Execution(format!(
                "the plugin host route did not answer '{tool}': {error}"
            ))),
        })
    }

    fn call_host(tool: &str, input: &Value) -> io::Result<Vec<u8>> {
        // SAFETY: borrowed only; `ManuallyDrop` keeps the wrapper from closing
        // a number this process inherited, and the caller checked it is a
        // datagram socket.
        let route = ManuallyDrop::new(unsafe { UnixDatagram::from_raw_fd(PLUGIN_HOST_ROUTE_FD) });
        let (mut mine, theirs) = UnixStream::pair()?;
        send_descriptor(&route, theirs.as_raw_fd())?;
        drop(theirs);
        let request = json!({ "schema_version": SCHEMA_VERSION, "tool": tool, "input": input });
        write_frame(&mut mine, request.to_string().as_bytes())?;
        read_frame(&mut mine, MAX_RESPONSE_BYTES)
    }

    fn descriptor_is_datagram_socket(fd: RawFd) -> bool {
        let mut kind: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: `kind` and `len` are valid for writes of the sizes passed; a
        // closed number or one that is not a socket fails with `EBADF` or
        // `ENOTSOCK`.
        let status = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut len,
            )
        };
        status == 0 && kind == libc::SOCK_DGRAM
    }

    /// Room for a handful of descriptors, aligned for `cmsghdr`.
    type ControlBuffer = [u64; 8];

    fn send_descriptor(route: &UnixDatagram, fd: RawFd) -> io::Result<()> {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let mut control: ControlBuffer = [0; 8];
        let fd_len = std::mem::size_of::<libc::c_int>() as libc::c_uint;
        // SAFETY: the header, iovec and control buffer outlive the call; the
        // control buffer is aligned for `cmsghdr` and larger than
        // `CMSG_SPACE(sizeof(int))`, so the one header written fits.
        let sent = unsafe {
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &raw mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = libc::CMSG_SPACE(fd_len) as _;
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            if header.is_null() {
                return Err(io::Error::other("no room for the descriptor header"));
            }
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(fd_len) as _;
            libc::CMSG_DATA(header)
                .cast::<libc::c_int>()
                .write_unaligned(fd);
            libc::sendmsg(route.as_raw_fd(), &raw const message, 0)
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// One message off the route: the stream the caller passed, if it passed
    /// exactly what a caller passes. Any other descriptor is closed.
    fn receive_connection(socket: &UnixDatagram) -> io::Result<Option<UnixStream>> {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let mut control: ControlBuffer = [0; 8];
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let mut received = Vec::new();
        // SAFETY: the header, iovec and control buffer outlive the call, and
        // every descriptor the kernel installed is taken into an `OwnedFd`
        // exactly once, so none is leaked or closed twice.
        unsafe {
            let mut message: libc::msghdr = std::mem::zeroed();
            message.msg_iov = &raw mut iov;
            message.msg_iovlen = 1;
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = std::mem::size_of::<ControlBuffer>() as _;
            if libc::recvmsg(socket.as_raw_fd(), &raw mut message, flags) < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut header = libc::CMSG_FIRSTHDR(&raw const message);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let data = libc::CMSG_DATA(header).cast::<libc::c_int>();
                    let payload = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    for index in 0..payload / std::mem::size_of::<libc::c_int>() {
                        received.push(OwnedFd::from_raw_fd(data.add(index).read_unaligned()));
                    }
                }
                header = libc::CMSG_NXTHDR(&raw const message, header);
            }
        }
        if received.len() != 1 {
            return Ok(None);
        }
        let fd = received.remove(0);
        #[cfg(not(target_os = "linux"))]
        {
            // SAFETY: `fd` is open and owned here.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if !descriptor_is_stream_socket(fd.as_raw_fd()) {
            return Ok(None);
        }
        Ok(Some(UnixStream::from(fd)))
    }

    fn descriptor_is_stream_socket(fd: RawFd) -> bool {
        let mut kind: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: as in `descriptor_is_datagram_socket`.
        let status = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut len,
            )
        };
        status == 0 && kind == libc::SOCK_STREAM
    }

    fn read_frame(reader: &mut impl Read, limit: u32) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header)?;
        let declared = u32::from_be_bytes(header);
        if declared > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame of {declared} bytes is over the {limit}-byte bound"),
            ));
        }
        let mut body = vec![0u8; declared as usize];
        reader.read_exact(&mut body)?;
        Ok(body)
    }

    fn write_frame(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
        let declared = u32::try_from(body.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame body too large"))?;
        writer.write_all(&declared.to_be_bytes())?;
        writer.write_all(body)?;
        writer.flush()
    }
}
