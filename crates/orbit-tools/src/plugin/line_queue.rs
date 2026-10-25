//! The queue between a plugin MCP server's stdout reader and its consumer.
//!
//! The reader thread must keep draining the pipe so a deadline can abandon a
//! wedged server, but the consumer only drains inside a request. A queue
//! bounded by line count alone would let an idle server park lines of up to
//! [`MAX_LINE_BYTES`] each; bounding the queued bytes caps what an idle
//! session holds.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Lines the reader may queue ahead of the consumer before it blocks.
const LINE_QUEUE: usize = 64;
/// Longest single line accepted from the server; every MCP message is one
/// JSON line, and anything past this is not one.
pub(super) const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;
/// Bytes the reader may queue ahead of the consumer. One maximal line always
/// fits, because a line is admitted whenever the queue is empty.
const MAX_QUEUED_BYTES: usize = MAX_LINE_BYTES as usize;

#[derive(Default)]
struct Budget {
    state: Mutex<BudgetState>,
    freed: Condvar,
}

#[derive(Default)]
struct BudgetState {
    queued: usize,
    closed: bool,
}

impl Budget {
    fn lock(&self) -> MutexGuard<'_, BudgetState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserve `len` bytes if they fit under `limit`. An empty queue admits any
    /// line, so a line longer than the limit is never starved.
    fn try_reserve(state: &mut BudgetState, len: usize, limit: usize) -> bool {
        if state.queued > 0 && state.queued.saturating_add(len) > limit {
            return false;
        }
        state.queued = state.queued.saturating_add(len);
        true
    }

    fn release(&self, len: usize) {
        let mut state = self.lock();
        state.queued = state.queued.saturating_sub(len);
        self.freed.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.freed.notify_all();
    }
}

/// The producing half, owned by the reader thread.
pub(super) struct LineSender {
    lines: SyncSender<String>,
    budget: Arc<Budget>,
    limit: usize,
}

/// The consuming half, owned by the session. Dropping it releases a reader
/// blocked on the byte budget.
pub(super) struct LineReceiver {
    lines: Receiver<String>,
    budget: Arc<Budget>,
}

pub(super) fn line_queue() -> (LineSender, LineReceiver) {
    bounded_line_queue(MAX_QUEUED_BYTES)
}

pub(super) fn bounded_line_queue(limit: usize) -> (LineSender, LineReceiver) {
    let (lines_tx, lines_rx) = sync_channel(LINE_QUEUE);
    let budget = Arc::new(Budget::default());
    (
        LineSender {
            lines: lines_tx,
            budget: Arc::clone(&budget),
            limit,
        },
        LineReceiver {
            lines: lines_rx,
            budget,
        },
    )
}

impl LineSender {
    /// Queue `line`, blocking while the queued bytes would exceed the budget.
    /// Fails once the consumer is gone.
    pub(super) fn send(&self, line: String) -> Result<(), ()> {
        let len = line.len();
        {
            let mut state = self.budget.lock();
            while !state.closed && !Budget::try_reserve(&mut state, len, self.limit) {
                state = self
                    .budget
                    .freed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.closed {
                return Err(());
            }
        }
        self.lines.send(line).map_err(|_| {
            self.budget.release(len);
        })
    }
}

impl LineReceiver {
    pub(super) fn recv_timeout(&self, timeout: Duration) -> Result<String, RecvTimeoutError> {
        let line = self.lines.recv_timeout(timeout)?;
        self.budget.release(line.len());
        Ok(line)
    }
}

impl Drop for LineReceiver {
    fn drop(&mut self) {
        self.budget.close();
    }
}

#[cfg(test)]
#[path = "tests/line_queue.rs"]
mod tests;
