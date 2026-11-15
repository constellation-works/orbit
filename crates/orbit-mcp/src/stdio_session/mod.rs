//! A stdio MCP session that survives replacing the server's executable.
//!
//! `orbit mcp serve` outlives upgrades. Between calls it asks whether the
//! installed executable still names its running image; when a newer one
//! replaced it — or a breaking upgrade is waiting for this process to yield —
//! it hands the session over at an idle point instead of holding the
//! generation or dropping its client.
//!
//! Handing over needs exact control of stdin: bytes a reader has buffered but
//! not delivered would be lost across `exec`. So this module reads fd 0
//! itself, without buffering beyond one partial line, and passes complete
//! lines to the MCP service. It tracks which requests are outstanding from
//! the lines it forwards and the responses the service flushes. The session
//! is idle when every forwarded line has been consumed and every request the
//! transport accepts has been answered. Rejected or dropped input never adds
//! a pending request, since its error may not echo the original id. The pump
//! writes that id-less invalid-request error itself, as one complete frame on
//! the same stdout writer the service uses: rmcp sends it from inside
//! `receive` after discarding the line, and the service loop drops that send
//! when another branch is ready.
//!
//! At that point it stops reading, stops the service, and returns
//! [`StdioExit::HandOver`] with the client's original `initialize` request
//! and any partial line it had read. A request can arrive between the idle
//! check and the reader stopping, so stopping the reader keeps the service's
//! input open: the service answers that request and waits to be stopped,
//! rather than reading end of input as the client leaving. The reader defers
//! a handover while that partial line exceeds [`MAX_CARRYOVER`], keeping the
//! service alive until a later idle check. The caller execs the new
//! executable with that state in [`RESUME_ENV`]. Because `exec` keeps the pid
//! and the stdio descriptors, the client sees one uninterrupted session. The
//! new process replays the `initialize` request into its server rather than
//! expecting a second handshake. Replay keeps the client's metadata and
//! restores the protocol version that handshake agreed.

use std::path::PathBuf;

use rmcp::model::InitializeRequestParams;

mod input;
mod lifecycle;
mod tracking;
#[cfg(unix)]
mod unix;
mod writer;

pub use lifecycle::stdin_supports_handover;
pub(crate) use lifecycle::{resumed_session, serve};

/// Carries a resumed session's state into the replacement image.
pub const RESUME_ENV: &str = "ORBIT_MCP_RESUME";

/// Debug-build fixture only. Milliseconds between stdio lifecycle checks.
/// Release builds keep the two-second budget whatever this says. Values
/// outside 20ms..=2s are ignored so a fixture can only shorten the tick.
pub const LIFECYCLE_INTERVAL_ENV: &str = "ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS";

/// A partial line longer than this is not handed over; the session keeps
/// serving and retries at a later idle point.
const MAX_CARRYOVER: usize = 32 * 1024;
const READ_CHUNK: usize = 64 * 1024;
/// Match rmcp's stream limit, including the terminating newline.
const MAX_STDIO_LINE: usize = rmcp::transport::async_rw::DEFAULT_MAX_LINE_LENGTH;

/// How a stdio session ended.
pub enum StdioExit {
    /// The client closed the session.
    Closed,
    /// A pending breaking upgrade asked this process to yield, and no
    /// installed executable can resume the session.
    Yielded,
    /// Replace this process with `executable`, exporting `resume` as
    /// [`RESUME_ENV`].
    HandOver { executable: PathBuf, resume: String },
}

/// A session this process was exec'd to resume.
pub(crate) struct Resumed {
    initialize: Option<InitializeRequestParams>,
    carryover: Vec<u8>,
}
