//! How many consecutive sweeps each run-scoped retryable error has survived.
//!
//! A retryable error exists so a later sweep can finish what this one could
//! not. When the same gap — one operation on one run's job — is still there
//! sweep after sweep, retrying is not going to close it, and failing every
//! sweep over it hides the failures that matter. After
//! [`PERSISTENT_AFTER_SWEEPS`] consecutive sightings the error is reported as
//! a persistent note instead, and the incomplete finding it belongs to leaves
//! the failure lists. An error that names no run (a repository, listing or
//! origin read) never degrades: losing that read means the sweep did not look.
//!
//! The counts live in one small file under the workspace data root. Each
//! distinct sweep is one sighting; an error absent from a sweep starts over.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Consecutive sweeps after which a run-scoped retryable error stops failing
/// the sweep.
pub(super) const PERSISTENT_AFTER_SWEEPS: u64 = 3;

/// Consecutive-sighting counts keyed by `run_id/job_id/operation`.
#[derive(Debug, Default)]
pub(super) struct RetryableHistory {
    counts: BTreeMap<String, SweepCount>,
}

#[derive(Debug, Clone)]
struct SweepCount {
    consecutive_sweeps: u64,
    last_sweep_id: String,
}

/// One collection's errors after [`RetryableHistory::observe`].
pub(super) struct Observed {
    pub(super) retryable: Vec<Value>,
    pub(super) persistent: Vec<Value>,
}

impl RetryableHistory {
    /// Record this sweep's errors, replacing the previous sightings.
    /// Repeated activity calls for one job run count once; only errors seen in
    /// distinct consecutive sweeps reach [`PERSISTENT_AFTER_SWEEPS`].
    pub(super) fn observe(&mut self, errors: Vec<Value>, sweep_id: &str) -> Observed {
        let previous = std::mem::take(&mut self.counts);
        let mut seen = BTreeSet::new();
        let mut observed = Observed {
            retryable: Vec::new(),
            persistent: Vec::new(),
        };
        for mut error in errors {
            let Some(key) = error_key(&error) else {
                observed.retryable.push(error);
                continue;
            };
            let prior = previous.get(&key);
            let sweeps = match prior {
                Some(count) if count.last_sweep_id == sweep_id => count.consecutive_sweeps,
                Some(count) => count.consecutive_sweeps + 1,
                None => 1,
            };
            if seen.insert(key.clone()) {
                self.counts.insert(
                    key,
                    SweepCount {
                        consecutive_sweeps: sweeps,
                        last_sweep_id: sweep_id.to_string(),
                    },
                );
            }
            if sweeps < PERSISTENT_AFTER_SWEEPS {
                observed.retryable.push(error);
                continue;
            }
            error["retryable"] = json!(false);
            error["persistent"] = json!(true);
            error["consecutive_sweeps"] = json!(sweeps);
            observed.persistent.push(error);
        }
        observed
    }

    fn from_json(value: &Value) -> Self {
        let counts = value
            .get("counts")
            .and_then(Value::as_object)
            .map(|counts| {
                counts
                    .iter()
                    .filter_map(|(key, count)| {
                        Some((
                            key.clone(),
                            SweepCount {
                                consecutive_sweeps: count.get("consecutive_sweeps")?.as_u64()?,
                                last_sweep_id: count.get("last_sweep_id")?.as_str()?.to_string(),
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { counts }
    }

    fn to_json(&self) -> Value {
        let counts = self
            .counts
            .iter()
            .map(|(key, count)| {
                (
                    key.clone(),
                    json!({
                        "consecutive_sweeps": count.consecutive_sweeps,
                        "last_sweep_id": count.last_sweep_id,
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        json!({"schema_version": 2, "counts": counts})
    }
}

/// The run, job and operation an error is about, or `None` for one that
/// names no run.
fn error_key(error: &Value) -> Option<String> {
    let run_id = error.get("run_id").filter(|value| !value.is_null())?;
    let job_id = error
        .get("job_id")
        .filter(|value| !value.is_null())
        .map_or_else(|| "-".to_string(), Value::to_string);
    let operation = error
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(format!("{run_id}/{job_id}/{operation}"))
}

/// Where the counts persist, or `None` when the host has no data root.
pub(super) fn history_path(data_root: &Path) -> Option<PathBuf> {
    (!data_root.as_os_str().is_empty()).then(|| {
        data_root
            .join("state")
            .join("ci_failure_sweep")
            .join("retryable_history.json")
    })
}

/// A missing or unreadable file is an empty history: the worst case is that
/// a persistent error fails a few more sweeps before it degrades again.
pub(super) fn load(path: &Path) -> RetryableHistory {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => RetryableHistory::from_json(&value),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring unreadable CI sweep retryable history");
                RetryableHistory::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RetryableHistory::default(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring unreadable CI sweep retryable history");
            RetryableHistory::default()
        }
    }
}

pub(super) fn save(path: &Path, history: &RetryableHistory) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        orbit_common::fs::io::create_private_dir_all(parent)?;
    }
    orbit_common::fs::io::atomic_write_text(path, &history.to_json().to_string())
}
