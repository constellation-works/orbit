//! The owner route: how a claimed worker's task access reaches the drain
//! owner, with the lock-busy read retry and the response decoding.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use orbit_common::{OrbitError, RemoteToolCode};
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::{McpCapability, ToolSessionContext, WorkerInvocation};
use serde_json::Value;

/// Pauses before each retry of an owner read that lost only to a lock-wait
/// deadline on the owner [ORB-15088]. Each attempt already waited out the
/// owner's own 30 s lock deadline, so two retries outlast a drain-start burst
/// without holding a step, or a final recovery's envelope load, for minutes.
const OWNER_READ_LOCK_BUSY_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_secs(1), Duration::from_secs(3)];

#[cfg(feature = "test-support")]
thread_local! {
    static TEST_RETRY_WAIT: std::cell::Cell<Option<TestWait>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(feature = "test-support")]
type TestWait = fn(Duration);

/// A claimed worker's route to the drain owner: its authenticated invocation
/// and the coordinator composition attached for it.
#[derive(Clone)]
pub struct OwnerRoute {
    invocation: Arc<WorkerInvocation>,
    /// Supplied by composition after the runtime is built; a call made before
    /// that is refused.
    coordinator: Option<Arc<dyn OwnerCoordinator>>,
}

impl OwnerRoute {
    pub(crate) fn new(
        invocation: Arc<WorkerInvocation>,
        coordinator: Option<Arc<dyn OwnerCoordinator>>,
    ) -> Self {
        Self {
            invocation,
            coordinator,
        }
    }

    /// The worker invocation every call on this route is bound to.
    pub(crate) fn invocation(&self) -> &WorkerInvocation {
        &self.invocation
    }

    pub(crate) fn coordinator(&self) -> Option<Arc<dyn OwnerCoordinator>> {
        self.coordinator.clone()
    }

    pub(crate) fn set_coordinator(&mut self, coordinator: Arc<dyn OwnerCoordinator>) {
        self.coordinator = Some(coordinator);
    }

    /// Bind `session` to this route's invocation, refusing one that already
    /// carries another.
    pub(crate) fn bind_session(&self, session: &mut ToolSessionContext) -> Result<(), OrbitError> {
        let bound = self.invocation();
        if session
            .worker_invocation
            .as_ref()
            .is_some_and(|provided| provided != bound)
        {
            return Err(OrbitError::PolicyDenied(
                "worker session binding mismatch".into(),
            ));
        }
        session.worker_invocation = Some(bound.clone());
        Ok(())
    }

    /// Send one tool call to the owner under this route's binding. `checkout`
    /// is this worker's repository root, the one local name its `workspace`
    /// input may carry.
    pub(crate) fn call(
        &self,
        checkout: &Path,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let bound = self.invocation();
        self.bind_session(&mut session)?;
        session
            .effective_capabilities
            .remove(&McpCapability::Operator);
        if let Some(value) = input.get("workspace") {
            let valid = value.as_str().is_some_and(|value| {
                value == bound.owner_destination
                    || value == bound.owner_workspace_id
                    || value == checkout.to_string_lossy()
            });
            if !valid {
                return Err(OrbitError::PolicyDenied(
                    "worker workspace binding mismatch".into(),
                ));
            }
        }
        if let Some(object) = input.as_object_mut() {
            object.insert(
                "workspace".into(),
                Value::String(bound.owner_destination.clone()),
            );
        }
        session.workspace = Some(bound.owner_destination.clone());
        // [ORB-19270] A worker reads any task of its claimed workspace. The
        // owner answers under the binding's workspace only, so a task it does
        // not hold there is outside the claim's scope rather than missing.
        let other_task = (!session.worker_host_call
            && matches!(name, "orbit.task.show" | "orbit.task.artifact.get"))
        .then(|| orbit_common::governance::task::task_id_alias_str(&input).map(str::to_owned))
        .flatten()
        .filter(|id| *id != bound.task_id);
        let owner = self
            .coordinator
            .as_ref()
            .ok_or_else(|| OrbitError::PolicyDenied("owner destination unavailable".into()))?;
        let result = owner
            .call(name, input, session.clone())
            .map_err(owner_route_failure);
        match (result, other_task) {
            (Err(error), Some(id)) if is_not_found(&error) => {
                // An artifact read's not-found may be the artifact's; ask
                // the owner for the task itself before deciding.
                let task = owner.call(
                    "orbit.task.show",
                    serde_json::json!({
                        "id": id,
                        "fields": ["id"],
                        "workspace": bound.owner_destination,
                    }),
                    session,
                );
                match task {
                    Err(task_error) if is_not_found(&task_error) => {
                        Err(OrbitError::PolicyDenied(format!(
                            "claimed_owner_bridge_refused: `{id}` is not a task in the claimed \
                             task's workspace; a claimed worker reads that workspace only"
                        )))
                    }
                    _ => Err(error),
                }
            }
            (result, _) => result,
        }
    }

    /// Read one host-owned projection from the owner. A read decides nothing,
    /// so one the owner could not serve only because a lock stayed held is
    /// retried, a bounded number of times, before its caller sees the
    /// failure. Unknown outcomes are propagated, never treated as absent
    /// evidence.
    pub(crate) fn read<T: serde::de::DeserializeOwned>(
        &self,
        checkout: &Path,
        tool: &str,
        request: Value,
    ) -> Result<T, OrbitError> {
        let mut delays = OWNER_READ_LOCK_BUSY_RETRY_DELAYS.iter();
        let value = loop {
            let session = ToolSessionContext {
                worker_host_call: true,
                ..Default::default()
            };
            match self.call(checkout, tool, request.clone(), session) {
                Err(error) if error.is_lock_busy() => {
                    let Some(delay) = delays.next() else {
                        return Err(error);
                    };
                    tracing::warn!(
                        target: "orbit.core.worker_coordination",
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "owner read lost to a lock-wait deadline; retrying",
                    );
                    retry_wait(*delay);
                }
                result => break result?,
            }
        };
        decode_owner_read(value)
    }
}

/// Substitute only the owner-read retry wait on this thread, restoring it on
/// return or unwind. Retry count, error handling and production delays stay intact.
#[cfg(feature = "test-support")]
pub fn with_owner_read_retry_wait<T>(wait: TestWait, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<TestWait>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_RETRY_WAIT.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(TEST_RETRY_WAIT.with(|slot| slot.replace(Some(wait))));
    operation()
}

fn retry_wait(delay: Duration) {
    #[cfg(feature = "test-support")]
    if let Some(wait) = TEST_RETRY_WAIT.with(std::cell::Cell::get) {
        wait(delay);
        return;
    }
    std::thread::sleep(delay);
}

/// The refusal of an owner call made by a process that is not a claimed
/// worker, which has no owner route.
pub(crate) fn worker_binding_missing() -> OrbitError {
    OrbitError::PolicyDenied("worker binding missing".into())
}

/// Whether an owner error says the record does not exist, in process or across the owner route.
fn is_not_found(error: &OrbitError) -> bool {
    matches!(
        error,
        OrbitError::NotFound { .. }
            | OrbitError::RemoteTool {
                code: RemoteToolCode::NotFound,
                ..
            }
    )
}

/// A call the owner never answered is an owner-route failure [ORB-14257]:
/// its variants keep their messages, and the typing is expressed through
/// the step-failure code parsed from the variant where a step failure is
/// ingested ([`OrbitError::step_failure_code`]), so a claimed leaf that
/// lost its route to the owner settles as a release rather than as a
/// failure of its candidate. A call the owner could not serve only
/// because a lock stayed held is typed transient [ORB-15088], so a step
/// it fails — a rebase onto a moved base included — never settles as the
/// candidate's failure or as a base conflict. It also carries the
/// owner-busy marker, so the release a claimed leaf settles with blames
/// neither its host nor its crew and the drain backs off instead. A
/// refusal, an ambiguous outcome and a local error are left as they are.
fn owner_route_failure(error: OrbitError) -> OrbitError {
    match error {
        OrbitError::RemoteTool {
            code: code @ RemoteToolCode::LockBusy,
            message,
            payload,
        } => OrbitError::RemoteTool {
            code,
            message: format!(
                "{} {} {message}",
                orbit_types::workflow::TRANSIENT_FAILURE_MARKER,
                orbit_types::workflow::OWNER_BUSY_MARKER
            ),
            payload,
        },
        error => error,
    }
}

/// Decode an owner read into the type the reader asked for.
///
/// An in-process owner returns the projection as-is. A routed owner answers
/// over MCP, whose `structuredContent` must be an object, so a list arrives as
/// `{"items": [...]}` and a scalar as `{"value": ...}` [ORB-13625]. The value
/// is parsed as sent first, so a projection that genuinely is an object never
/// loses a field; only when that fails is the single-key transport envelope
/// unwrapped.
fn decode_owner_read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, OrbitError> {
    let direct = match serde_json::from_value::<T>(value.clone()) {
        Ok(decoded) => return Ok(decoded),
        Err(error) => error,
    };
    if let Value::Object(object) = &value
        && object.len() == 1
        && let Some(inner) = object.get("items").or_else(|| object.get("value"))
        && let Ok(decoded) = serde_json::from_value::<T>(inner.clone())
    {
        return Ok(decoded);
    }
    Err(OrbitError::Store(format!("owner read response: {direct}")))
}
