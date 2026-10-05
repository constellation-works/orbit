use chrono::Utc;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, DESKTOP_TASK_EDIT, authorize,
};
use orbit_common::{OrbitError, security::redaction::redact_all};
use orbit_types::{
    desktop::*,
    task::{Task, TaskStatus},
    tool::ToolSessionContext,
};

use crate::OrbitRuntime;
use crate::application::task::lifecycle::task_status_transition_allowed;

use super::validation::invalid;

fn action(result: Result<(), OrbitError>) -> DesktopAction {
    match result {
        Ok(()) => DesktopAction {
            enabled: true,
            reason: None,
        },
        Err(e) => DesktopAction {
            enabled: false,
            reason: Some(redact_all(&e.to_string()).chars().take(2048).collect()),
        },
    }
}
fn clip(value: &mut String, max: usize, field: &str, truncated: &mut Vec<String>) {
    let safe = redact_all(value);
    if safe != *value {
        truncated.push(field.into());
    }
    *value = safe;
    if value.len() > max {
        let boundary = orbit_common::text::floor_char_boundary(value, max);
        value.truncate(boundary);
        truncated.push(field.into());
    }
}
// An opaque reference must either retain its exact identity or be omitted.
fn retain_reference(value: &str, max: usize, field: &str, truncated: &mut Vec<String>) -> bool {
    if value.len() > max || redact_all(value) != value {
        truncated.push(field.into());
        false
    } else {
        true
    }
}

fn truncate_task(task: &mut Task, truncated: &mut Vec<String>) {
    clip(&mut task.title, 512, "task.title", truncated);
    clip(&mut task.description, 32768, "task.description", truncated);
    clip(&mut task.plan, 16384, "task.plan", truncated);
    clip(
        &mut task.execution_summary,
        16384,
        "task.execution_summary",
        truncated,
    );
    if task.acceptance_criteria.len() > 100 {
        task.acceptance_criteria.truncate(100);
        truncated.push("task.acceptance_criteria".into());
    }
    for (index, criterion) in task.acceptance_criteria.iter_mut().enumerate() {
        clip(
            criterion,
            2048,
            &format!("task.acceptance_criteria[{index}]"),
            truncated,
        );
    }
    for (name, value, max) in [
        ("task.crew", &mut task.crew, 128),
        ("task.orchestrator", &mut task.orchestrator, 128),
        ("task.pr_status", &mut task.pr_status, 128),
        ("task.created_by", &mut task.created_by, 512),
        ("task.planned_by", &mut task.planned_by, 512),
        ("task.implemented_by", &mut task.implemented_by, 512),
    ] {
        if let Some(value) = value {
            clip(value, max, name, truncated);
        }
    }
    if task
        .job_run_id
        .as_ref()
        .is_some_and(|id| !retain_reference(id, 512, "task.job_run_id", truncated))
    {
        task.job_run_id = None;
    }
    if let Some(location) = &mut task.job_run_machine {
        if !retain_reference(
            &location.machine_id,
            512,
            "task.job_run_machine.machine_id",
            truncated,
        ) {
            task.job_run_machine = None;
        } else if let Some(name) = &mut location.machine_name {
            clip(name, 128, "task.job_run_machine.machine_name", truncated);
        }
    }
    for (name, values) in [
        ("task.context_files", &mut task.context_files),
        ("task.required_tools", &mut task.required_tools),
    ] {
        if values.len() > 100 {
            values.truncate(100);
            truncated.push(name.into());
        }
        values.retain(|value| retain_reference(value, 1024, name, truncated));
    }
    if task.tags.len() > 100 {
        task.tags.truncate(100);
        truncated.push("task.tags".into());
    }
    for (index, value) in task.tags.iter_mut().enumerate() {
        clip(value, 1024, &format!("task.tags[{index}]"), truncated);
    }
    if task.external_refs.len() > 100 {
        task.external_refs.truncate(100);
        truncated.push("task.external_refs".into());
    }
    task.external_refs.retain(|reference| {
        retain_reference(
            &reference.system,
            128,
            "task.external_refs.system",
            truncated,
        ) && retain_reference(&reference.id, 1024, "task.external_refs.id", truncated)
            && reference
                .url
                .as_ref()
                .is_none_or(|url| retain_reference(url, 2048, "task.external_refs.url", truncated))
    });
    if task.relations.len() > 100 {
        task.relations.truncate(100);
        truncated.push("task.relations".into());
    }
    task.relations.retain(|relation| {
        retain_reference(&relation.target, 512, "task.relations.target", truncated)
    });
}

impl OrbitRuntime {
    pub fn desktop_task_snapshot(
        &self,
        id: &str,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskSnapshot, OrbitError> {
        self.desktop_task_snapshot_page(id, 0, 0, 0, 50, session)
    }
    pub fn desktop_task_snapshot_page(
        &self,
        id: &str,
        comments_offset: usize,
        history_offset: usize,
        artifacts_offset: usize,
        limit: usize,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskSnapshot, OrbitError> {
        let orbit_store::contracts::DesktopTaskRead {
            mut task,
            revision,
            comments,
            history,
            artifacts,
            write_disabled_reason,
        } = self.stores().tasks().read_desktop_task(id)?;
        let (review_projection, review_reason) =
            match crate::application::review::task_review_projection(self, &task, &artifacts) {
                Ok(Some(value)) => {
                    let bytes = serde_json::to_string(&value)
                        .map_err(|error| OrbitError::Execution(error.to_string()))?;
                    let safe = redact_all(&bytes);
                    if safe.len() > 32768 {
                        (None, Some("structured review evidence exceeds the desktop 32 KiB limit; use the authoritative review artifact".into()))
                    } else {
                        (
                            Some(
                                serde_json::from_str(&safe)
                                    .map_err(|error| OrbitError::Execution(error.to_string()))?,
                            ),
                            None,
                        )
                    }
                }
                Ok(None) => (
                    None,
                    Some("structured review gate evidence is unavailable".into()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
        let comments_total = comments.len();
        let history_total = history.len();
        let artifacts_total = artifacts.len();
        let limit = limit.clamp(1, 100);
        let view_run = action(self.desktop_run_allowed(&task, session));
        let mut truncated_fields = Vec::new();
        truncate_task(&mut task, &mut truncated_fields);
        let content_truncated = !truncated_fields.is_empty();
        let comments = comments
            .into_iter()
            .skip(comments_offset)
            .take(limit)
            .enumerate()
            .map(|(index, mut comment)| {
                clip(
                    &mut comment.message,
                    4096,
                    &format!("comments[{index}].message"),
                    &mut truncated_fields,
                );
                clip(
                    &mut comment.by,
                    512,
                    &format!("comments[{index}].by"),
                    &mut truncated_fields,
                );
                comment
            })
            .collect();
        let history = history
            .into_iter()
            .skip(history_offset)
            .take(limit)
            .enumerate()
            .map(|(index, mut history)| {
                if let Some(note) = &mut history.note {
                    clip(
                        note,
                        4096,
                        &format!("history[{index}].note"),
                        &mut truncated_fields,
                    );
                }
                clip(
                    &mut history.by,
                    512,
                    &format!("history[{index}].by"),
                    &mut truncated_fields,
                );
                clip(
                    &mut history.event,
                    128,
                    &format!("history[{index}].event"),
                    &mut truncated_fields,
                );
                history
            })
            .collect();
        let artifacts = artifacts
            .into_iter()
            .skip(artifacts_offset)
            .take(limit)
            .enumerate()
            .filter_map(|(index, mut artifact)| {
                let field = format!("artifacts[{}]", artifacts_offset.saturating_add(index));
                if !retain_reference(
                    &artifact.path,
                    2048,
                    &format!("{field}.path"),
                    &mut truncated_fields,
                ) {
                    return None;
                }
                clip(
                    &mut artifact.media_type,
                    128,
                    &format!("{field}.media_type"),
                    &mut truncated_fields,
                );
                clip(
                    &mut artifact.created_by,
                    512,
                    &format!("{field}.created_by"),
                    &mut truncated_fields,
                );
                Some(DesktopArtifactMetadata {
                    path: artifact.path,
                    media_type: artifact.media_type,
                    size_bytes: artifact.size_bytes,
                    sha256: artifact.sha256,
                    created_by: artifact.created_by,
                    created_at: artifact.created_at.to_rfc3339(),
                })
            })
            .collect();
        let writable = self.ensure_coordination_task_write_permitted();
        let reason = writable
            .err()
            .map(|e| e.to_string())
            .or(write_disabled_reason);
        let edit_authority = authorize(
            &DESKTOP_TASK_EDIT,
            &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
        )
        .err()
        .map(|denial| denial.to_string());
        let editable = edit_authority.is_none()
            && !content_truncated
            && !matches!(task.status, TaskStatus::Done | TaskStatus::Archived);
        let review = !content_truncated && task.status == TaskStatus::Review;
        let completion = if let Some(reason) = &reason {
            Err(invalid(reason))
        } else if content_truncated {
            Err(invalid(
                "task content is truncated; use the full authoritative task before editing or reviewing",
            ))
        } else {
            self.desktop_completion_allowed(&task, session)
        };
        let status_actions = [
            TaskStatus::Proposed,
            TaskStatus::Backlog,
            TaskStatus::InProgress,
            TaskStatus::Review,
            TaskStatus::Blocked,
            TaskStatus::Done,
            TaskStatus::Rejected,
            TaskStatus::Archived,
            TaskStatus::Someday,
        ]
        .into_iter()
        .filter(|target| {
            *target != task.status && task_status_transition_allowed(task.status, *target)
        })
        .map(|target| DesktopStatusAction {
            status: target,
            action: action(if let Some(reason) = &reason {
                Err(invalid(reason))
            } else if !editable {
                Err(invalid("task content unavailable or terminal"))
            } else {
                self.desktop_status_allowed(&task, target, session)
            }),
        })
        .collect();
        let mut snapshot = DesktopTaskSnapshot {
            schema_version: 1,
            observed_at: Utc::now().to_rfc3339(),
            revision,
            task: task.clone(),
            actions: DesktopTaskActions {
                status: status_actions,
                ship: Some(action(self.desktop_ship_allowed(&task, session).and_then(
                    |()| {
                        if let Some(reason) = &reason {
                            Err(invalid(reason))
                        } else if !editable {
                            Err(invalid("task content unavailable or terminal"))
                        } else {
                            Ok(())
                        }
                    },
                ))),
                view_run: Some(view_run),
                edit: DesktopAction {
                    enabled: editable && reason.is_none(),
                    reason: reason
                        .clone()
                        .or(edit_authority)
                        .or_else(|| {
                            content_truncated.then(|| {
                                "task content is truncated; use the full authoritative task".into()
                            })
                        })
                        .or_else(|| {
                            (!editable)
                                .then(|| "terminal tasks cannot be edited through desktop".into())
                        }),
                },
                comment: DesktopAction {
                    enabled: reason.is_none(),
                    reason: reason.clone(),
                },
                review: DesktopAction {
                    enabled: review && reason.is_none(),
                    reason: reason
                        .or_else(|| {
                            content_truncated.then(|| {
                                "task content is truncated; use the full authoritative task".into()
                            })
                        })
                        .or_else(|| (!review).then(|| "task must be in review".into())),
                },
                complete: action(completion),
            },
            comments,
            history,
            artifacts,
            comments_total,
            history_total,
            artifacts_total,
            reviewed_head: None,
            reviewed_head_reason: None,
            truncated_fields,
            content_truncated,
            review: review_projection,
            review_reason,
        };
        // The same observation a review write binds to: the PR the task
        // references, or the one its accepted foreign handoff delivered.
        if !snapshot.content_truncated && snapshot.task.status == TaskStatus::Review {
            match self.desktop_current_pull_request(&snapshot.task) {
                Ok(pull_request) => {
                    if let Some(reason) = pull_request
                        .as_ref()
                        .and_then(|pr| pr.completion_refusal.clone())
                        && snapshot.actions.complete.enabled
                    {
                        snapshot.actions.complete = DesktopAction {
                            enabled: false,
                            reason: Some(reason),
                        };
                    }
                    snapshot.reviewed_head = pull_request.map(|pr| pr.head);
                }
                Err(error) => {
                    let reason = error.to_string();
                    snapshot.reviewed_head_reason = Some(reason.clone());
                    snapshot.actions.complete = DesktopAction {
                        enabled: false,
                        reason: Some(reason),
                    };
                }
            }
        }
        for (field, reason) in [
            ("review_reason", &mut snapshot.review_reason),
            ("reviewed_head_reason", &mut snapshot.reviewed_head_reason),
            ("actions.edit.reason", &mut snapshot.actions.edit.reason),
            (
                "actions.comment.reason",
                &mut snapshot.actions.comment.reason,
            ),
            ("actions.review.reason", &mut snapshot.actions.review.reason),
            (
                "actions.complete.reason",
                &mut snapshot.actions.complete.reason,
            ),
        ] {
            if let Some(value) = reason {
                clip(value, 2048, field, &mut snapshot.truncated_fields);
            }
        }
        if snapshot.reviewed_head.as_ref().is_some_and(|head| {
            !retain_reference(head, 128, "reviewed_head", &mut snapshot.truncated_fields)
        }) {
            snapshot.reviewed_head = None;
            let reason =
                "PR head identity exceeds the desktop limit or contains redacted data".to_string();
            snapshot.reviewed_head_reason = Some(reason.clone());
            snapshot.actions.review = DesktopAction {
                enabled: false,
                reason: Some(reason.clone()),
            };
            snapshot.actions.complete = DesktopAction {
                enabled: false,
                reason: Some(reason),
            };
        }
        if self.stores().tasks().desktop_task_revision(id)? != snapshot.revision {
            return Err(OrbitError::TaskRevisionConflict { task_id: id.into() });
        }
        Ok(snapshot)
    }
}
