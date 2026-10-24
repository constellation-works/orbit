//! What task-pilot apply needs from CI-sweep admission policy [ORB-17936].
//!
//! A CI-sweep pilot admits the one task the sweep filed only when the
//! assessment correlates with the sweep's source evidence, and settles a
//! filed task another pilot already prepared as superseded. `sweep_filing`
//! owns that policy, and it also files tasks, so a direct call from here
//! would close a cycle. This port is the way in: `sweep_filing` implements
//! it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime
//! binds it.

use orbit_engine::DispatchError;
use orbit_types::task::Task;
use serde_json::Value;

/// What CI-sweep admission decided for a validated assessment.
pub(crate) enum CiSweepAdmission {
    /// The admission decision apply records.
    Decision(Value),
    /// The task outcome of a filing another admission already met.
    Superseded(Value),
}

/// The CI-sweep admission policy task-pilot apply calls, implemented by
/// `sweep_filing`.
pub(in crate::application) trait TaskSweepFilingPort {
    /// Admit the task `filing` names from its validated `assessment` and
    /// `selectors`, or refuse it, correlating both with the sweep's source
    /// evidence.
    #[allow(clippy::too_many_arguments)]
    fn ci_sweep_admission(
        &self,
        action: &str,
        task_id: &str,
        task: &Task,
        assessment: &Value,
        selectors: &[String],
        filing: &Value,
        promotion_authorized: bool,
        dependencies: &[String],
        is_done: &dyn Fn(&str) -> bool,
    ) -> Result<CiSweepAdmission, DispatchError>;

    /// The superseded outcome for CI-sweep task `task_id` when `prepared`
    /// reports it excluded only because another active pilot prepared it.
    fn ci_sweep_piloted_elsewhere(
        &self,
        action: &str,
        task_id: &str,
        prepared: &Value,
    ) -> Result<Option<Value>, DispatchError>;
}
