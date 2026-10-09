//! Web-focused subset of log tailing / rendering logic (no colored output, no
//! clap ValueEnum usage for CLI flags). Preserves exact `resolve_log_path`
//! (ORBIT_LOG_PATH + HOME fallback via orbit-common) and the HTML rendering
//! used by /api/log and /api/diagnostics.

mod filter;
mod render;
mod scan;

pub(crate) use filter::{Filters, LevelFilter};
pub(crate) use render::{RenderedLogEvent, format_message_html, render_log_event_for_web};
pub(crate) use scan::{
    parse_matching_event, read_recent_matching_events_across_segments, read_recent_rendered_tail,
    resolve_log_path,
};

/// Longest single log record the web surfaces buffer. The SSE stream drops a
/// longer record, and the snapshot tail skips an unterminated trailing one
/// this large instead of reading it into memory.
pub(crate) const MAX_LOG_RECORD_BYTES: usize = 1 << 20;

#[cfg(test)]
mod tests;
