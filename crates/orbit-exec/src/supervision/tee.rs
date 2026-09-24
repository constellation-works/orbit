use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use orbit_common::process::output_capture::{BoundedOutputCapture, capture_limit_from_env};
use orbit_common::security::redaction::redact_sensitive_env_text;

pub(super) const ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV: &str =
    "ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES";
pub(super) const DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

pub(super) fn output_capture_limit() -> usize {
    capture_limit_from_env(
        ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV,
        DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES,
    )
}

pub(super) fn spawn_stdout_drain<R>(
    out: R,
    debug: bool,
    limit: usize,
    limit_tx: Sender<&'static str>,
) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    spawn_drain(out, debug, limit, limit_tx, "stdout")
}

pub(super) fn spawn_stderr_drain<R>(
    err: R,
    debug: bool,
    limit: usize,
    limit_tx: Sender<&'static str>,
) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    spawn_drain(err, debug, limit, limit_tx, "stderr")
}

/// Capture a child stream's raw bytes up to `limit`; in debug mode also echo
/// it to Orbit's stderr with sensitive values redacted.
fn spawn_drain<R>(
    mut reader: R,
    debug: bool,
    limit: usize,
    limit_tx: Sender<&'static str>,
    stream: &'static str,
) -> JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut capture = BoundedOutputCapture::new(limit);
        let mut echo = debug.then(|| RedactingEcho::new(std::io::stderr()));
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(echo) = echo.as_mut() {
                        echo.push(&chunk[..n]);
                    }
                    if capture.push(&chunk[..n]) {
                        let _ = limit_tx.send(stream);
                        break;
                    }
                }
            }
        }
        if let Some(echo) = echo {
            echo.finish();
        }
        capture.into_bytes()
    })
}

/// A partial line held longer than this is echoed anyway, so a child that
/// never writes a newline cannot grow the buffer without bound.
const MAX_PENDING_ECHO_BYTES: usize = 64 * 1024;

/// Debug echo that redacts whole lines. Redacting each read on its own would
/// miss a secret split across two reads and garble a multi-byte character
/// split across them.
pub(super) struct RedactingEcho<W: Write> {
    sink: W,
    pending: Vec<u8>,
}

impl<W: Write> RedactingEcho<W> {
    pub(super) fn new(sink: W) -> Self {
        Self {
            sink,
            pending: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let cut = match self.pending.iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => newline + 1,
            None if self.pending.len() >= MAX_PENDING_ECHO_BYTES => self.pending.len(),
            None => return,
        };
        let complete: Vec<u8> = self.pending.drain(..cut).collect();
        self.write_redacted(&complete);
    }

    pub(super) fn finish(mut self) -> W {
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.write_redacted(&rest);
        }
        self.sink
    }

    fn write_redacted(&mut self, bytes: &[u8]) {
        let redacted = redact_sensitive_env_text(&String::from_utf8_lossy(bytes));
        let _ = self.sink.write_all(redacted.as_bytes());
    }
}

pub(super) fn spawn_stdin_write<W>(
    mut stdin: W,
    bytes: Vec<u8>,
    result_tx: Sender<Result<(), String>>,
) -> JoinHandle<()>
where
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        let result = stdin
            .write_all(&bytes)
            .map_err(|e| format!("failed to write process stdin: {e}"));
        let _ = result_tx.send(result);
    })
}
