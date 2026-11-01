//! `visual_review_prepare`: assemble the comparison review's input from
//! `capture.json` (§8), check its budgets before dispatch (§10) and draw
//! its crews (§9.4). The agent does not choose what to look at: this step
//! stages the images, orders them canary-first (the engine stages its
//! canary ahead of `image_input.images`), and names every mock the
//! budget kept out as not shown. A delivery with no image-capable crew
//! records `not_established: no_vision_crew`, never a pass, and dispatches
//! no agent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, create_private_dir_all};
use orbit_engine::DispatchError;
use orbit_types::identity::Crew;
use orbit_types::task::{
    MOCK_DEVIATIONS_ARTIFACT, MockDeviations, Task, TaskComplexity, VISUAL_NO_VISION_CREW,
    VisualBudgetMock, VisualCaptureOutcome, VisualCaptureRecord, VisualChecksRecord, VisualPass,
    VisualReviewContract, VisualShownForm, select_visual_budget, visual_image_dimensions,
};
use serde_json::{Value, json};

use super::contract::MockDigestSource;
use super::{read_record, required_string, step_pass};
use crate::OrbitRuntime;
use crate::application::contracts::CrewCandidate;
use crate::application::tag_instructions::read_blob_at;

/// One prepared review, as later steps read it.
struct PreparedReview {
    record: Value,
    dispatch: Option<Value>,
}

pub(crate) fn visual_review_prepare_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace = PathBuf::from(required_string(input, action, "workspace_path")?);
    let captures = input
        .get("capture")
        .and_then(|capture| capture.get("captures"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pass = step_pass(input);
    let mut records = Vec::new();
    let mut reviews = Vec::new();
    for capture in &captures {
        let prepared = prepare_task(runtime, &workspace, capture, pass)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        if let Some(dispatch) = prepared.dispatch {
            reviews.push(dispatch);
        }
        records.push(prepared.record);
    }
    Ok(json!({
        "applies": !records.is_empty(),
        "pass": pass.as_str(),
        "review_count": reviews.len(),
        "reviews": reviews,
        "records": records,
    }))
}

fn prepare_task(
    runtime: &OrbitRuntime,
    workspace: &Path,
    capture: &Value,
    pass: VisualPass,
) -> Result<PreparedReview, OrbitError> {
    let task_id = capture
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| OrbitError::InvalidInput("capture entry names no task_id".into()))?;
    let capture_artifact = pass.capture_artifact();
    let (record, _) = read_record::<VisualCaptureRecord>(runtime, task_id, &capture_artifact)?
        .ok_or_else(|| OrbitError::Execution(format!("{task_id} has no {capture_artifact}")))?;
    let contract = review_contract(runtime, task_id)?;
    let (max_mocks, reviewer_on, max_images, max_image_tokens, max_findings) = match &contract {
        Some(contract) => (
            contract.max_mocks,
            contract.reviewer,
            contract.max_images,
            contract.max_image_tokens,
            contract.max_findings,
        ),
        None => {
            let settings = runtime.visual_review_settings();
            (
                settings.max_mocks,
                settings.reviewer,
                settings.max_images,
                settings.max_image_tokens,
                settings.max_findings,
            )
        }
    };
    // The post-landing comparison measures both experiment arms the same
    // way, so it always reviews (§12.2); a delivery honours the arm's
    // `visual_review.reviewer` overlay (§12.1).
    let reviewer_on = match pass {
        VisualPass::Measure => true,
        _ => runtime.experiment_visual_reviewer(task_id, reviewer_on)?,
    };
    let task = runtime.get_task(task_id).ok();
    let deviations = declared_deviations(runtime, task_id);
    let mut base = json!({
        "task_id": task_id,
        "pass": pass.as_str(),
        "candidate_sha": record.candidate_sha,
        "mode": record.mode.as_str(),
        "budget": {
            "max_mocks": max_mocks,
            "max_images": max_images,
            "max_image_tokens": max_image_tokens,
            "max_findings": max_findings,
            "images_shown": 0,
            "estimated_image_tokens": 0,
            "shown": [],
            "not_shown": [],
        },
        "deviations": deviations,
        "reviewer_crew": Value::Null,
        "verifier_crew": Value::Null,
        "escalation_crew": Value::Null,
        "not_established": [],
    });
    let finish = |base: Value, outcome: &str| PreparedReview {
        record: with_outcome(base, outcome),
        dispatch: None,
    };
    if !reviewer_on {
        return Ok(finish(base, "reviewer_disabled"));
    }
    if record.outcome != VisualCaptureOutcome::Captured {
        base["not_established"] = json!(record.not_established);
        return Ok(finish(base, "not_established"));
    }
    // Unstable captures stay out of the model review (§4.3).
    let stable: Vec<_> = record
        .mocks
        .iter()
        .filter(|mock| {
            mock.capture
                .as_ref()
                .is_some_and(|capture| capture.stability != "unstable")
        })
        .collect();
    if stable.is_empty() {
        return Ok(finish(base, "no_stable_captures"));
    }
    let tier = task
        .as_ref()
        .and_then(|task| task.complexity)
        .filter(|tier| *tier != TaskComplexity::Unassessed)
        .unwrap_or(TaskComplexity::Medium);
    let implementer = task.as_ref().and_then(|task| task.crew.clone());
    let crews = draw_visual_crews(
        runtime,
        task_id,
        &record.candidate_sha,
        tier,
        implementer.as_deref(),
    )?;
    let Some(reviewer) = crews.reviewer else {
        base["not_established"] = json!([{
            "reason": VISUAL_NO_VISION_CREW,
            "detail": "capture and checks ran, but no crew in \
                       workflow.purpose_crews.visual_review or the task's complexity pool \
                       declares image_input without vision = false",
        }]);
        return Ok(finish(base, "not_established"));
    };
    let effective_max_images = max_images.min(reviewer.max_images);
    let dimensions = image_dimensions(runtime, task_id, &record);
    let ordered = ordered_mocks(&stable, task.as_ref());
    let budget_mocks: Vec<VisualBudgetMock> = ordered
        .iter()
        .map(|mock| VisualBudgetMock {
            mock: mock.artifact.clone(),
            mock_dimensions: dimensions.get(&mock.artifact).copied(),
            capture_dimensions: mock
                .capture
                .as_ref()
                .and_then(|capture| dimensions.get(&capture.path).copied()),
            side_by_side_dimensions: mock
                .capture
                .as_ref()
                .and_then(|capture| dimensions.get(&capture.side_by_side).copied()),
        })
        .collect();
    let selection = select_visual_budget(
        &budget_mocks,
        &reviewer.crew.assignment.provider,
        effective_max_images,
        max_image_tokens,
    );
    let scratch = capture
        .get("scratch_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join(".orbit/tmp/visual-delivery").join(task_id));
    let staged = stage_shown(
        runtime,
        task_id,
        &record,
        &selection.shown,
        &scratch.join("review"),
    )?;
    base["budget"] = json!({
        "max_mocks": max_mocks,
        "max_images": effective_max_images,
        "max_image_tokens": max_image_tokens,
        "max_findings": max_findings,
        "images_shown": selection.images,
        "estimated_image_tokens": selection.estimated_image_tokens,
        "shown": selection.shown,
        "not_shown": selection.not_shown,
    });
    base["reviewer_crew"] = json!(reviewer.crew.name);
    base["verifier_crew"] = json!(crews.verifier.as_ref().map(|crew| &crew.name));
    base["escalation_crew"] = json!(crews.escalation.as_ref().map(|crew| &crew.name));
    base["crew_source"] = json!(crews.source);
    base["catalog"] = json!(
        dimensions
            .iter()
            .map(|(path, (width, height))| (path.clone(), json!([width, height])))
            .collect::<serde_json::Map<String, Value>>()
    );
    base["scratch_dir"] = json!(scratch.display().to_string());
    let checks = read_record::<VisualChecksRecord>(runtime, task_id, &pass.checks_artifact())?
        .map(|(checks, _)| checks)
        .filter(|checks| checks.candidate_sha == record.candidate_sha);
    let dispatch = json!({
        "task_id": task_id,
        "candidate_sha": record.candidate_sha,
        "workspace_path": workspace.display().to_string(),
        "crew": reviewer.crew.name,
        "image_input": {"images": staged.paths},
        "staged_images": staged.entries,
        "shown": selection.shown,
        "not_shown": selection.not_shown,
        "max_findings": max_findings,
        "task": task_payload(runtime, task.as_ref(), task_id),
        "deviations": base["deviations"],
        "check_findings": checks.map(|checks| checks.findings).unwrap_or_default(),
        "design_rules": design_rule_excerpts(contract.as_ref(), workspace),
        "catalog": base["catalog"],
    });
    Ok(PreparedReview {
        record: with_outcome(base, "ready"),
        dispatch: Some(dispatch),
    })
}

fn with_outcome(mut record: Value, outcome: &str) -> Value {
    record["outcome"] = json!(outcome);
    record
}

/// The contract this delivery captures and reviews against: the claim's
/// on a leaf, the owner's current settings and frozen digests at home.
/// `None` only when it cannot be read, which the capture already
/// recorded; budgets then fall back to the live settings.
fn review_contract(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<Option<VisualReviewContract>, OrbitError> {
    if runtime.worker_invocation().is_some() {
        return runtime.claimed_visual_review(task_id);
    }
    let Ok(task) = runtime.get_task(task_id) else {
        return Ok(None);
    };
    Ok(runtime
        .visual_review_contract_for(&task, MockDigestSource::Frozen)
        .unwrap_or_default())
}

/// `mock-deviations.json` as prepared deviations (§2.5); an unreadable
/// or absent artifact declares none (its write was already validated).
fn declared_deviations(runtime: &OrbitRuntime, task_id: &str) -> Value {
    let parsed = runtime
        .get_task_artifact(task_id, MOCK_DEVIATIONS_ARTIFACT)
        .ok()
        .flatten()
        .and_then(|artifact| MockDeviations::parse_attachment(&artifact.content).ok());
    match parsed {
        Some(deviations) => json!(deviations.deviations),
        None => json!([]),
    }
}

/// The reviewer, verifier and escalation crews of one delivery.
struct VisualCrews {
    reviewer: Option<VisualCrew>,
    verifier: Option<Crew>,
    escalation: Option<Crew>,
    source: String,
}

struct VisualCrew {
    crew: Crew,
    max_images: u32,
}

/// Draw the reviewer from `workflow.purpose_crews.visual_review` (else
/// the complexity pool) filtered to `image_input` crews, seeded by task
/// and candidate; the verifier from the same filtered pool passing over
/// the reviewer's and the implementer's crews; and one escalation crew
/// from the next tier's filtered pool (§9.4, §11.2).
fn draw_visual_crews(
    runtime: &OrbitRuntime,
    task_id: &str,
    candidate_sha: &str,
    tier: TaskComplexity,
    implementer: Option<&str>,
) -> Result<VisualCrews, OrbitError> {
    let (pool, source) = runtime.visual_review_pool(tier)?;
    let capable = runtime.visual_image_candidates(pool)?;
    let seed = format!("visual_review:{task_id}:{candidate_sha}");
    let reviewer_candidates: Vec<CrewCandidate> = capable
        .iter()
        .map(|(candidate, _)| candidate.clone())
        .collect();
    let reviewer = runtime
        .visual_seeded_draw(reviewer_candidates, &source, &seed)?
        .map(|crew| {
            let max_images = capable
                .iter()
                .find(|(candidate, _)| candidate.crew.name == crew.name)
                .map_or(u32::MAX, |(_, declared)| declared.max_images);
            VisualCrew { crew, max_images }
        });
    let mut excluded: Vec<&str> = implementer.into_iter().collect();
    if let Some(reviewer) = &reviewer {
        excluded.push(reviewer.crew.name.as_str());
    }
    let verifier_candidates: Vec<CrewCandidate> = capable
        .iter()
        .filter(|(candidate, _)| !excluded.contains(&candidate.crew.name.as_str()))
        .map(|(candidate, _)| candidate.clone())
        .collect();
    let verifier =
        runtime.visual_seeded_draw(verifier_candidates, &source, &format!("{seed}:verify"))?;
    if let Some(verifier) = &verifier {
        excluded.push(verifier.name.as_str());
    }
    let escalation = match next_tier(tier) {
        Some(next) => {
            let (pool, next_source) = runtime.visual_review_pool(next)?;
            let capable = runtime.visual_image_candidates(pool)?;
            let candidates: Vec<CrewCandidate> = capable
                .into_iter()
                .filter(|(candidate, _)| !excluded.contains(&candidate.crew.name.as_str()))
                .map(|(candidate, _)| candidate)
                .collect();
            runtime.visual_seeded_draw(candidates, &next_source, &format!("{seed}:escalate"))?
        }
        None => None,
    };
    Ok(VisualCrews {
        reviewer,
        verifier,
        escalation,
        source,
    })
}

fn next_tier(tier: TaskComplexity) -> Option<TaskComplexity> {
    match tier {
        TaskComplexity::Low | TaskComplexity::Unassessed => Some(TaskComplexity::Medium),
        TaskComplexity::Medium => Some(TaskComplexity::Hard),
        TaskComplexity::Hard => Some(TaskComplexity::XHard),
        TaskComplexity::XHard => None,
    }
}

/// Captured mocks in §10 order: ones an acceptance criterion names
/// first, then by artifact name.
fn ordered_mocks<'a>(
    mocks: &[&'a orbit_types::task::VisualCapturedMock],
    task: Option<&Task>,
) -> Vec<&'a orbit_types::task::VisualCapturedMock> {
    let mut ordered: Vec<_> = mocks.to_vec();
    ordered.sort_by(|left, right| {
        let named = |mock: &orbit_types::task::VisualCapturedMock| {
            task.is_some_and(|task| {
                task.acceptance_criteria
                    .iter()
                    .any(|criterion| criterion.contains(&mock.artifact))
            })
        };
        named(right)
            .cmp(&named(left))
            .then_with(|| left.artifact.cmp(&right.artifact))
    });
    ordered
}

/// Dimensions of every image `record` names, from the stored bytes.
fn image_dimensions(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &VisualCaptureRecord,
) -> BTreeMap<String, (u32, u32)> {
    let mut dimensions = BTreeMap::new();
    let mut paths: Vec<&str> = Vec::new();
    for mock in &record.mocks {
        paths.push(&mock.artifact);
        if let Some(capture) = &mock.capture {
            paths.push(&capture.path);
            paths.push(&capture.side_by_side);
        }
    }
    for path in paths {
        if dimensions.contains_key(path) {
            continue;
        }
        if let Ok(Some(artifact)) = runtime.get_task_artifact(task_id, path)
            && let Some(size) = visual_image_dimensions(&artifact.content)
        {
            dimensions.insert(path.to_string(), size);
        }
    }
    dimensions
}

struct StagedImages {
    paths: Vec<Value>,
    entries: Vec<Value>,
}

/// Copy the shown mocks' images into `dir` in budget order, mock before
/// capture (§8). The engine stages its canary ahead of these paths, so
/// the reviewer sees it first (§9.3).
fn stage_shown(
    runtime: &OrbitRuntime,
    task_id: &str,
    record: &VisualCaptureRecord,
    shown: &[orbit_types::task::VisualShownMock],
    dir: &Path,
) -> Result<StagedImages, OrbitError> {
    create_private_dir_all(dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    let mut staged = StagedImages {
        paths: Vec::new(),
        entries: Vec::new(),
    };
    for item in shown {
        let Some(mock) = record.mocks.iter().find(|mock| mock.artifact == item.mock) else {
            continue;
        };
        let Some(capture) = &mock.capture else {
            continue;
        };
        let images: Vec<(&str, &str)> = match item.form {
            VisualShownForm::Pair => vec![
                (mock.artifact.as_str(), "mock"),
                (capture.path.as_str(), "capture"),
            ],
            VisualShownForm::SideBySide => vec![(capture.side_by_side.as_str(), "side_by_side")],
        };
        for (artifact, role) in images {
            let Some(bytes) = runtime
                .get_task_artifact(task_id, artifact)?
                .map(|stored| stored.content)
            else {
                return Err(OrbitError::Execution(format!(
                    "{task_id} no longer stores {artifact} for visual review"
                )));
            };
            let leaf = Path::new(artifact)
                .file_name()
                .and_then(|leaf| leaf.to_str())
                .unwrap_or("image.png");
            let path = dir.join(format!("{:02}-{leaf}", staged.paths.len() + 1));
            atomic_write_bytes(&path, &bytes)
                .map_err(|error| OrbitError::Io(format!("stage {}: {error}", path.display())))?;
            staged.paths.push(json!(path.display().to_string()));
            staged.entries.push(json!({
                "artifact": artifact,
                "role": role,
                "mock": mock.artifact,
            }));
        }
    }
    Ok(staged)
}

/// The task text the reviewer judges against (§8): title, description,
/// acceptance criteria and operator comments.
fn task_payload(runtime: &OrbitRuntime, task: Option<&Task>, task_id: &str) -> Value {
    let comments = runtime
        .get_task_comments(task_id)
        .unwrap_or_default()
        .into_iter()
        .map(|comment| json!({"by": comment.by, "message": comment.message}))
        .collect::<Vec<_>>();
    match task {
        Some(task) => json!({
            "id": task.id.to_string(),
            "title": task.title,
            "description": task.description,
            "acceptance_criteria": task.acceptance_criteria,
            "operator_comments": comments,
        }),
        None => json!({"id": task_id, "operator_comments": comments}),
    }
}

/// Design-rule and token excerpts at the contract's base revision,
/// capped at 8 KiB each and 16 KiB together (§8). The manifest's
/// `design_rules` and `tokens` name them.
fn design_rule_excerpts(contract: Option<&VisualReviewContract>, workspace: &Path) -> Value {
    let Some(contract) = contract else {
        return json!([]);
    };
    let mut excerpts = Vec::new();
    let mut total = 0usize;
    for manifest in &contract.manifests {
        let Ok(document) = toml::from_str::<toml::Value>(&manifest.content) else {
            continue;
        };
        let mut paths: Vec<&str> = Vec::new();
        if let Some(apps) = document.get("app").and_then(toml::Value::as_table) {
            for app in apps.values() {
                for key in ["design_rules", "tokens"] {
                    if let Some(entries) = app.get(key).and_then(toml::Value::as_array) {
                        paths.extend(entries.iter().filter_map(toml::Value::as_str));
                    }
                }
            }
        }
        for path in paths {
            if total >= 16 * 1024 {
                break;
            }
            let Ok(bytes) = read_blob_at(workspace, &manifest.reference.revision, path) else {
                continue;
            };
            let excerpt_limit = (8 * 1024).min(16 * 1024 - total);
            if excerpt_limit == 0 {
                break;
            }
            let mut content = String::from_utf8_lossy(&bytes).into_owned();
            if content.len() > excerpt_limit {
                let mut end = excerpt_limit;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                content.truncate(end);
            }
            total += content.len();
            excerpts.push(json!({"path": path, "content": content}));
        }
    }
    json!(excerpts)
}
