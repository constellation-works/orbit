//! Store-independent worker provenance in the already-open worker log.
//!
//! A submitter's observer thread can disappear with its process, and a store
//! outage can reject its exit audit. These diagnostic receipts survive both.
//! They never grant execution authority or change failure classification.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::observability::logging::redact_event_text;
use orbit_common::process::identity::{process_start_identity_token, stable_tokens_match};
use orbit_types::workflow::JobRun;
use serde::{Deserialize, Serialize};

use super::{ScopeEvidence, WorkerExit};
use crate::application::job::run::definition::pipeline_worker_file_name;
use crate::application::job::run::worker_scope::WorkerScopeCgroup;

const LIFETIME_MARKER: &str = "orbit pipeline worker lifetime: ";
const REAPED_MARKER: &str = "orbit pipeline worker reaped: ";
const SIGNAL_MARKER: &str = "orbit pipeline worker terminating signal:";
const LOG_WINDOW_BYTES: u64 = 16 * 1024;

#[derive(Serialize, Deserialize)]
struct LifetimeRecord {
    run_id: String,
    worker_pid: u32,
    pid_start_time: Option<String>,
    parent_pid: Option<u32>,
    parent_start_time: Option<String>,
    entered_at: DateTime<Utc>,
    scope: Option<ScopeEvidence>,
    returned_at: Option<DateTime<Utc>>,
    error: Option<String>,
    unwound: bool,
}

/// Record worker entry and application return independently of the run store.
pub(crate) struct WorkerLifetime {
    record: LifetimeRecord,
}

impl WorkerLifetime {
    pub(crate) fn enter(run_id: &str) -> Self {
        #[cfg(unix)]
        // SAFETY: getppid only reads this process's parent ID.
        let parent_pid = Some(unsafe { libc::getppid() } as u32);
        #[cfg(not(unix))]
        let parent_pid = None;
        let record = LifetimeRecord {
            run_id: run_id.to_string(),
            worker_pid: std::process::id(),
            pid_start_time: process_start_identity_token(std::process::id()),
            parent_pid,
            parent_start_time: parent_pid.and_then(process_start_identity_token),
            entered_at: Utc::now(),
            scope: WorkerScopeCgroup::of_current_process()
                .map(|scope| ScopeEvidence::read(&scope, false)),
            returned_at: None,
            error: None,
            unwound: false,
        };
        write_log_receipt(&mut std::io::stderr().lock(), LIFETIME_MARKER, &record);
        Self { record }
    }

    pub(crate) fn record_return(mut self, error: Option<&OrbitError>) {
        self.record.returned_at = Some(Utc::now());
        self.record.error = error.map(|error| bounded_provenance(&error.to_string()));
        write_log_receipt(&mut std::io::stderr().lock(), LIFETIME_MARKER, &self.record);
    }
}

impl Drop for WorkerLifetime {
    fn drop(&mut self) {
        if self.record.returned_at.is_none() && std::thread::panicking() {
            self.record.returned_at = Some(Utc::now());
            self.record.unwound = true;
            write_log_receipt(&mut std::io::stderr().lock(), LIFETIME_MARKER, &self.record);
        }
    }
}

/// Save a reap before the fallible audit insert, through the retained log FD.
pub(crate) fn record_worker_reap(file: &mut File, exit: &WorkerExit) {
    write_log_receipt(file, REAPED_MARKER, exit);
}

fn write_log_receipt(writer: &mut impl Write, marker: &str, record: &impl Serialize) {
    if let Ok(json) = serde_json::to_string(record) {
        let line = format!("\n{marker}{}\n", redact_event_text(&json));
        if let Err(error) = writer.write_all(line.as_bytes()) {
            tracing::warn!(%error, "could not preserve worker provenance in its log");
        }
    }
}

fn bounded_provenance(text: &str) -> String {
    let text = redact_event_text(text);
    if text.chars().count() > 2048 {
        format!("{}…", text.chars().take(2048).collect::<String>())
    } else {
        text
    }
}

pub(crate) struct WorkerLogProvenance {
    pub(crate) description: String,
    pub(crate) reaped: Option<WorkerExit>,
}

/// Log receipts are diagnostic only: validate run, PID, start identity and
/// lifetime before quoting them. Read a bounded head (entry) and tail (return,
/// signal or reap), so a large provider transcript cannot hide both ends.
pub(crate) fn worker_log_provenance(logs_dir: &Path, run: &JobRun) -> Option<WorkerLogProvenance> {
    let pid = run.pid?;
    let path = logs_dir.join(pipeline_worker_file_name(&run.run_id, ".worker.log").ok()?);
    let mut file = orbit_common::fs::io::open_read_only_no_follow(&path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let len = metadata.len();
    let mut bytes = Vec::new();
    (&mut file)
        .take(len.min(LOG_WINDOW_BYTES))
        .read_to_end(&mut bytes)
        .ok()?;
    if len > LOG_WINDOW_BYTES {
        let start = len.saturating_sub(LOG_WINDOW_BYTES).max(LOG_WINDOW_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        // Do not turn two partial records into one valid record.
        bytes.push(b'\n');
        (&mut file).take(len - start).read_to_end(&mut bytes).ok()?;
    }
    let text = String::from_utf8_lossy(&bytes);
    let now = Utc::now();
    let lifetime = text
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<LifetimeRecord>(line.strip_prefix(LIFETIME_MARKER)?).ok()
        })
        .rfind(|record| {
            record.run_id == run.run_id
                && record.worker_pid == pid
                && record.entered_at >= run.scheduled_at
                && record.entered_at <= now
                && match (
                    run.pid_start_time.as_deref(),
                    record.pid_start_time.as_deref(),
                ) {
                    (Some(expected), Some(observed)) => stable_tokens_match(expected, observed),
                    (None, _) => true,
                    _ => false,
                }
        })?;
    let parent = lifetime
        .parent_pid
        .map_or_else(|| "unknown".to_string(), |pid| pid.to_string());
    let mut details = format!(
        "worker pid {pid} entered at {}; parent pid {parent} (start identity {}); {}",
        lifetime.entered_at.to_rfc3339(),
        lifetime
            .parent_start_time
            .as_deref()
            .unwrap_or("unobserved"),
        lifetime.scope.as_ref().map_or_else(
            || "no worker scope at entry".to_string(),
            |scope| format!(
                "entry scope '{}' (oom_kills={:?}, tasks_refused={:?})",
                scope.unit, scope.oom_kills, scope.tasks_refused
            ),
        ),
    );
    if let Some(returned_at) = lifetime
        .returned_at
        .filter(|at| *at >= lifetime.entered_at && *at <= now)
    {
        details.push_str(&format!(
            "; worker {} at {returned_at} ({}ms after entry)",
            if lifetime.unwound {
                "unwound"
            } else {
                "returned"
            },
            (returned_at - lifetime.entered_at).num_milliseconds()
        ));
        if let Some(error) = lifetime.error {
            details.push_str(&format!(" with error: {}", bounded_provenance(&error)));
        }
        details.push_str("; application return is not an observed process exit");
    } else {
        details.push_str("; no return was recorded; exit time and status remain unobserved");
    }
    let reaped = text
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<WorkerExit>(line.strip_prefix(REAPED_MARKER)?).ok()
        })
        .rfind(|exit| {
            exit.worker_pid == pid
                && exit.observed_at >= lifetime.entered_at
                && exit.observed_at <= now
        });
    if let Some(signal) = text
        .lines()
        .filter_map(|line| line.strip_prefix(SIGNAL_MARKER))
        .rfind(|line| {
            line.split_whitespace().any(|field| {
                field
                    .strip_prefix("worker_pid=")
                    .and_then(|pid| pid.parse::<u32>().ok())
                    == Some(pid)
            })
        })
    {
        details.push_str(&format!(
            "; captured terminating signal:{}",
            bounded_provenance(signal)
        ));
    }
    Some(WorkerLogProvenance {
        description: details,
        reaped,
    })
}
