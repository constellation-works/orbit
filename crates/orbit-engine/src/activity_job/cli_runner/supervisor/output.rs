use std::sync::PoisonError;

use orbit_common::process::supervisor::{OutputSink, SinkVerdict};

use super::capture::SharedOutputCapture;

const OUTPUT_LINE_EVENT_LIMIT_BYTES: usize = 64 * 1024;

pub(super) struct OutputLineContext {
    pub(super) provider: String,
    pub(super) stream: &'static str,
    pub(super) job_run_id: String,
    pub(super) task_id: Option<String>,
    pub(super) cwd: Option<String>,
    /// The job's dispatch: the supervisor's pipe worker has no default of its
    /// own, so each line event is emitted into this one.
    pub(super) dispatch: tracing::Dispatch,
}

/// One provider output stream: every chunk goes into the shared rolling
/// capture and becomes per-line tracing events. The capture keeps going past
/// its limit, so the sink never stops the run.
pub(super) struct TracedCaptureSink {
    capture: SharedOutputCapture,
    context: OutputLineContext,
    line_buf: Vec<u8>,
}

impl TracedCaptureSink {
    pub(super) fn new(capture: SharedOutputCapture, context: OutputLineContext) -> Self {
        Self {
            capture,
            context,
            line_buf: Vec::new(),
        }
    }
}

impl OutputSink for TracedCaptureSink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        self.capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes);
        let Self {
            context, line_buf, ..
        } = self;
        tracing::dispatcher::with_default(&context.dispatch, || {
            emit_output_chunk(context, bytes, line_buf);
        });
        SinkVerdict::Continue
    }

    /// Flushes a final partial line. The caller reads the capture itself
    /// once supervision returns, so the sink reports no bytes.
    fn finish(self: Box<Self>) -> Vec<u8> {
        tracing::dispatcher::with_default(&self.context.dispatch, || {
            if !self.line_buf.is_empty() && output_events_enabled() {
                emit_output_line(&self.context, &self.line_buf);
            }
        });
        Vec::new()
    }
}

fn output_events_enabled() -> bool {
    tracing::enabled!(target: "orbit_engine::activity_job::cli_runner::supervisor", tracing::Level::INFO)
}

fn emit_output_chunk(context: &OutputLineContext, raw: &[u8], line_buf: &mut Vec<u8>) {
    if !output_events_enabled() {
        return;
    }

    for segment in raw.split_inclusive(|byte| *byte == b'\n') {
        line_buf.extend_from_slice(segment);
        if segment.ends_with(b"\n") || line_buf.len() >= OUTPUT_LINE_EVENT_LIMIT_BYTES {
            emit_output_line(context, line_buf);
            line_buf.clear();
        }
    }
}

fn emit_output_line(context: &OutputLineContext, raw_line: &[u8]) {
    let line = line_text(raw_line);
    let provider = context.provider.as_str();
    let stream = context.stream;
    let job_run_id = context.job_run_id.as_str();
    let task_id = context.task_id.as_deref();
    if let Some(cwd) = context.cwd.as_deref() {
        tracing::info!(target: "orbit_engine::activity_job::cli_runner::supervisor",
            agent_output = true,
            provider = provider,
            stream = stream,
            job_run_id = job_run_id,
            task_id = task_id,
            cwd = cwd,
            line = line.as_str()
        );
    } else {
        tracing::info!(target: "orbit_engine::activity_job::cli_runner::supervisor",
            agent_output = true,
            provider = provider,
            stream = stream,
            job_run_id = job_run_id,
            task_id = task_id,
            line = line.as_str()
        );
    }
}

fn line_text(raw_line: &[u8]) -> String {
    let line = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}
