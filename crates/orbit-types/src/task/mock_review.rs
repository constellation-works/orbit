//! `verify-mock-ui` mocks: name grammar, admission validation against the
//! view manifest, deliberate deviations and per-delivery mock digests
//! (visual-review design §2).
//!
//! A mock is an image task artifact under `mocks/` named
//! `mocks/<view>[.<state>][.<theme>][.<width>].<ext>`. Parts after the view
//! id are recognised by value, not position. Validation is pure: callers
//! supply the task's artifacts and the parsed manifests, and every failing
//! artifact is named in the returned refusals.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ArtifactPresentation, TaskArtifact, artifact_presentation};
use crate::identity::lower_hex;

/// Tag marking a task whose mocks are its visual specification.
pub const VERIFY_MOCK_UI_TAG: &str = "verify-mock-ui";
/// Artifact prefix holding a task's mocks.
pub const MOCKS_ARTIFACT_PREFIX: &str = "mocks/";
/// Artifact the implementer writes to declare deliberate departures.
pub const MOCK_DEVIATIONS_ARTIFACT: &str = "mock-deviations.json";
/// System artifact recording mock digests frozen at delivery admission.
pub const MOCK_DIGESTS_ARTIFACT: &str = "visual/mock-digests.json";
/// States a mock name may carry.
pub const MOCK_STATES: [&str; 4] = ["default", "empty", "loading", "error"];
/// Image extensions a mock name may carry.
pub const MOCK_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// Whether `tags` carry `verify-mock-ui` (exact, ignoring case/whitespace).
#[must_use]
pub fn has_verify_mock_ui_tag(tags: &[String]) -> bool {
    tags.iter()
        .any(|tag| tag.trim().eq_ignore_ascii_case(VERIFY_MOCK_UI_TAG))
}

/// Whether a canonical artifact path lies under `mocks/` (any ASCII case,
/// so a case-insensitive owner cannot be bypassed with `Mocks/`).
#[must_use]
pub fn is_mock_artifact(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("mocks"))
        && path.contains('/')
}

/// One app of a view manifest: only the fields mock validation reads.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestApp {
    pub themes: Vec<String>,
}

/// One view of a view manifest: only the fields mock validation reads.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestView {
    pub id: String,
    pub app: String,
    #[serde(default = "default_responsive")]
    pub responsive: bool,
    #[serde(default = "default_states")]
    pub states: Vec<String>,
}

fn default_responsive() -> bool {
    true
}
fn default_states() -> Vec<String> {
    vec!["default".to_string()]
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    schema_version: u32,
    #[serde(default)]
    app: BTreeMap<String, ManifestApp>,
    #[serde(default)]
    view: Vec<ManifestView>,
}

/// Every configured manifest merged: apps by name, views by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewManifestSet {
    pub apps: BTreeMap<String, ManifestApp>,
    pub views: BTreeMap<String, ManifestView>,
}

impl ViewManifestSet {
    /// Parse one manifest's TOML text and merge it in. Load validation
    /// mirrors the UX-rules loader: a view id containing a state or theme
    /// word as a dot-separated part is refused naming it, themes and
    /// states must be non-empty, and states must be known words.
    pub fn add_toml(&mut self, source: &str, text: &str) -> Result<(), String> {
        let raw: RawManifest =
            toml::from_str(text).map_err(|error| format!("{source}: {error}"))?;
        if raw.schema_version != 1 {
            return Err(format!(
                "{source}: schema_version {} is not supported (expected 1)",
                raw.schema_version
            ));
        }
        let mut theme_words: Vec<String> = vec!["light".into(), "dark".into()];
        for (name, app) in &raw.app {
            if app.themes.is_empty() {
                return Err(format!("{source}: app {name}: themes must not be empty"));
            }
            theme_words.extend(app.themes.iter().cloned());
        }
        for view in &raw.view {
            for part in view.id.split('.') {
                if MOCK_STATES.contains(&part) || theme_words.iter().any(|w| w == part) {
                    return Err(format!(
                        "{source}: view id {} contains state or theme word \"{part}\" as a dot-separated part",
                        view.id
                    ));
                }
            }
            if view.states.is_empty() {
                return Err(format!(
                    "{source}: view {}: states must not be empty",
                    view.id
                ));
            }
            for state in &view.states {
                if !MOCK_STATES.contains(&state.as_str()) {
                    return Err(format!("{source}: view {}: unknown state {state}", view.id));
                }
            }
            if !raw.app.contains_key(&view.app) && !self.apps.contains_key(&view.app) {
                return Err(format!(
                    "{source}: view {} names unknown app {}",
                    view.id, view.app
                ));
            }
        }
        self.apps.extend(raw.app);
        for view in raw.view {
            self.views.insert(view.id.clone(), view);
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

/// Why a `verify-mock-ui` task is not admitted (design §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockRefusalCode {
    NoViewManifest,
    NoMocks,
    InvalidMockName,
    UnknownMockView,
    UndeclaredMockVariant,
    InvalidMockImage,
}

impl MockRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoViewManifest => "no_view_manifest",
            Self::NoMocks => "no_mocks",
            Self::InvalidMockName => "invalid_mock_name",
            Self::UnknownMockView => "unknown_mock_view",
            Self::UndeclaredMockVariant => "undeclared_mock_variant",
            Self::InvalidMockImage => "invalid_mock_image",
        }
    }
}

/// One admission refusal, naming the failing artifact when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockRefusal {
    pub code: MockRefusalCode,
    pub artifact: Option<String>,
    pub detail: String,
}

impl MockRefusal {
    /// `code: artifact: detail`, the readiness line for this refusal.
    pub fn readiness_line(&self) -> String {
        match &self.artifact {
            Some(artifact) => format!("{}: {artifact}: {}", self.code.as_str(), self.detail),
            None => format!("{}: {}", self.code.as_str(), self.detail),
        }
    }
}

/// A mock name resolved against the manifest, defaults applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMock {
    pub artifact: String,
    pub view: String,
    pub state: String,
    pub theme: String,
    pub width: u32,
}

fn refusal(code: MockRefusalCode, artifact: &str, detail: String) -> MockRefusal {
    MockRefusal {
        code,
        artifact: Some(artifact.to_string()),
        detail,
    }
}

fn is_name_word(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse `path` as a mock name against `manifests`, applying defaults:
/// state `default`, the app's first theme, `default_width`.
pub fn parse_mock_name(
    path: &str,
    manifests: &ViewManifestSet,
    default_width: u32,
) -> Result<ResolvedMock, MockRefusal> {
    let invalid = |detail: String| refusal(MockRefusalCode::InvalidMockName, path, detail);
    let Some(file) = path.strip_prefix(MOCKS_ARTIFACT_PREFIX) else {
        return Err(invalid("mock is not under `mocks/`".into()));
    };
    if file.contains('/') || file.is_empty() {
        return Err(invalid(
            "expected mocks/<view>[.<state>][.<theme>][.<width>].<ext>".into(),
        ));
    }
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return Err(invalid("mock name has no extension".into()));
    };
    if !MOCK_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
        return Err(invalid(format!(
            "extension `.{ext}` is not one of {}",
            MOCK_EXTENSIONS.join(", ")
        )));
    }
    let mut parts = stem.split('.');
    let view_id = parts.next().unwrap_or_default();
    if !is_name_word(view_id) {
        return Err(invalid(format!(
            "view part `{view_id}` is not a lowercase hyphenated view id"
        )));
    }
    let Some(view) = manifests.views.get(view_id) else {
        return Err(refusal(
            MockRefusalCode::UnknownMockView,
            path,
            format!("view `{view_id}` is not in the view manifest"),
        ));
    };
    let app = manifests.apps.get(&view.app);
    let themes: &[String] = app.map_or(&[], |app| &app.themes);
    let (mut state, mut theme, mut width) = (None::<String>, None::<String>, None::<u32>);
    for part in parts {
        if MOCK_STATES.contains(&part) {
            if state.replace(part.into()).is_some() {
                return Err(invalid(format!("state part `{part}` appears twice")));
            }
        } else if let Ok(parsed) = part.parse::<u32>() {
            if parsed == 0 {
                return Err(invalid("width part `0` is not a positive width".into()));
            }
            if width.replace(parsed).is_some() {
                return Err(invalid(format!("width part `{part}` appears twice")));
            }
        } else if themes.iter().any(|candidate| candidate == part) {
            if theme.replace(part.into()).is_some() {
                return Err(invalid(format!("theme part `{part}` appears twice")));
            }
        } else if is_name_word(part) {
            return Err(refusal(
                MockRefusalCode::UndeclaredMockVariant,
                path,
                format!(
                    "part `{part}` is not a state of view `{view_id}`, a theme of app `{}`, or a width",
                    view.app
                ),
            ));
        } else {
            return Err(invalid(format!("part `{part}` does not parse")));
        }
    }
    let state = state.unwrap_or_else(|| "default".into());
    if !view.states.contains(&state) {
        return Err(refusal(
            MockRefusalCode::UndeclaredMockVariant,
            path,
            format!("state `{state}` is not declared for view `{view_id}`"),
        ));
    }
    let Some(theme) = theme.or_else(|| themes.first().cloned()) else {
        return Err(refusal(
            MockRefusalCode::UndeclaredMockVariant,
            path,
            format!("app `{}` declares no theme", view.app),
        ));
    };
    let width = width.unwrap_or(default_width);
    if width != default_width && !view.responsive {
        return Err(refusal(
            MockRefusalCode::UndeclaredMockVariant,
            path,
            format!(
                "width {width} on view `{view_id}`, which is not responsive (only the default width {default_width} is allowed)"
            ),
        ));
    }
    Ok(ResolvedMock {
        artifact: path.to_string(),
        view: view_id.to_string(),
        state,
        theme,
        width,
    })
}

/// Validate every mock of a `verify-mock-ui` task. `manifests_configured`
/// is false when `visual_review.manifests` is empty. Returns the resolved
/// mocks, or every refusal (one per failing artifact, plus task-level
/// `no_view_manifest` / `no_mocks`).
pub fn validate_task_mocks(
    tags: &[String],
    artifacts: &[TaskArtifact],
    manifests: &ViewManifestSet,
    manifests_configured: bool,
    default_width: u32,
) -> Result<Vec<ResolvedMock>, Vec<MockRefusal>> {
    if !has_verify_mock_ui_tag(tags) {
        return Ok(Vec::new());
    }
    if !manifests_configured || manifests.is_empty() {
        return Err(vec![MockRefusal {
            code: MockRefusalCode::NoViewManifest,
            artifact: None,
            detail: "no view manifest is configured (`visual_review.manifests` is empty or declares no view)".into(),
        }]);
    }
    let mocks: Vec<&TaskArtifact> = artifacts
        .iter()
        .filter(|artifact| is_mock_artifact(&artifact.path))
        .collect();
    if mocks.is_empty() {
        return Err(vec![MockRefusal {
            code: MockRefusalCode::NoMocks,
            artifact: None,
            detail: "the task has no `mocks/` artifact".into(),
        }]);
    }
    let mut resolved = Vec::new();
    let mut refusals = Vec::new();
    for artifact in mocks {
        match parse_mock_name(&artifact.path, manifests, default_width) {
            Ok(mock) => {
                if artifact_presentation(&artifact.media_type, &artifact.content)
                    == ArtifactPresentation::Image
                {
                    resolved.push(mock);
                } else {
                    refusals.push(refusal(
                        MockRefusalCode::InvalidMockImage,
                        &artifact.path,
                        "artifact bytes are not a byte-checked raster image for its extension (export HTML/SVG mocks to PNG, JPG or WebP)".into(),
                    ));
                }
            }
            Err(error) => refusals.push(error),
        }
    }
    if refusals.is_empty() {
        Ok(resolved)
    } else {
        Err(refusals)
    }
}

/// Readiness text naming every refusal, for admission surfaces.
#[must_use]
pub fn mock_refusals_detail(refusals: &[MockRefusal]) -> String {
    format!(
        "verify-mock-ui mock validation failed: {}",
        refusals
            .iter()
            .map(MockRefusal::readiness_line)
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// Basis of a deliberate deviation (design §2.5).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MockDeviationBasis {
    AcceptanceCriteria,
    DesignRules,
    Operator,
    Data,
    Infeasible,
}

/// One declared departure from a mock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MockDeviation {
    pub mock: String,
    pub element: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[u32; 4]>,
    pub reason: String,
    pub basis: MockDeviationBasis,
}

/// `mock-deviations.json` (design §2.5), written by the implementer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MockDeviations {
    pub schema_version: u32,
    #[serde(default)]
    pub deviations: Vec<MockDeviation>,
}

impl MockDeviations {
    /// Parse and validate an attachment's bytes.
    pub fn parse_attachment(content: &[u8]) -> Result<Self, String> {
        let parsed: Self = serde_json::from_slice(content).map_err(|error| error.to_string())?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "schema_version {} is not supported (expected 1)",
                self.schema_version
            ));
        }
        for (index, deviation) in self.deviations.iter().enumerate() {
            let at = format!("deviations[{index}]");
            if !deviation.mock.starts_with(MOCKS_ARTIFACT_PREFIX) {
                return Err(format!("{at}.mock must name a `mocks/` artifact"));
            }
            if deviation.element.trim().is_empty() {
                return Err(format!("{at}.element must not be empty"));
            }
            if deviation.reason.trim().is_empty() {
                return Err(format!("{at}.reason must not be empty"));
            }
            if let Some([_, _, width, height]) = deviation.region
                && (width == 0 || height == 0)
            {
                return Err(format!("{at}.region width and height must be positive"));
            }
        }
        Ok(())
    }
}

/// Mock digests frozen at delivery admission (design §2.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MockDigestRecord {
    pub schema_version: u32,
    pub mocks: Vec<MockDigest>,
}

/// One frozen mock digest with its resolved name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MockDigest {
    #[serde(flatten)]
    pub mock: ResolvedMock,
    pub sha256: String,
}

/// SHA-256 of `content`, lowercase hex (the artifact manifest encoding).
#[must_use]
pub fn mock_sha256_hex(content: &[u8]) -> String {
    lower_hex(Sha256::digest(content))
}

/// Freeze the digests of `resolved` mocks from their current bytes.
#[must_use]
pub fn record_mock_digests(
    resolved: &[ResolvedMock],
    artifacts: &[TaskArtifact],
) -> MockDigestRecord {
    MockDigestRecord {
        schema_version: 1,
        mocks: resolved
            .iter()
            .filter_map(|mock| {
                artifacts
                    .iter()
                    .find(|artifact| artifact.path == mock.artifact)
                    .map(|artifact| MockDigest {
                        mock: mock.clone(),
                        sha256: mock_sha256_hex(&artifact.content),
                    })
            })
            .collect(),
    }
}

/// Artifacts whose current bytes differ from the frozen record (or are
/// gone): a mock replaced mid-delivery, detectable as `mock_changed`.
#[must_use]
pub fn changed_mock_artifacts(
    record: &MockDigestRecord,
    artifacts: &[TaskArtifact],
) -> Vec<String> {
    record
        .mocks
        .iter()
        .filter(|digest| {
            artifacts
                .iter()
                .find(|artifact| artifact.path == digest.mock.artifact)
                .is_none_or(|artifact| mock_sha256_hex(&artifact.content) != digest.sha256)
        })
        .map(|digest| digest.mock.artifact.clone())
        .collect()
}
