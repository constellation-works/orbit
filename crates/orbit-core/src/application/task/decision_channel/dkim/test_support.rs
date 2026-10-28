//! Thread-scoped DNS substitution for public decision submission fixtures.

use std::{cell::RefCell, sync::Arc};

use super::DkimTxtResolver;

thread_local! {
    static RESOLVER: RefCell<Option<Arc<dyn DkimTxtResolver>>> = const { RefCell::new(None) };
}

/// Substitute only DNS TXT lookups on this thread during `operation`.
/// The governed tool path and signature verification still run normally.
/// Restore the previous resolver on return or unwind.
pub fn with_decision_dkim_resolver<T>(
    resolver: impl DkimTxtResolver + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    let _restore = Restore(RESOLVER.with(|slot| slot.replace(Some(Arc::new(resolver)))));
    operation()
}

struct Restore(Option<Arc<dyn DkimTxtResolver>>);

impl Drop for Restore {
    fn drop(&mut self) {
        RESOLVER.with(|slot| slot.replace(self.0.take()));
    }
}

pub(super) fn lookup_txt(name: &str) -> Option<Result<Vec<String>, String>> {
    let resolver = RESOLVER.with(|slot| slot.borrow().clone());
    resolver.map(|resolver| resolver.lookup_txt(name))
}
