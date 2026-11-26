//! Managed-worker binding and test wait seams on `RuntimeKernel`: how a runtime
//! becomes a claimed worker and how a worker process is recorded. What a worker
//! sends its owner is in `run/transport.rs`.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::WorkerInvocation;

use crate::RuntimeKernel;

#[cfg(feature = "test-support")]
type TestWait = fn(std::time::Duration);

#[cfg(feature = "test-support")]
thread_local! {
    static TEST_PIPELINE_POLL_WAIT: std::cell::Cell<Option<TestWait>> = const {
        std::cell::Cell::new(None)
    };
}

/// Substitute only a pipeline wait's polling sleep on this thread. Elapsed
/// time, caller deadlines, snapshots and child/process operations remain real.
/// Restore the previous callback on return or unwind; worker threads must opt in
/// separately, and cadence/deadline conformance tests must use the system wait.
#[cfg(feature = "test-support")]
pub fn with_pipeline_poll_wait<T>(wait: TestWait, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<TestWait>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_PIPELINE_POLL_WAIT.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(TEST_PIPELINE_POLL_WAIT.with(|slot| slot.replace(Some(wait))));
    operation()
}

#[cfg(feature = "test-support")]
pub fn pipeline_poll_wait_override() -> Option<TestWait> {
    TEST_PIPELINE_POLL_WAIT.with(std::cell::Cell::get)
}

impl RuntimeKernel {
    pub fn current_worker_invocation(
        env: &orbit_common::Env,
        global_root: &std::path::Path,
    ) -> Result<Option<WorkerInvocation>, OrbitError> {
        restore_process_binding(env, global_root)
            .map(|binding| binding.map(|value| (*value).clone()))
    }

    /// Bind a runtime clone to an admitted worker. Composition must obtain the
    /// binding from its managed invocation, never editable job input or env.
    pub fn with_worker_invocation(
        mut self,
        invocation: WorkerInvocation,
        coordinator: Arc<dyn OwnerCoordinator>,
    ) -> Result<Self, OrbitError> {
        invocation.validate()?;
        if self
            .base()
            .role()
            .invocation()
            .is_some_and(|old| old != &invocation)
        {
            return Err(OrbitError::PolicyDenied(
                "worker binding is immutable".into(),
            ));
        }
        let role = crate::base::role::Role::resolve(
            self.base().workspace_binding.as_deref(),
            Some(Arc::new(invocation)),
        );
        self.stored_mut().set_role(role);
        self.stored_mut().set_owner_coordinator(coordinator);
        Ok(self)
    }

    pub fn without_worker_routing(&self) -> Self {
        let mut owner = self.clone();
        let role =
            crate::base::role::Role::resolve(owner.base().workspace_binding.as_deref(), None);
        owner.stored_mut().set_role(role);
        owner
    }

    pub fn with_owner_coordinator(mut self, coordinator: Arc<dyn OwnerCoordinator>) -> Self {
        self.stored_mut().set_owner_coordinator(coordinator);
        self
    }

    pub fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        if let Some(binding) = self.role().invocation() {
            crate::stored::recovery_authority::RecoveryAuthority::open(&self.global_root())?
                .bind_worker_process(pid, binding)?;
        }
        Ok(())
    }
}

/// A required child can start before the parent has recorded its PID. Wait
/// briefly for that record; never use environment payload as a substitute.
pub(crate) fn restore_process_binding(
    env: &orbit_common::Env,
    global_root: &std::path::Path,
) -> Result<Option<Arc<WorkerInvocation>>, OrbitError> {
    let required = env.orbit_worker_context_required().is_some();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(binding) =
            crate::stored::recovery_authority::current_worker_binding(global_root)?
        {
            return Ok(Some(Arc::new(binding)));
        }
        if !required {
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            return Err(OrbitError::PolicyDenied(
                "managed worker runtime binding unavailable".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
