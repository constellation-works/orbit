//! The dashboard's task writes and read models. Each calls the runtime
//! method the dashboard's handler called before.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use serde_json::Value;

use super::projection::{TaskListProjection, task_detail_json};
use super::{
    DashboardAction, DashboardWrite, Outcome, PageRead, PageSelection, Read, RowRead, RowsRead,
    ScreenedWrite, TaskPages,
};
use crate::OrbitRuntime;
use crate::app::codebase::TaskRiskinessModel;
use crate::application::task::{
    DashboardPageReads, ScreenedTaskWrite, TaskUpdateIdentity, TaskUpdateParams,
    TaskWriteResponsePolicy, TaskWriteScreen, screened_task_write_response_json,
    with_task_write_provenance,
};

/// What one dashboard write produced, before its answer is rendered.
enum Applied {
    Screened(Box<ScreenedTaskWrite>),
    Outcome(Outcome),
}

/// How one read's pages are rendered.
struct PageRender<'a> {
    /// The task whose detail the page holding it answers.
    detail: Option<&'a str>,
    riskiness: Option<Arc<TaskRiskinessModel>>,
    detail_candidate: bool,
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
        Read::Pages {
            select,
            detail,
            riskiness,
            detail_candidate,
        } => {
            let workspace_id = runtime.workspace_id()?;
            let selection = (select.0)(&workspace_id)?;
            let render = PageRender {
                detail: detail.as_deref(),
                riskiness,
                detail_candidate,
            };
            Ok(Outcome::Pages(TaskPages {
                workspace_id,
                pages: pages(runtime, selection, &render),
            }))
        }
        Read::Row { id } => {
            // Display reads answer from the settled snapshot, outside the
            // task-commit boundary, so a held task writer never stalls them.
            let row = runtime.observe_task_row(&id)?;
            // The coordination registry's global status index, not a
            // workspace-scoped list: a dependency on another registered
            // workspace's task labels with its real status. Archived
            // dependencies that reached `done` first label as `done`.
            let dependency_statuses = runtime.observe_dependency_status_index([&row.task]);
            Ok(Outcome::Row(Box::new(RowRead {
                row,
                dependency_statuses,
            })))
        }
        Read::Detail {
            read,
            riskiness,
            include_candidate,
        } => {
            let RowRead {
                row,
                dependency_statuses,
            } = *read;
            task_detail_json(
                runtime,
                &row,
                &dependency_statuses?,
                riskiness.as_deref(),
                &DashboardPageReads::default(),
                include_candidate,
            )
            .map(Outcome::Answer)
        }
        Read::Rows(read) => {
            let RowsRead {
                rows,
                dependency_statuses,
                delivery_attention,
                run_branches,
                riskiness,
            } = *read;
            let projection = TaskListProjection::new(runtime)
                .with_delivery_attention(delivery_attention)
                .with_run_branches(run_branches)
                .with_riskiness(riskiness);
            rows.iter()
                .map(|row| projection.row_to_json(runtime, row, &dependency_statuses))
                .collect::<Result<Vec<_>, _>>()
                .map(Outcome::Rows)
        }
        Read::Locks => runtime.task_locks_snapshot().map(Outcome::Answer),
        Read::CompletionByComplexity => runtime
            .task_completion_by_complexity()
            .map(Outcome::Completion),
    }
}

/// The selected pages, each rendered with what its rows show beside the task.
fn pages(
    runtime: &OrbitRuntime,
    selection: PageSelection,
    render: &PageRender<'_>,
) -> Result<Vec<PageRead>, OrbitError> {
    // Display pages answer from the settled snapshot, outside the
    // task-commit boundary (see `observe_task_rows`).
    let pages = match selection {
        PageSelection::One(query) => vec![runtime.observe_task_rows(&query)?],
        PageSelection::Sections { query, sections } => {
            runtime.observe_task_row_pages(&query, &sections)?
        }
    };
    pages
        .into_iter()
        .map(|page| {
            let delivery_attention = runtime.delivery_attention_map(page.items.iter())?;
            let run_branches = runtime.local_run_branches(page.items.iter());
            let detail_row = render
                .detail
                .and_then(|id| page.items.iter().find(|row| row.task.id == id));
            let detail_statuses =
                detail_row.map(|row| runtime.observe_dependency_status_index([&row.task]));
            let projection = TaskListProjection::new(runtime)
                .with_delivery_attention(delivery_attention)
                .with_run_branches(run_branches)
                .with_riskiness(render.riskiness.clone());
            let items = page
                .items
                .iter()
                .map(|row| projection.row_to_json(runtime, row, &page.status_by_id))
                .collect::<Result<Vec<_>, _>>()?;
            // The page's rows already read the workspace-wide triage, pilot
            // preparation, pull-request links and delivery attention this
            // detail shows; it borrows them rather than reading each again
            // [ORB-21825].
            let detail = detail_row.zip(detail_statuses).map(|(row, statuses)| {
                task_detail_json(
                    runtime,
                    row,
                    &statuses?,
                    render.riskiness.as_deref(),
                    &projection.detail_page_reads(),
                    render.detail_candidate,
                )
            });
            drop(projection);
            Ok(PageRead {
                page,
                items,
                detail,
            })
        })
        .collect()
}
