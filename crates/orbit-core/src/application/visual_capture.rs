//! The implementer's visual self-check (`orbit visual capture`,
//! visual-review design §5): run the capture driver (§4) against the
//! current working tree for a task's mocks, writing `mock.png`,
//! `capture.png` and `side-by-side.png` per mock plus `rules.json` under
//! an output directory. It attaches nothing and records nothing on the
//! task, and it works in every `visual_review.mode` — mode governs the
//! delivery review phase, not this command. Without a verified browser
//! kit or a view manifest it fails naming what is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, create_private_dir_all};
use orbit_common::process::{CapturedOutput, run_bounded};
use orbit_types::task::{
    ArtifactPresentation, MOCKS_ARTIFACT_PREFIX, MockRefusal, TaskArtifact, ViewManifestSet,
    artifact_presentation, is_mock_artifact, mock_refusals_detail, parse_mock_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::bootstrap::browser_kit::{
    KitInspection, KitLayout, PIXEL_HELPER_PACKAGE, PIXEL_HELPER_VERSION, PLAYWRIGHT_VERSION,
};

/// The shipped capture driver (§4.1), written to scratch and run with the
/// kit's `node` for each capture.
const CAPTURE_DRIVER: &str = include_str!("../../assets/visual/capture-driver.mjs");

/// The instant every capture freezes its browser clock at (§4.2); the UX
/// rules runner freezes at the same instant.
pub(crate) const FROZEN_INSTANT: &str = "2026-01-01T12:00:00.000Z";

/// One mock's capture outcome, as the driver reported it. Paths are
/// relative to the report's `out_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualCaptureMock {
    pub artifact: String,
    pub name: String,
    pub view: String,
    pub state: String,
    pub theme: String,
    pub width: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_by_side: Option<String>,
    /// `single` for the self-check; `stable`, `stable_after_retry` or
    /// `unstable` for a double capture (§4.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_region: Option<[u32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_loopback_requests: Option<Vec<String>>,
    /// When the capture was taken (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_by_side_sha256: Option<String>,
    /// `png`, or `webp` when the PNG was over the artifact limit (§4.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_lossy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_by_side_encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_by_side_lossy: Option<bool>,
    /// A `color_scheme_invariant` app rendered differently under the
    /// opposite colour preference (§7); delivery capture only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme_leak: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_scheme_region: Option<[u32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<VisualCaptureMockError>,
}

/// Why one mock produced no capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualCaptureMockError {
    /// `setup_failed` or `render_timeout`.
    pub code: String,
    pub message: String,
}

/// What `orbit visual capture` reports.
#[derive(Debug, Clone, Serialize)]
pub struct VisualCaptureReport {
    pub schema_version: u32,
    pub task_id: String,
    pub out_dir: String,
    pub outcome: String,
    pub kit: Value,
    pub frozen_instant: String,
    pub captures_per_mock: u32,
    pub mocks: Vec<VisualCaptureMock>,
    /// `rules.json`, relative to `out_dir`.
    pub rules: String,
    pub rules_summary: Value,
}

/// What one capture-driver run reported on its last stdout line.
#[derive(Deserialize)]
pub(crate) struct DriverReport {
    pub(crate) outcome: String,
    pub(crate) kit: Value,
    pub(crate) frozen_instant: String,
    pub(crate) captures_per_mock: u32,
    pub(crate) mocks: Vec<VisualCaptureMock>,
    pub(crate) rules: String,
    pub(crate) rules_summary: Value,
}

impl OrbitRuntime {
    /// Capture the current working tree for `task_id`'s mocks (or only
    /// `mock`, one `mocks/…` artifact path) into `out` (default
    /// `.orbit/tmp/visual/<task-id>` under the checkout). Writes files
    /// only under the output directory and the run's scratch; the task
    /// record is read, never written.
    pub fn visual_capture(
        &self,
        task_id: &str,
        mock: Option<&str>,
        out: Option<&str>,
    ) -> Result<VisualCaptureReport, OrbitError> {
        let repo_root = self.paths().repo_root.clone();
        let task = self.get_task(task_id)?;
        let settings = self.visual_review_settings().clone();
        if settings.manifests.is_empty() {
            return Err(OrbitError::InvalidInput(
                "no_view_manifest: no view manifest is configured; set \
                 `visual_review.manifests` to the repository's view manifest(s) \
                 so mock names can be resolved to views"
                    .to_string(),
            ));
        }
        let manifests = load_working_tree_manifests(&repo_root, &settings.manifests)?;
        let artifacts = self.get_task_artifacts(&task.id)?;
        let resolved = resolve_capture_mocks(&artifacts, mock, &manifests, settings.default_width)
            .map_err(OrbitError::InvalidInput)?;

        let layout = KitLayout::current_user()?;
        let inspection = KitInspection::inspect(&layout);
        require_verified_kit(&layout, &inspection)?;

        let out_dir = match out {
            Some(out) if !out.trim().is_empty() => {
                let path = PathBuf::from(out.trim());
                if path.is_absolute() {
                    path
                } else {
                    repo_root.join(path)
                }
            }
            _ => repo_root.join(".orbit/tmp/visual").join(&task.id),
        };
        create_private_dir_all(&out_dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", out_dir.display())))?;

        let scratch = repo_root.join(".orbit/tmp/visual-capture").join(&task.id);
        let input_dir = scratch.join("input");
        create_private_dir_all(&input_dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", input_dir.display())))?;
        let mut request_mocks = Vec::new();
        for (resolved_mock, artifact) in &resolved {
            request_mocks.push(driver_mock_request(
                resolved_mock,
                &artifact.content,
                &manifests,
                &input_dir,
            )?);
        }

        let request = json!({
            "repo_root": repo_root.display().to_string(),
            "out_dir": out_dir.display().to_string(),
            "kit_npm_project": layout.npm_project().display().to_string(),
            "captures_per_mock": 1,
            "frozen_instant": FROZEN_INSTANT,
            "mocks": request_mocks,
            "rules_dir": rules_dir_in(&repo_root),
        });
        let driver = run_capture_driver(
            &layout,
            &inspection,
            &scratch,
            &request,
            &repo_root,
            resolved.len(),
        )?;
        Ok(VisualCaptureReport {
            schema_version: 1,
            task_id: task.id,
            out_dir: out_dir.display().to_string(),
            outcome: driver.outcome,
            kit: driver.kit,
            frozen_instant: driver.frozen_instant,
            captures_per_mock: driver.captures_per_mock,
            mocks: driver.mocks,
            rules: driver.rules,
            rules_summary: driver.rules_summary,
        })
    }
}

/// The driver request entry for one resolved mock: its bytes written under
/// `input_dir`, and the view and app definitions it renders with. Shared by
/// the self-check and the delivery capture (§4.1: one driver, one request).
pub(crate) fn driver_mock_request(
    resolved_mock: &orbit_types::task::ResolvedMock,
    content: &[u8],
    manifests: &ViewManifestSet,
    input_dir: &Path,
) -> Result<Value, OrbitError> {
    let view = manifests.views.get(&resolved_mock.view).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "unknown_mock_view: view `{}` is not in the view manifest",
            resolved_mock.view
        ))
    })?;
    let app = manifests.apps.get(&view.app).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "view `{}` names unknown app `{}` in the view manifest",
            view.id, view.app
        ))
    })?;
    if app.setup.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "app `{}` in the view manifest declares no `setup` command, so view \
             `{}` cannot be served for capture",
            view.app, view.id
        )));
    }
    let name = mock_output_name(&resolved_mock.artifact);
    let extension = Path::new(&resolved_mock.artifact)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("png")
        .to_string();
    let mock_file = input_dir.join(format!("{name}.{extension}"));
    write_file(&mock_file, content)?;
    Ok(json!({
        "artifact": resolved_mock.artifact,
        "name": name,
        "mock_file": mock_file.display().to_string(),
        "mock_ext": extension,
        "view": resolved_mock.view,
        "state": resolved_mock.state,
        "theme": resolved_mock.theme,
        "width": resolved_mock.width,
        "view_def": {
            "id": view.id,
            "app": view.app,
            "title": view.title,
            "route": view.route,
            "fixture": view.fixture,
            "ready": view.ready,
            "responsive": view.responsive,
            "states": view.states,
            "clip": view.clip,
            "mask": view.mask,
        },
        "app_def": {
            "name": view.app,
            "setup": app.setup,
            "setup_timeout_s": app.setup_timeout_s,
            "themes": app.themes,
            "color_scheme_invariant": app.color_scheme_invariant,
        },
    }))
}

/// The checkout's UI rules directory for the driver, when it has rules.
pub(crate) fn rules_dir_in(checkout: &Path) -> Option<String> {
    let rules_dir = checkout.join("scripts/ux-rules");
    rules_dir
        .join("ui")
        .is_dir()
        .then(|| rules_dir.display().to_string())
}

/// Run the shipped capture driver with the kit's `node` over `request`
/// (written under `scratch`), from `cwd`, and read its report.
pub(crate) fn run_capture_driver(
    layout: &KitLayout,
    inspection: &KitInspection,
    scratch: &Path,
    request: &Value,
    cwd: &Path,
    mock_count: usize,
) -> Result<DriverReport, OrbitError> {
    create_private_dir_all(scratch)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", scratch.display())))?;
    let driver_path = scratch.join("capture-driver.mjs");
    write_file(&driver_path, CAPTURE_DRIVER.as_bytes())?;
    let request_path = scratch.join("request.json");
    write_file(
        &request_path,
        serde_json::to_string_pretty(request)
            .map_err(|error| OrbitError::Execution(format!("encode capture request: {error}")))?
            .as_bytes(),
    )?;
    // Source env.sh in the driver's own shell, exactly as a worker's
    // check does, so the kit's libraries and fonts apply only there.
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            r#". "$1" && exec node "$2" --request "$3""#,
            "orbit-visual-capture",
        ])
        .arg(layout.env_script())
        .arg(&driver_path)
        .arg(&request_path)
        .current_dir(cwd)
        .env("ORBIT_VISUAL_FROZEN_INSTANT", FROZEN_INSTANT);
    for (name, value) in inspection.worker_env(layout) {
        command.env(name, value);
    }
    // The driver's own per-step timeouts bound each setup and capture;
    // this outer deadline only stops a wedged driver from holding the
    // command open forever.
    let deadline = Duration::from_secs(300 + 300 * mock_count as u64);
    let output = run_bounded(&mut command, deadline)
        .map_err(|error| OrbitError::Execution(format!("run capture driver: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(report_line) = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
    else {
        return Err(driver_failure(&output));
    };
    let driver: DriverReport =
        serde_json::from_str(report_line).map_err(|_| driver_failure(&output))?;
    if !output.status.success() {
        return Err(driver_failure(&output));
    }
    Ok(driver)
}

/// Why this host's kit cannot capture (§6.1), as `(reason, detail)`, or
/// `None` when it is verified at this build's pin.
pub(crate) fn kit_not_established(
    layout: &KitLayout,
    inspection: &KitInspection,
) -> Option<(&'static str, String)> {
    if !inspection.present {
        return Some((
            "browser_kit_missing",
            format!(
                "no browser kit at {}; run `orbit browser-kit install` on this host, then retry",
                layout.root().display()
            ),
        ));
    }
    if !inspection.matches_pin() {
        return Some((
            "kit_pin_mismatch",
            format!(
                "the kit has Playwright {}; this Orbit release pins {PLAYWRIGHT_VERSION}; run \
                 `orbit browser-kit install`",
                inspection
                    .playwright_version
                    .as_deref()
                    .unwrap_or("none installed")
            ),
        ));
    }
    if !inspection.is_verified() {
        return Some((
            "browser_kit_missing",
            format!(
                "the browser kit at {} has not passed `orbit browser-kit verify` at its \
                 installed version; run `orbit browser-kit install` (it verifies), then retry",
                layout.root().display()
            ),
        ));
    }
    if !inspection.pixel_helper_verified() {
        return Some((
            "browser_kit_missing",
            format!(
                "the kit's pixel-compare helper {PIXEL_HELPER_PACKAGE} is {} and this release \
                 pins {PIXEL_HELPER_VERSION} verified by the kit smoke test; run `orbit \
                 browser-kit install`, then retry",
                inspection
                    .pixel_helper_version
                    .as_deref()
                    .unwrap_or("missing")
            ),
        ));
    }
    None
}

/// The manifests named in `visual_review.manifests`, read from the
/// working tree: the self-check renders uncommitted changes, so it
/// resolves views against the manifest as edited, not the base revision
/// admission validates against.
fn load_working_tree_manifests(
    repo_root: &Path,
    manifest_paths: &[String],
) -> Result<ViewManifestSet, OrbitError> {
    let mut set = ViewManifestSet::default();
    for path in manifest_paths {
        let file = repo_root.join(path);
        let text = std::fs::read_to_string(&file).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "no_view_manifest: view manifest `{path}` configured in \
                 `visual_review.manifests` cannot be read from the working tree \
                 ({error}); create it or fix the configured path"
            ))
        })?;
        set.add_toml(path, &text)
            .map_err(OrbitError::InvalidInput)?;
    }
    if set.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "no_view_manifest: the configured view manifest(s) ({}) declare no view",
            manifest_paths.join(", ")
        )));
    }
    Ok(set)
}

/// The task's mocks (or the one named by `only`) resolved against the
/// manifest, each with its stored artifact. Unlike admission this does
/// not require the `verify-mock-ui` tag: the self-check is a tool for
/// whoever runs it, in every mode.
fn resolve_capture_mocks(
    artifacts: &[TaskArtifact],
    only: Option<&str>,
    manifests: &ViewManifestSet,
    default_width: u32,
) -> Result<Vec<(orbit_types::task::ResolvedMock, TaskArtifact)>, String> {
    let mut selected: Vec<&TaskArtifact> = artifacts
        .iter()
        .filter(|artifact| is_mock_artifact(&artifact.path))
        .collect();
    selected.sort_by(|left, right| left.path.cmp(&right.path));
    if let Some(only) = only {
        let only = only.trim();
        if !only.starts_with(MOCKS_ARTIFACT_PREFIX) {
            return Err(format!(
                "`mock` must name a `mocks/` artifact of the task, got `{only}`"
            ));
        }
        selected.retain(|artifact| artifact.path == only);
        if selected.is_empty() {
            return Err(format!("the task has no mock artifact `{only}`"));
        }
    }
    if selected.is_empty() {
        return Err("no_mocks: the task has no `mocks/` artifact to capture".to_string());
    }
    let mut resolved = Vec::new();
    let mut refusals: Vec<MockRefusal> = Vec::new();
    for artifact in selected {
        match parse_mock_name(&artifact.path, manifests, default_width) {
            Ok(mock) => {
                if artifact_presentation(&artifact.media_type, &artifact.content)
                    == ArtifactPresentation::Image
                {
                    resolved.push((mock, artifact.clone()));
                } else {
                    refusals.push(MockRefusal {
                        code: orbit_types::task::MockRefusalCode::InvalidMockImage,
                        artifact: Some(artifact.path.clone()),
                        detail: "artifact bytes are not a byte-checked raster image for its \
                                 extension (export HTML/SVG mocks to PNG, JPG or WebP)"
                            .to_string(),
                    });
                }
            }
            Err(refusal) => refusals.push(refusal),
        }
    }
    if refusals.is_empty() {
        Ok(resolved)
    } else {
        Err(mock_refusals_detail(&refusals))
    }
}

/// `<mock-name>` for output paths: the artifact name without `mocks/`
/// and its extension (§4.5).
pub(crate) fn mock_output_name(artifact: &str) -> String {
    let file = artifact
        .strip_prefix(MOCKS_ARTIFACT_PREFIX)
        .unwrap_or(artifact);
    match file.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => file.to_string(),
    }
}

fn require_verified_kit(layout: &KitLayout, inspection: &KitInspection) -> Result<(), OrbitError> {
    match kit_not_established(layout, inspection) {
        Some((reason, detail)) => Err(OrbitError::InvalidInput(format!("{reason}: {detail}"))),
        None => Ok(()),
    }
}

fn driver_failure(output: &CapturedOutput) -> OrbitError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr
        .chars()
        .skip(stderr.chars().count().saturating_sub(2000))
        .collect();
    OrbitError::Execution(format!(
        "capture driver failed ({}): {}",
        output.status,
        tail.trim()
    ))
}

pub(crate) fn write_file(path: &Path, content: &[u8]) -> Result<(), OrbitError> {
    atomic_write_bytes(path, content)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}
