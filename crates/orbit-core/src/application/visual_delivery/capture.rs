//! `visual_capture`: the capture of record (§4). One capture per budgeted
//! mock of the final candidate, in a detached checkout of that commit, at
//! the mock digests the delivery froze, with a side-by-side image each, and
//! the system-written `visual/capture.json`. A host that cannot capture
//! records `not_established` with its reason, never a pass.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::io::create_private_dir_all;
use orbit_common::process::run_bounded;
use orbit_engine::DispatchError;
use orbit_types::task::{
    ResolvedMock, TaskArtifact, VISUAL_CAPTURE_ARTIFACT, VISUAL_MAX_IMAGE_BYTES, ViewManifestSet,
    VisualCaptureError, VisualCaptureImage, VisualCaptureOutcome, VisualCaptureRecord,
    VisualCapturedMock, VisualKitPin, VisualNotEstablished, VisualReviewContract, mock_sha256_hex,
    visual_review_summary,
};
use serde_json::{Value, json};

use super::contract::MockDigestSource;
use super::{
    attach_system_artifacts, json_artifact, required_string, step_task_ids, system_artifact,
};
use crate::OrbitRuntime;
use crate::application::visual_capture::{
    DriverReport, FROZEN_INSTANT, VisualCaptureMock, driver_mock_request, kit_not_established,
    mock_output_name, rules_dir_in, run_capture_driver,
};
use crate::bootstrap::browser_kit::{KitInspection, KitLayout};

/// One task's capture, as the step reports it and later steps read it.
struct TaskCapture {
    task_id: String,
    record: VisualCaptureRecord,
    capture_sha256: String,
    scratch: PathBuf,
    require_established: bool,
}

/// Run the capture of record for every task of the step whose visual
/// review applies. `applies: false` (and nothing written) for every other
/// delivery, so it runs exactly as before.
pub(crate) fn visual_capture_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    // A `no-diff-expected` skip delivers no candidate to capture.
    if input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Ok(capture_output(&[]));
    }
    let workspace = PathBuf::from(required_string(input, action, "workspace_path")?);
    let run_id = input
        .get("job_run_id")
        .and_then(Value::as_str)
        .filter(|run| !run.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            runtime
                .worker_invocation()
                .map(|binding| binding.bound_run_id.clone())
        })
        .unwrap_or_else(|| "run".to_string());
    let mut captures = Vec::new();
    for task_id in step_task_ids(input) {
        let capture = capture_task(runtime, &task_id, &workspace, &run_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        if let Some(capture) = capture {
            captures.push(capture);
        }
    }
    Ok(capture_output(&captures))
}

fn capture_output(captures: &[TaskCapture]) -> Value {
    let entries: Vec<Value> = captures
        .iter()
        .map(|capture| {
            json!({
                "task_id": capture.task_id,
                "outcome": match capture.record.outcome {
                    VisualCaptureOutcome::Captured => "captured",
                    VisualCaptureOutcome::NotEstablished => "not_established",
                },
                "mode": capture.record.mode.as_str(),
                "candidate_sha": capture.record.candidate_sha,
                "capture_artifact": VISUAL_CAPTURE_ARTIFACT,
                "capture_sha256": capture.capture_sha256,
                "scratch_dir": capture.scratch.display().to_string(),
                "require_established": capture.require_established,
                "summary": visual_review_summary(&capture.record, None, None),
            })
        })
        .collect();
    let mut output = json!({
        "applies": !entries.is_empty(),
        "captures": entries,
    });
    // A claim binds one task: its handoff names this capture directly.
    if let [single] = captures
        && let Some(object) = output.as_object_mut()
    {
        object.insert("task_id".into(), json!(single.task_id));
        object.insert("capture_artifact".into(), json!(VISUAL_CAPTURE_ARTIFACT));
        object.insert("capture_sha256".into(), json!(single.capture_sha256));
    }
    output
}

fn capture_task(
    runtime: &OrbitRuntime,
    task_id: &str,
    workspace: &Path,
    run_id: &str,
) -> Result<Option<TaskCapture>, OrbitError> {
    let mut unreadable = None;
    let contract = if runtime.worker_invocation().is_some() {
        runtime.claimed_visual_review(task_id)?
    } else {
        let task = runtime.get_task(task_id)?;
        match runtime.visual_review_contract_for(&task, MockDigestSource::Frozen) {
            Ok(contract) => contract,
            // Mocks or manifests that no longer pass admission are recorded
            // as a capture that is not established, never skipped.
            Err(detail) => {
                let settings = runtime.visual_review_settings();
                unreadable = Some(detail);
                Some(VisualReviewContract {
                    task_id: task.id.clone(),
                    mode: settings.mode,
                    default_width: settings.default_width,
                    max_mocks: settings.max_mocks,
                    reviewer: settings.reviewer,
                    max_images: settings.max_images,
                    max_image_tokens: settings.max_image_tokens,
                    max_findings: settings.max_findings,
                    require_established: settings.require_established,
                    manifests: Vec::new(),
                    mocks: Vec::new(),
                })
            }
        }
    };
    let Some(contract) = contract else {
        return Ok(None);
    };
    let candidate_sha = crate::application::tag_instructions::commit_at(workspace, "HEAD")
        .map_err(|reason| OrbitError::Execution(format!("candidate head: {reason}")))?;
    let scratch = runtime
        .paths()
        .repo_root
        .join(".orbit/tmp/visual-delivery")
        .join(sanitize(run_id))
        .join(sanitize(task_id));
    let host = runtime
        .automation_machine_identity()
        .unwrap_or("local")
        .to_string();
    let (budgeted, over_budget) = contract.budgeted_mocks();
    let mut record = VisualCaptureRecord {
        schema_version: 1,
        outcome: VisualCaptureOutcome::NotEstablished,
        mode: contract.mode,
        task_id: task_id.to_string(),
        host,
        kit: None,
        candidate_sha: candidate_sha.clone(),
        manifests: contract
            .manifests
            .iter()
            .map(|manifest| manifest.reference.clone())
            .collect(),
        frozen_instant: None,
        mocks: Vec::new(),
        over_budget,
        not_established: Vec::new(),
    };
    let mut images = Vec::new();
    let kit = match unreadable {
        Some(detail) => Err(VisualNotEstablished {
            reason: "capture_failed".to_string(),
            detail: format!("the visual review contract cannot be read: {detail}"),
        }),
        None => established_kit(),
    };
    match kit {
        Err(reason) => record.not_established.push(reason),
        Ok((layout, inspection)) => {
            let mocks: Vec<_> = budgeted.into_iter().cloned().collect();
            match capture_mocks(
                runtime,
                &contract,
                &mocks,
                workspace,
                &candidate_sha,
                &scratch,
                &layout,
                &inspection,
            ) {
                Ok(captured) => {
                    record.kit = Some(captured.kit);
                    record.frozen_instant = Some(FROZEN_INSTANT.to_string());
                    record.mocks = captured.mocks;
                    images = captured.images;
                    if record.mocks.iter().any(|mock| mock.capture.is_some()) {
                        record.outcome = VisualCaptureOutcome::Captured;
                    } else if !record.mocks.is_empty()
                        && record.mocks.iter().all(|mock| {
                            mock.error
                                .as_ref()
                                .is_some_and(|error| error.code == "setup_failed")
                        })
                    {
                        record.not_established.push(VisualNotEstablished {
                            reason: "setup_failed".to_string(),
                            detail: "every app's candidate setup failed before rendering"
                                .to_string(),
                        });
                    } else {
                        record.not_established.push(VisualNotEstablished {
                            reason: "capture_failed".to_string(),
                            detail: "no mock produced an attachable capture".to_string(),
                        });
                    }
                }
                Err(error) => record.not_established.push(VisualNotEstablished {
                    reason: "capture_failed".to_string(),
                    detail: error.to_string(),
                }),
            }
        }
    }
    let capture_artifact = json_artifact(VISUAL_CAPTURE_ARTIFACT, &record)?;
    let capture_sha256 = mock_sha256_hex(&capture_artifact.content);
    // Images first: the record names only images already attached.
    attach_system_artifacts(runtime, task_id, images)?;
    attach_system_artifacts(runtime, task_id, vec![capture_artifact])?;
    Ok(Some(TaskCapture {
        task_id: task_id.to_string(),
        record,
        capture_sha256,
        scratch,
        require_established: contract.require_established,
    }))
}

/// This host's kit, when it is verified at this build's pin.
fn established_kit() -> Result<(KitLayout, KitInspection), VisualNotEstablished> {
    let layout = KitLayout::current_user().map_err(|error| VisualNotEstablished {
        reason: "browser_kit_missing".to_string(),
        detail: error.to_string(),
    })?;
    let inspection = KitInspection::inspect(&layout);
    match kit_not_established(&layout, &inspection) {
        Some((reason, detail)) => Err(VisualNotEstablished {
            reason: reason.to_string(),
            detail,
        }),
        None => Ok((layout, inspection)),
    }
}

struct CapturedMocks {
    kit: VisualKitPin,
    mocks: Vec<VisualCapturedMock>,
    images: Vec<TaskArtifact>,
}

#[allow(clippy::too_many_arguments)]
fn capture_mocks(
    runtime: &OrbitRuntime,
    contract: &VisualReviewContract,
    mocks: &[orbit_types::task::VisualMockDigest],
    workspace: &Path,
    candidate_sha: &str,
    scratch: &Path,
    layout: &KitLayout,
    inspection: &KitInspection,
) -> Result<CapturedMocks, OrbitError> {
    let mut manifests = ViewManifestSet::default();
    for manifest in &contract.manifests {
        manifests
            .add_toml(&manifest.reference.path, &manifest.content)
            .map_err(OrbitError::InvalidInput)?;
    }
    let input_dir = scratch.join("input");
    let out_dir = scratch.join("out");
    for dir in [&input_dir, &out_dir] {
        create_private_dir_all(dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    }
    // The mocks as stored now: a replaced mock is captured as stored and
    // refused by `visual_settle` against the frozen digest.
    let mut request_mocks = Vec::new();
    let mut stored_digest = BTreeMap::new();
    let mut missing = Vec::new();
    for mock in mocks {
        let Some(artifact) = runtime.get_task_artifact(&contract.task_id, &mock.artifact)? else {
            missing.push(mock);
            continue;
        };
        stored_digest.insert(mock.artifact.clone(), mock_sha256_hex(&artifact.content));
        let resolved = ResolvedMock {
            artifact: mock.artifact.clone(),
            view: mock.view.clone(),
            state: mock.state.clone(),
            theme: mock.theme.clone(),
            width: mock.width,
        };
        request_mocks.push(driver_mock_request(
            &resolved,
            &artifact.content,
            &manifests,
            &input_dir,
        )?);
    }
    let tree = tree_of(workspace, candidate_sha)?;
    let checkout = DetachedCheckout::add(workspace, &scratch.join("checkout"), candidate_sha)?;
    let request = json!({
        "repo_root": checkout.path.display().to_string(),
        "out_dir": out_dir.display().to_string(),
        "kit_npm_project": layout.npm_project().display().to_string(),
        "captures_per_mock": 2,
        "max_image_bytes": VISUAL_MAX_IMAGE_BYTES - 1,
        "frozen_instant": FROZEN_INSTANT,
        "mocks": request_mocks,
        "rules_dir": rules_dir_in(&checkout.path),
    });
    let driver = if request_mocks.is_empty() {
        None
    } else {
        Some(run_capture_driver(
            layout,
            inspection,
            &scratch.join("driver"),
            &request,
            &checkout.path,
            request_mocks.len(),
        ))
    };
    checkout.remove();
    let driver = driver.transpose()?;
    let kit = kit_pin(driver.as_ref(), inspection);
    let mut captured = Vec::new();
    let mut images = Vec::new();
    for mock in missing {
        captured.push(VisualCapturedMock {
            artifact: mock.artifact.clone(),
            sha256: String::new(),
            view: mock.view.clone(),
            state: mock.state.clone(),
            theme: mock.theme.clone(),
            width: mock.width,
            capture: None,
            error: Some(VisualCaptureError {
                code: "mock_missing".to_string(),
                message: "the mock artifact is no longer stored on the task".to_string(),
                stability: None,
                diff_region: None,
                off_loopback_requests: Vec::new(),
                color_scheme_leak: None,
            }),
        });
    }
    for entry in driver.map(|driver| driver.mocks).unwrap_or_default() {
        let digest = stored_digest
            .get(&entry.artifact)
            .cloned()
            .unwrap_or_default();
        let (capture, error) = match &entry.error {
            Some(error) => (
                None,
                Some(VisualCaptureError {
                    code: error.code.clone(),
                    message: error.message.clone(),
                    stability: entry.stability.clone(),
                    diff_region: entry.diff_region,
                    off_loopback_requests: entry.off_loopback_requests.clone().unwrap_or_default(),
                    color_scheme_leak: entry.color_scheme_leak,
                }),
            ),
            None => {
                let (image, attached) =
                    capture_image(&entry, &out_dir, candidate_sha, &tree, &manifests)?;
                images.extend(attached);
                (Some(image), None)
            }
        };
        captured.push(VisualCapturedMock {
            artifact: entry.artifact.clone(),
            sha256: digest,
            view: entry.view.clone(),
            state: entry.state.clone(),
            theme: entry.theme.clone(),
            width: entry.width,
            capture,
            error,
        });
    }
    captured.sort_by(|left, right| left.artifact.cmp(&right.artifact));
    Ok(CapturedMocks {
        kit,
        mocks: captured,
        images,
    })
}

/// One rendered mock's record and the two images it attaches under
/// `visual/<mock-name>/` (§4.5).
fn capture_image(
    entry: &VisualCaptureMock,
    out_dir: &Path,
    candidate_sha: &str,
    tree: &str,
    manifests: &ViewManifestSet,
) -> Result<(VisualCaptureImage, Vec<TaskArtifact>), OrbitError> {
    let missing = |field: &str| {
        OrbitError::Execution(format!(
            "capture driver reported {} without `{field}`",
            entry.artifact
        ))
    };
    let capture_file = entry.capture.as_deref().ok_or_else(|| missing("capture"))?;
    let side_file = entry
        .side_by_side
        .as_deref()
        .ok_or_else(|| missing("side_by_side"))?;
    let name = mock_output_name(&entry.artifact);
    let artifact_path = |file: &str| {
        let leaf = Path::new(file)
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .unwrap_or(file);
        format!("visual/{name}/{leaf}")
    };
    let capture_bytes = read_output(out_dir, capture_file)?;
    let side_bytes = read_output(out_dir, side_file)?;
    let capture_path = artifact_path(capture_file);
    let side_path = artifact_path(side_file);
    let fixture = entry.fixture.clone().unwrap_or_default();
    let setup = manifests
        .views
        .get(&entry.view)
        .and_then(|view| manifests.apps.get(&view.app))
        .map(|app| app.setup.clone())
        .unwrap_or_default();
    let fixture_key =
        json!({"setup": setup, "fixture": fixture, "state": entry.state, "tree": tree});
    let image = VisualCaptureImage {
        path: capture_path.clone(),
        side_by_side: side_path.clone(),
        side_by_side_sha256: mock_sha256_hex(&side_bytes),
        commit: candidate_sha.to_string(),
        viewport: entry.viewport.unwrap_or([entry.width, 0]),
        device_scale_factor: 1,
        fixture,
        fixture_sha256: mock_sha256_hex(fixture_key.to_string().as_bytes()),
        captured_at: entry
            .captured_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
        sha256: mock_sha256_hex(&capture_bytes),
        stability: entry
            .stability
            .clone()
            .unwrap_or_else(|| "stable".to_string()),
        diff_region: entry.diff_region,
        encoding: entry
            .capture_encoding
            .clone()
            .unwrap_or_else(|| "png".to_string()),
        lossy: entry.capture_lossy.unwrap_or(false),
        off_loopback_requests: entry.off_loopback_requests.clone().unwrap_or_default(),
        color_scheme_leak: entry.color_scheme_leak,
    };
    Ok((
        image,
        vec![
            system_artifact(&capture_path, capture_bytes),
            system_artifact(&side_path, side_bytes),
        ],
    ))
}

fn read_output(out_dir: &Path, file: &str) -> Result<Vec<u8>, OrbitError> {
    let path = out_dir.join(file);
    if !path.starts_with(out_dir) || file.contains("..") {
        return Err(OrbitError::Execution(format!(
            "capture driver named `{file}` outside its output directory"
        )));
    }
    std::fs::read(&path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))
}

fn kit_pin(driver: Option<&DriverReport>, inspection: &KitInspection) -> VisualKitPin {
    let reported = |field: &str| {
        driver
            .and_then(|driver| driver.kit.get(field))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let verification = inspection.verification.as_ref();
    VisualKitPin {
        playwright: reported("playwright")
            .or_else(|| inspection.playwright_version.clone())
            .unwrap_or_default(),
        browser_version: reported("browser_version")
            .or_else(|| verification.map(|verified| verified.browser_version.clone())),
        pixel_helper: inspection.pixel_helper_version.clone(),
        verified_at: verification.map(|verified| verified.verified_at.clone()),
    }
}

fn tree_of(workspace: &Path, commit: &str) -> Result<String, OrbitError> {
    let repo = git2::Repository::open(workspace)
        .map_err(|error| OrbitError::Execution(format!("open candidate: {}", error.message())))?;
    repo.revparse_single(commit)
        .and_then(|object| object.peel_to_tree())
        .map(|tree| tree.id().to_string())
        .map_err(|error| OrbitError::Execution(format!("candidate tree: {}", error.message())))
}

/// Path segments derived from ids, kept to one plain segment.
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// A detached `git worktree` of the candidate commit, so capture renders
/// the commit and nothing the delivery worktree holds beside it.
struct DetachedCheckout {
    repo: PathBuf,
    path: PathBuf,
}

impl DetachedCheckout {
    fn add(repo: &Path, path: &Path, commit: &str) -> Result<Self, OrbitError> {
        if path.exists() {
            let _ = git(
                repo,
                &["worktree", "remove", "--force", &path.display().to_string()],
            );
            let _ = git(repo, &["worktree", "prune"]);
        }
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
        }
        git(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--force",
                &path.display().to_string(),
                commit,
            ],
        )?;
        Ok(Self {
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
        })
    }

    fn remove(self) {
        let path = self.path.display().to_string();
        if let Err(error) = git(&self.repo, &["worktree", "remove", "--force", &path]) {
            tracing::warn!(%error, path, "remove visual capture checkout");
        }
        let _ = git(&self.repo, &["worktree", "prune"]);
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<(), OrbitError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    let output = run_bounded(&mut command, Duration::from_secs(300))?;
    if output.status.success() {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
