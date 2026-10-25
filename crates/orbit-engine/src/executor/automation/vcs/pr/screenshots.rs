//! The `## Screenshots` section of a single-task pull request body.
//!
//! A task's `screenshots/<view>-<theme>-<width>[-before|-after].<ext>`
//! artifacts are committed to the task's permanent evidence ref on `origin`
//! (`refs/orbit/evidence/<task-id>`), and the body links each image by that
//! commit's sha, so the images keep resolving after the pull request merges
//! and its branch is deleted. `pr.screenshot_section` and the task's tags
//! decide whether the section is rendered at all.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, Task, TaskArtifact};
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
/// section; with it off nothing is read or pushed.
pub(super) fn publish_screenshots<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    tasks: &[Task],
    pr_config: &PrConfig,
) -> Result<Screenshots, OrbitError> {
    let [task] = tasks else {
        return Ok(Screenshots::Off);
    };
    if !pr_config.conventions.screenshot_section.applies(&task.tags) {
        return Ok(Screenshots::Off);
    }
    let shots = collect(&task.id, host.get_task_artifacts(&task.id)?);
    if shots.is_empty() {
        return Ok(Screenshots::Missing);
    }
    let tree_sha = write_tree(workspace_path, &shots)?;
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
        "published pull request screenshots to the task's evidence ref"
    );
    Ok(Screenshots::Table(render_table(
        repository_url,
        commit_sha,
        &shots,
    )))
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

/// Write the shots as blobs and return the tree holding exactly
/// `screenshots/<file>` for each.
fn write_tree(workspace_path: &Path, shots: &[Shot]) -> Result<String, OrbitError> {
    let mut entries = String::new();
    for shot in shots {
        let blob = git_stdin(
            workspace_path,
            &["hash-object", "-w", "--stdin"],
            &shot.content,
        )?;
        entries.push_str(&format!("100644 blob {blob}\t{}\n", shot.file));
    }
    let screenshots = git_stdin(workspace_path, &["mktree"], entries.as_bytes())?;
    let directory = SCREENSHOT_ARTIFACT_PREFIX.trim_end_matches('/');
    git_stdin(
        workspace_path,
        &["mktree"],
        format!("040000 tree {screenshots}\t{directory}\n").as_bytes(),
    )
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
