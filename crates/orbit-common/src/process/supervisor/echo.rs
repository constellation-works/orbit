//! Redacting debug echo of a child's output stream.

use std::io::Write;

use crate::security::redaction::redact_sensitive_env_bytes;

/// A partial line held longer than this is echoed anyway, so a child that
/// never writes a newline cannot grow the buffer without bound.
pub(super) const MAX_PENDING_ECHO_BYTES: usize = 64 * 1024;

/// Debug echo that redacts sensitive environment values.
///
/// Bytes stay buffered through a newline so a value, or a multibyte character,
/// split across reads is still whole when it is written. A partial line is
/// flushed at [`MAX_PENDING_ECHO_BYTES`]. The longest suffix that is a proper
/// prefix of a sensitive value is kept across that flush, and across a newline
/// inside a multiline value, until the value completes. Retention stops at the
/// same cap, so a value longer than the cap can still be split.
pub(super) struct RedactingEcho<W: Write> {
    sink: W,
    pending: Vec<u8>,
    pending_newline: bool,
}

impl<W: Write> RedactingEcho<W> {
    pub(super) fn new(sink: W) -> Self {
        Self {
            sink,
            pending: Vec::new(),
            pending_newline: false,
        }
    }

    /// Bytes not yet written to the sink.
    ///
    /// Supervision tests assert this stays within [`MAX_PENDING_ECHO_BYTES`]
    /// after every read, including a partial line that follows a newline.
    #[cfg(test)]
    pub(super) fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        if !self.pending_newline && bytes.contains(&b'\n') {
            self.pending_newline = true;
        }
        self.pending.extend_from_slice(bytes);
        self.drain(false);
    }

    pub(super) fn finish(mut self) -> W {
        self.drain(true);
        self.sink
    }

    fn drain(&mut self, finishing: bool) {
        if finishing {
            let _holdback = redact_sensitive_env_bytes(&mut self.pending);
            if !self.pending.is_empty() {
                let rest = std::mem::take(&mut self.pending);
                let _ = self.sink.write_all(&rest);
            }
            self.pending_newline = false;
            return;
        }
        loop {
            if self.pending.is_empty() {
                self.pending_newline = false;
                return;
            }
            if !self.pending_newline && self.pending.len() < MAX_PENDING_ECHO_BYTES {
                return;
            }
            let secret_holdback = redact_sensitive_env_bytes(&mut self.pending);
            self.pending_newline = self.pending.contains(&b'\n');
            if self.pending.is_empty() {
                return;
            }
            let keep = retained_suffix(&self.pending, secret_holdback);
            if self.emit_settled(keep) {
                continue;
            }
            return;
        }
    }

    /// Write every byte that is safe to release. Returns whether a later pass
    /// should look at what remains.
    fn emit_settled(&mut self, keep: usize) -> bool {
        if self.pending.len() <= keep {
            if self.pending.len() < MAX_PENDING_ECHO_BYTES {
                return false;
            }
            let min_cut = self
                .pending
                .len()
                .saturating_sub(MAX_PENDING_ECHO_BYTES - 1)
                .max(1);
            return self.emit(utf8_progress_cut(&self.pending, min_cut));
        }
        let emit_limit = self.pending.len() - keep;
        if let Some(newline) = self.pending[..emit_limit]
            .iter()
            .rposition(|&byte| byte == b'\n')
        {
            return self.emit(newline + 1);
        }
        if self.pending.len() >= MAX_PENDING_ECHO_BYTES {
            let cut = utf8_floor_cut(&self.pending, emit_limit);
            let cut = if cut == 0 {
                utf8_progress_cut(&self.pending, 1)
            } else {
                cut
            };
            return self.emit(cut);
        }
        false
    }

    fn emit(&mut self, cut: usize) -> bool {
        if cut == 0 || cut > self.pending.len() {
            return false;
        }
        let chunk: Vec<u8> = self.pending.drain(..cut).collect();
        let _ = self.sink.write_all(&chunk);
        self.pending_newline = self.pending.contains(&b'\n');
        true
    }
}

/// Bytes held back so a sensitive prefix, and a trailing incomplete UTF-8
/// sequence, are not written before the rest of the value or character arrives.
fn retained_suffix(bytes: &[u8], secret_holdback: usize) -> usize {
    secret_holdback
        .max(incomplete_utf8_tail(bytes))
        .min(MAX_PENDING_ECHO_BYTES.saturating_sub(1))
        .min(bytes.len())
}

fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    let len = bytes.len();
    if len == 0 {
        return 0;
    }
    let start = len.saturating_sub(3);
    for index in (start..len).rev() {
        if !is_utf8_boundary(bytes, index) {
            continue;
        }
        let width = utf8_sequence_len(bytes[index]);
        let have = len - index;
        if width > have && have < 4 {
            return have;
        }
        return 0;
    }
    0
}

fn utf8_sequence_len(lead: u8) -> usize {
    if lead & 0b1000_0000 == 0 {
        1
    } else if lead & 0b1110_0000 == 0b1100_0000 {
        2
    } else if lead & 0b1111_0000 == 0b1110_0000 {
        3
    } else if lead & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        0
    }
}

fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || index >= bytes.len() || bytes[index] & 0b1100_0000 != 0b1000_0000
}

/// Largest cut at or before `cut` that does not split a UTF-8 sequence.
fn utf8_floor_cut(bytes: &[u8], mut cut: usize) -> usize {
    cut = cut.min(bytes.len());
    while cut > 0 && !is_utf8_boundary(bytes, cut) {
        cut -= 1;
    }
    cut
}

/// Smallest cut at or after `min_cut` that does not split a UTF-8 sequence.
fn utf8_progress_cut(bytes: &[u8], min_cut: usize) -> usize {
    if bytes.is_empty() || min_cut == 0 {
        return 0;
    }
    let mut cut = min_cut.min(bytes.len());
    while cut < bytes.len() && !is_utf8_boundary(bytes, cut) {
        cut += 1;
    }
    cut
}
