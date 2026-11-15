use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

thread_local! {
    /// Execution ids of audited dispatches on this thread, outermost first.
    /// A callback dispatched while an entry is still running sees the entry's
    /// id underneath its own, and the entry row is written with the same id
    /// the callback recorded.
    static DISPATCH_EXECUTION_IDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

static AUDIT_EXECUTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn audit_execution_id(prefix: &str) -> String {
    let prefix = if prefix.trim().is_empty() {
        "exec"
    } else {
        prefix.trim()
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let sequence = AUDIT_EXECUTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos}-{pid}-{sequence}")
}

/// Mint this dispatch's audit execution id before the tool runs, and push it
/// so a callback bound during the call can name the entry row.
pub fn push_dispatch_execution_id() -> String {
    let id = audit_execution_id("exec");
    DISPATCH_EXECUTION_IDS.with(|stack| stack.borrow_mut().push(id.clone()));
    id
}

/// Drop the id [`push_dispatch_execution_id`] pushed for the dispatch that is
/// ending. Nested dispatches pop their own id first.
pub fn pop_dispatch_execution_id() {
    DISPATCH_EXECUTION_IDS.with(|stack| {
        stack.borrow_mut().pop();
    });
}

/// The execution id of the dispatch currently running on this thread.
pub fn current_dispatch_execution_id() -> Option<String> {
    DISPATCH_EXECUTION_IDS.with(|stack| stack.borrow().last().cloned())
}
