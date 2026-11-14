//! Production activity-catalog validation used by `orbit doctor`.
//!
//! Definition-artifact health already parsed each managed activity file with
//! `load_activity_asset`, but that walk never saw workspace-local files and
//! never ran the registry tool-allowlist check. Catalog construction does
//! both, so a workspace `spec.backend: http` (or a removed tool name) could
//! fail every job run while doctor reported the activity catalog healthy.
//! Names are also checked within each root: duplicates block loading even
//! when another root shadows them, while cross-root overrides remain legal.
//!
//! Discovery is confined to each configured catalog root: links below a root
//! are reported rather than followed, and only regular files are read, so a
//! linked or special entry cannot redirect or stall the scan. Traversal failures
//! report the affected path rather than treating an unavailable inspection as healthy.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::fs::open_read_only_no_follow;
use orbit_engine::activity_job::{
    CatalogError, load_activity_catalog_asset, validate_catalog_activity_tools,
};

use crate::OrbitRuntime;

/// One catalog-invalid activity file, ready to become an [`super::artifact::ArtifactFinding`].
#[derive(Debug, Clone)]
pub(crate) struct ActivityCatalogFault {
    pub name: String,
    pub path: PathBuf,
    pub detail: String,
}

/// Inspect every production catalog directory with the same load + tool
/// validation and per-root duplicate-name rules catalog construction uses.
/// Missing directories and retired definitions are skipped.
pub(crate) fn collect_activity_catalog_faults(
    runtime: &OrbitRuntime,
) -> (usize, Vec<ActivityCatalogFault>) {
    let registered_tools = runtime.allowlist_known_tool_names();
    let registered: Vec<&str> = registered_tools.iter().map(String::as_str).collect();
    let entries = activity_catalog_entries(runtime);
    let scanned = entries.len();
    let mut faults = Vec::new();
    let mut names = BTreeMap::<(PathBuf, String), PathBuf>::new();
    for entry in entries {
        let (root, path) = match entry {
            CatalogEntry::File { root, path } => (root, path),
            CatalogEntry::Refused { path, reason } => {
                faults.push(ActivityCatalogFault {
                    name: file_stem_name(&path),
                    detail: format!("`{}` — {reason}", path.display()),
                    path,
                });
                continue;
            }
        };
        let name = file_stem_name(&path);
        let Some(raw) = read_confined_text(&path) else {
            faults.push(ActivityCatalogFault {
                name,
                path,
                detail: "file is unreadable".to_string(),
            });
            continue;
        };
        match inspect_activity_file(&path, &raw, &registered) {
            ActivityInspection::Healthy(name) => {
                let key = (root, name.clone());
                if let Some(first) = names.get(&key) {
                    let error = CatalogError::DuplicateName {
                        name: name.clone(),
                        first: first.clone(),
                        second: path.clone(),
                    };
                    faults.push(ActivityCatalogFault {
                        name,
                        path,
                        detail: error.to_string(),
                    });
                } else {
                    names.insert(key, path);
                }
            }
            ActivityInspection::RetiredSkipped => {}
            ActivityInspection::Fault(fault) => faults.push(fault),
        }
    }
    (scanned, faults)
}

#[derive(Debug)]
enum ActivityInspection {
    Healthy(String),
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
                });
            }
            ActivityInspection::Healthy(asset.name)
        }
        Err(error) => {
            let backend = spec_backend_value(raw);
            let detail = match backend.as_deref() {
                Some(value) => format!("`{}` spec.backend: {value} — {error}", path.display()),
                None => format!(
                    "`{}` — {error}; remedy: fix or remove this stale activity file \
                         (a removed activity or key will not return — migrate as the error \
                         directs, for example to a task workflow), then rerun `orbit doctor`",
                    path.display()
                ),
            };
            ActivityInspection::Fault(ActivityCatalogFault {
                name: file_stem_name(path),
                path: path.to_path_buf(),
                detail,
            })
        }
    }
}

fn spec_backend_value(raw: &str) -> Option<String> {
    let document = parse_mapping(raw)?;
    mapping_scalar(mapping_get(&document, "spec")?, "backend")
}

/// Linked `.yaml`/`.yml` file below a catalog root.
const LINKED_FILE: &str = "symbolic link below the catalog root; activity health reads only regular files inside the catalog, so it was not inspected or rewritten";
/// Linked directory below a catalog root.
const LINKED_DIR: &str = "linked directory below the catalog root; activity health does not follow it, so activities beneath it were not inspected or rewritten";
/// FIFO, socket, device, or other special `.yaml`/`.yml` entry.
const NOT_REGULAR: &str =
    "not a regular file; activity health does not read special files, so it was not inspected";

/// One activity catalog entry beneath a configured catalog root.
#[derive(Debug)]
enum CatalogEntry {
    /// A regular YAML file reached without crossing a link below `root`.
    File { root: PathBuf, path: PathBuf },
    /// An unsafe or unreadable entry reported instead of followed or read.
    Refused { path: PathBuf, reason: String },
}

/// Enumerate every production catalog root. Configured roots are resolved as
/// given (a linked root is still a root); nothing below a root is followed.
fn activity_catalog_entries(runtime: &OrbitRuntime) -> Vec<CatalogEntry> {
    let mut entries = BTreeMap::new();
    for root in runtime.v2_activity_catalog_paths() {
        if root.is_dir() {
            collect_catalog_entries(&root, &root, &mut entries);
        }
    }
    entries.into_values().collect()
}

/// Walk `dir` using each entry's own file type, so links are never traversed
/// (which also makes a linked directory cycle impossible to revisit).
fn collect_catalog_entries(root: &Path, dir: &Path, entries: &mut BTreeMap<PathBuf, CatalogEntry>) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(error) => {
            record_catalog_io_fault(dir, "list directory", error, entries);
            return;
        }
    };
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                record_catalog_io_fault(dir, "read directory entry", error, entries);
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_catalog_io_fault(&path, "inspect entry type", error, entries);
                continue;
            }
        };
        if file_type.is_dir() {
            collect_catalog_entries(root, &path, entries);
            continue;
        }
        let reason = if file_type.is_symlink() {
            if path.is_dir() {
                LINKED_DIR
            } else if is_yaml(&path) {
                LINKED_FILE
            } else {
                continue;
            }
        } else if !is_yaml(&path) {
            continue;
        } else if file_type.is_file() {
            entries.entry(path.clone()).or_insert(CatalogEntry::File {
                root: root.to_path_buf(),
                path,
            });
            continue;
        } else {
            NOT_REGULAR
        };
        entries
            .entry(path.clone())
            .or_insert(CatalogEntry::Refused {
                path,
                reason: reason.to_string(),
            });
    }
}

fn record_catalog_io_fault(
    path: &Path,
    operation: &str,
    error: std::io::Error,
    entries: &mut BTreeMap<PathBuf, CatalogEntry>,
) {
    entries
        .entry(path.to_path_buf())
        .or_insert_with(|| CatalogEntry::Refused {
            path: path.to_path_buf(),
            reason: format!(
                "cannot {operation}: {error}; check catalog permissions and availability, then rerun `orbit doctor`"
            ),
        });
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml")
        })
}

fn file_stem_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Read a discovered file without following a final link, and only when the
/// opened descriptor is a regular file. The open is nonblocking, so a FIFO
/// swapped in after discovery cannot stall doctor.
pub(super) fn read_confined_text(path: &Path) -> Option<String> {
    let mut file = open_read_only_no_follow(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    Some(raw)
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
