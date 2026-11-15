//! One absolute budget for prerequisite reads, rerun and reconciliation.
use super::operations::{DEFAULT_TIMEOUT_MS, process};
use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use serde_json::Value;
use std::path::Path;

pub(super) fn now_ms() -> Result<u64, OrbitError> {
    #[cfg(feature = "test-support")]
    if let Some(now) = fixture::now() {
        return Ok(now);
    }
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| error("completion clock"))?
            .as_millis(),
    )
    .map_err(|_| error("completion clock overflow"))
}
fn error(reason: &str) -> OrbitError {
    OrbitError::workflow_evidence_unavailable(reason)
}
fn expired() -> OrbitError {
    OrbitError::DeliveryRefused {
        refusal: DeliveryRefusal::CompletionBudgetExpired,
        message: "workflow prerequisite evidence is unavailable (completion deadline expired)"
            .to_string(),
    }
}
#[derive(Clone, Copy)]
pub(super) struct Deadline(pub(super) u64);
impl Deadline {
    pub(super) fn from_input(input: &Value) -> Result<Self, OrbitError> {
        input["deadline_ms"]
            .as_u64()
            .map(Self)
            .ok_or_else(|| error("missing completion deadline"))
    }
    fn remaining_uncapped(self) -> Result<u64, OrbitError> {
        self.0
            .checked_sub(now_ms()?)
            .filter(|ms| *ms > 0)
            .ok_or_else(expired)
    }
    pub(super) fn remaining(self) -> Result<u64, OrbitError> {
        Ok(self.remaining_uncapped()?.min(DEFAULT_TIMEOUT_MS))
    }
    /// Run one read bounded by the completion deadline. A read killed because
    /// the deadline arrived is the typed expiry, not a failure of the read; a
    /// read killed at the per-read cap while budget remains keeps its error.
    pub(super) fn execute(
        self,
        program: &str,
        args: Vec<String>,
        workspace: &Path,
        label: &str,
    ) -> Result<orbit_exec::ExecutionResult, OrbitError> {
        let timeout = self.remaining()?;
        let result = process::run_vcs_process(program, args, Some(workspace), timeout)?;
        probe(label, timeout);
        // A timeout below the per-read cap was set by the deadline itself; at
        // the cap, only an arrived deadline explains the kill.
        if result.timed_out && (timeout < DEFAULT_TIMEOUT_MS || self.remaining().is_err()) {
            return Err(expired());
        }
        let result = process::succeeded(result, label)?;
        self.remaining()?;
        Ok(result)
    }
}
impl Deadline {
    /// Type a failed read that a caller ran with its own per-request timeouts.
    /// `last_timeout` is the timeout the final request carried. A read killed
    /// by the timeout (a typed `ProcessTimeout`, never its text) below the per-read cap, or at
    /// the cap once the deadline has arrived, is the typed expiry; any other
    /// failure, including a kill at the cap with budget left, is unchanged.
    pub(super) fn type_killed_read(self, error: OrbitError, last_timeout: u64) -> OrbitError {
        let killed = matches!(error, OrbitError::ProcessTimeout { .. });
        if killed && (last_timeout < DEFAULT_TIMEOUT_MS || self.remaining().is_err()) {
            expired()
        } else {
            error
        }
    }
}
pub(super) fn probe(label: &str, timeout: u64) {
    #[cfg(feature = "test-support")]
    fixture::probe(label, timeout);
    #[cfg(not(feature = "test-support"))]
    let _ = (label, timeout);
}

#[cfg(feature = "test-support")]
mod fixture {
    use std::{cell::RefCell, rc::Rc};
    type Clock = Rc<dyn Fn() -> u64>;
    type Probe = Rc<dyn Fn(&str, u64)>;
    thread_local! {
        static CLOCK: RefCell<Option<Clock>> = const { RefCell::new(None) };
        static PROBE: RefCell<Option<Probe>> = const { RefCell::new(None) };
    }
    /// Control the prerequisite clock and observe completed reads / DAG work.
    /// Real subprocess supervision still uses its real clock and finite timeout.
    pub fn with_prerequisite_clock<T>(
        clock: impl Fn() -> u64 + 'static,
        probe: impl Fn(&str, u64) + 'static,
        run: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Clock>, Option<Probe>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CLOCK.with(|slot| slot.replace(self.0.take()));
                PROBE.with(|slot| slot.replace(self.1.take()));
            }
        }
        let _restore = Restore(
            CLOCK.with(|slot| slot.replace(Some(Rc::new(clock)))),
            PROBE.with(|slot| slot.replace(Some(Rc::new(probe)))),
        );
        run()
    }
    pub(super) fn now() -> Option<u64> {
        CLOCK
            .with(|slot| slot.borrow().clone())
            .map(|clock| clock())
    }
    pub(super) fn probe(label: &str, timeout: u64) {
        let probe = PROBE.with(|slot| slot.borrow().clone());
        if let Some(probe) = probe {
            probe(label, timeout);
        }
    }
}
#[cfg(feature = "test-support")]
pub use fixture::with_prerequisite_clock;
