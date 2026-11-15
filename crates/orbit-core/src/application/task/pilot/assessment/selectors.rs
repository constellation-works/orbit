//! Selector normalization, source validation and missing-target findings.
use super::super::input::required_string;
use super::super::source::{GitPathKind, SourceSnapshot};
use orbit_common::fs::selector::{
    anchor_path, canonical_selector, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_engine::DispatchError;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

pub(in super::super) struct ValidatedSelectors {
    pub(in super::super) values: Vec<String>,
    pub(in super::super) normalizations: Vec<Value>,
}

/// The task, pilot disposition and pinned context that a selector list is
/// validated against.
pub(in super::super) struct SelectorCheck<'a> {
    pub(in super::super) action: &'a str,
    pub(in super::super) task_id: &'a str,
    pub(in super::super) disposition: &'a str,
    pub(in super::super) assessment: &'a Value,
    pub(in super::super) workspace_root: &'a Path,
    pub(in super::super) source: Option<&'a SourceSnapshot>,
    pub(in super::super) authorized_creation: &'a [String],
}

pub(in super::super) fn validate_after_selectors(
    check: SelectorCheck<'_>,
    selectors: &[String],
) -> Result<ValidatedSelectors, DispatchError> {
    let SelectorCheck {
        action,
        task_id,
        disposition,
        assessment,
        workspace_root,
        source,
        authorized_creation,
    } = check;
    if selectors.is_empty() {
        if !matches!(disposition, "verified_no_diff" | "host_operational") {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "task {task_id} may keep empty context_files only with verified_no_diff or host_operational disposition"
                ),
            ));
        }
        required_string(assessment, "evidence", action)?;
        return Ok(ValidatedSelectors {
            values: Vec::new(),
            normalizations: Vec::new(),
        });
    }
    if disposition != "selectors" {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "task {task_id} has non-empty context_files_after but disposition is {disposition}"
            ),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut values = Vec::with_capacity(selectors.len());
    let mut normalizations = Vec::new();
    for selector in selectors {
        let trimmed = selector.trim();
        let has_kind = matches!(
            trimmed.split_once(':').map(|(kind, _)| kind),
            Some("file" | "dir" | "symbol")
        );
        let candidate = if has_kind {
            trimmed.to_string()
        } else {
            if trimmed.contains(':') {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} bare selector {selector:?} is ambiguous because it contains ':'"
                    ),
                ));
            }
            let source = source.ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} must use file:, dir:, or symbol: when no pinned source is available"
                    ),
                )
            })?;
            let path_candidate =
                canonical_selector(&format!("file:{trimmed}")).map_err(|error| {
                    DispatchError::action_failed(
                        action,
                        format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                    )
                })?;
            let anchor = anchor_path(&path_candidate).map_err(|error| {
                DispatchError::action_failed(
                    action,
                    format!("task {task_id} bare selector {selector:?} is invalid: {error}"),
                )
            })?;
            let kind = source.path_kind(action, workspace_root, &anchor)?;
            let normalized = match kind {
                GitPathKind::Blob => path_candidate,
                GitPathKind::Tree => canonical_selector(&format!("dir:{trimmed}"))
                    .map_err(|error| DispatchError::action_failed(action, error.to_string()))?,
                GitPathKind::Missing => {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} does not resolve at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
                GitPathKind::Other => {
                    return Err(DispatchError::action_failed(
                        action,
                        format!(
                            "task {task_id} bare selector {selector:?} has an unsupported or ambiguous kind at pinned source revision {}",
                            source.source_revision
                        ),
                    ));
                }
            };
            normalizations.push(json!({
                "original": selector,
                "normalized": normalized,
            }));
            normalized
        };
        // A dirty primary may replace an anchor with a symlink or a different
        // kind. Only syntax comes from this parser; pinned Git objects own
        // containment and target validation when source identity is present.
        let canonical = if source.is_some() {
            canonical_selector(&candidate)
        } else {
            canonical_selector_in_workspace(&candidate, workspace_root)
        }
        .map_err(|error| {
            DispatchError::action_failed(
                action,
                format!("task {task_id} selector {selector:?} is invalid: {error}"),
            )
        })?;
        if has_kind && canonical != trimmed {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "task {task_id} selector {selector:?} is not canonical; expected {canonical:?}"
                ),
            ));
        }
        // Only an exact selector the task's durable creation grant names may
        // be missing; it still has to be absent, never a target of another
        // kind or a symlink.
        let authorized = has_kind && authorized_creation.contains(&canonical);
        if let Some(source) = source {
            validate_selector_at_source(
                action,
                task_id,
                &canonical,
                workspace_root,
                source,
                authorized,
            )?;
        } else if !exists_in_workspace(&canonical, workspace_root) {
            if !authorized {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target{}",
                        reauthorization_hint(&canonical)
                    ),
                ));
            }
        } else {
            validate_selector_target_kind(action, task_id, &canonical, workspace_root)?;
        }
        if !seen.insert(canonical) {
            return Err(DispatchError::action_failed(
                action,
                format!("task {task_id} repeats selector {selector:?}"),
            ));
        }
        values.push(candidate);
    }
    Ok(ValidatedSelectors {
        values,
        normalizations,
    })
}

/// Selectors the task's current scope declares that do not exist where the
/// assessment was validated and that no durable creation grant covers, which
/// the assessment dropped. Each finding names the existing reauthorization
/// path; nothing here infers that the target was meant to be created.
pub(in super::super) fn unauthorized_missing_targets(
    action: &str,
    before: &[String],
    after: &[String],
    authorized_creation: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<Vec<String>, DispatchError> {
    let mut findings = Vec::new();
    for selector in before {
        if after.contains(selector) || authorized_creation.contains(selector) {
            continue;
        }
        let Ok(anchor) = anchor_path(selector) else {
            continue;
        };
        let missing = match source {
            Some(source) => {
                source.path_kind(action, workspace_root, &anchor)? == GitPathKind::Missing
            }
            None => !exists_in_workspace(selector, workspace_root),
        };
        if missing {
            findings.push(format!(
                "{selector} was dropped: it does not exist and carries no durable creation authorization{}",
                reauthorization_hint(selector)
            ));
        }
    }
    Ok(findings)
}

/// How an operator records that a missing selector is a target the task will
/// create. Agents' proposals, comments and plans never grant it.
fn reauthorization_hint(selector: &str) -> String {
    format!(
        "; if the task will create it, an operator must re-declare it with orbit.task.update \
         (the full context_files list including {selector}, plus allow_missing_context: true)"
    )
}

fn validate_selector_at_source(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
    source: &SourceSnapshot,
    authorized_creation: bool,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let kind = source.path_kind(action, workspace_root, &anchor)?;
    let expected_dir = selector.starts_with("dir:");
    match (kind, expected_dir) {
        (GitPathKind::Tree, true) | (GitPathKind::Blob, false) => Ok(()),
        (GitPathKind::Missing, _) if authorized_creation => Ok(()),
        (GitPathKind::Missing, _) => Err(DispatchError::action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not resolve to an existing in-workspace target at source revision {} ({}){}",
                source.source_revision,
                source.source_ref,
                reauthorization_hint(selector)
            ),
        )),
        _ => Err(DispatchError::action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind at source revision {} ({})",
                source.source_revision, source.source_ref
            ),
        )),
    }
}

pub(super) fn validate_selector_target_kind(
    action: &str,
    task_id: &str,
    selector: &str,
    workspace_root: &Path,
) -> Result<(), DispatchError> {
    let anchor = anchor_path(selector).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!("task {task_id} selector {selector:?} has no filesystem anchor: {error}"),
        )
    })?;
    let resolved = workspace_root.join(anchor);
    let correct_kind = if selector.starts_with("dir:") {
        resolved.is_dir()
    } else {
        resolved.is_file()
    };
    if correct_kind {
        Ok(())
    } else {
        Err(DispatchError::action_failed(
            action,
            format!(
                "task {task_id} selector {selector:?} does not match the target's file/directory kind"
            ),
        ))
    }
}
