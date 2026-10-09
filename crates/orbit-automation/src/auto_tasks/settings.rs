//! Operator settings layered over bundled auto-task bodies [ORB-14909].
//!
//! A shipped default's YAML body is a managed asset: Orbit refreshes it on
//! `orbit workspace sync` only while its bytes match the digest Orbit last
//! wrote. Enabling, rescheduling or re-crewing a definition by rewriting that
//! file would freeze the whole template as a local fork. Instead the fields an
//! operator is meant to tune live in one settings table beside the
//! definitions, `<orbit_dir>/auto_tasks/.orbit-auto-task-settings.json`, keyed
//! by definition name. Loading a definition applies its entry over the body,
//! so every consumer (scheduler, mint, list, show, doctor) sees the same
//! effective definition while the body keeps refreshing.
//!
//! Settings fields: `enabled`, `schedule`, `dedupe`, and the template's
//! `crew`, `priority`, `complexity` and tag additions. Everything else is
//! body. [`split_overrides`] decides whether an effective definition differs
//! from a body only in settings fields, and names the body fields otherwise.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_types::task::{TaskComplexity, TaskPriority};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, DedupePolicy};
use serde::{Deserialize, Serialize};

/// File name of the settings table inside the auto-task directory. It is not
/// YAML, so definition discovery never mistakes it for a definition.
pub const AUTO_TASK_SETTINGS_FILE: &str = ".orbit-auto-task-settings.json";
const AUTO_TASK_SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Operator settings for one definition. An absent field leaves the body's
/// value in force.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoTaskSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<AutoTaskSchedule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe: Option<DedupePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<TaskComplexity>,
    /// Tags added after the body's template tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Actor of the last settings edit, shown as the definition's `updated_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// RFC 3339 timestamp of the last settings edit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated_at: String,
}

impl AutoTaskSettings {
    /// Names of the fields this entry overrides, in definition order.
    pub fn field_names(&self) -> Vec<&'static str> {
        [
            ("enabled", self.enabled.is_some()),
            ("schedule", self.schedule.is_some()),
            ("dedupe", self.dedupe.is_some()),
            ("template.crew", self.crew.is_some()),
            ("template.priority", self.priority.is_some()),
            ("template.complexity", self.complexity.is_some()),
            ("template.tags", !self.tags.is_empty()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }

    /// Apply this entry over a body, producing the effective definition.
    pub fn apply(&self, definition: &mut AutoTaskDefinition) {
        if let Some(enabled) = self.enabled {
            definition.enabled = enabled;
        }
        if let Some(schedule) = &self.schedule {
            definition.schedule = schedule.clone();
        }
        if let Some(dedupe) = self.dedupe {
            definition.dedupe = dedupe;
        }
        let template = &mut definition.template;
        if let Some(crew) = &self.crew {
            template.crew = Some(crew.clone());
        }
        if let Some(priority) = self.priority {
            template.priority = priority;
        }
        if let Some(complexity) = self.complexity {
            template.complexity = Some(complexity);
        }
        for tag in &self.tags {
            if !template.tags.contains(tag) {
                template.tags.push(tag.clone());
            }
        }
        if self.updated_by.is_some() || !self.updated_at.is_empty() {
            definition.updated_by = self.updated_by.clone();
            definition.updated_at = self.updated_at.clone();
        }
    }
}

/// How an effective definition differs from a body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoTaskOverrides {
    /// The differences a settings entry can carry. Its timestamps are unset.
    pub settings: AutoTaskSettings,
    /// Body fields that differ and no settings entry can express. Empty means
    /// the effective definition is the body plus `settings`.
    pub body_fields: Vec<&'static str>,
}

/// Split the differences between `body` and `effective` into settings and
/// body edits. Creation and update stamps are ignored.
pub fn split_overrides(
    body: &AutoTaskDefinition,
    effective: &AutoTaskDefinition,
) -> AutoTaskOverrides {
    // Destructured without `..`, so a new definition field must be classified
    // here before this compiles.
    let AutoTaskDefinition {
        schema_version,
        name,
        description,
        enabled,
        schedule,
        template,
        dedupe,
        skip_if_unchanged,
        created_by: _,
        created_at: _,
        updated_by: _,
        updated_at: _,
    } = effective;
    let orbit_types::workflow::AutoTaskTemplate {
        title,
        description: template_description,
        acceptance_criteria,
        task_type,
        tags,
        required_tools,
        context_files,
        priority,
        complexity,
        crew,
        status,
    } = template;
    let base = &body.template;

    let mut body_fields = Vec::new();
    for (field, differs) in [
        ("schemaVersion", *schema_version != body.schema_version),
        ("name", *name != body.name),
        ("description", *description != body.description),
        (
            "skip_if_unchanged",
            *skip_if_unchanged != body.skip_if_unchanged,
        ),
        ("template.title", *title != base.title),
        (
            "template.description",
            *template_description != base.description,
        ),
        (
            "template.acceptance_criteria",
            *acceptance_criteria != base.acceptance_criteria,
        ),
        ("template.task_type", *task_type != base.task_type),
        (
            "template.required_tools",
            *required_tools != base.required_tools,
        ),
        (
            "template.context_files",
            *context_files != base.context_files,
        ),
        ("template.status", *status != base.status),
        // Settings can set a crew or complexity, and add tags, but cannot
        // clear a value or drop a body tag.
        ("template.crew", crew.is_none() && base.crew.is_some()),
        (
            "template.complexity",
            complexity.is_none() && base.complexity.is_some(),
        ),
        (
            "template.tags",
            base.tags.iter().any(|tag| !tags.contains(tag)),
        ),
    ] {
        if differs {
            body_fields.push(field);
        }
    }

    let mut added_tags = Vec::new();
    for tag in tags {
        if !base.tags.contains(tag) && !added_tags.contains(tag) {
            added_tags.push(tag.clone());
        }
    }
    let settings = AutoTaskSettings {
        enabled: (*enabled != body.enabled).then_some(*enabled),
        schedule: (*schedule != body.schedule).then(|| schedule.clone()),
        dedupe: (*dedupe != body.dedupe).then_some(*dedupe),
        crew: (*crew != base.crew).then(|| crew.clone()).flatten(),
        priority: (*priority != base.priority).then_some(*priority),
        complexity: (*complexity != base.complexity)
            .then_some(*complexity)
            .flatten(),
        tags: added_tags,
        updated_by: None,
        updated_at: String::new(),
    };
    AutoTaskOverrides {
        settings,
        body_fields,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AutoTaskSettingsDocument {
    schema_version: u32,
    #[serde(default)]
    definitions: BTreeMap<String, AutoTaskSettings>,
}

/// Every definition's settings, keyed by definition name.
pub type AutoTaskSettingsTable = BTreeMap<String, AutoTaskSettings>;

/// Path of the settings table for an auto-task directory.
pub fn settings_path(auto_tasks_dir: &Path) -> PathBuf {
    auto_tasks_dir.join(AUTO_TASK_SETTINGS_FILE)
}

/// Read the settings table of an auto-task directory. A missing file is an
/// empty table; a link, a non-file, or an unparsable document is an error.
pub fn load_settings_table(auto_tasks_dir: &Path) -> Result<AutoTaskSettingsTable, String> {
    let path = settings_path(auto_tasks_dir);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(format!(
                "auto-task settings table {} must be a regular file",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AutoTaskSettingsTable::new());
        }
        Err(error) => {
            return Err(format!(
                "inspect auto-task settings table {}: {error}",
                path.display()
            ));
        }
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("read auto-task settings table {}: {error}", path.display()))?;
    let document: AutoTaskSettingsDocument = serde_json::from_str(&raw)
        .map_err(|error| format!("parse auto-task settings table {}: {error}", path.display()))?;
    if document.schema_version != AUTO_TASK_SETTINGS_SCHEMA_VERSION {
        return Err(format!(
            "auto-task settings table {} has unsupported schemaVersion {} (this binary supports {})",
            path.display(),
            document.schema_version,
            AUTO_TASK_SETTINGS_SCHEMA_VERSION
        ));
    }
    Ok(document.definitions)
}

/// Write the settings table, removing the file once no entry remains.
pub fn write_settings_table(
    auto_tasks_dir: &Path,
    table: &AutoTaskSettingsTable,
) -> Result<(), OrbitError> {
    let path = settings_path(auto_tasks_dir);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && !metadata.file_type().is_file()
    {
        return Err(OrbitError::InvalidInput(format!(
            "auto-task settings table {} must be a regular file",
            path.display()
        )));
    }
    if table.is_empty() {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OrbitError::io_with_context(
                &error,
                format!(
                    "remove auto-task settings table {}: {error}",
                    path.display()
                ),
            )),
        };
    }
    let document = AutoTaskSettingsDocument {
        schema_version: AUTO_TASK_SETTINGS_SCHEMA_VERSION,
        definitions: table.clone(),
    };
    let mut encoded = serde_json::to_string_pretty(&document).map_err(|error| {
        OrbitError::InvalidInput(format!("encode auto-task settings table: {error}"))
    })?;
    encoded.push('\n');
    atomic_write_text(&path, &encoded).map_err(|error| {
        OrbitError::io_with_context(
            &error,
            format!("write auto-task settings table {}: {error}", path.display()),
        )
    })
}
