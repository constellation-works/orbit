/// Suffix appended once to captured output when the byte budget is exceeded.
pub const OUTPUT_TRUNCATED_MARKER: &[u8] = b"\n[orbit: output truncated after capture limit]\n";

/// Retains the prefix of a byte stream up to a caller-supplied limit, then records truncation.
#[derive(Debug, Clone)]
pub struct BoundedOutputCapture {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl BoundedOutputCapture {
    /// Start an empty capture. The byte limit excludes the truncation marker.
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            truncated: false,
        }
    }

    /// Append bytes up to the limit. Return true only on the first append that discards bytes and
    /// adds the marker.
    pub fn push(&mut self, chunk: &[u8]) -> bool {
        if self.truncated {
            return false;
        }

        let remaining = self.limit.saturating_sub(self.bytes.len());
        if chunk.len() <= remaining {
            self.bytes.extend_from_slice(chunk);
            return false;
        }

        if remaining > 0 {
            self.bytes.extend_from_slice(&chunk[..remaining]);
        }
        self.bytes.extend_from_slice(OUTPUT_TRUNCATED_MARKER);
        self.truncated = true;
        true
    }

    /// Take the captured prefix, including the marker if bytes were discarded.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Borrow the captured prefix and any marker without copying.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return whether any input bytes have been discarded. Filling the budget exactly leaves this
    /// false.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Read a positive byte limit from the environment, falling back for absent, zero or invalid
/// values.
pub fn capture_limit_from_env(value: Option<&std::ffi::OsStr>, default: usize) -> usize {
    value
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|limit| *limit > 0)
        .unwrap_or(default)
}
