//! The `## Screenshots` section of a single-task pull request body.
//!
//! A task's `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`
//! artifacts are committed to the task's permanent evidence ref on `origin`
//! (`refs/orbit/evidence/<task-id>`), and the body links each image by that
//! commit's sha, so the images keep resolving after the pull request merges
//! and its branch is deleted. `pr.screenshot_section` and the task's tags
//! decide whether the section is rendered at all.
//!
//! A `verify-mock-ui` task's capture of record (`visual/capture.json`) leads
//! the section whatever `pr.screenshot_section` says: its one-line visual
//! review summary (including why it is not established), then each mock's
//! side-by-side image, ahead of the implementer's `screenshots/`.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{
    MAX_TASK_ARTIFACT_CONTENT_BYTES, Task, TaskArtifact, VISUAL_CAPTURE_ARTIFACT,
    VISUAL_CHECKS_ARTIFACT, VISUAL_REVIEW_ARTIFACT, VISUAL_SETTLE_ARTIFACT, VisualCaptureRecord,
    VisualChecksRecord, VisualReviewRecord, VisualSettleRecord, has_verify_mock_ui_tag,
    visual_review_summary_with_review,
};
use orbit_types::workflow::{EVIDENCE_REF_PREFIX, SCREENSHOT_ARTIFACT_PREFIX};
use serde_json::{Value, json};

use crate::context::{PrConfig, RuntimeHost};

use super::super::git::git_run_bytes;
use super::super::operations;

/// Image width for a capture at most this many pixels wide: a phone-sized
/// viewport renders narrower than a desktop one.
const NARROW_VIEWPORT_MAX: u32 = 480;
const NARROW_IMAGE_WIDTH: u32 = 200;
const WIDE_IMAGE_WIDTH: u32 = 420;

/// What the body renders under the screenshots heading.
#[derive(Default)]
pub(super) enum Screenshots {
    /// The section is off for this delivery; the body is unchanged.
    #[default]
    Off,
    /// The section is on, but the task has no publishable screenshot.
    Missing,
    /// The section is on: its rendered table.
    Table(String),
}

/// Publish the task's screenshots when the section applies to this delivery,
/// and return what the body renders. Only a single-task delivery has the
/// section; with it off, and no capture of record, nothing is read or pushed.
pub(super) fn publish_screenshots<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    tasks: &[Task],
    pr_config: &PrConfig,
) -> Result<Screenshots, OrbitError> {
    let [task] = tasks else {
        return Ok(Screenshots::Off);
    };
    let section = pr_config.conventions.screenshot_section.applies(&task.tags);
    let visual_applies = has_verify_mock_ui_tag(&task.tags);
    if !section && !visual_applies {
        return Ok(Screenshots::Off);
    }
    let artifacts = host.get_task_artifacts(&task.id)?;
    let visual = visual_evidence(&task.id, &artifacts);
    let shots = if section {
        collect(&task.id, artifacts)
    } else {
        Vec::new()
    };
    let Some(visual) = visual else {
        if !section {
            return Ok(Screenshots::Off);
        }
        if shots.is_empty() {
            return Ok(Screenshots::Missing);
        }
        return publish(host, workspace_path, task, &shots, None);
    };
    if shots.is_empty() && visual.images.is_empty() {
        let mut content = visual.line();
        if section {
            content.push_str("\n\nNo screenshots attached.");
        }
        return Ok(Screenshots::Table(content));
    }
    publish(host, workspace_path, task, &shots, Some(&visual))
}

/// Push the evidence tree and render the section around it.
fn publish<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    task: &Task,
    shots: &[Shot],
    visual: Option<&VisualEvidence>,
) -> Result<Screenshots, OrbitError> {
    let mut files: Vec<(String, &[u8])> = Vec::new();
    if let Some(visual) = visual {
        for image in &visual.images {
            files.push((image.path.clone(), &image.content));
        }
    }
    for shot in shots {
        files.push((
            format!("{SCREENSHOT_ARTIFACT_PREFIX}{}", shot.file),
            &shot.content,
        ));
    }
    let tree_sha = write_tree(workspace_path, &files)?;
    let published = host.run_private_vcs_operation(
        operations::EVIDENCE_REF_PUSH,
        json!({
            "repo_root": workspace_path,
            "tree_sha": tree_sha,
            "target_ref": format!("{EVIDENCE_REF_PREFIX}{}", task.id),
            "message": format!("Screenshots for {}", task.id),
        }),
    )?;
    let field = |key: &str| {
        published
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "private automation VCS evidence push did not return '{key}'"
                ))
            })
    };
    let commit_sha = field("commit_sha")?;
    let repository_url = field("repository_url")?;
    let reused = published["reused"].as_bool().unwrap_or(false);
    tracing::info!(
        task_id = task.id,
        commit_sha,
        reused,
        screenshots = shots.len(),
        visual_images = visual.map_or(0, |visual| visual.images.len()),
        "published pull request screenshots to the task's evidence ref"
    );
    let mut content = String::new();
    if let Some(visual) = visual {
        content.push_str(&visual.render(repository_url, commit_sha));
    }
    if !shots.is_empty() {
        if !content.is_empty() {
            content.push_str("\n\n");
        }
        content.push_str(&render_table(repository_url, commit_sha, shots));
    }
    Ok(Screenshots::Table(content))
}

/// The capture of record's summary line and its side-by-side images.
struct VisualEvidence {
    summary: String,
    images: Vec<VisualImage>,
}

/// One mock's side-by-side image, published at its artifact path.
struct VisualImage {
    path: String,
    label: String,
    width: u32,
    content: Vec<u8>,
}

impl VisualEvidence {
    fn line(&self) -> String {
        format!("**Visual review** — {}", self.summary)
    }

    fn render(&self, repository_url: &str, commit_sha: &str) -> String {
        let mut content = self.line();
        if self.images.is_empty() {
            return content;
        }
        content.push_str("\n\n| Mock | Mock · Capture |\n|---|---|");
        for image in &self.images {
            content.push_str(&format!(
                "\n| {} | <img src=\"{repository_url}/blob/{commit_sha}/{}?raw=true\" width=\"{}\" \
                 alt=\"{}\"> |",
                image.label,
                image.path,
                if image.width <= NARROW_VIEWPORT_MAX {
                    NARROW_IMAGE_WIDTH * 2
                } else {
                    WIDE_IMAGE_WIDTH * 2
                },
                image.path
            ));
        }
        content
    }
}

/// The task's capture of record, when it has a readable one: never a pass
/// when it is not established, and only side-by-sides the record names at
/// the digest it recorded.
fn visual_evidence(task_id: &str, artifacts: &[TaskArtifact]) -> Option<VisualEvidence> {
    let find = |path: &str| artifacts.iter().find(|artifact| artifact.path == path);
    let capture = find(VISUAL_CAPTURE_ARTIFACT)?;
    let record = match VisualCaptureRecord::parse(&capture.content) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(task_id, %error, "pr_open skipped an unreadable visual capture record");
            return None;
        }
    };
    let checks = find(VISUAL_CHECKS_ARTIFACT)
        .and_then(|artifact| serde_json::from_slice::<VisualChecksRecord>(&artifact.content).ok())
        .filter(|checks| checks.candidate_sha == record.candidate_sha);
    let settle = find(VISUAL_SETTLE_ARTIFACT)
        .and_then(|artifact| serde_json::from_slice::<VisualSettleRecord>(&artifact.content).ok())
        .filter(|settle| settle.candidate_sha == record.candidate_sha);
    let mut images = Vec::new();
    for mock in &record.mocks {
        let Some(capture) = &mock.capture else {
            continue;
        };
        let Some(artifact) = find(&capture.side_by_side) else {
            continue;
        };
        let digest = orbit_common::security::release::sha256_hex(&artifact.content);
        let name_ok = capture.side_by_side.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with('.')
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        });
        if digest != capture.side_by_side_sha256 || !name_ok {
            tracing::warn!(
                task_id,
                artifact = capture.side_by_side,
                "pr_open skipped a side-by-side whose bytes or name the capture record does not pin"
            );
            continue;
        }
        images.push(VisualImage {
            path: capture.side_by_side.clone(),
            label: format!(
                "{}, {}, {}, {}",
                mock.view, mock.state, mock.theme, mock.width
            ),
            width: mock.width,
            content: artifact.content.clone(),
        });
    }
    let review = find(VISUAL_REVIEW_ARTIFACT)
        .and_then(|artifact| VisualReviewRecord::parse(&artifact.content).ok())
        .filter(|review| review.candidate_sha == record.candidate_sha);
    Some(VisualEvidence {
        summary: visual_review_summary_with_review(
            &record,
            checks.as_ref(),
            settle.as_ref(),
            review.as_ref(),
        ),
        images,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Before,
    After,
}

/// One publishable screenshot artifact.
struct Shot {
    /// The name under `screenshots/`, which is also its path in the commit.
    file: String,
    view: String,
    theme: String,
    width: u32,
    phase: Phase,
    content: Vec<u8>,
}

/// The task's publishable screenshots, in artifact path order. Anything that
/// is not a conventionally named image within the artifact limit is skipped
/// and logged, as is a second shot of a view, theme, width and phase already
/// taken.
fn collect(task_id: &str, artifacts: Vec<TaskArtifact>) -> Vec<Shot> {
    let mut artifacts = artifacts
        .into_iter()
        .filter(|artifact| artifact.path.starts_with(SCREENSHOT_ARTIFACT_PREFIX))
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| left.path.cmp(&right.path));
    let mut shots: Vec<Shot> = Vec::new();
    for artifact in artifacts {
        let file = &artifact.path[SCREENSHOT_ARTIFACT_PREFIX.len()..];
        let skip = |reason: &str| {
            tracing::warn!(
                task_id,
                artifact = artifact.path,
                "pr_open skipped a screenshot artifact: {reason}"
            );
        };
        if artifact.content.len() as u64 > MAX_TASK_ARTIFACT_CONTENT_BYTES {
            skip("it is larger than the task artifact limit");
            continue;
        }
        if !is_image(file, &artifact.content) {
            skip("it is not a PNG, JPEG, GIF or WebP image");
            continue;
        }
        let Some((view, theme, width, phase)) = parse_name(file) else {
            skip("its name is not <view>-<theme>-<width>[-before|-after].<ext>");
            continue;
        };
        if shots.iter().any(|shot| {
            shot.view == view && shot.theme == theme && shot.width == width && shot.phase == phase
        }) {
            skip("an earlier artifact is already that view, theme, width and phase");
            continue;
        }
        shots.push(Shot {
            file: file.to_string(),
            view: view.to_string(),
            theme: theme.to_string(),
            width,
            phase,
            content: artifact.content,
        });
    }
    shots
}

/// Whether `file` names an image by extension and its bytes start like one.
fn is_image(file: &str, content: &[u8]) -> bool {
    let Some((_, extension)) = file.rsplit_once('.') else {
        return false;
    };
    match extension.to_ascii_lowercase().as_str() {
        "png" => content.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => content.starts_with(b"\xff\xd8\xff"),
        "gif" => content.starts_with(b"GIF87a") || content.starts_with(b"GIF89a"),
        "webp" => content.len() >= 12 && &content[..4] == b"RIFF" && &content[8..12] == b"WEBP",
        _ => false,
    }
}

/// `<view>-<theme>-<width>[-before|-after].<ext>` as `(view, theme, width,
/// phase)`; no suffix is an After shot. A view may hold hyphens, so the width
/// is the last token and the theme the one before it. The name is one plain
/// path segment of ASCII letters, digits, `-`, `_` and `.`, so it is safe as
/// a tree entry and in a URL as it stands.
fn parse_name(file: &str) -> Option<(&str, &str, u32, Phase)> {
    if file.starts_with('.')
        || !file
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let (stem, _) = file.rsplit_once('.')?;
    let (stem, phase) = match (stem.strip_suffix("-before"), stem.strip_suffix("-after")) {
        (Some(stem), _) => (stem, Phase::Before),
        (None, Some(stem)) => (stem, Phase::After),
        (None, None) => (stem, Phase::After),
    };
    let (rest, width) = stem.rsplit_once('-')?;
    if width.is_empty() || !width.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let width = width.parse().ok().filter(|width| *width > 0)?;
    let (view, theme) = rest.rsplit_once('-')?;
    (!view.is_empty() && !theme.is_empty()).then_some((view, theme, width, phase))
}

/// Write `files` as blobs and return the tree holding exactly each at its
/// relative path (`screenshots/<file>`, `visual/<mock-name>/<file>`).
fn write_tree(workspace_path: &Path, files: &[(String, &[u8])]) -> Result<String, OrbitError> {
    let mut entries: BTreeMap<String, Vec<(String, &[u8])>> = BTreeMap::new();
    let mut blobs = String::new();
    for (path, content) in files {
        match path.split_once('/') {
            Some((directory, rest)) => entries
                .entry(directory.to_string())
                .or_default()
                .push((rest.to_string(), content)),
            None => {
                let blob = git_stdin(workspace_path, &["hash-object", "-w", "--stdin"], content)?;
                blobs.push_str(&format!("100644 blob {blob}\t{path}\n"));
            }
        }
    }
    for (directory, children) in entries {
        let tree = write_tree(workspace_path, &children)?;
        blobs.push_str(&format!("040000 tree {tree}\t{directory}\n"));
    }
    git_stdin(workspace_path, &["mktree"], blobs.as_bytes())
}

/// Run a local object-writing Git command over `stdin`; its trimmed output.
fn git_stdin(workspace_path: &Path, args: &[&str], stdin: &[u8]) -> Result<String, OrbitError> {
    let outcome = git_run_bytes(workspace_path, args, Some(stdin))?;
    if !outcome.success || outcome.timed_out {
        return Err(OrbitError::Execution(format!(
            "pr_open: git {} failed writing the screenshot evidence tree: {}",
            args.join(" "),
            outcome.stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&outcome.stdout).trim().to_string())
}

/// A table row's view, viewport width (wider first) and theme.
type RowKey<'a> = (&'a str, Reverse<u32>, &'a str);

/// One row per view, width and theme, views in name order and wider
/// viewports first, with a Before and an After column. The row names the
/// theme only when the shots carry more than one.
fn render_table(repository_url: &str, commit_sha: &str, shots: &[Shot]) -> String {
    let mut rows: BTreeMap<RowKey<'_>, [Option<&Shot>; 2]> = BTreeMap::new();
    for shot in shots {
        let row = rows
            .entry((&shot.view, Reverse(shot.width), &shot.theme))
            .or_default();
        row[usize::from(shot.phase == Phase::After)] = Some(shot);
    }
    let themed = shots.iter().any(|shot| shot.theme != shots[0].theme);
    let cell = |shot: Option<&Shot>| match shot {
        Some(shot) => format!(
            "<img src=\"{repository_url}/blob/{commit_sha}/{SCREENSHOT_ARTIFACT_PREFIX}{}?raw=true\" \
             width=\"{}\" alt=\"{}\">",
            shot.file,
            if shot.width <= NARROW_VIEWPORT_MAX {
                NARROW_IMAGE_WIDTH
            } else {
                WIDE_IMAGE_WIDTH
            },
            shot.file
        ),
        None => "—".to_string(),
    };
    let mut table = String::from("| | Before | After |\n|---|---|---|");
    for ((view, Reverse(width), theme), [before, after]) in rows {
        let label = if themed {
            format!("{view}, {theme}, {width}")
        } else {
            format!("{view}, {width}")
        };
        table.push_str(&format!(
            "\n| {label} | {} | {} |",
            cell(before),
            cell(after)
        ));
    }
    table
}
