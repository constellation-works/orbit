use clap::Args;
use orbit_core::application::job::PipelineWorkerLogSnapshot;
use orbit_core::runtime::audit::run::RunCliInvocationRecord;
use orbit_core::{JobRun, OrbitRuntime};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{DefaultHasher, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::time::Duration;

use crate::command::{CommandOut, Execute, Payload};
use crate::output::sink::OutputMode;

use super::steps::{RunRead, resolve_run, resolve_step_filter};

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run_id\":\"...\",\"job_id\":\"...\",\"records\":[{\"step_id\":...,\"stdout_blob_ref\":...,\"stderr_blob_ref\":...,\"stdout\":\"...\",\"stderr\":\"...\"}]}\nWith --follow, JSON modes emit JSONL records {run_id, provider, stream, text}. Live provider lines come from the retained redacted tracing feed; completed captures supply remaining output. With --step, captures are emitted as each invocation finishes. Stops at any terminal outcome; Ctrl-C stops observing without cancelling.\nExamples:\n  orbit run logs\n  orbit run logs jrun-20260426-0631 --follow\n  orbit run logs jrun-20260426-0631 -s implement_one --json"
)]
pub struct RunLogsArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Show raw logs for a single activity step.id from the v2 job YAML
    #[arg(short = 's', long = "step")]
    pub step_id: Option<String>,

    /// Stream output until the run is terminal. JSON output is JSONL.
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted and
    /// releases its task reservations
    #[arg(long)]
    pub no_reconcile: bool,
}

impl Execute for RunLogsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.follow {
            return follow_logs_payload(
                runtime,
                self.run_id.as_deref(),
                self.step_id.as_deref(),
                RunRead::from_no_reconcile(self.no_reconcile),
            );
        }
        run_logs_payload(
            runtime,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
        )
    }
}

fn follow_logs_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let run = resolve_run(runtime, run_id, read)?;
    let steps = runtime.collect_run_audit_steps(&run.run_id)?;
    let step_filter = resolve_step_filter(&run, &steps, step_id)?;
    let runtime = runtime.clone();
    let log_path = orbit_common::observability::logging::global_jsonl_log_path()?;
    Ok(Payload::stream(
        json!({"run_id": run.run_id, "follow": true}),
        Box::new(move |sink, writer| {
            let json = matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
            let result = follow_logs(
                &runtime,
                &run.run_id,
                step_filter.as_deref(),
                read,
                &log_path,
                json,
                writer,
            );
            match result {
                Err(FollowError::Io(error)) if crate::output::pipe::is_broken_pipe(&error) => {
                    Ok(())
                }
                Err(FollowError::Io(error)) => Err(error.into()),
                Err(FollowError::Runtime(error)) => Err(error),
                Ok(()) => Ok(()),
            }
        }),
    )
    .into())
}

/// Reuse the existing provider line feed instead of introducing another capture
/// writer. Keep only offsets, counts and checksums while following. Durable
/// captures cover a disabled or rotated trace feed without discarding output.
fn follow_logs(
    runtime: &OrbitRuntime,
    run_id: &str,
    step: Option<&str>,
    read: RunRead,
    path: &std::path::Path,
    json: bool,
    writer: &mut dyn Write,
) -> Result<(), FollowError> {
    let mut offset = 0;
    let mut streamed = HashMap::<(String, String), FollowedPrefix>::new();
    let mut completed = HashSet::new();
    loop {
        // Read state first, then drain output. A terminal record guarantees its
        // invocation captures have already been persisted.
        let run = read.show(runtime, run_id)?;
        if step.is_none() {
            drain_provider_lines(path, run_id, &mut offset, &mut streamed, json, writer)?;
        }
        if step.is_some() || run.state.is_terminal() {
            let records =
                filter_cli_invocation_records(runtime.collect_run_cli_invocations(run_id)?, step);
            let mut captures = std::collections::BTreeMap::<(String, String), String>::new();
            for record in &records {
                if !completed.insert(record.event_id.clone()) {
                    continue;
                }
                let provider = record.provider.as_deref().unwrap_or("unknown");
                for (stream, text) in [("stdout", &record.stdout), ("stderr", &record.stderr)] {
                    let capture = captures
                        .entry((provider.to_string(), stream.to_string()))
                        .or_default();
                    for line in text.lines() {
                        capture.push_str(line);
                        capture.push('\n');
                    }
                }
            }
            for ((provider, stream), capture) in captures {
                let prefix = streamed.get(&(provider.clone(), stream.clone()));
                let remainder = prefix.and_then(|prefix| {
                    let initial = capture.get(..prefix.bytes)?;
                    let mut hash = DefaultHasher::new();
                    for line in initial.split_inclusive('\n') {
                        hash.write(line.as_bytes());
                    }
                    (hash.finish() == prefix.hash.finish()).then(|| &capture[prefix.bytes..])
                });
                // Trace rotation, dropped rows, line chunking or different
                // redaction can make the live feed disagree with the capture.
                // Only skip a proven prefix; otherwise replay the capture so
                // a following reader never silently loses retained evidence.
                if prefix.is_some() && remainder.is_none() {
                    eprintln!(
                        "Live logs differ from the retained capture; replaying captured {stream} for {provider}."
                    );
                }
                let remainder = remainder.unwrap_or(&capture);
                if !remainder.is_empty() {
                    emit_follow_record(run_id, &provider, &stream, remainder, json, writer)?;
                }
            }
            if run.state.is_terminal() {
                if records.is_empty()
                    && step.is_none()
                    && let Some(snapshot) = runtime.read_pipeline_worker_log(run_id)?
                    && let Some(content) = snapshot.content
                {
                    emit_follow_record(run_id, "worker", "stderr", &content, json, writer)?;
                }
                writer.flush()?;
                return Ok(());
            }
        }
        writer.flush()?;
        std::thread::sleep(Duration::from_millis(100));
    }
}

enum FollowError {
    Runtime(orbit_core::OrbitError),
    Io(std::io::Error),
}

impl From<orbit_core::OrbitError> for FollowError {
    fn from(error: orbit_core::OrbitError) -> Self {
        Self::Runtime(error)
    }
}

impl From<std::io::Error> for FollowError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Default)]
struct FollowedPrefix {
    bytes: usize,
    hash: DefaultHasher,
}

fn drain_provider_lines(
    path: &std::path::Path,
    run_id: &str,
    offset: &mut u64,
    streamed: &mut HashMap<(String, String), FollowedPrefix>,
    json: bool,
    writer: &mut dyn Write,
) -> std::io::Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() < *offset {
        *offset = 0;
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(*offset))?;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let length = reader.read_until(b'\n', &mut raw)?;
        if length == 0 || raw.last() != Some(&b'\n') {
            break;
        }
        *offset = reader.stream_position()?;
        let Ok(event) = serde_json::from_slice::<Value>(&raw) else {
            continue;
        };
        let fields = &event["fields"];
        if fields["job_run_id"].as_str() != Some(run_id) {
            continue;
        }
        let (Some(provider), Some(stream @ ("stdout" | "stderr")), Some(line)) = (
            fields["provider"].as_str(),
            fields["stream"].as_str(),
            fields["line"].as_str(),
        ) else {
            continue;
        };
        let text = format!("{line}\n");
        emit_follow_record(run_id, provider, stream, &text, json, writer)?;
        let prefix = streamed
            .entry((provider.to_string(), stream.to_string()))
            .or_default();
        prefix.bytes = prefix.bytes.saturating_add(text.len());
        prefix.hash.write(text.as_bytes());
    }
    Ok(())
}

fn emit_follow_record(
    run_id: &str,
    provider: &str,
    stream: &str,
    text: &str,
    json: bool,
    writer: &mut dyn Write,
) -> std::io::Result<()> {
    if json {
        serde_json::to_writer(
            &mut *writer,
            &json!({"run_id": run_id, "provider": provider, "stream": stream, "text": text}),
        )
        .map_err(|error| {
            std::io::Error::new(
                error.io_error_kind().unwrap_or(std::io::ErrorKind::Other),
                error,
            )
        })?;
        writeln!(writer)?;
    } else if stream == "stderr" {
        eprint!("{text}");
    } else {
        writer.write_all(text.as_bytes())?;
    }
    Ok(())
}

fn run_logs_payload(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let run = resolve_run(runtime, run_id, read)?;
    let audit_steps = runtime.collect_run_audit_steps(&run.run_id)?;
    let step_filter = resolve_step_filter(&run, &audit_steps, step_id)?;
    let records = filter_cli_invocation_records(
        runtime.collect_run_cli_invocations(&run.run_id)?,
        step_filter.as_deref(),
    );

    if records.is_empty() {
        return worker_log_fallback_payload(runtime, &run);
    }

    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "records": records.iter().map(cli_invocation_record_to_json).collect::<Vec<_>>(),
    });

    // Subprocess stderr is diagnostic: keep it off the record stream so
    // `--format json` stdout stays parseable.
    for record in &records {
        eprint!("{}", record.stderr);
    }
    let text = records
        .iter()
        .map(|record| record.stdout.as_str())
        .collect::<String>();
    Ok(Payload::detail(doc, text).into())
}

/// [ORB-12038] `records` is empty for a run that never reached step
/// execution — most notably a routine-dispatch workspace mismatch, which
/// fails the run before any step can run and so has no audited CLI
/// invocation to show. That worker still writes its own
/// `<run_id>.worker.log` directly; fall back to it instead of reporting no
/// logs when the file is actually sitting on disk.
fn worker_log_fallback_payload(runtime: &OrbitRuntime, run: &JobRun) -> CommandOut {
    let worker_log = runtime.read_pipeline_worker_log(&run.run_id)?;
    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "records": Value::Array(Vec::new()),
        "worker_log_path": worker_log.as_ref().map(|snapshot| snapshot.path.display().to_string()),
    });
    let detail = match worker_log {
        Some(PipelineWorkerLogSnapshot {
            path,
            content: Some(content),
        }) => format!("worker log ({}):\n{content}", path.display()),
        Some(PipelineWorkerLogSnapshot {
            path,
            content: None,
        }) => format!(
            "worker log recorded with no readable content: {}",
            path.display()
        ),
        None => "No raw stdout/stderr blobs recorded.".to_string(),
    };
    Ok(Payload::detail(doc, detail).into())
}

fn filter_cli_invocation_records(
    records: Vec<RunCliInvocationRecord>,
    step_filter: Option<&str>,
) -> Vec<RunCliInvocationRecord> {
    records
        .into_iter()
        .filter(|record| step_filter.is_none_or(|filter| record.step_id.as_deref() == Some(filter)))
        .collect()
}

fn cli_invocation_record_to_json(record: &RunCliInvocationRecord) -> Value {
    json!({
        "step_id": record.step_id,
        "provider": record.provider,
        "stdout_blob_ref": record.stdout_blob_ref,
        "stderr_blob_ref": record.stderr_blob_ref,
        "stdout": record.stdout,
        "stderr": record.stderr,
    })
}
