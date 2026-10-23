//! One absolute budget for prerequisite reads, rerun and reconciliation.
use super::operations::{DEFAULT_TIMEOUT_MS, process};
use orbit_common::OrbitError;
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
    OrbitError::Execution(format!(
        "workflow prerequisite evidence is unavailable ({reason})"
    ))
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
    pub(super) fn remaining(self) -> Result<u64, OrbitError> {
        let remaining = self
            .0
            .checked_sub(now_ms()?)
            .filter(|ms| *ms > 0)
            .ok_or_else(|| error("completion deadline expired"))?;
        Ok(remaining.min(DEFAULT_TIMEOUT_MS))
    }
    pub(super) fn execute(
        self,
        program: &str,
        args: Vec<String>,
        workspace: &Path,
        label: &str,
    ) -> Result<orbit_exec::ExecutionResult, OrbitError> {
        let timeout = self.remaining()?;
        let result = process::execute(program, args, Some(workspace), timeout, label)?;
        probe(label, timeout);
        self.remaining()?;
        Ok(result)
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
