//! A scoped adapter process substitute for public-runtime completion fixtures.

use std::cell::RefCell;
use std::rc::Rc;

use orbit_engine::{ScriptStepRequest, ScriptStepRun};

type AdapterRun = Rc<dyn Fn(&ScriptStepRequest) -> ScriptStepRun>;

thread_local! {
    static ADAPTER_RUN: RefCell<Option<AdapterRun>> = const { RefCell::new(None) };
}

/// Substitute only the completion adapter process on this thread. Owner
/// provenance, evidence admission and verdict validation still run normally.
/// Restores the previous substitute on return or unwind; never launches a
/// process or relaxes a sandbox profile.
pub fn with_completion_adapter_run<T>(
    adapter: impl Fn(&ScriptStepRequest) -> ScriptStepRun + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let _restore = AdapterRestore(ADAPTER_RUN.with(|slot| slot.replace(Some(Rc::new(adapter)))));
    operation()
}

struct AdapterRestore(Option<AdapterRun>);

impl Drop for AdapterRestore {
    fn drop(&mut self) {
        ADAPTER_RUN.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn completion_adapter_run(request: &ScriptStepRequest) -> Option<ScriptStepRun> {
    let adapter = ADAPTER_RUN.with(|slot| slot.borrow().clone());
    adapter.map(|adapter| adapter(request))
}
