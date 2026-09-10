use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{OrbitId, is_valid_adr_id, is_valid_friction_id};
use crate::task::{ExternalRef, TaskComplexity, TaskError, TaskPriority, TaskStatus, TaskType};

pub const TASK_ARTIFACT_SCHEMA_VERSION: u32 = 1;
/// Historical task prefix retained for source compatibility. Task parsing is
/// prefix-agnostic; new ids should be formatted with [`format_task_id`].
pub const ORB_TASK_ID_PREFIX: &str = "ORB-";
/// Minimum numeric width used when minting task ids. Parsers accept wider ids.
pub const ORB_TASK_ID_WIDTH: usize = 5;
/// Maximum representable allocator value, no longer the five-digit boundary.
pub const ORB_TASK_ID_MAX: u32 = u32::MAX;

pub const TASK_ENVELOPE_FILE_NAME: &str = "task.yaml";
pub const TASK_DESCRIPTION_FILE_NAME: &str = "description.md";
pub const TASK_ACCEPTANCE_FILE_NAME: &str = "acceptance.md";
pub const TASK_PLAN_FILE_NAME: &str = "plan.md";
pub const TASK_EXECUTION_SUMMARY_FILE_NAME: &str = "execution-summary.md";
pub const TASK_EVENTS_FILE_NAME: &str = "events.jsonl";
pub const TASK_COMMENTS_FILE_NAME: &str = "comments.jsonl";
pub const TASK_ARTIFACTS_DIR_NAME: &str = "artifacts";
pub const TASK_ARTIFACT_MANIFEST_FILE_NAME: &str = "manifest.yaml";
pub const TASK_ARTIFACT_FILES_DIR_NAME: &str = "files";

pub fn is_valid_orb_task_id(id: &str) -> bool {
    let Some((prefix, suffix)) = id.split_once('-') else {
        return false;
    };
    is_valid_task_id_prefix(prefix)
        && !suffix.is_empty()
        && suffix.chars().all(|character| character.is_ascii_digit())
}

/// Validate a stored task-id prefix. `ORB` remains valid for migrated hosts;
/// other artifact namespaces cannot be interpreted as tasks.
pub fn is_valid_task_id_prefix(prefix: &str) -> bool {
    (2..=5).contains(&prefix.len())
        && prefix.bytes().all(|byte| byte.is_ascii_uppercase())
        && !matches!(prefix, "ADR" | "L" | "F")
}

pub fn task_id_prefix(id: &str) -> Option<&str> {
    is_valid_orb_task_id(id).then(|| id.split_once('-').map(|(prefix, _)| prefix))?
}

pub fn parse_task_number(id: &str) -> Option<u32> {
    is_valid_orb_task_id(id)
        .then(|| id.split_once('-').map(|(_, suffix)| suffix))?
        .and_then(|suffix| suffix.parse().ok())
}

pub fn validate_orb_task_id(id: &str) -> Result<(), TaskError> {
    if is_valid_orb_task_id(id) {
        return Ok(());
    }
    Err(TaskError::Invalid(format!(
        "task id '{id}' must match <2-5 uppercase-letter prefix>-<digits>"
    )))
}

pub fn format_orb_task_id(value: u32) -> Result<String, TaskError> {
    format_task_id("ORB", value)
}

pub fn format_task_id(prefix: &str, value: u32) -> Result<String, TaskError> {
    if !is_valid_task_id_prefix(prefix) {
        return Err(TaskError::Invalid(format!(
            "task id prefix '{prefix}' must be 2-5 uppercase ASCII letters and must not use a reserved artifact namespace"
        )));
    }
    Ok(format!("{prefix}-{value:0ORB_TASK_ID_WIDTH$}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskEnvelopeV2 {
    pub schema_version: u32,
    pub id: OrbitId,
    pub title: String,
    pub status: TaskStatus,
    #[serde(rename = "type")]
    pub task_type: TaskType,
    pub priority: TaskPriority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complexity: Option<TaskComplexity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Named crew responsible for task orchestration, distinct from execution crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<TaskRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Exact canonical tool names added to the selected agent activity.
    #[serde(
        default,
        deserialize_with = "crate::task::deserialize_required_tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub required_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_refs: Vec<ExternalRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implemented_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TaskEnvelopeV2 {
    pub fn validate(&self) -> Result<(), TaskError> {
        validate_schema_version(self.schema_version, "task envelope")?;
        validate_orb_task_id(&self.id)?;
        if self.title.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task title must not be empty".to_string(),
            ));
        }
        validate_task_relations_for_source(&self.id, &self.relations, &[])
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TaskRelationType {
    BlockedBy,
    ChildOf,
    SpawnedFrom,
    RegressionFrom,
    Supersedes,
    RelatedTo,
    Produces,
    Resolves,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskRelation {
    #[serde(rename = "type")]
    pub relation_type: TaskRelationType,
    pub target: OrbitId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskRelationEdge {
    pub source: OrbitId,
    #[serde(rename = "type")]
    pub relation_type: TaskRelationType,
    pub target: OrbitId,
}

pub fn validate_task_relations_for_source(
    source_id: &str,
    relations: &[TaskRelation],
    existing_edges: &[TaskRelationEdge],
) -> Result<(), TaskError> {
    // Existing edges are assumed acyclic; this check only rejects new edges that close a cycle.
    validate_orb_task_id(source_id)?;
    let mut seen = BTreeSet::new();
    for edge in existing_edges {
        validate_orb_task_id(&edge.source)?;
        validate_orb_task_id(&edge.target)?;
        if edge.source == edge.target {
            return Err(TaskError::Invalid(format!(
                "task relation from '{}' must not target itself",
                edge.source
            )));
        }
    }
    for relation in relations {
        validate_target_for(relation.relation_type, &relation.target)?;
        if relation.target == source_id {
            return Err(TaskError::Invalid(format!(
                "task relation from '{source_id}' must not target itself"
            )));
        }
        let key = (relation.relation_type, relation.target.as_str());
        if !seen.insert(key) {
            return Err(TaskError::Invalid(format!(
                "duplicate task relation {:?} -> '{}'",
                relation.relation_type, relation.target
            )));
        }
    }

    for relation in relations {
        if let Some(family) = cyclic_relation_family(relation.relation_type) {
            let mut graph = relation_graph_for_family(family, existing_edges);
            for candidate in relations {
                if cyclic_relation_family(candidate.relation_type) == Some(family) {
                    graph
                        .entry(source_id.to_string())
                        .or_default()
                        .insert(candidate.target.clone());
                }
            }
            if reaches(&graph, &relation.target, source_id) {
                return Err(TaskError::Invalid(format!(
                    "task relation {:?} from '{}' to '{}' would create a cycle",
                    relation.relation_type, source_id, relation.target
                )));
            }
        }
    }

    Ok(())
}

fn validate_target_for(relation_type: TaskRelationType, target: &str) -> Result<(), TaskError> {
    match relation_type {
        TaskRelationType::Produces | TaskRelationType::Resolves => {
            if is_valid_orb_task_id(target)
                || is_valid_friction_id(target)
                || is_valid_adr_id(target)
            {
                return Ok(());
            }
            Err(TaskError::Invalid(format!(
                "relation target '{target}' must be an ORB-/F-/ADR- id"
            )))
        }
        TaskRelationType::BlockedBy
        | TaskRelationType::ChildOf
        | TaskRelationType::SpawnedFrom
        | TaskRelationType::RegressionFrom
        | TaskRelationType::Supersedes
        | TaskRelationType::RelatedTo => validate_orb_task_id(target),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskEventRowV2 {
    pub schema_version: u32,
    pub event_id: String,
    pub at: DateTime<Utc>,
    pub by: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_status: Option<TaskStatus>,
}

impl TaskEventRowV2 {
    pub fn validate(&self) -> Result<(), TaskError> {
        validate_schema_version(self.schema_version, "task event row")?;
        if self.event_id.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task event id must not be empty".to_string(),
            ));
        }
        if self.by.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task event actor must not be empty".to_string(),
            ));
        }
        if self.event_type.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task event type must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskCommentRowV2 {
    pub schema_version: u32,
    pub comment_id: String,
    pub at: DateTime<Utc>,
    pub by: String,
    pub body: String,
}

impl TaskCommentRowV2 {
    pub fn validate(&self) -> Result<(), TaskError> {
        validate_schema_version(self.schema_version, "task comment row")?;
        if self.comment_id.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task comment id must not be empty".to_string(),
            ));
        }
        if self.by.trim().is_empty() {
            return Err(TaskError::Invalid(
                "task comment actor must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifestV2 {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ArtifactManifestFileV2>,
}

impl ArtifactManifestV2 {
    pub fn validate(&self) -> Result<(), TaskError> {
        validate_schema_version(self.schema_version, "artifact manifest")?;
        for file in &self.files {
            file.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifestFileV2 {
    pub path: String,
    pub blob: String,
    /// Lowercase hex SHA-256 digest; writers should format bytes with `{:x}`.
    pub sha256: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

impl ArtifactManifestFileV2 {
    pub fn validate(&self) -> Result<(), TaskError> {
        validate_relative_artifact_path(&self.path)?;
        validate_relative_artifact_path(&self.blob)?;
        if !is_sha256_hex(&self.sha256) {
            return Err(TaskError::Invalid(
                "artifact sha256 must be 64 lowercase hexadecimal characters".to_string(),
            ));
        }
        if self.media_type.trim().is_empty() {
            return Err(TaskError::Invalid(
                "artifact media type must not be empty".to_string(),
            ));
        }
        if self.created_by.trim().is_empty() {
            return Err(TaskError::Invalid(
                "artifact created_by must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

/// Common metadata interface for task artifacts across manifest and runtime DTO representations.
pub trait TaskArtifactMetadata {
    fn path(&self) -> &str;
    fn media_type(&self) -> &str;
    fn size(&self) -> u64;
    fn created_by(&self) -> Option<&str>;
}

impl TaskArtifactMetadata for ArtifactManifestFileV2 {
    fn path(&self) -> &str {
        &self.path
    }

    fn media_type(&self) -> &str {
        &self.media_type
    }

    fn size(&self) -> u64 {
        self.size_bytes
    }

    fn created_by(&self) -> Option<&str> {
        let trimmed = self.created_by.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }
}

impl TaskArtifactMetadata for crate::task::TaskArtifact {
    fn path(&self) -> &str {
        &self.path
    }

    fn media_type(&self) -> &str {
        &self.media_type
    }

    fn size(&self) -> u64 {
        self.content.len() as u64
    }

    fn created_by(&self) -> Option<&str> {
        self.created_by
            .as_deref()
            .map(str::trim)
            .filter(|trimmed| !trimmed.is_empty())
    }
}

/// Serialize task artifact metadata for task discovery and projection surfaces (`task.show`).
///
/// Emits bounded metadata only (`path`, `media_type`, `size`, `created_by`); payload content
/// is never included and must be retrieved explicitly via `orbit.task.artifact.get`.
pub fn serialize_task_artifacts<T: TaskArtifactMetadata>(artifacts: &[T]) -> serde_json::Value {
    serde_json::Value::Array(
        artifacts
            .iter()
            .map(|artifact| {
                let mut object = serde_json::Map::new();
                object.insert(
                    "path".to_string(),
                    serde_json::Value::String(artifact.path().to_string()),
                );
                object.insert(
                    "media_type".to_string(),
                    serde_json::Value::String(artifact.media_type().to_string()),
                );
                if let Some(created_by) = artifact.created_by() {
                    object.insert(
                        "created_by".to_string(),
                        serde_json::Value::String(created_by.to_string()),
                    );
                }
                object.insert(
                    "size".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(artifact.size())),
                );
                serde_json::Value::Object(object)
            })
            .collect(),
    )
}

pub fn validate_relative_artifact_path(path: &str) -> Result<(), TaskError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(TaskError::Invalid(
            "artifact path must not be empty".to_string(),
        ));
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(TaskError::Invalid(format!(
            "artifact path '{trimmed}' must be relative"
        )));
    }
    if trimmed.contains('\\') {
        return Err(TaskError::Invalid(format!(
            "artifact path '{trimmed}' must use slash separators"
        )));
    }
    let mut has_normal_component = false;
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(TaskError::Invalid(format!(
                    "artifact path '{trimmed}' must not contain '..'"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(TaskError::Invalid(format!(
                    "artifact path '{trimmed}' must be relative"
                )));
            }
            Component::CurDir => {
                // Manifest paths are stored canonical; writers may strip a leading "./" before validation.
                return Err(TaskError::Invalid(format!(
                    "artifact path '{trimmed}' must not contain '.' components"
                )));
            }
            Component::Normal(_) => {
                has_normal_component = true;
            }
        }
    }
    if !has_normal_component {
        return Err(TaskError::Invalid(
            "artifact path must contain at least one path component".to_string(),
        ));
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_schema_version(schema_version: u32, label: &str) -> Result<(), TaskError> {
    if schema_version == TASK_ARTIFACT_SCHEMA_VERSION {
        return Ok(());
    }
    Err(TaskError::Invalid(format!(
        "{label} schema_version must be {TASK_ARTIFACT_SCHEMA_VERSION}, got {schema_version}"
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationCycleFamily {
    Blocking,
    Hierarchy,
}

fn cyclic_relation_family(relation_type: TaskRelationType) -> Option<RelationCycleFamily> {
    match relation_type {
        TaskRelationType::BlockedBy => Some(RelationCycleFamily::Blocking),
        TaskRelationType::ChildOf => Some(RelationCycleFamily::Hierarchy),
        // Temporal and associative relation types are queryable metadata, not reachability families.
        TaskRelationType::SpawnedFrom => None,
        TaskRelationType::RegressionFrom
        | TaskRelationType::Supersedes
        | TaskRelationType::RelatedTo
        | TaskRelationType::Produces
        | TaskRelationType::Resolves => None,
    }
}

fn relation_graph_for_family(
    family: RelationCycleFamily,
    edges: &[TaskRelationEdge],
) -> BTreeMap<OrbitId, BTreeSet<OrbitId>> {
    let mut graph = BTreeMap::new();
    for edge in edges {
        if cyclic_relation_family(edge.relation_type) == Some(family) {
            graph
                .entry(edge.source.clone())
                .or_insert_with(BTreeSet::new)
                .insert(edge.target.clone());
        }
    }
    graph
}

fn reaches(graph: &BTreeMap<OrbitId, BTreeSet<OrbitId>>, start: &str, target: &str) -> bool {
    let mut stack = vec![start.to_string()];
    let mut seen = BTreeSet::new();
    while let Some(current) = stack.pop() {
        if current == target {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(next) = graph.get(&current) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}
