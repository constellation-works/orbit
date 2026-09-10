//! Shared on-disk primitives for the `.orbit/state/scoreboard/` files.
//!
//! Per-model counter maps (`pr.json`, `task_review.json`) share a
//!   `{ "<metric>": { "<model>": <count> } }` map incremented under the same
//! lock. See [`increment_model_metric`].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::identity::normalize_attribution_label;

use orbit_common::fs::io::{atomic_write_text_volatile as write_atomic, with_exclusive_file_lock};

/// Per-model counters for one metric.
pub(crate) type ModelScores = HashMap<String, u64>;
/// Metric name → per-model counters — the shape of `pr.json` and
/// `task_review.json`.
pub(crate) type CounterScoreboard = HashMap<String, ModelScores>;

fn validated_scoreboard_file_name(file_name: &str) -> Result<&'static str, OrbitError> {
    match file_name {
        "pr.json" => Ok("pr.json"),
        "task_review.json" => Ok("task_review.json"),
        _ => Err(OrbitError::InvalidInput(format!(
            "unsupported scoreboard file: {file_name}"
        ))),
    }
}

/// Resolve a scoreboard file beneath its canonical directory.
///
/// Scoreboard filenames are an internal allow-list, not caller-selected
/// paths. Canonicalizing the directory and rejecting a symlinked or non-file
/// target keeps the read-modify-write cycle within the intended scoreboard
/// directory before any filesystem access uses the result.
fn validated_scoreboard_file_path(
    scoreboard_dir: &Path,
    file_name: &str,
) -> Result<PathBuf, OrbitError> {
    let file_name = validated_scoreboard_file_name(file_name)?;

    let canonical_dir = fs::canonicalize(scoreboard_dir)
        .map_err(|error| OrbitError::Io(format!("canonicalize scoreboard directory: {error}")))?;
    if !canonical_dir.is_dir() {
        return Err(OrbitError::InvalidInput(
            "scoreboard path must be a directory".to_string(),
        ));
    }

    let path = canonical_dir.join(file_name);
    if !path.starts_with(&canonical_dir) {
        return Err(OrbitError::InvalidInput(
            "scoreboard file must remain within its directory".to_string(),
        ));
    }

    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(OrbitError::InvalidInput(
            "scoreboard file must not be a symlink".to_string(),
        )),
        Ok(metadata) if !metadata.is_file() => Err(OrbitError::InvalidInput(
            "scoreboard file must be a regular file".to_string(),
        )),
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(OrbitError::Io(format!("inspect scoreboard file: {error}"))),
    }
}

/// Increment `metric` for the (normalized) `model` in
/// `scoreboard_dir/<file_name>`, creating the file on first use. `migrate`
/// runs on the freshly-loaded scoreboard before the increment so callers can
/// fold legacy metric keys into their canonical form.
pub(crate) fn increment_model_metric(
    scoreboard_dir: &Path,
    file_name: &str,
    lock_label: &str,
    metric: &str,
    model: &str,
    migrate: impl FnOnce(&mut CounterScoreboard),
) -> Result<(), OrbitError> {
    let file_name = validated_scoreboard_file_name(file_name)?;
    let lock_path = scoreboard_dir.join(file_name);
    let normalized_model = normalize_attribution_label(model, None);
    with_exclusive_file_lock(&lock_path, lock_label, || {
        let path = validated_scoreboard_file_path(scoreboard_dir, file_name)?;
        let mut scoreboard: CounterScoreboard = if path.exists() {
            let content = fs::read_to_string(&path)
                .map_err(|e| OrbitError::Io(format!("read {file_name}: {e}")))?;
            serde_json::from_str(&content)
                .map_err(|e| OrbitError::Io(format!("parse {file_name}: {e}")))?
        } else {
            HashMap::new()
        };

        migrate(&mut scoreboard);

        let model_map = scoreboard.entry(metric.to_string()).or_default();
        let counter = model_map.entry(normalized_model.clone()).or_insert(0);
        *counter += 1;

        let json = serde_json::to_string_pretty(&scoreboard)
            .map_err(|e| OrbitError::Io(format!("serialize {file_name}: {e}")))?;
        write_atomic(&path, &format!("{json}\n")).map_err(Into::into)
    })
}
