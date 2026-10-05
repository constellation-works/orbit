//! Shared on-disk primitives for the `.orbit/state/scoreboard/` files.
//!
//! Per-model counter maps (`pr.json`, `task_review.json`) share a
//!   `{ "<metric>": { "<model>": <count> } }` map incremented under the same
//! lock. See [`increment_model_metric`].
//!
//! Every snapshot read goes through [`read_scoreboard_file`], which reads an
//! already-validated path through a descriptor opened without following its
//! final component, so a symlink or FIFO swapped in after validation is
//! refused instead of read or waited on.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::identity::normalize_attribution_label;

use orbit_common::fs::io::{atomic_write_text_volatile as write_atomic, with_exclusive_file_lock};
use orbit_common::fs::open_read_only_no_follow;

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

/// Read a validated scoreboard file through a descriptor opened without
/// following its final component.
///
/// `path` has already been checked by pathname; `before_open` runs between
/// that check and the open so tests can swap the file. The open refuses a
/// symlinked final component, is nonblocking on Unix so a FIFO cannot stall
/// it, and the opened descriptor must be a regular file before any byte is
/// read. `Ok(None)` means the file does not exist.
pub(crate) fn read_scoreboard_file(
    path: &Path,
    before_open: impl FnOnce(&Path) -> Result<(), OrbitError>,
) -> Result<Option<String>, OrbitError> {
    before_open(path)?;

    let mut file = match open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_open_refusal(&error) => {
            return Err(symlinked_scoreboard_file(path));
        }
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(OrbitError::InvalidInput(format!(
                "scoreboard file rejected {}: {error}",
                path.display()
            )));
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "open scoreboard file {}: {error}",
                path.display()
            )));
        }
    };

    let metadata = file.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "inspect scoreboard file {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "scoreboard file must be a regular file: {}",
            path.display()
        )));
    }

    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|error| {
        OrbitError::Io(format!("read scoreboard file {}: {error}", path.display()))
    })?;
    Ok(Some(raw))
}

fn symlinked_scoreboard_file(path: &Path) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "scoreboard file must not be a symlink: {}",
        path.display()
    ))
}

/// `O_NOFOLLOW` reports a symlinked final component as `ELOOP`, which has no
/// stable [`std::io::ErrorKind`] to match on.
#[cfg(unix)]
fn is_symlink_open_refusal(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

/// Elsewhere a symlinked final component is caught by the descriptor's own
/// file-type check rather than by an open-time refusal.
#[cfg(not(unix))]
fn is_symlink_open_refusal(_error: &std::io::Error) -> bool {
    false
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
    increment_model_metric_with_hook(
        scoreboard_dir,
        file_name,
        lock_label,
        metric,
        model,
        migrate,
        |_| Ok(()),
    )
}

/// Increment through a descriptor read, letting a test perturb the file
/// between the pathname check and the open.
#[cfg(test)]
pub(crate) fn increment_model_metric_after_check(
    scoreboard_dir: &Path,
    file_name: &str,
    metric: &str,
    model: &str,
    before_open: impl FnOnce(&Path) -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    increment_model_metric_with_hook(
        scoreboard_dir,
        file_name,
        "test scoreboard",
        metric,
        model,
        |_| {},
        before_open,
    )
}

fn increment_model_metric_with_hook(
    scoreboard_dir: &Path,
    file_name: &str,
    lock_label: &str,
    metric: &str,
    model: &str,
    migrate: impl FnOnce(&mut CounterScoreboard),
    before_open: impl FnOnce(&Path) -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    let file_name = validated_scoreboard_file_name(file_name)?;
    let lock_path = scoreboard_dir.join(file_name);
    let normalized_model = normalize_attribution_label(model, None);
    with_exclusive_file_lock(&lock_path, lock_label, || {
        let path = validated_scoreboard_file_path(scoreboard_dir, file_name)?;
        let mut scoreboard: CounterScoreboard = match read_scoreboard_file(&path, before_open)? {
            Some(content) => serde_json::from_str(&content)
                .map_err(|e| OrbitError::Io(format!("parse {file_name}: {e}")))?,
            None => HashMap::new(),
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
