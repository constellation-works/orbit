//! Resolve an assessment's selectors against creation grants and review evidence.
use super::super::no_diff::VerifiedNoDiff;
use super::super::persist::task_outcome;
use super::super::{
    SelectorCheck, required_string, required_string_array, retained_review_evidence, review_filed,
    reviewed_evidence_drops, unauthorized_missing_targets, validate_after_selectors,
};
use super::ApplyContext;
use super::admission::PreparedTaskSnapshot;
use super::task::{Scope, Settled, refused};
use orbit_types::task::{ContextCreationState, Task};
use serde_json::{Value, json};

pub(super) fn scope_with(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &mut Value,
    snapshot: &PreparedTaskSnapshot,
    pre_current: Option<&Task>,
    pre_history: &[orbit_types::task::TaskHistoryEntry],
) -> Settled<Scope> {
    let action = context.action;
    let mut disposition = required_string(assessment, "disposition", action)
        .map_err(refused(task_id, "invalid"))?
        .to_string();
    let proposed_after = required_string_array(assessment, "context_files_after", action)
        .map_err(refused(task_id, "invalid"))?;
    // The grant is read from the task, never from the prepared payload or the
    // agent's result. The write boundary refuses a grant that moved since
    // preparation, after its replay receipt check, so a retried apply still
    // settles as already applied. The caller shares its pre-lock read, so no
    // second task or history read happens here.
    let (current, creation) = match pre_current {
        Some(task) => {
            let creation = ContextCreationState::resolve(
                &task.id,
                &task.context_files,
                task.updated_at,
                pre_history
                    .iter()
                    .map(|entry| (entry.event.as_str(), entry.note.as_deref())),
            );
            (Some(task.clone()), creation)
        }
        None => (None, ContextCreationState::Absent),
    };
    let authorized_creation = creation.selectors();
    let mut selectors = validate_after_selectors(
        SelectorCheck {
            action,
            task_id,
            disposition: &disposition,
            assessment,
            workspace_root: context.workspace_root,
            source: context.source,
            authorized_creation,
        },
        &proposed_after,
    )
    .map_err(refused(task_id, "invalid"))?;
    if !authorized_creation.is_empty()
        && (disposition == "verified_no_diff"
            || (disposition == "selectors"
                && assessment
                    .get("already_landed")
                    .is_some_and(|value| !value.is_null())))
    {
        let task = current.as_ref().ok_or_else(|| {
            task_outcome(
                task_id,
                "invalid",
                Some("creation task no longer exists".into()),
            )
        })?;
        selectors.values = landed_creation_scope(
            context,
            task,
            assessment,
            &selectors.values,
            authorized_creation,
        )?;
        disposition = "selectors".into();
    }
    if !authorized_creation.is_empty() && disposition != "selectors" {
        return Err(task_outcome(
            task_id,
            "invalid",
            Some(format!(
                "task {task_id} holds operator-authorized creation targets {}; a {disposition} \
                 assessment cannot drop them. Keep them as selectors; only an operator \
                 revokes them, by replacing context_files through orbit.task.update",
                authorized_creation.join(", ")
            )),
        ));
    }
    // The pilot cannot revoke operator intent by omission: every granted
    // target it left out is kept, and reported.
    let mut after = selectors.values;
    let retained = authorized_creation
        .iter()
        .filter(|selector| !after.contains(selector))
        .cloned()
        .collect::<Vec<_>>();
    after.extend(retained.iter().cloned());
    // A review-filed task's selectors are the review's evidence: the pilot
    // may drop verified read-only evidence only with an explicit audit record.
    // No-diff assessments audit the same prepared evidence, but retain no
    // lock footprint. Their no-change proof and creation-grant checks remain
    // independent of these explanations.
    let retain_evidence = disposition == "selectors" && review_filed(&snapshot.tags);
    let inspect_evidence = matches!(disposition.as_str(), "selectors" | "verified_no_diff")
        && review_filed(&snapshot.tags);
    let mut evidence = if inspect_evidence {
        retained_review_evidence(
            action,
            &snapshot.context_files,
            &after,
            context.workspace_root,
            context.source,
        )
        .map_err(refused(task_id, "apply_failed"))?
    } else {
        Vec::new()
    };
    let dropped = reviewed_evidence_drops(action, assessment, &evidence)
        .map_err(refused(task_id, "invalid"))?;
    evidence.retain(|selector| retain_evidence && !dropped.contains(selector));
    if retain_evidence {
        after = snapshot
            .context_files
            .iter()
            .filter(|selector| after.contains(selector) || evidence.contains(selector))
            .chain(
                after
                    .iter()
                    .filter(|selector| !snapshot.context_files.contains(selector)),
            )
            .cloned()
            .collect();
    }
    let reauthorization = unauthorized_missing_targets(
        action,
        &snapshot.context_files,
        &after,
        authorized_creation,
        context.workspace_root,
        context.source,
    )
    .map_err(refused(task_id, "apply_failed"))?;
    Ok(Scope {
        after,
        normalizations: selectors.normalizations,
        retained,
        evidence,
        reauthorization,
    })
}

/// A no-change finding cannot revoke a creation grant. Prove the complete
/// retained scope at the inspection revision, then preserve it as an existing
/// already-landed hold for an operator to reconcile.
fn landed_creation_scope(
    context: &ApplyContext<'_>,
    task: &Task,
    assessment: &mut Value,
    proposed_after: &[String],
    authorized_creation: &[String],
) -> Settled<Vec<String>> {
    if let Some(finding) = assessment
        .get("already_landed")
        .filter(|value| !value.is_null())
    {
        required_string(finding, "evidence", context.action)
            .map_err(refused(task.id.as_str(), "invalid"))?;
    }
    let invalid = refused(task.id.as_str(), "invalid");
    let source = context.source.ok_or_else(|| {
        task_outcome(
            task.id.as_str(),
            "invalid",
            Some("already-landed creation work requires a pinned source revision".into()),
        )
    })?;
    let mut paths = task.context_files.clone();
    for selector in proposed_after.iter().chain(authorized_creation) {
        if !paths.contains(selector) {
            paths.push(selector.clone());
        }
    }
    // No missing-target exception: a grant authorizes future creation, but
    // does not establish that the target already exists at this revision.
    let selectors = validate_after_selectors(
        SelectorCheck {
            action: context.action,
            task_id: task.id.as_str(),
            disposition: "selectors",
            assessment,
            workspace_root: context.workspace_root,
            source: Some(source),
            authorized_creation: &[],
        },
        &paths,
    )
    .map_err(invalid)?;
    let finding = VerifiedNoDiff::from_landed_evidence(assessment);
    if finding.evidence.is_empty() {
        return Err(task_outcome(
            task.id.as_str(),
            "invalid",
            Some("already-landed creation work requires evidence".into()),
        ));
    }
    let covering = finding
        .creation_covering_proof(
            context.runtime,
            task,
            &selectors.values,
            &source.source_revision,
        )
        .map_err(refused(task.id.as_str(), "invalid"))?;
    let mut already_landed = assessment
        .get("already_landed")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    already_landed["evidence"] = json!(finding.evidence);
    already_landed["covering_commits"] = json!(covering);
    already_landed["source_revision"] = json!(source.source_revision);
    already_landed["operator_reconciliation_required"] = json!(true);
    assessment["already_landed"] = already_landed;
    assessment["disposition"] = json!("selectors");
    assessment["context_files_after"] = json!(selectors.values);
    Ok(selectors.values)
}
