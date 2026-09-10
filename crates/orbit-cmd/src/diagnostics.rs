//! Read access to per-step diagnostics (`metrics/`, `friction/` JSONL streams).
//!
//! Append paths are owned by the engine; this module exposes the symmetric
//! reader so command-layer surfaces (CLI, dashboard) can show recent entries
//! without bypassing `OrbitRuntime`.
//!
//! Tolerant of malformed JSONL lines: bad lines are skipped with a tracing
//! warning rather than failing the whole month. The on-disk logs are
//! append-only and have historically picked up partial writes from crashes;
//! a dashboard that crashes on one bad line is worse than one that omits it.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::record::FrictionEntry;
use orbit_types::telemetry::MetricsEntry;
use serde::de::DeserializeOwned;

use orbit_core::OrbitRuntime;

/// Diagnostics-stream read surface for [`OrbitRuntime`] (extension trait —
/// the implementation moved out of orbit-core in [ORB-10016]).
pub trait DiagnosticsCommands {
    /// All `YYYY-MM` partitions that have a metrics log on disk, ascending.
    /// Used to enumerate the lifetime population without guessing a fixed
    /// number of trailing months.
    fn list_metrics_months(&self) -> Result<Vec<String>, OrbitError>;
    /// All metrics entries recorded in `year_month` (`YYYY-MM`).
    fn read_metrics_entries(&self, year_month: &str) -> Result<Vec<MetricsEntry>, OrbitError>;
    /// Most recent `limit` metrics entries recorded in `year_month`.
    fn read_metrics_entries_limited(
        &self,
        year_month: &str,
        limit: usize,
    ) -> Result<Vec<MetricsEntry>, OrbitError>;
    /// All friction entries recorded in `year_month` (`YYYY-MM`).
    fn read_friction_entries(&self, year_month: &str) -> Result<Vec<FrictionEntry>, OrbitError>;
    /// Most recent `limit` friction entries recorded in `year_month`.
    fn read_friction_entries_limited(
        &self,
        year_month: &str,
        limit: usize,
    ) -> Result<Vec<FrictionEntry>, OrbitError>;
}

impl DiagnosticsCommands for OrbitRuntime {
    fn list_metrics_months(&self) -> Result<Vec<String>, OrbitError> {
        list_jsonl_months(&self.data_root(), "metrics")
    }

    fn read_metrics_entries(&self, year_month: &str) -> Result<Vec<MetricsEntry>, OrbitError> {
        read_jsonl_month::<MetricsEntry>(&self.data_root(), "metrics", year_month)
    }

    fn read_metrics_entries_limited(
        &self,
        year_month: &str,
        limit: usize,
    ) -> Result<Vec<MetricsEntry>, OrbitError> {
        read_jsonl_month_limited::<MetricsEntry>(&self.data_root(), "metrics", year_month, limit)
    }

    fn read_friction_entries(&self, year_month: &str) -> Result<Vec<FrictionEntry>, OrbitError> {
        read_jsonl_month::<FrictionEntry>(&self.data_root(), "friction", year_month)
    }

    fn read_friction_entries_limited(
        &self,
        year_month: &str,
        limit: usize,
    ) -> Result<Vec<FrictionEntry>, OrbitError> {
        read_jsonl_month_limited::<FrictionEntry>(&self.data_root(), "friction", year_month, limit)
    }
}

/// Ascending list of `YYYY-MM` subdirectories under `state/diagnostics/<category>/`.
fn list_jsonl_months(root: &Path, category: &str) -> Result<Vec<String>, OrbitError> {
    let category_dir = validated_diagnostics_category_dir(root, category)?;
    if !category_dir.exists() {
        return Ok(Vec::new());
    }
    let mut months: Vec<String> = fs::read_dir(&category_dir)
        .map_err(|e| OrbitError::Io(e.to_string()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_year_month(name))
        .collect();
    months.sort();
    Ok(months)
}

/// `true` for a `YYYY-MM` directory name (e.g. `2026-03`).
fn is_year_month(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 7
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..].iter().all(u8::is_ascii_digit)
}

fn validated_diagnostics_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_root = root.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "resolve diagnostics root {}: {error}",
            root.display()
        ))
    })?;
    if !canonical_root.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "diagnostics root must be a directory: {}",
            canonical_root.display()
        )));
    }
    Ok(canonical_root)
}

/// Resolve a diagnostics category beneath the runtime's canonical data root.
/// Categories are an internal allow-list rather than path segments supplied by
/// callers, and the returned path is bounded to that root before any read.
fn validated_diagnostics_category_dir(root: &Path, category: &str) -> Result<PathBuf, OrbitError> {
    let canonical_root = validated_diagnostics_root(root)?;
    let category = validated_diagnostics_category_name(category)?;

    validate_diagnostics_path(
        &canonical_root,
        canonical_root
            .join("state")
            .join("diagnostics")
            .join(category),
    )
}

/// Resolve a diagnostics month after converting the accepted text into a
/// canonical value. The original caller-provided string never reaches a path
/// operation, so traversal and separator characters cannot affect the lookup.
fn validated_diagnostics_month_dir(
    root: &Path,
    category: &str,
    year_month: &str,
) -> Result<PathBuf, OrbitError> {
    let canonical_root = validated_diagnostics_root(root)?;
    let category = validated_diagnostics_category_name(category)?;
    let canonical_month = validated_year_month(year_month)?;
    let category_dir = canonical_root
        .join("state")
        .join("diagnostics")
        .join(category);

    validate_diagnostics_path(&canonical_root, category_dir.join(canonical_month))
}

fn validated_diagnostics_category_name(category: &str) -> Result<&'static str, OrbitError> {
    match category {
        "metrics" => Ok("metrics"),
        "friction" => Ok("friction"),
        _ => Err(OrbitError::InvalidInput(format!(
            "unsupported diagnostics category: {category}"
        ))),
    }
}

fn validated_year_month(year_month: &str) -> Result<String, OrbitError> {
    if !is_year_month(year_month) {
        return Err(OrbitError::InvalidInput(format!(
            "diagnostics month must use YYYY-MM format: {year_month}"
        )));
    }

    let year = year_month[..4].parse::<u16>().map_err(|_| {
        OrbitError::InvalidInput("diagnostics month contains an invalid year".to_string())
    })?;
    let month = year_month[5..].parse::<u8>().map_err(|_| {
        OrbitError::InvalidInput("diagnostics month contains an invalid month".to_string())
    })?;
    Ok(format!("{year:04}-{month:02}"))
}

fn validate_diagnostics_path(root: &Path, path: PathBuf) -> Result<PathBuf, OrbitError> {
    match path.canonicalize() {
        Ok(canonical) if canonical.starts_with(root) => Ok(canonical),
        Ok(canonical) => Err(OrbitError::InvalidInput(format!(
            "diagnostics path '{}' resolves outside {}",
            canonical.display(),
            root.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(error) => Err(OrbitError::Io(format!(
            "resolve diagnostics path {}: {error}",
            path.display()
        ))),
    }
}

fn read_jsonl_month<T: DeserializeOwned>(
    root: &Path,
    category: &str,
    year_month: &str,
) -> Result<Vec<T>, OrbitError> {
    let month_dir = validated_diagnostics_month_dir(root, category, year_month)?;
    if !month_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(&month_dir)
        .map_err(|e| OrbitError::Io(e.to_string()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|v| v.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    files.sort();

    let mut entries = Vec::new();
    for path in files {
        let raw = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        for (index, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_jsonl_values::<T>(line) {
                Ok(parsed) => entries.extend(parsed),
                Err(err) => {
                    tracing::warn!(
                        target: "orbit::diagnostics",
                        path = %path.display(),
                        line = index + 1,
                        error = %err,
                        "skipping malformed diagnostics line"
                    );
                }
            }
        }
    }
    Ok(entries)
}

fn read_jsonl_month_limited<T: DeserializeOwned>(
    root: &Path,
    category: &str,
    year_month: &str,
    limit: usize,
) -> Result<Vec<T>, OrbitError> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let month_dir = validated_diagnostics_month_dir(root, category, year_month)?;
    if !month_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(&month_dir)
        .map_err(|e| OrbitError::Io(e.to_string()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|v| v.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    files.sort();

    let mut entries = Vec::new();
    for path in files.into_iter().rev() {
        let raw = fs::read_to_string(&path).map_err(|e| OrbitError::Io(e.to_string()))?;
        let lines = raw.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_jsonl_values::<T>(line) {
                Ok(parsed) => {
                    for entry in parsed.into_iter().rev() {
                        entries.push(entry);
                        if entries.len() >= limit {
                            return Ok(entries);
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        target: "orbit::diagnostics",
                        path = %path.display(),
                        line = index + 1,
                        error = %err,
                        "skipping malformed diagnostics line"
                    );
                }
            }
        }
    }
    Ok(entries)
}

fn parse_jsonl_values<T: DeserializeOwned>(line: &str) -> Result<Vec<T>, serde_json::Error> {
    match serde_json::from_str::<T>(line) {
        Ok(entry) => Ok(vec![entry]),
        Err(single_value_error) => {
            let mut values = Vec::new();
            let stream = serde_json::Deserializer::from_str(line).into_iter::<T>();
            for result in stream {
                match result {
                    Ok(entry) => values.push(entry),
                    Err(_) => return Err(single_value_error),
                }
            }

            if values.len() > 1 {
                Ok(values)
            } else {
                Err(single_value_error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        is_year_month, list_jsonl_months, parse_jsonl_values, read_jsonl_month,
        validated_diagnostics_month_dir, validated_year_month,
    };

    #[test]
    fn parse_jsonl_values_recovers_concatenated_objects() {
        let values = parse_jsonl_values::<Value>(r#"{"step":"one"}{"step":"two"}"#).unwrap();

        assert_eq!(values, vec![json!({"step": "one"}), json!({"step": "two"})]);
    }

    #[test]
    fn parse_jsonl_values_rejects_trailing_garbage() {
        let err = parse_jsonl_values::<Value>(r#"{"step":"one"}oops"#).unwrap_err();

        assert!(err.to_string().contains("trailing characters"));
    }

    #[test]
    fn is_year_month_accepts_only_canonical_form() {
        assert!(is_year_month("2026-03"));
        assert!(!is_year_month("2026-3"));
        assert!(!is_year_month("26-03"));
        assert!(!is_year_month("2026/03"));
        assert!(!is_year_month(""));
    }

    #[test]
    fn validated_year_month_rebuilds_only_the_allowed_components() {
        assert_eq!(validated_year_month("2026-03").unwrap(), "2026-03");
        assert!(validated_year_month("2026/03").is_err());
        assert!(validated_year_month("../secrets").is_err());
    }

    #[test]
    fn validated_month_dir_rejects_unknown_categories() {
        let root = tempfile::tempdir().expect("tempdir");

        let error = validated_diagnostics_month_dir(root.path(), "secrets", "2026-03").unwrap_err();

        assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));
    }

    #[test]
    fn read_month_rejects_path_traversal() {
        let root = tempfile::tempdir().expect("tempdir");

        let error = read_jsonl_month::<Value>(root.path(), "metrics", "../secrets").unwrap_err();

        assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));
    }

    #[test]
    fn list_jsonl_months_returns_sorted_existing_partitions() {
        let root = tempfile::tempdir().expect("tempdir");
        let category_dir = root
            .path()
            .join("state")
            .join("diagnostics")
            .join("metrics");
        std::fs::create_dir_all(category_dir.join("2026-01")).unwrap();
        std::fs::create_dir_all(category_dir.join("2025-12")).unwrap();
        std::fs::write(category_dir.join("not-a-month.txt"), "ignored").unwrap();

        let months = list_jsonl_months(root.path(), "metrics").unwrap();

        assert_eq!(months, vec!["2025-12".to_string(), "2026-01".to_string()]);
    }

    #[test]
    fn list_jsonl_months_missing_category_dir_is_empty() {
        let root = tempfile::tempdir().expect("tempdir");

        let months = list_jsonl_months(root.path(), "metrics").unwrap();

        assert!(months.is_empty());
    }
}
