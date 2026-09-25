//! Production activity-catalog validation and the opt-in retired-backend
//! repair used by `orbit doctor` [ORB-10838].
//!
//! Definition-artifact health already parsed each managed activity file with
//! `load_activity_asset`, but that walk never saw workspace-local files and
//! never ran the registry tool-allowlist check. Catalog construction does
//! both, so a workspace `spec.backend: http` (or a removed tool name) could
//! fail every job run while doctor reported the activity catalog healthy.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_catalog_asset, validate_catalog_activity_tools};

use crate::OrbitRuntime;

/// The single opt-in repair command named by retired-backend findings.
pub const FIX_RETIRED_ACTIVITY_BACKENDS_CMD: &str = "orbit doctor --fix-retired-activity-backends";

/// Known retired `spec.backend` values that this repair may delete.
const REPAIRABLE_BACKENDS: &[&str] = &["http", "auto"];

/// Outcome of one `--fix-retired-activity-backends` pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetiredActivityBackendRepair {
    pub repaired: Vec<PathBuf>,
    pub skipped: Vec<RetiredActivityBackendSkip>,
}

/// A catalog activity the repair refused to rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredActivityBackendSkip {
    pub path: PathBuf,
    pub reason: String,
}

/// One catalog-invalid activity file, ready to become an [`super::artifact::ArtifactFinding`].
#[derive(Debug, Clone)]
pub(crate) struct ActivityCatalogFault {
    pub name: String,
    pub path: PathBuf,
    pub detail: String,
    /// When set, this is a known retired backend and names the repair command.
    pub repair_command: Option<&'static str>,
}

/// Inspect every production catalog directory with the same load + tool
/// validation catalog construction uses. Missing directories are skipped.
pub(crate) fn collect_activity_catalog_faults(
    runtime: &OrbitRuntime,
) -> (usize, Vec<ActivityCatalogFault>) {
    let registered_tools = runtime.allowlist_known_tool_names();
    let registered: Vec<&str> = registered_tools.iter().map(String::as_str).collect();
    let paths = activity_catalog_yaml_files(runtime);
    let scanned = paths.len();
    let mut faults = Vec::new();
    for path in paths {
        let name = file_stem_name(&path);
        let Some(raw) = read_text(&path) else {
            faults.push(ActivityCatalogFault {
                name,
                path,
                detail: "file is unreadable".to_string(),
                repair_command: None,
            });
            continue;
        };
        match inspect_activity_file(&path, &raw, &registered) {
            ActivityInspection::Healthy | ActivityInspection::RetiredSkipped => {}
            ActivityInspection::Fault(fault) => faults.push(fault),
        }
    }
    (scanned, faults)
}

/// Remove `spec.backend` only when it is a known retired value on a
/// schemaVersion 2 agent-loop activity. Everything else is left untouched
/// and, when it would fail catalog construction, reported for a manual edit.
pub(crate) fn repair_retired_activity_backends(
    runtime: &OrbitRuntime,
) -> Result<RetiredActivityBackendRepair, OrbitError> {
    let registered_tools = runtime.allowlist_known_tool_names();
    let registered: Vec<&str> = registered_tools.iter().map(String::as_str).collect();
    let mut report = RetiredActivityBackendRepair::default();
    for path in activity_catalog_yaml_files(runtime) {
        let Some(raw) = read_text(&path) else {
            report.skipped.push(RetiredActivityBackendSkip {
                path,
                reason: "file is unreadable".to_string(),
            });
            continue;
        };
        match classify_backend_repair(&raw) {
            BackendRepairClass::AlreadyClean => {}
            BackendRepairClass::Repairable { value } => match remove_spec_backend_key(&raw, &value)
            {
                Ok(next) => {
                    std::fs::write(&path, next).map_err(|error| {
                        OrbitError::Io(format!(
                            "remove retired spec.backend from {}: {error}",
                            path.display()
                        ))
                    })?;
                    report.repaired.push(path);
                }
                Err(reason) => report
                    .skipped
                    .push(RetiredActivityBackendSkip { path, reason }),
            },
            BackendRepairClass::UnknownBackend { value } => {
                report.skipped.push(RetiredActivityBackendSkip {
                    path,
                    reason: format!(
                        "unknown spec.backend value `{value}`; remove or replace it manually"
                    ),
                });
            }
            BackendRepairClass::UnrelatedMalformed => {
                if !matches!(
                    inspect_activity_file(&path, &raw, &registered),
                    ActivityInspection::Healthy | ActivityInspection::RetiredSkipped
                ) {
                    report.skipped.push(RetiredActivityBackendSkip {
                        path,
                        reason: "unrelated malformed activity; repair the definition manually"
                            .to_string(),
                    });
                }
            }
        }
    }
    Ok(report)
}

#[derive(Debug)]
enum ActivityInspection {
    Healthy,
    RetiredSkipped,
    Fault(ActivityCatalogFault),
}

fn inspect_activity_file(path: &Path, raw: &str, registered_tools: &[&str]) -> ActivityInspection {
    match load_activity_catalog_asset(path, raw, true) {
        Ok(None) => ActivityInspection::RetiredSkipped,
        Ok(Some(asset)) => {
            if let Err(error) = validate_catalog_activity_tools(
                &asset.name,
                &asset.spec,
                registered_tools.iter().copied(),
            ) {
                return ActivityInspection::Fault(ActivityCatalogFault {
                    name: asset.name,
                    path: path.to_path_buf(),
                    detail: format!("`{}` — {error}", path.display()),
                    repair_command: None,
                });
            }
            ActivityInspection::Healthy
        }
        Err(error) => {
            let backend = spec_backend_value(raw);
            let (detail, repair_command) = match backend.as_deref() {
                Some(value) => (
                    format!("`{}` spec.backend: {value} — {error}", path.display()),
                    repairable_backend(value).then_some(FIX_RETIRED_ACTIVITY_BACKENDS_CMD),
                ),
                None => (format!("`{}` — {error}", path.display()), None),
            };
            ActivityInspection::Fault(ActivityCatalogFault {
                name: file_stem_name(path),
                path: path.to_path_buf(),
                detail,
                repair_command,
            })
        }
    }
}

#[derive(Debug)]
enum BackendRepairClass {
    AlreadyClean,
    Repairable { value: String },
    UnknownBackend { value: String },
    UnrelatedMalformed,
}

fn classify_backend_repair(raw: &str) -> BackendRepairClass {
    let Some(document) = parse_mapping(raw) else {
        return BackendRepairClass::UnrelatedMalformed;
    };
    if !is_schema_v2_activity(&document) {
        return BackendRepairClass::UnrelatedMalformed;
    }
    let Some(spec) = mapping_get(&document, "spec") else {
        return BackendRepairClass::UnrelatedMalformed;
    };
    if mapping_str(spec, "type") != Some("agent_loop") {
        return BackendRepairClass::UnrelatedMalformed;
    }
    match mapping_scalar(spec, "backend") {
        None => BackendRepairClass::AlreadyClean,
        Some(value) if repairable_backend(&value) => BackendRepairClass::Repairable { value },
        Some(value) if value == "cli" => BackendRepairClass::AlreadyClean,
        Some(value) => BackendRepairClass::UnknownBackend { value },
    }
}

fn spec_backend_value(raw: &str) -> Option<String> {
    let document = parse_mapping(raw)?;
    mapping_scalar(mapping_get(&document, "spec")?, "backend")
}

fn is_schema_v2_activity(document: &serde_yaml::Value) -> bool {
    let version = mapping_get(document, "schemaVersion")
        .and_then(serde_yaml::Value::as_u64)
        .or_else(|| {
            mapping_get(document, "schemaVersion")
                .and_then(serde_yaml::Value::as_i64)
                .and_then(|value| u64::try_from(value).ok())
        });
    version == Some(2) && mapping_str(document, "kind") == Some("Activity")
}

fn repairable_backend(value: &str) -> bool {
    REPAIRABLE_BACKENDS.contains(&value)
}

/// Delete the unique block-style `spec.backend` line whose value is `backend`.
/// Refuses flow-style mappings and ambiguous duplicates so unrelated YAML
/// is never rewritten.
pub(crate) fn remove_spec_backend_key(raw: &str, backend: &str) -> Result<String, String> {
    let had_trailing_newline = raw.ends_with('\n');
    let lines: Vec<&str> = raw.lines().collect();
    let spec_idx = find_block_key(&lines, 0, 0, "spec")
        .ok_or_else(|| "could not find a block-style `spec:` mapping to edit".to_string())?;
    let spec_indent = leading_spaces(lines[spec_idx]);
    let child_indent = first_child_indent(&lines, spec_idx, spec_indent).ok_or_else(|| {
        "could not find block-style keys under `spec:`; flow-style mappings are left untouched"
            .to_string()
    })?;
    let mut matches = Vec::new();
    let mut idx = spec_idx + 1;
    while idx < lines.len() {
        let line = lines[idx];
        let indent = leading_spaces(line);
        if !is_blank_or_comment(line) && indent <= spec_indent {
            break;
        }
        if indent == child_indent && is_key_with_scalar(line, indent, "backend", backend) {
            matches.push(idx);
        }
        idx += 1;
    }
    match matches.as_slice() {
        [only] => {
            let mut next: Vec<&str> = Vec::with_capacity(lines.len() - 1);
            next.extend(lines.iter().take(*only).copied());
            next.extend(lines.iter().skip(*only + 1).copied());
            let mut rendered = next.join("\n");
            if had_trailing_newline {
                rendered.push('\n');
            }
            Ok(rendered)
        }
        [] => Err(format!(
            "could not uniquely locate block-style `spec.backend: {backend}`"
        )),
        _ => Err("multiple `spec.backend` lines; refusing to guess which to remove".to_string()),
    }
}

fn find_block_key(lines: &[&str], start: usize, indent: usize, key: &str) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(idx, line)| {
            (leading_spaces(line) == indent && is_bare_block_key(line, indent, key)).then_some(idx)
        })
}

fn first_child_indent(lines: &[&str], parent_idx: usize, parent_indent: usize) -> Option<usize> {
    for line in lines.iter().skip(parent_idx + 1) {
        if is_blank_or_comment(line) {
            continue;
        }
        let indent = leading_spaces(line);
        if indent <= parent_indent {
            return None;
        }
        return Some(indent);
    }
    None
}

fn is_bare_block_key(line: &str, indent: usize, key: &str) -> bool {
    let rest = line.get(indent..).unwrap_or("");
    let Some(after) = rest
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix(':'))
    else {
        return false;
    };
    after.trim().is_empty() || after.trim_start().starts_with('#')
}

fn is_key_with_scalar(line: &str, indent: usize, key: &str, expected: &str) -> bool {
    let rest = line.get(indent..).unwrap_or("");
    let Some(after) = rest
        .strip_prefix(key)
        .and_then(|value| value.strip_prefix(':'))
    else {
        return false;
    };
    let value = after.split('#').next().unwrap_or(after).trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        })
        .unwrap_or(value);
    unquoted == expected
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|ch| *ch == ' ').count()
}

fn is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

fn activity_catalog_yaml_files(runtime: &OrbitRuntime) -> Vec<PathBuf> {
    let mut files = BTreeSet::new();
    for dir in runtime.v2_activity_catalog_paths() {
        if dir.is_dir() {
            collect_yaml_files(&dir, &mut files);
        }
    }
    files.into_iter().collect()
}

fn collect_yaml_files(dir: &Path, files: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_yaml_files(&path, files);
            continue;
        }
        let is_yaml = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
            });
        if is_yaml {
            files.insert(path);
        }
    }
}

fn file_stem_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn parse_mapping(raw: &str) -> Option<serde_yaml::Value> {
    let value: serde_yaml::Value = serde_yaml::from_str(raw).ok()?;
    value.as_mapping()?;
    Some(value)
}

fn mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_string()))
}

fn mapping_str<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a str> {
    mapping_get(value, key).and_then(serde_yaml::Value::as_str)
}

fn mapping_scalar(value: &serde_yaml::Value, key: &str) -> Option<String> {
    let field = mapping_get(value, key)?;
    if let Some(text) = field.as_str() {
        return Some(text.to_string());
    }
    if let Some(boolean) = field.as_bool() {
        return Some(boolean.to_string());
    }
    if let Some(number) = field.as_i64() {
        return Some(number.to_string());
    }
    None
}
