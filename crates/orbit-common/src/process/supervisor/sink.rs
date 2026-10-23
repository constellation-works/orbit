//! Where a supervised child's output goes.

use std::io::{PipeWriter, Stderr, Write};

use super::super::output_capture::BoundedOutputCapture;
use super::ProcessStopCause;
use super::echo::RedactingEcho;

/// What a sink wants after receiving a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkVerdict {
    /// Keep reading.
    Continue,
    /// Stop reading this stream; the run continues. A child that keeps
    /// writing to it then sees a broken pipe.
    Close,
    /// Stop reading this stream and end the run with this cause.
    Stop(ProcessStopCause),
}

/// Receives one output stream of a supervised child on its pipe worker.
pub trait OutputSink: Send {
    /// Consume a chunk read from the stream.
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict;

    /// Finish the stream and return the bytes the outcome reports for it.
    /// Runs on the pipe worker before the supervisor joins it.
    fn finish(self: Box<Self>) -> Vec<u8>;
}

/// Capture a stream up to a byte limit. Reaching the limit stops the run
/// with [`ProcessStopCause::OutputCaptureLimit`]; the captured bytes keep the
/// prefix and end with the truncation marker.
pub struct CaptureSink {
    stream: &'static str,
    capture: BoundedOutputCapture,
    echo: Option<RedactingEcho<Stderr>>,
}

impl CaptureSink {
    /// Capture up to `limit` bytes of the stream named `stream` in stop
    /// causes ("stdout", "stderr").
    pub fn new(stream: &'static str, limit: usize) -> Self {
        Self {
            stream,
            capture: BoundedOutputCapture::new(limit),
            echo: None,
        }
    }

    /// Also echo the stream to this process's stderr with sensitive
    /// environment values redacted, when `enabled`.
    pub fn with_redacting_echo(mut self, enabled: bool) -> Self {
        self.echo = enabled.then(|| RedactingEcho::new(std::io::stderr()));
        self
    }
}

impl OutputSink for CaptureSink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        if let Some(echo) = self.echo.as_mut() {
            echo.push(bytes);
        }
        if self.capture.push(bytes) {
            return SinkVerdict::Stop(ProcessStopCause::OutputCaptureLimit {
                stream: self.stream,
            });
        }
        SinkVerdict::Continue
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        if let Some(echo) = self.echo {
            echo.finish();
        }
        self.capture.into_bytes()
    }
}

/// Forward a stream into a pipe whose read end a consumer owns. The relay
/// closes when the stream finishes, within the drain budget, so the consumer
/// always reaches EOF. A consumer that drops its end closes the stream.
/// Reports no bytes.
pub struct RelaySink(PipeWriter);

impl RelaySink {
    pub fn new(relay: PipeWriter) -> Self {
        Self(relay)
    }
}

impl OutputSink for RelaySink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        match self.0.write_all(bytes) {
            Ok(()) => SinkVerdict::Continue,
            Err(_) => SinkVerdict::Close,
        }
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        Vec::new()
    }
}
