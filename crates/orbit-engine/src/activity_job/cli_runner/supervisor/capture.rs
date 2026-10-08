use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use orbit_common::process::output_capture::capture_limit_from_env;
use orbit_common::security::redaction::redact_all;
use orbit_common::text::floor_char_boundary;

const CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_ENV: &str = "ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES";
const DEFAULT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

pub(super) type SharedOutputCapture = Arc<Mutex<RollingOutputCapture>>;

#[derive(Debug)]
pub(in crate::activity_job::cli_runner) struct CapturedOutput {
    bytes: Vec<u8>,
    protocol_offset: usize,
    observed_bytes: usize,
    capture_limit_bytes: usize,
    truncated: bool,
}

impl CapturedOutput {
    pub(in crate::activity_job::cli_runner) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(in crate::activity_job::cli_runner) fn protocol_bytes(&self) -> &[u8] {
        &self.bytes[self.protocol_offset..]
    }

    pub(in crate::activity_job::cli_runner) fn observed_bytes(&self) -> usize {
        self.observed_bytes
    }

    pub(in crate::activity_job::cli_runner) fn capture_limit_bytes(&self) -> usize {
        self.capture_limit_bytes
    }

    pub(in crate::activity_job::cli_runner) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug)]
pub(super) struct RollingOutputCapture {
    prefix: Vec<u8>,
    tail: VecDeque<u8>,
    pub(super) observed_bytes: usize,
    limit: usize,
    truncated: bool,
}

impl RollingOutputCapture {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            prefix: Vec::new(),
            tail: VecDeque::new(),
            observed_bytes: 0,
            limit,
            truncated: false,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.observed_bytes = self.observed_bytes.saturating_add(chunk.len());
        if !self.truncated && self.prefix.len().saturating_add(chunk.len()) <= self.limit {
            self.prefix.extend_from_slice(chunk);
            return;
        }

        let prefix_limit = self.limit / 2;
        let tail_limit = self.limit.saturating_sub(prefix_limit);
        if !self.truncated {
            let mut window = std::mem::take(&mut self.prefix);
            window.extend_from_slice(chunk);
            // Keep the protocol tail raw. Redact the diagnostic window before
            // its final cut, with the rest of the capture as lookahead: cutting
            // first can turn a complete provider token into an unknown prefix.
            self.tail.extend(window[prefix_limit..].iter().copied());
            let redacted = redact_all(&String::from_utf8_lossy(&window));
            let boundary = floor_char_boundary(&redacted, prefix_limit);
            // A token or field longer than the lookahead must not leave a
            // partial line in the prefix. This also avoids cutting UTF-8 or a
            // redaction marker after substitutions expand or shrink text.
            let prefix_end = redacted[..boundary].rfind('\n').map_or(0, |idx| idx + 1);
            self.prefix = redacted.as_bytes()[..prefix_end].to_vec();
            self.truncated = true;
        } else {
            self.tail.extend(chunk);
        }
        while self.tail.len() > tail_limit {
            self.tail.pop_front();
        }
    }

    /// The newest complete lines of the capture, at most `window` bytes.
    pub(super) fn recent(&self, window: usize) -> Vec<u8> {
        let (bytes, from_start) = if self.truncated {
            let skip = self.tail.len().saturating_sub(window);
            (
                self.tail.iter().skip(skip).copied().collect::<Vec<_>>(),
                false,
            )
        } else {
            let start = self.prefix.len().saturating_sub(window);
            (self.prefix[start..].to_vec(), start == 0)
        };
        if from_start {
            return bytes;
        }
        // The window may open mid-line; that fragment is not a line.
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or_else(Vec::new, |idx| bytes[idx + 1..].to_vec())
    }

    fn finish(&self) -> CapturedOutput {
        if !self.truncated {
            return CapturedOutput {
                bytes: self.prefix.clone(),
                protocol_offset: 0,
                observed_bytes: self.observed_bytes,
                capture_limit_bytes: self.limit,
                truncated: false,
            };
        }

        // The tail may start in the middle of a structured JSONL event. Drop
        // that partial line so protocol consumers can still parse the final
        // complete provider events (including the Orbit response envelope).
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let complete_tail = tail
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |idx| &tail[idx + 1..]);
        let marker = format!(
            "\n[orbit: output capture truncated; observed_bytes={}; capture_limit_bytes={}]\n",
            self.observed_bytes, self.limit
        );
        let protocol_offset = self.prefix.len() + marker.len();
        let mut bytes = Vec::with_capacity(protocol_offset + complete_tail.len());
        bytes.extend_from_slice(&self.prefix);
        bytes.extend_from_slice(marker.as_bytes());
        bytes.extend_from_slice(complete_tail);

        CapturedOutput {
            bytes,
            protocol_offset,
            observed_bytes: self.observed_bytes,
            capture_limit_bytes: self.limit,
            truncated: true,
        }
    }
}

/// [ORB-13899] What a running child has written to stdout so far.
pub(in crate::activity_job::cli_runner) struct OutputProgress {
    pub(in crate::activity_job::cli_runner) observed_bytes: usize,
    /// The newest complete stdout lines, bounded.
    pub(in crate::activity_job::cli_runner) recent: Vec<u8>,
}

pub(super) fn finish_captured_output(
    buf: &SharedOutputCapture,
    output_limit: usize,
) -> CapturedOutput {
    buf.lock()
        .map(|buf| buf.finish())
        .unwrap_or_else(|_| CapturedOutput {
            bytes: Vec::new(),
            protocol_offset: 0,
            observed_bytes: 0,
            capture_limit_bytes: output_limit,
            truncated: false,
        })
}

pub(super) fn default_output_capture_limit() -> usize {
    capture_limit_from_env(
        CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_ENV,
        DEFAULT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES,
    )
}
