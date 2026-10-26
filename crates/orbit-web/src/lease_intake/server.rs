//! Serving the intake socket: the peer check, handshake, frame bounds, and
//! one audited runtime lease operation per request.

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use orbit_auth::scope::{HardLeasePermission, LeasePriorityCeiling, PrincipalClass};
use orbit_core::runtime::task::locks::lease::{
    self, LeasePrincipalClass, LeasePriority, LeaseTokenGrant,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::telemetry::AuditEventStatus;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use super::socket::LeaseIntakeSocket;
use super::{
    LEASE_INTAKE_MAX_FRAME_BYTES, LEASE_INTAKE_PROTOCOL_VERSION, LeaseIntakeAuthority,
    LeaseIntakeError, LeaseIntakeHello, LeaseIntakeOperation, LeaseIntakeRequest,
    LeaseIntakeResponse, version_mismatch_message,
};
use crate::state::DashboardState;

/// How long the server waits for each frame before closing the connection.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the server waits to write a frame.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Intake connections served at once; further ones are refused as busy.
const MAX_CONNECTIONS: usize = 16;

/// Per-connection bounds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IntakeLimits {
    pub(crate) max_frame_bytes: u32,
    pub(crate) read_timeout: Duration,
    pub(crate) write_timeout: Duration,
    pub(crate) max_connections: usize,
}

impl Default for IntakeLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: LEASE_INTAKE_MAX_FRAME_BYTES,
            read_timeout: READ_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            max_connections: MAX_CONNECTIONS,
        }
    }
}

struct IntakeServer {
    state: DashboardState,
    peer_uid: libc::uid_t,
    limits: IntakeLimits,
    connections: Arc<Semaphore>,
}

impl LeaseIntakeSocket {
    /// The accept loop, to spawn on the server's Tokio runtime; it runs until
    /// the runtime drops it. Call from inside that runtime. The socket file
    /// stays in place for as long as `self` lives.
    pub(crate) fn serve(
        &mut self,
        state: DashboardState,
        limits: IntakeLimits,
    ) -> Result<impl Future<Output = ()> + Send + 'static, OrbitError> {
        let std_listener = self.listener.take().ok_or_else(|| {
            OrbitError::Execution("the lease intake socket is already being served".to_string())
        })?;
        let listener = std_listener
            .set_nonblocking(true)
            .and_then(|()| UnixListener::from_std(std_listener))
            .map_err(|error| OrbitError::Io(format!("lease intake socket: {error}")))?;
        tracing::info!(
            socket = %self.path.display(),
            peer_user = %self.peer.name,
            peer_uid = self.peer.uid,
            "serving the lease intake socket"
        );
        let server = Arc::new(IntakeServer {
            state,
            peer_uid: self.peer.uid,
            limits,
            connections: Arc::new(Semaphore::new(limits.max_connections)),
        });
        Ok(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "lease intake accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let server = Arc::clone(&server);
                tokio::spawn(async move { server.connection(stream).await });
            }
        })
    }
}

impl IntakeServer {
    async fn connection(self: Arc<Self>, mut stream: UnixStream) {
        let peer_uid = match stream.peer_cred() {
            Ok(credentials) => credentials.uid(),
            Err(error) => {
                tracing::warn!(%error, "lease intake refused a connection: no peer credentials");
                return;
            }
        };
        if peer_uid != self.peer_uid {
            tracing::warn!(
                peer_uid,
                allowed_uid = self.peer_uid,
                "lease intake refused a connection from a user other than lease.intake.peer_user"
            );
            self.refuse(
                &mut stream,
                LeaseIntakeError::new(
                    "peer_refused",
                    format!(
                        "uid {peer_uid} may not use the lease intake socket; only \
                         lease.intake.peer_user may"
                    ),
                ),
            )
            .await;
            return;
        }
        let Ok(_permit) = Arc::clone(&self.connections).try_acquire_owned() else {
            tracing::warn!(peer_uid, "lease intake refused a connection: too many open");
            self.refuse(
                &mut stream,
                LeaseIntakeError::new("busy", "too many lease intake connections; retry"),
            )
            .await;
            return;
        };
        let hello: LeaseIntakeHello = match self.read(&mut stream).await {
            Ok(Some(hello)) => hello,
            Ok(None) => return,
            Err(error) => {
                self.refuse(&mut stream, error).await;
                return;
            }
        };
        if hello.protocol_version != LEASE_INTAKE_PROTOCOL_VERSION {
            let message = version_mismatch_message(
                "Orbit Web",
                LEASE_INTAKE_PROTOCOL_VERSION,
                "the lease listener",
                hello.protocol_version,
            );
            tracing::warn!(peer_uid, "{message}");
            self.refuse(
                &mut stream,
                LeaseIntakeError::new("protocol_version_mismatch", message),
            )
            .await;
            return;
        }
        let accepted = LeaseIntakeHello {
            protocol_version: LEASE_INTAKE_PROTOCOL_VERSION,
            error: None,
        };
        if self.write(&mut stream, &accepted).await.is_err() {
            return;
        }
        loop {
            let request: LeaseIntakeRequest = match self.read(&mut stream).await {
                Ok(Some(request)) => request,
                Ok(None) => return,
                Err(error) => {
                    let response = LeaseIntakeResponse::Error(error);
                    let _ = self.write(&mut stream, &response).await;
                    return;
                }
            };
            let state = self.state.clone();
            let response = tokio::task::spawn_blocking(move || dispatch(&state, request))
                .await
                .unwrap_or_else(|error| {
                    LeaseIntakeResponse::Error(LeaseIntakeError::new(
                        "operation_failed",
                        format!("lease operation panicked: {error}"),
                    ))
                });
            if self.write(&mut stream, &response).await.is_err() {
                return;
            }
        }
    }

    /// Send a refusal as the server's hello and close.
    async fn refuse(&self, stream: &mut UnixStream, error: LeaseIntakeError) {
        let refusal = LeaseIntakeHello {
            protocol_version: LEASE_INTAKE_PROTOCOL_VERSION,
            error: Some(error),
        };
        let _ = self.write(stream, &refusal).await;
    }

    /// Read one frame. `Ok(None)` is a clean close before a frame began.
    async fn read<T: for<'de> Deserialize<'de>>(
        &self,
        stream: &mut UnixStream,
    ) -> Result<Option<T>, LeaseIntakeError> {
        let limits = self.limits;
        let frame = tokio::time::timeout(limits.read_timeout, async {
            let mut header = [0u8; 4];
            match stream.read_exact(&mut header).await {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => {
                    return Err(LeaseIntakeError::new("io", format!("read failed: {error}")));
                }
            }
            let declared = u32::from_be_bytes(header);
            if declared > limits.max_frame_bytes {
                return Err(LeaseIntakeError::new(
                    "frame_too_large",
                    format!(
                        "a {declared}-byte frame exceeds the {}-byte limit",
                        limits.max_frame_bytes
                    ),
                ));
            }
            let mut body = vec![0u8; declared as usize];
            stream
                .read_exact(&mut body)
                .await
                .map_err(|error| LeaseIntakeError::new("io", format!("read failed: {error}")))?;
            Ok(Some(body))
        })
        .await
        .map_err(|_| {
            LeaseIntakeError::new(
                "timeout",
                format!(
                    "no complete frame within {} seconds",
                    limits.read_timeout.as_secs()
                ),
            )
        })??;
        let Some(body) = frame else {
            return Ok(None);
        };
        serde_json::from_slice(&body).map(Some).map_err(|error| {
            LeaseIntakeError::new("malformed", format!("malformed frame: {error}"))
        })
    }

    async fn write(&self, stream: &mut UnixStream, frame: &impl Serialize) -> Result<(), ()> {
        let Ok(body) = serde_json::to_vec(frame) else {
            return Err(());
        };
        let Ok(declared) = u32::try_from(body.len()) else {
            return Err(());
        };
        let written = tokio::time::timeout(self.limits.write_timeout, async {
            stream.write_all(&declared.to_be_bytes()).await?;
            stream.write_all(&body).await?;
            stream.flush().await
        })
        .await;
        match written {
            Ok(Ok(())) => Ok(()),
            _ => Err(()),
        }
    }
}

/// Run one operation and audit it.
fn dispatch(state: &DashboardState, request: LeaseIntakeRequest) -> LeaseIntakeResponse {
    let LeaseIntakeRequest {
        operation,
        workspace,
        authority,
        input,
    } = request;
    let runtime = workspace_runtime(state, &workspace);
    let outcome = run_operation(runtime.as_ref(), operation, &workspace, &authority, input);
    let (status, outcome_label) = match &outcome {
        Ok(result) if result.get("granted") == Some(&Value::Bool(false)) => {
            (AuditEventStatus::Denied, "refused")
        }
        Ok(_) => (AuditEventStatus::Success, "ok"),
        Err(error) if error.code == "scope_refused" => (AuditEventStatus::Denied, "refused"),
        Err(_) => (AuditEventStatus::Failure, "error"),
    };
    let lease_id = outcome
        .as_ref()
        .ok()
        .and_then(|result| result.pointer("/lease/lease_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut payload = json!({
        "operation": operation.as_str(),
        "workspace": workspace,
        "principal": authority.scope.principal,
        "principal_class": authority.scope.principal_class,
        "token_id": authority.token_id,
        "client_address": authority.client_address,
        "outcome": outcome_label,
        "lease_id": lease_id,
    });
    if let Err(error) = &outcome {
        payload["error_code"] = json!(error.code);
        payload["error"] = json!(error.message);
    }
    match &runtime {
        Some(runtime) => {
            if let Err(error) = lease::record_lease_intake_audit(
                runtime,
                operation.as_str(),
                lease_id.as_deref(),
                status,
                payload,
            ) {
                tracing::error!(%error, "lease intake could not write its audit row");
            }
        }
        None => tracing::warn!(%payload, "lease intake operation for an unknown workspace"),
    }
    match outcome {
        Ok(result) => LeaseIntakeResponse::Ok { result },
        Err(error) => LeaseIntakeResponse::Error(error),
    }
}

/// The runtime of the active workspace whose id, or else whose unique name,
/// is `selector`; tokens name workspaces either way.
fn workspace_runtime(state: &DashboardState, selector: &str) -> Option<Arc<OrbitRuntime>> {
    let pinned = state.pin();
    let entries = pinned.entries();
    let id = match entries.iter().find(|entry| entry.id == selector) {
        Some(entry) => entry.id.clone(),
        None => {
            let mut named = entries.iter().filter(|entry| entry.name == selector);
            match (named.next(), named.next()) {
                (Some(entry), None) => entry.id.clone(),
                _ => return None,
            }
        }
    };
    match pinned.runtime_for(&id) {
        Ok(runtime) => Some(runtime),
        Err(rejection) => {
            tracing::warn!(workspace = %id, ?rejection, "lease intake could not open a workspace");
            None
        }
    }
}

fn run_operation(
    runtime: Option<&Arc<OrbitRuntime>>,
    operation: LeaseIntakeOperation,
    workspace: &str,
    authority: &LeaseIntakeAuthority,
    input: Value,
) -> Result<Value, LeaseIntakeError> {
    let scope = authority
        .scope
        .clone()
        .validated()
        .map_err(|error| LeaseIntakeError::new("invalid_authority", error.to_string()))?;
    if authority.token_id.trim().is_empty() {
        return Err(LeaseIntakeError::new(
            "invalid_authority",
            "the authority names no token id",
        ));
    }
    scope
        .permits(workspace, operation.verb())
        .map_err(|refusal| {
            LeaseIntakeError::new(
                "scope_refused",
                format!(
                    "the lease token does not allow `{}` in workspace `{workspace}` ({refusal})",
                    operation.verb().as_str()
                ),
            )
        })?;
    let Some(runtime) = runtime else {
        return Err(LeaseIntakeError::new(
            "unknown_workspace",
            format!("no active workspace `{workspace}` on this owner"),
        ));
    };
    let mut input = match input {
        Value::Null => json!({}),
        Value::Object(map) => Value::Object(map),
        _ => {
            return Err(LeaseIntakeError::new(
                "invalid_input",
                "lease input must be a JSON object",
            ));
        }
    };
    if let Some(declared) = input.get("principal")
        && declared.as_str() != Some(scope.principal.as_str())
    {
        return Err(LeaseIntakeError::new(
            "invalid_input",
            "the input's principal must be the token's principal",
        ));
    }
    input["principal"] = json!(scope.principal);
    let result = match operation {
        LeaseIntakeOperation::Request => {
            let grant = LeaseTokenGrant {
                principal: scope.principal.clone(),
                principal_class: match scope.principal_class {
                    PrincipalClass::Human => LeasePrincipalClass::Human,
                    PrincipalClass::Agent => LeasePrincipalClass::Agent,
                },
                token_id: authority.token_id.clone(),
                max_priority: match scope.max_priority {
                    LeasePriorityCeiling::Low => LeasePriority::Low,
                    LeasePriorityCeiling::Normal => LeasePriority::Normal,
                    LeasePriorityCeiling::High => LeasePriority::High,
                },
                hard_allowed: scope.hard == HardLeasePermission::Allow,
                client_address: authority.client_address.clone(),
            };
            lease::request_with_token(runtime, input, &grant)
        }
        LeaseIntakeOperation::Renew => lease::renew(runtime, input),
        LeaseIntakeOperation::Expand => {
            return Err(LeaseIntakeError::new(
                "not_available_in_this_phase",
                "lease expansion is not available in this phase",
            ));
        }
        LeaseIntakeOperation::Release => lease::release(runtime, input, None, None),
        LeaseIntakeOperation::Show => lease::show(runtime, input),
        LeaseIntakeOperation::List => lease::list(runtime),
        LeaseIntakeOperation::Check => lease::check(runtime, input),
    };
    result.map_err(|error| {
        let code = match error {
            OrbitError::InvalidInput(_) => "invalid_input",
            _ => "operation_failed",
        };
        LeaseIntakeError::new(code, error.to_string())
    })
}
