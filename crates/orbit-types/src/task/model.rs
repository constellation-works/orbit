//! Task types: status lifecycle, priority, complexity, and the [`Task`] struct itself.
//!
//! ## Task Status Lifecycle
//!
//! An explicit, authorized edit may move a task directly between any two
//! statuses. Status is an audited classification; workflow admission,
//! completion, and delivery keep their own stricter operational gates.
//!
//! ### Statuses
//! | Status       | Purpose |
//! |--------------|---------|
//! | Proposed     | Awaiting human approval before entering the backlog. |
//! | Backlog      | Approved and queued for work. |
//! | Someday      | Future-scoped — wanted but not yet actionable. Agents skip someday tasks. |
//! | InProgress   | Actively being worked on. |
//! | Review       | Implementation complete; awaiting review/merge. |
//! | Done         | Accepted and closed. May be reopened by an explicit edit. |
//! | Blocked      | Temporarily paused. |
//! | Archived     | Soft-deleted. Restorable to any other status. |
//! | Rejected     | Declined. Can be re-opened. |

// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::identity::OrbitId;
use crate::task::TaskError;
use crate::task::artifacts::{
    TaskRelation, TaskRelationType, is_valid_orb_task_id, task_id_prefix,
};

/// Tasks carrying this tag may complete successfully without producing a
/// repository diff. Their durable side effects live outside git (for example,
/// QA validation tasks file follow-up Orbit tasks).
pub const NO_DIFF_EXPECTED_TAG: &str = "no-diff-expected";

/// Operator-facing projection for a valid task reference whose prefix is not
/// represented in this machine's coordination registry.
pub const TASK_REFERENCE_NOT_VERIFIABLE_HERE: &str = "not verifiable here";

/// Default maximum number of tasks a task listing returns (ORB-10310). The
/// `orbit task list` CLI and `orbit.task.list` MCP tool return at most the
/// newest `DEFAULT_TASK_LIST_LIMIT` matching tasks, with status-aware ordering
/// when no lifecycle status filter is supplied.
pub const DEFAULT_TASK_LIST_LIMIT: usize = 50;

/// Named bucket for an optional indexed label that nobody set.
///
/// Aggregates must keep this visible rather than dropping the row or folding it
/// into a populated band — the unlabeled set is often the largest bucket
/// (ORB-10889 / ORB-10891).
pub const UNSET_BUCKET: &str = "unset";

/// Map an optional indexed label onto a display bucket.
///
/// Empty or missing values become [`UNSET_BUCKET`]. A present non-empty value
/// is returned unchanged so unexpected labels stay their own band instead of
/// being merged into a known one.
pub fn labeled_or_unset(value: Option<&str>) -> &str {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(label) => label,
        None => UNSET_BUCKET,
    }
}

/// Map an indexed complexity label onto a display bucket.
///
/// [`TaskComplexity::Unassessed`] is the stored spelling of "nobody assessed
/// this" ([`TaskComplexity::is_assessed`] is `false`), and rows indexed before
/// the column existed spell the same thing as `NULL`. Reporting surfaces get
/// one bucket for that concept — [`UNSET_BUCKET`] — rather than splitting the
/// unlabeled set across `unset` and `unassessed` (ORB-10895). The fold happens
/// here, at read time: the persisted and indexed value stays the literal
/// `unassessed` so provenance and `NULL`-backfill detection remain honest.
pub fn complexity_bucket(value: Option<&str>) -> &str {
    let label = labeled_or_unset(value);
    if label == TaskComplexity::Unassessed.as_str() {
        UNSET_BUCKET
    } else {
        label
    }
}

/// Stable display order: `unset`, then `low` / `medium` / `hard`, then any
/// unexpected label alphabetically.
pub fn complexity_bucket_ord(label: &str) -> (u8, &str) {
    match label {
        UNSET_BUCKET => (0, ""),
        "low" => (1, ""),
        "medium" => (2, ""),
        "hard" => (3, ""),
        other => (4, other),
    }
}

/// Current lifecycle state of a task.
///
/// See the module-level documentation for status-edit semantics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Awaiting human approval before entering the backlog.
    Proposed,
    /// Approved and queued for work; not yet started.
    Backlog,
    /// Actively being worked on.
    #[cfg_attr(feature = "clap", value(name = "in-progress", alias = "in_progress"))]
    #[serde(alias = "in-progress")]
    InProgress,
    /// Implementation complete; awaiting review/merge.
    Review,
    /// Accepted and closed. May be reopened by an explicit status edit.
    Done,
    /// Temporarily paused (waiting on a dependency or decision).
    Blocked,
    /// Soft-deleted. Can be restored to any other status.
    Archived,
    /// Declined. Can be reclassified to any other status.
    Rejected,
    /// Future-scoped — wanted but not yet actionable. Agents skip someday tasks.
    Someday,
}

/// Lifecycle states that may be selected when a task is first created.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskCreateStatus {
    /// Awaiting human approval before entering the backlog.
    Proposed,
    /// Approved and queued for work; not yet started.
    Backlog,
    /// Future-scoped — wanted but not yet actionable.
    Someday,
}

impl From<TaskCreateStatus> for TaskStatus {
    fn from(value: TaskCreateStatus) -> Self {
        match value {
            TaskCreateStatus::Proposed => Self::Proposed,
            TaskCreateStatus::Backlog => Self::Backlog,
            TaskCreateStatus::Someday => Self::Someday,
        }
    }
}

impl Display for TaskStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cli_name())
    }
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "proposed" => Ok(TaskStatus::Proposed),
            "backlog" => Ok(TaskStatus::Backlog),
            "in-progress" => Ok(TaskStatus::InProgress),
            "in_progress" => Ok(TaskStatus::InProgress),
            "review" => Ok(TaskStatus::Review),
            "done" => Ok(TaskStatus::Done),
            "blocked" => Ok(TaskStatus::Blocked),
            "archived" => Ok(TaskStatus::Archived),
            "rejected" => Ok(TaskStatus::Rejected),
            "someday" => Ok(TaskStatus::Someday),
            other => Err(format!("unknown task status: {other}")),
        }
    }
}

impl TaskStatus {
    pub fn cli_name(self) -> &'static str {
        match self {
            TaskStatus::Proposed => "proposed",
            TaskStatus::Backlog => "backlog",
            TaskStatus::InProgress => "in-progress",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Archived => "archived",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Someday => "someday",
        }
    }

    /// Returns true when the status satisfies a task dependency.
    ///
    /// Orbit's lifecycle has no standalone terminal `approved` status for
    /// tasks today; accepted work lands in `done`.
    pub fn satisfies_dependency(self) -> bool {
        matches!(self, TaskStatus::Done)
    }

    /// Classifies a *non*-satisfying status as either a legitimate wait or a
    /// dead end.
    ///
    /// `satisfies_dependency` answers "is this edge satisfied now?"; this
    /// answers the complementary "could waiting ever satisfy it?". A
    /// dependency in `backlog` / `proposed` / `in-progress` / `review` /
    /// `blocked` / `someday` returns `None` — it can still reach `done`, so
    /// callers must keep waiting. `archived` and `rejected` return `Some`:
    /// both are restorable, but only by an operator editing the task graph,
    /// never by the passage of time.
    ///
    /// This deliberately does *not* widen what counts as satisfied
    /// (`Done`-only stays) — it only lets dispatch fail loudly instead of
    /// polling out its whole budget against an edge that cannot close.
    pub fn dependency_dead_end(self) -> Option<DependencyDeadEnd> {
        match self {
            TaskStatus::Archived => Some(DependencyDeadEnd::Archived),
            TaskStatus::Rejected => Some(DependencyDeadEnd::Rejected),
            TaskStatus::Proposed
            | TaskStatus::Backlog
            | TaskStatus::InProgress
            | TaskStatus::Review
            | TaskStatus::Done
            | TaskStatus::Blocked
            | TaskStatus::Someday => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

impl Display for TaskPriority {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TaskPriority::Low => "low",
            TaskPriority::Medium => "medium",
            TaskPriority::High => "high",
            TaskPriority::Critical => "critical",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TaskPriority {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(TaskPriority::Low),
            "medium" => Ok(TaskPriority::Medium),
            "high" => Ok(TaskPriority::High),
            "critical" => Ok(TaskPriority::Critical),
            other => Err(format!("unknown task priority: {other}")),
        }
    }
}

/// How much work a task is expected to take.
///
/// `Low` / `Medium` / `Hard` are operator assessments. `Unassessed` is the
/// explicit non-answer for automated creation (auto-task mint, unlabeled
/// import). It is never a silent stand-in for `Medium`. Human and agent
/// create *and update* surfaces accept only assessed values, so an agent
/// cannot satisfy the create assessment and clear it on the next update.
/// Persisted `Task.complexity` stays `Option` so the historical unlabeled set
/// remains `None`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Low,
    Medium,
    Hard,
    /// Explicit non-answer for automated creation. Not offered on CLI
    /// `--complexity` (clap skips it) and rejected on human/agent create and
    /// update surfaces so agents cannot dodge an assessment.
    #[cfg_attr(feature = "clap", value(skip))]
    Unassessed,
}

impl Display for TaskComplexity {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for TaskComplexity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(TaskComplexity::Low),
            "medium" => Ok(TaskComplexity::Medium),
            "hard" => Ok(TaskComplexity::Hard),
            "unassessed" => Ok(TaskComplexity::Unassessed),
            other => Err(format!("unknown task complexity: {other}")),
        }
    }
}

impl TaskComplexity {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskComplexity::Low => "low",
            TaskComplexity::Medium => "medium",
            TaskComplexity::Hard => "hard",
            TaskComplexity::Unassessed => "unassessed",
        }
    }

    /// `low` / `medium` / `hard` — values an operator or agent may assign.
    pub fn is_assessed(self) -> bool {
        !matches!(self, TaskComplexity::Unassessed)
    }

    /// Reject [`TaskComplexity::Unassessed`] on human/agent create and update
    /// surfaces.
    pub fn require_assessed(self) -> Result<Self, String> {
        if self.is_assessed() {
            Ok(self)
        } else {
            Err(
                "complexity must be an assessed value (low, medium, or hard); \
                 unassessed is reserved for automated creation"
                    .to_string(),
            )
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Feature,
    /// An attributable defect; lineage is recorded through typed task relations.
    Bug,
    Refactor,
    Chore,
}

impl Display for TaskType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TaskType::Feature => "feature",
            TaskType::Bug => "bug",
            TaskType::Refactor => "refactor",
            TaskType::Chore => "chore",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TaskType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "feature" => Ok(TaskType::Feature),
            "bug" => Ok(TaskType::Bug),
            "refactor" => Ok(TaskType::Refactor),
            "chore" => Ok(TaskType::Chore),
            other => Err(format!(
                "unknown task type: {other} (valid types: {})",
                TaskType::valid_names().join(", ")
            )),
        }
    }
}

impl TaskType {
    pub fn valid_names() -> &'static [&'static str] {
        &["feature", "bug", "refactor", "chore"]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskComment {
    pub at: DateTime<Utc>,
    pub by: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskHistoryEntry {
    pub at: DateTime<Utc>,
    pub by: String,
    pub event: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtifact {
    pub path: String,
    #[serde(default)]
    pub content: Vec<u8>,
    #[serde(default = "default_task_artifact_media_type")]
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

impl TaskArtifact {
    pub fn from_text(path: impl Into<String>, content: impl Into<String>) -> Self {
        let path = path.into();
        let content = content.into();
        Self {
            media_type: media_type_for_artifact_path(&path).to_string(),
            path,
            content: content.into_bytes(),
            created_by: None,
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        std::str::from_utf8(&self.content).ok()
    }
}

fn default_task_artifact_media_type() -> String {
    "application/octet-stream".to_string()
}

pub fn media_type_for_artifact_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        Some("json") => "application/json",
        Some("yaml" | "yml") => "application/yaml",
        Some("toml") => "application/toml",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Largest task artifact payload Orbit will carry inline through a tool call.
///
/// One limit governs both directions: `orbit.task.artifact.put` refuses to
/// read a larger source, and `orbit.task.artifact.get` refuses to return a
/// larger stored payload. The dashboard's streaming download route has no such
/// bound, so an oversize artifact is still reachable — just not inline.
pub const MAX_TASK_ARTIFACT_CONTENT_BYTES: u64 = 1_048_576;

/// How a stored artifact may be handed to a viewer.
///
/// This is a *rendering* decision, not an access decision: every artifact
/// remains downloadable byte-for-byte. `Opaque` only means "do not let a
/// renderer interpret these bytes", which is what keeps active content such as
/// SVG and HTML from executing in the dashboard or in an MCP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactPresentation {
    /// UTF-8 text safe to show inline.
    Text,
    /// Raster image safe to show inline; carries no active content.
    Image,
    /// Anything else — offered as a download, never interpreted.
    Opaque,
}

impl ArtifactPresentation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Opaque => "opaque",
        }
    }
}

/// Strip media-type parameters and case so `image/PNG; charset=x` and
/// `image/png` compare equal. Returns `None` for a blank media type.
pub fn normalized_artifact_media_type(media_type: &str) -> Option<String> {
    let base = media_type
        .split_once(';')
        .map_or(media_type, |(base, _params)| base)
        .trim();
    if base.is_empty() {
        return None;
    }
    Some(base.to_ascii_lowercase())
}

/// The canonical media type the HTTP artifact route will serve *inline*, or
/// `None` for a type the browser must be told to download.
///
/// This answers one narrow question: may a browser render these bytes from an
/// artifact URL? The allowlist is therefore deliberately closed —
/// `image/svg+xml` and `text/html` are an image and text respectively but are
/// also script hosts, so they are absent and fall through to the download path.
///
/// It is *not* the question [`artifact_presentation`] answers. Handing a
/// caller `text/markdown` as a UTF-8 string renders nothing and is safe even
/// though serving it inline from a URL is not.
pub fn inline_safe_artifact_media_type(media_type: &str) -> Option<&'static str> {
    match normalized_artifact_media_type(media_type).as_deref() {
        Some("application/json") => Some("application/json"),
        Some("application/toml") => Some("application/toml"),
        Some("application/yaml") => Some("application/yaml"),
        Some("image/gif") => Some("image/gif"),
        Some("image/jpeg") => Some("image/jpeg"),
        Some("image/png") => Some("image/png"),
        Some("image/webp") => Some("image/webp"),
        Some("text/csv") => Some("text/csv"),
        Some("text/plain") => Some("text/plain"),
        _ => None,
    }
}

/// Whether this media type names a raster image Orbit will render inline.
pub fn is_inline_image_media_type(media_type: &str) -> bool {
    inline_safe_artifact_media_type(media_type)
        .is_some_and(|media_type| media_type.starts_with("image/"))
}

/// Whether `content` actually begins with the signature of its declared image
/// media type.
///
/// A stored artifact's media type is derived from its file extension, so a
/// caller can attach arbitrary bytes as `diagram.png`. Checking the signature
/// before presenting an image keeps a mislabeled — possibly active — payload
/// from reaching a renderer that would trust the declared type. Bytes that do
/// not match are still retrievable; they are just classified [`Opaque`].
///
/// [`Opaque`]: ArtifactPresentation::Opaque
pub fn image_bytes_match_media_type(media_type: &str, content: &[u8]) -> bool {
    match normalized_artifact_media_type(media_type).as_deref() {
        Some("image/png") => content.starts_with(b"\x89PNG\r\n\x1a\n"),
        Some("image/jpeg") => content.starts_with(&[0xFF, 0xD8, 0xFF]),
        Some("image/gif") => content.starts_with(b"GIF87a") || content.starts_with(b"GIF89a"),
        // RIFF container with a `WEBP` form type at offset 8.
        Some("image/webp") => {
            content.len() >= 12 && content.starts_with(b"RIFF") && &content[8..12] == b"WEBP"
        }
        _ => false,
    }
}

/// Whether this media type names content a caller can be handed as a UTF-8
/// string rather than as opaque bytes.
///
/// Broader than the inline-HTTP allowlist, because returning text in a string
/// field is not rendering it — `text/markdown` is the most common artifact
/// there is, and base64-encoding it would help nobody. Active content is still
/// excluded: anything ending in `+xml` (which covers `image/svg+xml`) and
/// `text/html` stay opaque, so no surface downstream can mistake a script host
/// for prose it may safely display.
pub fn is_textual_artifact_media_type(media_type: &str) -> bool {
    let Some(base) = normalized_artifact_media_type(media_type) else {
        return false;
    };
    if base == "text/html" || base.ends_with("+xml") {
        return false;
    }
    base.starts_with("text/")
        || matches!(
            base.as_str(),
            "application/json" | "application/toml" | "application/yaml" | "application/x-yaml"
        )
        || base.ends_with("+json")
        || base.ends_with("+yaml")
}

/// Classify one stored artifact for retrieval and display.
///
/// Both non-opaque outcomes are byte-checked rather than assumed: a declared
/// image must carry a matching signature, and declared text must hold valid
/// UTF-8. Anything that fails its own claim falls back to [`Opaque`], so a
/// caller never receives lossily-decoded content or a mislabeled payload
/// dressed up as something a renderer may trust. `Opaque` still carries the
/// complete bytes — it withholds interpretation, not access.
///
/// [`Opaque`]: ArtifactPresentation::Opaque
pub fn artifact_presentation(media_type: &str, content: &[u8]) -> ArtifactPresentation {
    if is_inline_image_media_type(media_type) {
        return if image_bytes_match_media_type(media_type, content) {
            ArtifactPresentation::Image
        } else {
            ArtifactPresentation::Opaque
        };
    }
    if is_textual_artifact_media_type(media_type) && std::str::from_utf8(content).is_ok() {
        return ArtifactPresentation::Text;
    }
    ArtifactPresentation::Opaque
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTaskDependency {
    pub id: OrbitId,
    pub status: String,
}

/// Read projection of a typed relation. Persisted relations remain only the
/// relation type and target; verification is derived from the local status
/// projection so a foreign target can become locally verifiable later.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedTaskRelation {
    /// Canonical typed edge kind.
    #[serde(rename = "type")]
    pub relation_type: TaskRelationType,
    /// Referenced task or artifact ID.
    pub target: OrbitId,
    /// Local verification limitation, present only for a foreign task prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
}

impl ResolvedTaskDependency {
    pub fn label(&self) -> String {
        format!("{} [{}]", self.id, self.status)
    }
}

/// Why a `blocked_by` edge can never close on its own.
///
/// Distinct from "unmet": an unmet dependency may simply be unfinished, and
/// waiting is the correct response. A dead end will still be unmet after any
/// amount of waiting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyDeadEnd {
    /// The dependency ID resolves to no task in this workspace.
    Missing,
    /// Soft-deleted. Restorable via `orbit task restore`, not by waiting.
    Archived,
    /// Declined. Re-openable to backlog/in-progress, not by waiting.
    Rejected,
}

impl DependencyDeadEnd {
    /// Operator-facing explanation of why the edge is a dead end, phrased so
    /// the remedy is obvious from the failure message alone.
    pub fn explanation(self) -> &'static str {
        match self {
            DependencyDeadEnd::Missing => {
                "no such task in this workspace (dangling dependency; drop the blocked_by edge or restore the task)"
            }
            DependencyDeadEnd::Archived => {
                "archived (soft-deleted; restore it or drop the blocked_by edge)"
            }
            DependencyDeadEnd::Rejected => {
                "rejected (re-open it to backlog or drop the blocked_by edge)"
            }
        }
    }
}

/// A dependency edge that dispatch must refuse rather than wait on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsatisfiableTaskDependency {
    /// The task that declares the edge.
    pub task_id: OrbitId,
    /// The `blocked_by` target that can never satisfy it.
    pub dependency_id: OrbitId,
    /// The dependency's current status, or `missing` when it does not resolve.
    pub status: String,
    pub reason: DependencyDeadEnd,
}

impl UnsatisfiableTaskDependency {
    pub fn label(&self) -> String {
        format!(
            "{} blocked_by {} [{}]: {}",
            self.task_id,
            self.dependency_id,
            self.status,
            self.reason.explanation()
        )
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExternalRef {
    pub system: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

pub const GITHUB_PR_EXTERNAL_REF_SYSTEM: &str = "github-pr";

impl ExternalRef {
    pub fn try_new(system: String, id: String, url: Option<String>) -> Result<Self, TaskError> {
        let system = Self::validate_system(&system)?;

        let id = id.trim();
        if id.is_empty() {
            return Err(TaskError::Invalid(
                "external ref id must not be empty".to_string(),
            ));
        }

        let url = url
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| {
                Url::parse(&value).map_err(|error| {
                    TaskError::Invalid(format!(
                        "external ref url '{value}' must be a valid URL: {error}"
                    ))
                })?;
                Ok::<String, TaskError>(value)
            })
            .transpose()?;

        Ok(Self {
            system,
            id: id.to_string(),
            url,
        })
    }

    pub fn is_valid_system(system: &str) -> bool {
        external_ref_system_regex().is_match(system.trim())
    }

    pub fn validate_system(system: &str) -> Result<String, TaskError> {
        let system = system.trim();
        if !Self::is_valid_system(system) {
            return Err(TaskError::Invalid(format!(
                "external ref system '{system}' must match ^[a-z][a-z0-9-]*$"
            )));
        }
        Ok(system.to_string())
    }

    pub fn parse_key(raw: &str) -> Result<Self, TaskError> {
        let (system, id) = raw.split_once(':').ok_or_else(|| {
            TaskError::Invalid(
                "external ref must use <system>:<id> form, for example jira:ENG-1234".to_string(),
            )
        })?;
        Self::try_new(system.to_string(), id.to_string(), None)
    }

    pub fn github_pr(id: impl Into<String>) -> Result<Self, TaskError> {
        Self::try_new(GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(), id.into(), None)
    }

    pub fn has_key(&self, system: &str, id: &str) -> bool {
        self.system == system && self.id == id
    }
}

pub fn push_external_ref_if_missing(refs: &mut Vec<ExternalRef>, external_ref: ExternalRef) {
    if !refs
        .iter()
        .any(|candidate| candidate.has_key(&external_ref.system, &external_ref.id))
    {
        refs.push(external_ref);
    }
}

impl<'de> Deserialize<'de> for ExternalRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawExternalRef {
            system: String,
            id: String,
            #[serde(default)]
            url: Option<String>,
        }

        let raw = RawExternalRef::deserialize(deserializer)?;
        ExternalRef::try_new(raw.system, raw.id, raw.url).map_err(serde::de::Error::custom)
    }
}

fn external_ref_system_regex() -> &'static Regex {
    static SYSTEM_REGEX: OnceLock<Regex> = OnceLock::new();
    SYSTEM_REGEX.get_or_init(|| {
        Regex::new(r"^[a-z][a-z0-9-]*$").expect("external ref system regex is valid")
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    pub id: OrbitId,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Exact canonical tool names the task adds to an agent activity baseline.
    #[serde(
        default,
        deserialize_with = "deserialize_required_tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub required_tools: Vec<String>,
    #[serde(default, alias = "instructions")]
    pub plan: String,
    #[serde(default)]
    pub execution_summary: String,
    pub context_files: Vec<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub planned_by: Option<String>,
    #[serde(default)]
    pub implemented_by: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    #[serde(default)]
    pub complexity: Option<TaskComplexity>,
    pub task_type: TaskType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_refs: Vec<ExternalRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<TaskRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// Explicit named crew that owns orchestration of this task. This is
    /// attribution metadata only; execution resolution continues to use `crew`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Display for Task {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\t{}\t{}\t{}",
            self.id, self.status, self.priority, self.title
        )
    }
}

impl Task {
    pub fn github_pr_number(&self) -> Option<&str> {
        self.external_refs
            .iter()
            .find(|external_ref| external_ref.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .map(|external_ref| external_ref.id.as_str())
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.relation_target(TaskRelationType::ChildOf)
    }

    pub fn dependencies(&self) -> Vec<OrbitId> {
        self.relation_targets(TaskRelationType::BlockedBy)
    }

    pub fn source_task_id(&self) -> Option<&str> {
        self.relation_target(TaskRelationType::RegressionFrom)
    }

    fn relation_target(&self, relation_type: TaskRelationType) -> Option<&str> {
        self.relations
            .iter()
            .find(|relation| relation.relation_type == relation_type)
            .map(|relation| relation.target.as_str())
    }

    fn relation_targets(&self, relation_type: TaskRelationType) -> Vec<OrbitId> {
        self.relations
            .iter()
            .filter(|relation| relation.relation_type == relation_type)
            .map(|relation| relation.target.clone())
            .collect()
    }
}

pub fn normalize_task_dependencies(
    raw_dependencies: Vec<String>,
) -> Result<Vec<OrbitId>, TaskError> {
    let mut normalized = Vec::with_capacity(raw_dependencies.len());
    let mut seen = BTreeSet::new();
    for raw in raw_dependencies {
        let dependency = raw.trim();
        if dependency.is_empty() {
            return Err(TaskError::Invalid(
                "task dependencies must not contain empty IDs".to_string(),
            ));
        }
        if seen.insert(dependency.to_string()) {
            normalized.push(dependency.to_string());
        }
    }
    Ok(normalized)
}

pub fn normalize_task_tags(raw_tags: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(raw_tags.len());
    let mut seen = BTreeSet::new();
    for raw in raw_tags {
        let tag = raw.trim().to_lowercase();
        if !tag.is_empty() && seen.insert(tag.clone()) {
            normalized.push(tag);
        }
    }
    normalized
}

/// Canonical persisted ordering for task-scoped tool requirements.
///
/// Names are deliberately not trimmed or case-folded here: admission owns
/// validating exact canonical registry names, and malformed values must remain
/// visible until that fail-closed check. A sorted set makes every task store
/// and transport serialize the same deduplicated list.
pub fn normalize_required_tools(raw_tools: Vec<String>) -> Vec<String> {
    raw_tools
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn deserialize_required_tools<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer).map(normalize_required_tools)
}

pub fn task_matches_tags(task: &Task, required_tags: &[String]) -> bool {
    let required_tags = normalize_task_tags(required_tags.to_vec());
    if required_tags.is_empty() {
        return true;
    }

    let available = normalize_task_tags(task.tags.clone())
        .into_iter()
        .collect::<BTreeSet<_>>();
    required_tags
        .iter()
        .all(|tag| available.contains(tag.as_str()))
}

pub fn build_task_status_index(tasks: &[Task]) -> BTreeMap<OrbitId, TaskStatus> {
    tasks
        .iter()
        .map(|task| (task.id.clone(), task.status))
        .collect::<BTreeMap<_, _>>()
}

pub fn resolve_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    resolve_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Resolve dependency targets using prefix knowledge derived from one status
/// projection. Callers that inspect several tasks from the same projection
/// should create one [`TaskReferenceIndex`] and reuse it.
pub fn resolve_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskDependency> {
    task.dependencies()
        .into_iter()
        .map(|dependency_id| ResolvedTaskDependency {
            id: dependency_id.clone(),
            status: status_by_id
                .get(&dependency_id)
                .map(|status| status.to_string())
                .unwrap_or_else(|| {
                    if reference_index.is_not_verifiable_here(task, &dependency_id, status_by_id) {
                        TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()
                    } else {
                        "missing".to_string()
                    }
                }),
        })
        .collect()
}

/// Project typed relations with a marker on unresolved foreign-prefix task
/// targets. Resolvable and locally missing targets retain their stored shape.
pub fn resolve_task_relations(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskRelation> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    resolve_task_relations_with_index(task, status_by_id, &reference_index)
}

/// Resolve relation targets using prefix knowledge derived from one status
/// projection. Callers that inspect several tasks from the same projection
/// should create one [`TaskReferenceIndex`] and reuse it.
pub fn resolve_task_relations_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskRelation> {
    task.relations
        .iter()
        .map(|relation| ResolvedTaskRelation {
            relation_type: relation.relation_type,
            target: relation.target.clone(),
            verification: reference_index
                .is_not_verifiable_here(task, &relation.target, status_by_id)
                .then(|| TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()),
        })
        .collect()
}

/// Prefix knowledge derived from one bounded status-index snapshot.
///
/// This is intentionally an owned, short-lived value rather than a global
/// cache: task stores may change between snapshots, and the snapshot's caller
/// already defines the correct freshness boundary.
#[derive(Debug)]
pub struct TaskReferenceIndex {
    known_prefixes: BTreeSet<String>,
    indexed_task_count: usize,
}

impl TaskReferenceIndex {
    /// Build prefix knowledge with one scan of a status-index snapshot.
    pub fn from_status_index(status_by_id: &BTreeMap<OrbitId, TaskStatus>) -> Self {
        Self {
            known_prefixes: status_by_id
                .keys()
                .filter_map(|id| task_id_prefix(id).map(ToOwned::to_owned))
                .collect(),
            indexed_task_count: status_by_id.len(),
        }
    }

    /// Number of task IDs examined while building this snapshot's prefix set.
    /// This provides bounded-work evidence without depending on wall-clock time.
    pub fn indexed_task_count(&self) -> usize {
        self.indexed_task_count
    }

    /// Whether a missing valid task target belongs to a prefix this snapshot
    /// cannot verify. The source task's prefix is local even when its task ID
    /// is absent from the projection.
    pub fn is_not_verifiable_here(
        &self,
        task: &Task,
        target: &str,
        status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    ) -> bool {
        if status_by_id.contains_key(target) || !is_valid_orb_task_id(target) {
            return false;
        }
        let Some(target_prefix) = task_id_prefix(target) else {
            return false;
        };
        if task_id_prefix(&task.id).is_some_and(|source_prefix| source_prefix == target_prefix) {
            return false;
        }
        !self.known_prefixes.contains(target_prefix)
    }
}

/// Whether a missing valid task target belongs to a prefix this status
/// projection cannot verify. The source prefix is always local for its task;
/// prefixes on projected task IDs cover additional locally registered legacy
/// or migrated partitions.
pub fn task_reference_is_not_verifiable_here(
    task: &Task,
    target: &str,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> bool {
    TaskReferenceIndex::from_status_index(status_by_id).is_not_verifiable_here(
        task,
        target,
        status_by_id,
    )
}

pub fn task_dependencies_ready(task: &Task, status_by_id: &BTreeMap<OrbitId, TaskStatus>) -> bool {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    task_dependencies_ready_with_index(task, status_by_id, &reference_index)
}

/// Check readiness using prefix knowledge derived from one status projection.
pub fn task_dependencies_ready_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> bool {
    task.dependencies().iter().all(|dependency_id| {
        status_by_id
            .get(dependency_id)
            .is_some_and(|status| status.satisfies_dependency())
            || reference_index.is_not_verifiable_here(task, dependency_id, status_by_id)
    })
}

pub fn unmet_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<ResolvedTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    unmet_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Return unmet dependencies using prefix knowledge derived from one status
/// projection.
pub fn unmet_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<ResolvedTaskDependency> {
    resolve_task_dependencies_with_index(task, status_by_id, reference_index)
        .into_iter()
        .filter(|dependency| {
            !reference_index.is_not_verifiable_here(task, &dependency.id, status_by_id)
                && status_by_id
                    .get(&dependency.id)
                    .is_none_or(|status| !status.satisfies_dependency())
        })
        .collect()
}

/// Returns the task's dependency edges that can never be satisfied by waiting.
///
/// Empty means every unmet edge is a legitimate wait, so the caller should
/// keep polling. Non-empty means polling is futile and the caller should fail
/// now, naming these edges.
pub fn unsatisfiable_task_dependencies(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
) -> Vec<UnsatisfiableTaskDependency> {
    let reference_index = TaskReferenceIndex::from_status_index(status_by_id);
    unsatisfiable_task_dependencies_with_index(task, status_by_id, &reference_index)
}

/// Return dependency dead ends using prefix knowledge derived from one status
/// projection.
pub fn unsatisfiable_task_dependencies_with_index(
    task: &Task,
    status_by_id: &BTreeMap<OrbitId, TaskStatus>,
    reference_index: &TaskReferenceIndex,
) -> Vec<UnsatisfiableTaskDependency> {
    task.dependencies()
        .into_iter()
        .filter_map(|dependency_id| {
            if reference_index.is_not_verifiable_here(task, &dependency_id, status_by_id) {
                return None;
            }
            let (status, reason) = match status_by_id.get(&dependency_id) {
                None => ("missing".to_string(), DependencyDeadEnd::Missing),
                Some(status) => (status.to_string(), status.dependency_dead_end()?),
            };
            Some(UnsatisfiableTaskDependency {
                task_id: task.id.clone(),
                dependency_id,
                status,
                reason,
            })
        })
        .collect()
}

pub fn validate_task_dependencies(
    tasks: &[Task],
    current_task_id: Option<&str>,
    dependencies: &[OrbitId],
) -> Result<(), TaskError> {
    let Some(current_task_id) = current_task_id else {
        return Ok(());
    };

    if dependencies
        .iter()
        .any(|dependency| dependency == current_task_id)
    {
        return Err(TaskError::Invalid(format!(
            "task '{current_task_id}' cannot declare a self-dependency (self-reference)"
        )));
    }

    let mut adjacency = tasks
        .iter()
        .map(|task| (task.id.clone(), task.dependencies()))
        .collect::<BTreeMap<_, _>>();
    adjacency.insert(current_task_id.to_string(), dependencies.to_vec());

    for dependency in dependencies {
        let mut visiting = BTreeSet::new();
        let mut trail = Vec::new();
        if let Some(path) = find_dependency_path(
            dependency,
            current_task_id,
            &adjacency,
            &mut visiting,
            &mut trail,
        ) {
            let mut cycle = Vec::with_capacity(path.len() + 1);
            cycle.push(current_task_id.to_string());
            cycle.extend(path);
            return Err(TaskError::Invalid(format!(
                "task dependency cycle detected: {}",
                cycle.join(" -> ")
            )));
        }
    }

    Ok(())
}

fn find_dependency_path(
    current: &str,
    target: &str,
    adjacency: &BTreeMap<OrbitId, Vec<OrbitId>>,
    visiting: &mut BTreeSet<OrbitId>,
    trail: &mut Vec<OrbitId>,
) -> Option<Vec<OrbitId>> {
    if !visiting.insert(current.to_string()) {
        return None;
    }

    trail.push(current.to_string());
    if current == target {
        return Some(trail.clone());
    }

    if let Some(next_dependencies) = adjacency.get(current) {
        for next in next_dependencies {
            if let Some(path) = find_dependency_path(next, target, adjacency, visiting, trail) {
                return Some(path);
            }
        }
    }

    trail.pop();
    visiting.remove(current);
    None
}
