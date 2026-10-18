//! Forward-compatible diagnostics for settings this binary does not read.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::registry::{
    CONFIG_KEY_REGISTRY, CREW_CONFIG_FIELDS, DEPRECATED_CONFIG_KEYS, REMOVED_CONFIG_KEYS,
};

type WarnedPaths = HashSet<(PathBuf, Vec<String>)>;

/// An unrecognised key or table. Unknown tables are reported once, as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownConfigKey {
    /// TOML path, with literal dots in key names quoted.
    pub key: String,
    /// Closest recognised path, when its spelling is sufficiently similar.
    pub suggestion: Option<String>,
    pub(crate) segments: Vec<String>,
}

/// Unknown settings in a parsed config document, in path order.
///
/// Registered table-valued settings and plugin-owned sections are opaque.
/// Compatibility settings keep their existing migration diagnostics.
pub fn unknown_config_keys_present(document: &toml::Value) -> Vec<UnknownConfigKey> {
    let mut known: Vec<Vec<String>> = CONFIG_KEY_REGISTRY
        .iter()
        .map(|entry| entry.key)
        .chain(
            REMOVED_CONFIG_KEYS
                .iter()
                .chain(DEPRECATED_CONFIG_KEYS)
                .map(|(key, _)| *key),
        )
        .chain([
            "task.artifact_store",
            "knowledge.task_id_pattern",
            "runtime.backend",
            "agent",
            "watch",
            "docs",
            "duel",
            "routines",
            "plugins",
            "plugin_enablement",
            crate::TAG_INSTRUCTIONS_KEY,
        ])
        .map(|key| key.split('.').map(str::to_owned).collect())
        .collect();
    if let Some(crews) = document.get("crews").and_then(toml::Value::as_table) {
        for name in crews.keys() {
            for field in CREW_CONFIG_FIELDS.iter().copied().chain([
                "backend",
                "planner",
                "implementer",
                "reviewer",
            ]) {
                known.push(vec!["crews".into(), name.clone(), field.into()]);
            }
        }
    }
    let mut suggestions = BTreeSet::new();
    for path in &known {
        for end in 1..=path.len() {
            suggestions.insert(display_path(&path[..end]));
        }
    }
    let mut unknown = Vec::new();
    visit(
        document,
        &mut Vec::new(),
        &known,
        &suggestions,
        &mut unknown,
    );
    unknown
}

/// Unknown settings in `path`; missing and empty files have none.
/// Uses the config store's no-follow read checks.
pub fn unknown_config_keys_at(path: &Path) -> Result<Vec<UnknownConfigKey>, OrbitError> {
    let Some(raw) = crate::store::read_config_file(path)? else {
        return Ok(Vec::new());
    };
    let document = toml::from_str(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {error}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    Ok(unknown_config_keys_present(&document))
}

fn visit(
    value: &toml::Value,
    path: &mut Vec<String>,
    known: &[Vec<String>],
    suggestions: &BTreeSet<String>,
    unknown: &mut Vec<UnknownConfigKey>,
) {
    if !path.is_empty() {
        if known.contains(path)
            || (path.len() == 1
                && path[0] == "crews"
                && value.as_table().is_some_and(|table| table.is_empty()))
        {
            return;
        }
        if !known.iter().any(|candidate| candidate.starts_with(path)) {
            let key = display_path(path);
            let suggestion = suggestions
                .iter()
                .map(|candidate| (distance(&key, candidate), candidate))
                .min()
                .filter(|(distance, candidate)| {
                    *distance
                        <= key
                            .chars()
                            .count()
                            .max(candidate.chars().count())
                            .div_ceil(3)
                })
                .map(|(_, candidate)| candidate.clone());
            unknown.push(UnknownConfigKey {
                key,
                suggestion,
                segments: path.clone(),
            });
            return;
        }
    }
    if let Some(table) = value.as_table() {
        for (key, value) in table {
            path.push(key.clone());
            visit(value, path, known, suggestions, unknown);
            path.pop();
        }
    }
}

fn display_path(segments: &[String]) -> String {
    segments
        .iter()
        .map(|segment| {
            if !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                segment.clone()
            } else {
                toml::Value::String(segment.clone()).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (index, left) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (column, right) in right.iter().enumerate() {
            let previous = row[column + 1];
            row[column + 1] = (diagonal + usize::from(left != *right))
                .min(previous + 1)
                .min(row[column] + 1);
            diagonal = previous;
        }
    }
    row[right.len()]
}

pub(crate) fn warn_unknown_keys(document: &toml::Value, path: &Path) {
    // Do not consume a warning before the caller has installed its subscriber.
    if !tracing::enabled!(tracing::Level::WARN) {
        return;
    }
    static WARNED: OnceLock<Mutex<WarnedPaths>> = OnceLock::new();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    for unknown in unknown_config_keys_present(document) {
        let first = match WARNED.get_or_init(Mutex::default).lock() {
            Ok(mut warned) => warned.insert((canonical.clone(), unknown.segments)),
            Err(_) => false,
        };
        if first {
            let config = redact_home_dir(&path.display().to_string());
            if let Some(suggestion) = unknown.suggestion {
                tracing::warn!(%config, key = %unknown.key, %suggestion, "unknown config key or table is ignored; did you mean the suggested key?");
            } else {
                tracing::warn!(%config, key = %unknown.key, "unknown config key or table is ignored");
            }
        }
    }
}
