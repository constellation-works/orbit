use clap::Args;
use orbit_common::Env;
use orbit_core::app::run::{Command, exec};
use orbit_core::application::job::PipelineWorkerLogSnapshot;
use orbit_core::runtime::audit::run_logs::RunCliInvocationProfile;
use orbit_core::{Core, JobRun};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{DefaultHasher, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::time::Duration;

use crate::command::{CommandOut, Payload};
use crate::output::common::sink::OutputMode;

use super::steps::RunRead;
use orbit_core::app::run::{LogFollower, LogSnapshot};

#[derive(Args)]
#[command(
    after_help = "JSON shape and --follow records: installed orbit-orchestrate skill `references/run-debugging-audit-output.md`.\n\nExamples:\n  orbit run logs\n  orbit run logs jrun-20260426-0631 --follow\n  orbit run logs jrun-20260426-0631 -s implement_one --json"
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

    /// Report stored run records as-is: skip stale-run reconciliation, which
    /// finalizes an orphaned pending or running run as interrupted, releases
    /// its task reservations, and settles with their owner the pull claims an
    /// ended drain or leaf left
    #[arg(long)]
    pub no_reconcile: bool,
}

impl RunLogsArgs {
    pub(super) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        if self.follow {
            return follow_logs_payload(
                core,
                env,
                self.run_id.as_deref(),
                self.step_id.as_deref(),
                RunRead::from_no_reconcile(self.no_reconcile),
            );
        }
        run_logs_payload(
            core,
            self.run_id.as_deref(),
            self.step_id.as_deref(),
            RunRead::from_no_reconcile(self.no_reconcile),
        )
    }
}

fn follow_logs_payload(
    core: &Core,
    env: &Env,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let follower = exec(core, Command::follow_logs(run_id, step_id, read))?.value;
    let log_path = orbit_common::observability::logging::global_jsonl_log_path(env)?;
    Ok(Payload::stream(
        json!({"run_id": follower.run_id(), "follow": true}),
        Box::new(move |sink, writer| {
            let json = matches!(sink.mode(), OutputMode::Json | OutputMode::Ndjson);
            let result = follow_logs(&follower, &log_path, json, writer);
            match result {
                Err(FollowError::Io(error))
                    if crate::output::common::pipe::is_broken_pipe(&error) =>
                {
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
    follower: &LogFollower,
    path: &std::path::Path,
    json: bool,
    writer: &mut dyn Write,
) -> Result<(), FollowError> {
    let run_id = follower.run_id();
    let mut feeds = vec![(path.to_path_buf(), 0)];
    if let Some(agent_path) = orbit_common::observability::logging::agent_jsonl_log_path(path) {
        feeds.push((agent_path, 0));
    }
    let mut streamed = HashMap::<(String, String), FollowedPrefix>::new();
    let mut completed = HashSet::new();
    loop {
        // Read state first, then drain output. A terminal record guarantees its
        // invocation captures have already been persisted.
        let run = follower.state()?;
        if !follower.step_filtered() {
            for (path, offset) in &mut feeds {
                drain_provider_lines(path, run_id, offset, &mut streamed, json, writer)?;
            }
        }
        if follower.step_filtered() || run.state.is_terminal() {
            // The step filter and the emitted-invocation set apply before any
            // blob is read, so each capture is loaded once however long the
            // run keeps polling.
            let records = follower.captures(&completed)?;
            let mut captures = std::collections::BTreeMap::<(String, String), String>::new();
            for record in &records {
                completed.insert(record.event_id.clone());
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
                    writeln!(
                        crate::output::common::sink::stderr().lock(),
                        "Live logs differ from the retained capture; replaying captured {stream} for {provider}."
                    )?;
                }
                let remainder = remainder.unwrap_or(&capture);
                if !remainder.is_empty() {
                    emit_follow_record(run_id, &provider, &stream, remainder, json, writer)?;
                }
            }
            if run.state.is_terminal() {
                if records.is_empty()
                    && !follower.step_filtered()
                    && let Some(snapshot) = follower.worker_log()?
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
        crate::output::common::sink::stderr()
            .lock()
            .write_all(text.as_bytes())?;
    } else {
        writer.write_all(text.as_bytes())?;
    }
    Ok(())
}

fn run_logs_payload(
    core: &Core,
    run_id: Option<&str>,
    step_id: Option<&str>,
    read: RunRead,
) -> CommandOut {
    let LogSnapshot {
        run,
        records,
        worker,
    } = exec(core, Command::logs(run_id, step_id, read))?.value;
    if records.is_empty() {
        return worker_log_fallback_payload(worker, &run);
    }
    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "records": records
            .iter()
            .map(|record| record.to_profile_json(RunCliInvocationProfile::Raw))
            .collect::<Vec<_>>(),
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
fn worker_log_fallback_payload(
    worker_log: Option<PipelineWorkerLogSnapshot>,
    run: &JobRun,
) -> CommandOut {
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
