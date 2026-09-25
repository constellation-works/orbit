use super::*;

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
