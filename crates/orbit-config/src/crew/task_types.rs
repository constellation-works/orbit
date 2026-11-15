//! `workflow.task_types`: the ordered task-type list that classifies a task
//! for `workflow.type_complexity_crews` routing and the implementer by-type
//! scorecard. `workflow.crew_performance_types` is its deprecated spelling,
//! translated on load ([`crate::aliases`]).

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::fs::path_glob::RelativePathGlob;
use serde::{Deserialize, Serialize};

/// The key as written in `config.toml`.
pub const TASK_TYPES_KEY: &str = "workflow.task_types";
/// The deprecated spelling of [`TASK_TYPES_KEY`] [ORB-22804].
pub const LEGACY_TASK_TYPES_KEY: &str = "workflow.crew_performance_types";

/// The fallback type for a task no configured type matches.
pub const OTHER_TYPE: &str = "other";

/// One `[[workflow.task_types]]` entry: a name, and the tags or
/// context-file path globs that put a task in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTypeDefinition {
    /// The type's name; unique across the list and never `other`.
    #[serde(default)]
    pub name: String,
    /// A task carrying any of these tags takes this type.
    #[serde(default)]
    pub tags: Vec<String>,
    /// A task with any context file matching one of these globs takes this type.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// The type of a task under `types`: the first entry whose `tags` it carries
/// (any of) or whose `paths` globs match any of its `context_files` (any of),
/// else `other`. Context-file selectors are `file:`, `dir:` or `symbol:`
/// prefixed; a `symbol:` selector matches by its backing file
/// (`symbol:<path>#<name>:<kind>`), and a `dir:` selector also matches a glob
/// that only matches beneath it. `module:` and `command:` selectors name no
/// path and never match. This is the one classification type-pool routing and
/// the by-type scorecard both use.
#[must_use]
pub fn classify_task_type<'a>(
    types: &'a [TaskTypeDefinition],
    tags: &[String],
    context_files: &[String],
) -> &'a str {
    for entry in types {
        if entry
            .tags
            .iter()
            .any(|tag| tags.iter().any(|held| held == tag))
        {
            return &entry.name;
        }
        for selector in context_files {
            let Some((path, is_dir)) = selector_path(selector) else {
                continue;
            };
            let matched = entry.paths.iter().any(|pattern| {
                RelativePathGlob::new(pattern).is_ok_and(|glob| {
                    glob.matches(path) || (is_dir && glob.may_match_descendant(path))
                })
            });
            if matched {
                return &entry.name;
            }
        }
    }
    OTHER_TYPE
}

/// The path a context-file selector names, and whether it names a directory.
fn selector_path(selector: &str) -> Option<(&str, bool)> {
    if let Some(path) = selector.strip_prefix("file:") {
        Some((path, false))
    } else if let Some(path) = selector.strip_prefix("dir:") {
        Some((path, true))
    } else if let Some(remainder) = selector.strip_prefix("symbol:") {
        let (path, _) = remainder.split_once('#')?;
        if path.is_empty() {
            None
        } else {
            Some((path, false))
        }
    } else if selector.starts_with("module:") || selector.starts_with("command:") {
        None
    } else {
        Some((selector, false))
    }
}

/// Admit `workflow.task_types`: ordered entries with unique
/// non-empty names, `other` reserved, each holding at least one of `tags` or
/// `paths`, and every path a valid relative glob. An empty list is admitted
/// and classifies every task as `other`.
pub(crate) fn admit_task_types(
    raw: Option<Vec<TaskTypeDefinition>>,
) -> Result<Vec<TaskTypeDefinition>, OrbitError> {
    let invalid = |reason: String| OrbitError::InvalidInput(format!("{TASK_TYPES_KEY} {reason}"));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut admitted = Vec::new();
    for (index, entry) in raw.unwrap_or_default().into_iter().enumerate() {
        let name = entry.name.trim().to_string();
        let label = if name.is_empty() {
            format!("entry {}", index + 1)
        } else {
            format!("entry `{name}`")
        };
        if name.is_empty() {
            return Err(invalid(format!("{label} has an empty name")));
        }
        if name == OTHER_TYPE {
            return Err(invalid(format!("{label} uses the reserved name `other`")));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid(format!("{label} repeats the name `{name}`")));
        }
        let tags: Vec<String> = entry
            .tags
            .iter()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty())
            .collect();
        let paths: Vec<String> = entry
            .paths
            .iter()
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty())
            .collect();
        if tags.is_empty() && paths.is_empty() {
            return Err(invalid(format!(
                "{label} needs at least one of `tags` or `paths`"
            )));
        }
        for pattern in &paths {
            RelativePathGlob::new(pattern)
                .map_err(|error| invalid(format!("{label} has an invalid path glob: {error}")))?;
        }
        admitted.push(TaskTypeDefinition { name, tags, paths });
    }
    Ok(admitted)
}
