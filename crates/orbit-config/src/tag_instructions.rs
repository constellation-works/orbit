//! `[workflow.tag_instructions.<tag>]`: which repository file gives an
//! activity extra instructions for a task carrying `<tag>`.
//!
//! Each table maps a role (`implement`, `review`, `recovery`) to a path
//! relative to the repository root, or to `false` to switch off Orbit's
//! shipped default for that tag and role. The tables layer like crews: a
//! workspace entry replaces the global one for the same tag and role. Only
//! the mapping lives here; the host reads the file at the delivery's base
//! revision, so a task can select instructions but never write them.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::workflow::{
    MAX_TAG_INSTRUCTION_TAGS, TagInstructionRole, normalize_instruction_tag,
};

/// The dotted config path of the tag table.
pub const TAG_INSTRUCTIONS_KEY: &str = "workflow.tag_instructions";

/// The dotted config path of the activity-to-corpus-index table.
pub const AGENT_CORPUS_KEY: &str = "workflow.agent_corpus";

/// What one tag maps for one role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagInstructionSetting {
    /// A file, relative to the repository root, read at the base revision.
    File(String),
    /// `false`: no instruction, not even Orbit's shipped default.
    Disabled,
}

/// The admitted tag table, keyed by normalized tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagInstructionSettings {
    agent_corpus: BTreeMap<String, String>,
    tags: BTreeMap<String, BTreeMap<TagInstructionRole, TagInstructionSetting>>,
}

impl TagInstructionSettings {
    /// Activity-to-index mapping from `[workflow.agent_corpus]`.
    pub fn agent_corpus(&self) -> &BTreeMap<String, String> {
        &self.agent_corpus
    }

    /// What `tag` (already normalized) maps for `role`, if anything.
    pub fn setting(&self, tag: &str, role: TagInstructionRole) -> Option<&TagInstructionSetting> {
        self.tags.get(tag)?.get(&role)
    }

    /// Every configured (tag, role, setting), in tag then role order.
    pub fn entries(
        &self,
    ) -> impl Iterator<Item = (&str, TagInstructionRole, &TagInstructionSetting)> {
        self.tags.iter().flat_map(|(tag, roles)| {
            roles
                .iter()
                .map(move |(role, setting)| (tag.as_str(), *role, setting))
        })
    }

    /// Whether no tag is mapped.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

/// Admit `workflow.tag_instructions` from one (possibly merged) document.
pub(crate) fn tag_instructions_from_document(
    document: &toml::Value,
    config_path: &Path,
) -> Result<TagInstructionSettings, OrbitError> {
    let mut settings = tag_table_from_document(document, config_path)?;
    if let Some(value) = document.get("workflow").and_then(|w| w.get("agent_corpus")) {
        let table = value.as_table().ok_or_else(|| OrbitError::InvalidInput(
            "workflow.agent_corpus must map activity names to repository-relative INDEX.md paths".into()
        ))?;
        if table.len() > 32 {
            return Err(OrbitError::InvalidInput(
                "workflow.agent_corpus maps more than 32 activities".into(),
            ));
        }
        for (activity, value) in table {
            if activity.is_empty()
                || !activity
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(OrbitError::InvalidInput(format!(
                    "invalid corpus activity: {activity}"
                )));
            }
            let raw = value.as_str().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "workflow.agent_corpus.{activity} must be a repository-relative INDEX.md path"
                ))
            })?;
            let path = repository_path(raw).map_err(OrbitError::InvalidInput)?;
            if path.len() > 512 {
                return Err(OrbitError::InvalidInput(
                    "agent corpus index path exceeds 512 bytes".into(),
                ));
            }
            if !path.ends_with("/INDEX.md") {
                return Err(OrbitError::InvalidInput(
                    "agent corpus path must end in /INDEX.md".into(),
                ));
            }
            if settings
                .agent_corpus
                .values()
                .any(|existing| existing != &path)
            {
                return Err(OrbitError::InvalidInput("workflow.agent_corpus must map every activity to the same workspace corpus index".into()));
            }
            settings.agent_corpus.insert(activity.clone(), path);
        }
    }
    Ok(settings)
}

fn tag_table_from_document(
    document: &toml::Value,
    config_path: &Path,
) -> Result<TagInstructionSettings, OrbitError> {
    let Some(value) = document
        .get("workflow")
        .and_then(|workflow| workflow.get("tag_instructions"))
    else {
        return Ok(TagInstructionSettings::default());
    };
    let invalid = |detail: String| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {detail}",
            redact_home_dir(&config_path.display().to_string())
        ))
    };
    let Some(table) = value.as_table() else {
        return Err(invalid(format!(
            "'{TAG_INSTRUCTIONS_KEY}' must be a table of `[{TAG_INSTRUCTIONS_KEY}.<tag>]` tables"
        )));
    };
    if table.len() > MAX_TAG_INSTRUCTION_TAGS {
        return Err(invalid(format!(
            "'{TAG_INSTRUCTIONS_KEY}' maps {} tags; at most {MAX_TAG_INSTRUCTION_TAGS} are allowed",
            table.len()
        )));
    }
    let mut tags = BTreeMap::new();
    for (raw_tag, roles) in table {
        let key = format!("{TAG_INSTRUCTIONS_KEY}.{raw_tag}");
        let Some(tag) = normalize_instruction_tag(raw_tag) else {
            return Err(invalid(format!("'{key}' names a blank tag")));
        };
        let Some(roles) = roles.as_table() else {
            return Err(invalid(format!(
                "'{key}' must be a table mapping implement, review or recovery to a file path \
                 or false"
            )));
        };
        let mut settings = BTreeMap::new();
        for (raw_role, setting) in roles {
            let Some(role) = TagInstructionRole::parse(raw_role) else {
                return Err(OrbitError::invalid_input_with_suggestions(
                    format!(
                        "invalid runtime config '{}': unknown role '{raw_role}' in '{key}'",
                        redact_home_dir(&config_path.display().to_string())
                    ),
                    TagInstructionRole::ALL
                        .iter()
                        .map(|role| format!("{key}.{}", role.as_str()))
                        .collect(),
                ));
            };
            let setting = match setting {
                toml::Value::Boolean(false) => TagInstructionSetting::Disabled,
                toml::Value::String(path) => TagInstructionSetting::File(
                    repository_path(path)
                        .map_err(|why| invalid(format!("'{key}.{raw_role}' {why}")))?,
                ),
                _ => {
                    return Err(invalid(format!(
                        "'{key}.{raw_role}' must be a repository-relative file path or false"
                    )));
                }
            };
            settings.insert(role, setting);
        }
        if tags.insert(tag.clone(), settings).is_some() {
            return Err(invalid(format!(
                "'{TAG_INSTRUCTIONS_KEY}' maps tag '{tag}' more than once; tags match \
                 case-insensitively"
            )));
        }
    }
    Ok(TagInstructionSettings {
        tags,
        ..Default::default()
    })
}

/// A path inside the repository: relative, `/`-separated, without `.` or
/// `..` segments. A leading `./` is dropped.
pub(crate) fn repository_path(raw: &str) -> Result<String, String> {
    let path = raw.trim();
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty() {
        return Err("is empty; name a file relative to the repository root".to_string());
    }
    if path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(format!(
            "'{raw}' must be relative to the repository root, with `/` separators"
        ));
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!(
            "'{raw}' must stay inside the repository: no empty, `.` or `..` segments"
        ));
    }
    Ok(path.to_string())
}
