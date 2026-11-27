//! The dashboard's task writes and read models. Each calls the runtime
//! method the dashboard's handler called before.

use orbit_common::OrbitError;
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use serde_json::Value;

use super::{
    DashboardAction, DashboardWrite, Outcome, PageRead, PageSelection, Read, RowRead,
    ScreenedWrite, TaskPages,
};
use crate::OrbitRuntime;
use crate::application::task::{
    ScreenedTaskWrite, TaskUpdateIdentity, TaskUpdateParams, TaskWriteResponsePolicy,
    TaskWriteScreen, screened_task_write_response_json, with_task_write_provenance,
};

/// What one dashboard write produced, before its answer is rendered.
enum Applied {
    Screened(Box<ScreenedTaskWrite>),
    Outcome(Outcome),
}

/// Apply one dashboard write as the dashboard's own, attributed to the
/// forwarding caller when there is one.
pub(super) fn write(runtime: &OrbitRuntime, write: DashboardWrite) -> Result<Outcome, OrbitError> {
    let DashboardWrite {
        caller,
        model,
        action,
        ..
    } = write;
    let provenance = TaskWriteProvenance::new(Some(TaskWriteSurface::Web), caller);
    let applied = with_task_write_provenance(provenance, || apply(runtime, action, model))?;
    Ok(match applied {
        Applied::Outcome(outcome) => outcome,
        Applied::Screened(written) => {
            let document = screened_task_write_response_json(
                runtime,
                &written,
                &TaskWriteResponsePolicy::DashboardFullRecord,
            );
            // The client reads human-crew identity off the task itself.
            let human_crew = document
                .as_ref()
                .ok()
                .and_then(Value::as_object)
                .and_then(|task| runtime.human_crew(task.get("crew").and_then(Value::as_str)));
            Outcome::Screened(Box::new(ScreenedWrite {
                written: *written,
                document,
                human_crew,
            }))
        }
    })
}

fn apply(
    runtime: &OrbitRuntime,
    action: DashboardAction,
    model: Option<String>,
) -> Result<Applied, OrbitError> {
    let screened = |written: Result<ScreenedTaskWrite, OrbitError>| {
        written.map(|written| Applied::Screened(Box::new(written)))
    };
    let outcome = |outcome: Outcome| Applied::Outcome(outcome);
    match action {
        DashboardAction::Add {
            params,
            allow_missing_context,
        } => screened(runtime.add_task_screened(
            *params,
            &TaskWriteScreen::unbound(allow_missing_context),
            None,
            model,
        )),
        DashboardAction::Update {
            id,
            params,
            allow_missing_context,
            authority,
            status_note,
        } => screened(runtime.update_task_screened_with_note(
            &id,
            *params,
            &TaskWriteScreen::unbound(allow_missing_context),
            authority,
            TaskUpdateIdentity { agent: None, model },
            status_note,
        )),
        DashboardAction::Comment {
            id,
            message,
            author,
        } => screened(
            runtime
                .update_task_as_human(
                    &id,
                    TaskUpdateParams {
                        comment: Some(message),
                        ..TaskUpdateParams::default()
                    },
                    author,
                )
                .map(ScreenedTaskWrite::from),
        ),
        DashboardAction::Approve { id, note, comment } => {
            screened(runtime.approve_task_screened(&id, note, comment, None, None))
        }
        DashboardAction::Reject { id, note, comment } => screened(
            runtime
                .reject_task(&id, note, comment)
                .map(ScreenedTaskWrite::from),
        ),
        DashboardAction::Requeue {
            id,
            reason,
            discard_candidate,
            actor,
        } => if discard_candidate {
            runtime.discard_task_candidate_as(&id, &reason, actor)
        } else {
            runtime.requeue_blocked_task_as(&id, &reason, false, Some(actor))
        }
        .map(|answer| outcome(Outcome::Answer(answer))),
        DashboardAction::AnswerDecision {
            id,
            answer,
            note,
            author,
        } => runtime
            .answer_decision_request_as_human(
                &id,
                &answer,
                note,
                author,
                Some("dashboard".to_string()),
            )
            .map(|request| outcome(Outcome::DecisionRequest(Box::new(request)))),
        DashboardAction::Archive { id, force } => runtime
            .archive_task_as_operator(&id, force)
            .map(|()| outcome(Outcome::Done)),
        DashboardAction::SettleMerged { id, actor } => runtime
            .settle_externally_merged_task_as(&id, Some(actor))
            .map(|settlement| outcome(Outcome::Settled(vec![settlement]))),
        DashboardAction::PilotAdmission { id, record, actor } => runtime
            .record_pilot_admission_as_human(&id, &record, actor.to_string())
            .map(|task| outcome(Outcome::Task(Box::new(task)))),
        DashboardAction::PostDeploy { id, record, actor } => runtime
            .record_post_deploy_as_human(&id, &record, actor.to_string())
            .map(|task| outcome(Outcome::Task(Box::new(task)))),
        DashboardAction::Propose(input) => runtime
            .create_proposed_task(*input)
            .map(|id| outcome(Outcome::Created(id))),
    }
}

/// One read model of the dashboard.
pub(super) fn read(runtime: &OrbitRuntime, read: Read) -> Result<Outcome, OrbitError> {
    match read {
        Read::Pages { select, detail } => {
            let workspace_id = runtime.workspace_id()?;
            let selection = (select.0)(&workspace_id)?;
            Ok(Outcome::Pages(TaskPages {
                workspace_id,
                pages: pages(runtime, selection, detail.as_deref()),
            }))
        }
        Read::Row { id } => {
            let row = runtime.get_task_row(&id)?;
            // The coordination registry's global status index, not a
            // workspace-scoped list: a dependency on another registered
            // workspace's task labels with its real status. Archived
            // dependencies that reached `done` first label as `done`.
            let dependency_statuses = runtime.dependency_status_index([&row.task]);
            Ok(Outcome::Row(Box::new(RowRead {
                row,
                dependency_statuses,
            })))
        }
        Read::Locks => runtime.task_locks_snapshot().map(Outcome::Answer),
        Read::CompletionByComplexity => runtime
            .task_completion_by_complexity()
            .map(Outcome::Completion),
    }
}

/// The selected pages, each with what its rows show beside the task.
fn pages(
    runtime: &OrbitRuntime,
    selection: PageSelection,
    detail: Option<&str>,
) -> Result<Vec<PageRead>, OrbitError> {
    let pages = match selection {
        PageSelection::One(query) => vec![runtime.query_task_rows(&query)?],
        PageSelection::Sections { query, sections } => {
            runtime.query_task_row_pages(&query, &sections)?
        }
    };
    pages
        .into_iter()
        .map(|page| {
            let delivery_attention = runtime.delivery_attention_map(page.items.iter())?;
            let run_branches = runtime.local_run_branches(page.items.iter());
            let detail_dependency_statuses = detail
                .and_then(|id| page.items.iter().find(|row| row.task.id == id))
                .map(|row| runtime.dependency_status_index([&row.task]));
            Ok(PageRead {
                page,
                delivery_attention,
                run_branches,
                detail_dependency_statuses,
            })
        })
        .collect()
}
