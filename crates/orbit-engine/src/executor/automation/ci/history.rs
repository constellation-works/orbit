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
//! A landing-branch failure is also counted by identity — branch, workflow
//! and job name — because a red integration branch gets a new run on every
//! push, and a gap keyed by run would start over each time. Collection uses
//! that streak to escalate a landing failure that stays unfileable instead of
//! deferring it forever.
//!
//! The counts live in one small file under the workspace data root. Each
//! distinct sweep is one sighting; an error absent from a sweep starts over.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Consecutive sweeps after which a run-scoped retryable error stops failing
/// the sweep.
pub(super) const PERSISTENT_AFTER_SWEEPS: u64 = 3;

/// Written on every save. Load does not check it: a different number, or a
/// value that is not a number, still restores the streaks.
const HISTORY_SCHEMA_VERSION: u64 = 2;

/// Consecutive-sighting counts keyed by `run_id/job_id/operation`, and
/// unfileable landing-failure streaks keyed by failure identity.
#[derive(Debug, Default)]
pub(super) struct RetryableHistory {
    counts: BTreeMap<String, SweepCount>,
    landing_gaps: BTreeMap<String, SweepCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SweepCount {
    consecutive_sweeps: u64,
    last_sweep_id: String,
}

/// Persisted `retryable_history.json`. Parsed once in [`load`] and written
/// whole in [`save`]. Map values that are not object-shaped [`SweepCount`]s
/// are dropped on their own; the rest of the file still loads.
#[derive(Debug, Serialize, Deserialize)]
struct RetryableHistoryFile {
    #[serde(skip_deserializing, default = "history_schema_version")]
    schema_version: u64,
    #[serde(default, deserialize_with = "lenient_counts")]
    counts: BTreeMap<String, SweepCount>,
    #[serde(default, deserialize_with = "lenient_counts")]
    landing_gaps: BTreeMap<String, SweepCount>,
}

fn history_schema_version() -> u64 {
    HISTORY_SCHEMA_VERSION
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
            let sweeps = next_count(previous.get(&key), sweep_id);
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

    /// Record which landing-failure identities this sweep could not file and
    /// return how many consecutive sweeps each has stayed that way. An
    /// identity absent from this sweep starts over.
    pub(super) fn observe_landing_gaps(
        &mut self,
        identities: &BTreeSet<String>,
        sweep_id: &str,
    ) -> BTreeMap<String, u64> {
        let previous = std::mem::take(&mut self.landing_gaps);
        identities
            .iter()
            .map(|identity| {
                let sweeps = next_count(previous.get(identity), sweep_id);
                self.landing_gaps.insert(
                    identity.clone(),
                    SweepCount {
                        consecutive_sweeps: sweeps,
                        last_sweep_id: sweep_id.to_string(),
                    },
                );
                (identity.clone(), sweeps)
            })
            .collect()
    }
}

impl From<RetryableHistoryFile> for RetryableHistory {
    fn from(file: RetryableHistoryFile) -> Self {
        Self {
            counts: file.counts,
            landing_gaps: file.landing_gaps,
        }
    }
}

impl From<&RetryableHistory> for RetryableHistoryFile {
    fn from(history: &RetryableHistory) -> Self {
        Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            counts: history.counts.clone(),
            landing_gaps: history.landing_gaps.clone(),
        }
    }
}

/// Repeated activity calls for one sweep count once.
fn next_count(prior: Option<&SweepCount>, sweep_id: &str) -> u64 {
    match prior {
        Some(count) if count.last_sweep_id == sweep_id => count.consecutive_sweeps,
        Some(count) => count.consecutive_sweeps + 1,
        None => 1,
    }
}

/// A count map, or anything else (which loads as no counts). One entry that
/// is not an object-shaped [`SweepCount`] is skipped; its neighbours stay.
#[derive(Deserialize)]
#[serde(untagged)]
enum LenientCounts {
    Map(BTreeMap<String, Value>),
    Other(IgnoredAny),
}

fn lenient_counts<'de, D>(deserializer: D) -> Result<BTreeMap<String, SweepCount>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match LenientCounts::deserialize(deserializer)? {
        LenientCounts::Map(entries) => entries
            .into_iter()
            .filter_map(|(key, entry)| {
                entry.is_object().then_some(entry).and_then(|entry| {
                    serde_json::from_value::<SweepCount>(entry)
                        .ok()
                        .map(|count| (key, count))
                })
            })
            .collect(),
        LenientCounts::Other(_) => BTreeMap::new(),
    })
}

/// Fields [`error_key`] reads. `run_id` and `job_id` stay in JSON display
/// form so a string id remains quoted and a number stays bare.
#[derive(Deserialize)]
struct ErrorKeyFields {
    #[serde(default)]
    run_id: Option<JsonRendered>,
    #[serde(default)]
    job_id: Option<JsonRendered>,
    #[serde(default)]
    operation: LenientText,
}

/// `Value`'s display text. Existing streak keys were built from that display.
struct JsonRendered(String);

impl JsonRendered {
    fn raw(text: &str) -> Self {
        Self(text.to_string())
    }

    fn text(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for JsonRendered {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Ok(Self(value.to_string()))
    }
}

/// A string operation, or any other JSON value, which counts as no operation.
#[derive(Deserialize)]
#[serde(untagged)]
enum LenientText {
    Text(String),
    Other(IgnoredAny),
}

impl Default for LenientText {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

/// The run, job and operation an error is about, or `None` for one that
/// names no run.
fn error_key(error: &Value) -> Option<String> {
    let fields = ErrorKeyFields::deserialize(error).ok()?;
    let run_id = fields.run_id?;
    let job_id = match fields.job_id {
        Some(job_id) => job_id,
        None => JsonRendered::raw("-"),
    };
    let operation = match fields.operation {
        LenientText::Text(text) => text,
        LenientText::Other(_) => String::new(),
    };
    Some(format!("{}/{}/{}", run_id.text(), job_id.text(), operation))
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
            Ok(value) if value.is_object() => {
                match serde_json::from_value::<RetryableHistoryFile>(value) {
                    Ok(file) => RetryableHistory::from(file),
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "ignoring unreadable CI sweep retryable history");
                        RetryableHistory::default()
                    }
                }
            }
            Ok(_) => RetryableHistory::default(),
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
    let text = serde_json::to_string(&RetryableHistoryFile::from(history))
        .map_err(std::io::Error::other)?;
    orbit_common::fs::io::atomic_write_text(path, &text)
}
