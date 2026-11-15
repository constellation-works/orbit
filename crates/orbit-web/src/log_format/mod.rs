//! Web log scanning and HTML rendering for /api/log and /api/diagnostics,
//! using the shared process-log filters and path resolution from orbit-cmd.

mod filter;
mod render;
mod scan;

pub(crate) use orbit_cmd::log_format::{Filters, LevelFilter, resolve_log_path};

pub(crate) use filter::filters_from_query_parts;
pub(crate) use render::{RenderedLogEvent, format_message_html, render_log_event_for_web};
pub(crate) use scan::{
    parse_matching_event, read_recent_matching_events_across_segments, read_recent_matching_tail,
};

/// Longest single log record the web surfaces buffer. The SSE stream drops a
/// longer record, as do the snapshot and Errors reverse scans. An oversized
/// unterminated snapshot tail leaves its cursor at the record's start.
pub(crate) const MAX_LOG_RECORD_BYTES: usize = 1 << 20;

#[cfg(test)]
mod tests;

/// Stable identity for an open log file; never derived from the path after a scan.
pub(crate) fn log_file_generation(metadata: &std::fs::Metadata) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!("{:x}-{:x}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let created = metadata
            .created()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?;
        Ok(format!(
            "{:x}-{:x}",
            created.as_secs(),
            created.subsec_nanos()
        ))
    }
}
