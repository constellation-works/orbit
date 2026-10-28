use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::operations;
use super::super::answers::{PullRequestSummary, read_pr_status};

/// [ORB-15308] Close the pull request an earlier claim opened for the
/// candidate this run resumed (`candidate_resume`'s `prior_pull_request`, on
/// its `prior_pull_request_head` [ORB-15570], else its `source_branch`) when
/// this run published through another one, with a comment naming it, so the
/// task never has two open pull requests. A pull request that is no longer
/// open, or whose head is not that branch, is left alone. The PR this run
/// opened stands either way: a failed close is reported in the output and
/// logged, never a step failure. `Null` when there was no earlier pull
/// request or this run reused it.
pub(super) fn supersede_prior_pull_request<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    input: &Value,
    opened: &Value,
) -> Value {
    let resumed = input.get("candidate_resume").unwrap_or(&Value::Null);
    let Some(prior) = input_string_field(resumed, "prior_pull_request") else {
        return Value::Null;
    };
    let number = opened["pr_number"].as_str().unwrap_or_default();
    if prior == number {
        return Value::Null;
    }
    let outcome = |decision: &str, detail: String| json!({"number": prior, "decision": decision, "detail": detail});
    let prior_branch = input_string_field(resumed, "prior_pull_request_head")
        .or_else(|| input_string_field(resumed, "source_branch"))
        .unwrap_or_default();
    let pull_request = match read_pr_status(host, workspace_path, &prior) {
        Ok(pull_request) => pull_request,
        Err(error) => return close_failed(&prior, number, error),
    };
    let state = pull_request.state.as_deref().unwrap_or_default();
    let head = pull_request
        .exact_head_branch
        .as_deref()
        .unwrap_or_default();
    if !state.eq_ignore_ascii_case("open") {
        return outcome("not_open", format!("pull request #{prior} is {state}"));
    }
    if prior_branch.is_empty() || head != prior_branch {
        return outcome(
            "other_branch",
            format!(
                "pull request #{prior} is on '{head}', not the resumed candidate's branch \
                 '{prior_branch}'"
            ),
        );
    }
    let new_pr = opened["pr_url"]
        .as_str()
        .map_or_else(|| format!("#{number}"), |url| format!("#{number} ({url})"));
    let comment = format!(
        "Superseded by {new_pr}.\n\nThe task's next run continued this pull request's \
         candidate but could not publish it on `{prior_branch}`, so it published on `{}` \
         instead. Orbit closed this pull request so the task has one open pull request. The \
         branch is kept.",
        opened["head"].as_str().unwrap_or_default()
    );
    match host.run_private_vcs_operation(
        operations::PR_CLOSE,
        json!({"pr": prior, "comment": comment, "workspace_path": workspace_path}),
    ) {
        Ok(_) => {
            tracing::info!(
                superseded = %prior,
                pr = number,
                "closed the pull request this run superseded"
            );
            outcome("closed", format!("superseded by {new_pr}"))
        }
        Err(error) => close_failed(&prior, number, error),
    }
}

/// Close every other open delivery pull request of the tasks this run
/// published, once it created its own, so each task keeps only that one.
///
/// A fresh attempt of a re-admitted task opens its own pull request on a new
/// branch, and the earlier attempt's, a `[BLOCKED]` failure-handoff one
/// included, would otherwise stay open beside it with nothing watching it.
/// Re-admission is what ends the failure handoff's hold on its pull request,
/// so it is closed here too. A pull request qualifies only on the same terms
/// a terminal decision closes one by: an `orbit/<TASK-ID>-…` head branch, a
/// body naming the task, and an author among `pr.delivery_authors` (when
/// that is empty, the author of the pull request this run published). Only
/// open pull requests are listed, so a merged one is never touched, and
/// branches are kept. `pr.close_on_terminal = false` turns this off. Best
/// effort: a failed lookup or close is reported and logged, never a step
/// failure.
pub(in crate::executor::automation::vcs) fn close_earlier_task_pull_requests<
    H: RuntimeHost + ?Sized,
>(
    host: &H,
    workspace_path: &std::path::Path,
    tasks: &[orbit_types::task::Task],
    kept: &str,
    kept_url: Option<&str>,
    head: &str,
) -> Value {
    let config = host.pr_config();
    if !config.close_superseded || kept.is_empty() || tasks.is_empty() {
        return json!([]);
    }
    let listed = match host
        .run_private_vcs_operation(
            operations::PR_LIST_OPEN,
            json!({ "workspace_path": workspace_path }),
        )
        .and_then(|response| PullRequestSummary::list_from_response(&response))
    {
        Ok(listed) => listed,
        Err(error) => {
            tracing::warn!(
                pr = kept,
                error = %error,
                "could not list open pull requests to close the tasks' earlier ones"
            );
            return json!([{
                "decision": "lookup_failed",
                "detail": format!(
                    "the tasks' earlier pull requests may still be open beside #{kept}: {error}"
                ),
            }]);
        }
    };
    let authors = if config.delivery_authors.is_empty() {
        listed
            .iter()
            .find(|pull_request| pull_request.number.as_deref() == Some(kept))
            .and_then(|pull_request| pull_request.author.clone())
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        config.delivery_authors.clone()
    };
    let authored = |author: Option<&str>| {
        author.is_some_and(|author| {
            authors
                .iter()
                .any(|delivery| delivery.eq_ignore_ascii_case(author))
        })
    };
    let new_pr = kept_url.map_or_else(|| format!("#{kept}"), |url| format!("#{kept} ({url})"));
    let mut outcomes = Vec::new();
    for pull_request in &listed {
        let (Some(number), Some(branch)) = (
            pull_request.number.as_deref(),
            pull_request.head_branch.as_deref(),
        ) else {
            continue;
        };
        if number == kept || !authored(pull_request.author.as_deref()) {
            continue;
        }
        let body = pull_request.body.as_deref().unwrap_or_default();
        let Some(task) = tasks.iter().find(|task| {
            orbit_types::task::names_task_delivery_pull_request(
                &config.conventions.branch_name,
                branch,
                body,
                &task.id,
            )
        }) else {
            continue;
        };
        let comment = format!(
            "Superseded by {new_pr}.\n\nA newer delivery attempt of task {} published on \
             `{head}`. Orbit closed this pull request so the task has one open pull request. \
             The branch is kept.",
            task.id
        );
        let (decision, detail) = match host.run_private_vcs_operation(
            operations::PR_CLOSE,
            json!({"pr": number, "comment": comment, "workspace_path": workspace_path}),
        ) {
            Ok(_) => {
                tracing::info!(
                    task_id = %task.id,
                    closed = number,
                    pr = kept,
                    "closed a task's earlier delivery pull request for this run's"
                );
                ("closed", format!("superseded by {new_pr}"))
            }
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id,
                    stale = number,
                    pr = kept,
                    error = %error,
                    "could not close a task's earlier delivery pull request"
                );
                (
                    "close_failed",
                    format!(
                        "pull request #{number} is still open beside #{kept}; close it by hand: \
                         {error}"
                    ),
                )
            }
        };
        outcomes.push(json!({
            "number": number,
            "task_id": task.id,
            "decision": decision,
            "detail": detail,
        }));
    }
    Value::Array(outcomes)
}

fn close_failed(prior: &str, number: &str, error: OrbitError) -> Value {
    tracing::warn!(
        superseded = %prior,
        pr = number,
        error = %error,
        "could not close the pull request this run superseded; two pull requests are open"
    );
    json!({
        "number": prior,
        "decision": "close_failed",
        "detail": format!(
            "pull request #{prior} is still open beside #{number}; close it by hand: {error}"
        ),
    })
}
