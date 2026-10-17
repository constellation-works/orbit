use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    FLAKY_VALIDATION_EVENT, FlakyValidation, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::input::{input_string_field, required_input_string};
use super::super::base_obsolescence::ensure_base_can_still_land;
use super::super::freshness::branch_freshness_against_ref;
use super::super::git::git_output;
use super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, record_failed_handoff,
};
use super::super::operations;
use super::answers::{PullRequestStatus, PullRequestSummary, read_pr_status};
use super::body::{
    GITHUB_PR_BODY_BYTE_LIMIT, bound_pr_body, build_batch_pr_body, default_pr_title,
    render_known_red_section,
};

pub(in crate::executor::automation) fn pr_open<H: RuntimeHost + Sync + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "pr_open")?;
    match open_or_reuse_pr(host, input, &context) {
        Ok(mut output) => {
            output["superseded_pull_request"] =
                supersede_prior_pull_request(host, &context.workspace_path, input, &output);
            // A reused pull request was opened by this lineage, whose create
            // already closed the tasks' other ones.
            output["closed_stale_pull_requests"] = if output["pr_created"] == true {
                close_earlier_task_pull_requests(
                    host,
                    &context.workspace_path,
                    &context.tasks,
                    output["pr_number"].as_str().unwrap_or_default(),
                    output["pr_url"].as_str(),
                    output["head"].as_str().unwrap_or_default(),
                )
            } else {
                json!([])
            };
            Ok(output)
        }
        Err(failure) => {
            let (phase, error) = *failure;
            record_failed_handoff(host, &context, input, phase, &error)?;
            Err(error)
        }
    }
}

fn open_or_reuse_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
) -> Result<Value, Box<(FailedHandoffPhase, OrbitError)>> {
    let head = required_input_string(input, "head").map_err(invalid_prepare)?;
    let base = required_input_string(input, "base").map_err(invalid_prepare)?;
    let base_ref = required_input_string(input, "base_ref").map_err(invalid_prepare)?;
    let base_sha = required_input_string(input, "base_sha").map_err(invalid_prepare)?;
    let current_branch = git_output(
        &context.workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .map_err(invalid_prepare)?;
    if current_branch.trim() != head {
        return Err(Box::new((
            FailedHandoffPhase::PrLookup,
            OrbitError::Execution(format!(
                "pr_open: prepared branch '{head}' is not checked out (found '{}')",
                current_branch.trim()
            )),
        )));
    }
    // ORB-10644: divergence against the pinned base says nothing about whether
    // that base is still a branch work can land through. A base that merged and
    // was deleted (or restored to its pre-merge tip) still resolves, so every
    // later step would report success against a PR nobody merges again.
    ensure_base_can_still_land(&context.workspace_path, "pr_open", base, base_sha, input)
        .map_err(|error| Box::new((FailedHandoffPhase::ObsoleteBase, error)))?;
    // [ORB-11333] A before-PR gate binds to exact head and base commits. The
    // PR is only opened for the candidate the reviewer actually settled.
    ensure_reviewed_candidate(&context.workspace_path, input, base_sha)
        .map_err(|error| Box::new((FailedHandoffPhase::StaleReviewGate, error)))?;
    let freshness = branch_freshness_against_ref(&context.workspace_path, head, base_ref, base_sha)
        .map_err(invalid_prepare)?;
    if freshness.commits_behind != 0 || freshness.commits_ahead == 0 {
        return Err(Box::new((
            FailedHandoffPhase::EmptyBranch,
            OrbitError::Execution(format!(
                "pr_open: prepared head '{head}' must be ahead of and not behind base checkpoint '{base_sha}'"
            )),
        )));
    }
    let diff_output = git_output(
        &context.workspace_path,
        &["diff", "--name-only", &format!("{base_sha}...{head}")],
    )
    .map_err(invalid_prepare)?;
    let changed_files = diff_output
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    if let Some(admission) =
        super::super::candidate_resume::source_repair::publication_admission(host, input)
            .map_err(|e| Box::new((FailedHandoffPhase::PrLookup, e)))?
    {
        let binding = &admission;
        super::source_identity::verify_publication(host, context, binding, head, base)
            .map_err(|e| Box::new((FailedHandoffPhase::PrLookup, e)))?;
        return Ok(pr_output(PrOutput {
            decision: "reused",
            pr_created: false,
            pr_reused: true,
            pr_number: binding.pull_request.clone(),
            pr_url: Some(binding.pull_request_url.clone()),
            base,
            head,
            base_ref,
            base_sha,
            freshness: &freshness,
        }));
    }

    let title = input_string_field(input, "title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default_pr_title(&context.tasks));
    let pr_config = host.pr_config();
    let pr_opener_model = host.actor_model_identity();
    let body = input_string_field(input, "body")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            build_batch_pr_body(
                &context.tasks,
                &freshness,
                &changed_files,
                &pr_config,
                pr_opener_model.as_deref(),
            )
        });
    let known_red = collect_validation_records(
        host,
        context,
        base_sha,
        input,
        (KNOWN_RED_VALIDATION_EVENT, "known_red"),
        KnownRedValidation::from_text,
        |record| ValidationRecord {
            command: &record.command,
            run_id: &record.run_id,
            base_sha: &record.base_sha,
            tested_head: &record.tested_head,
            valid: true,
        },
    );
    let body = append_section(body, render_known_red_section(&known_red));
    let flaky = collect_validation_records(
        host,
        context,
        base_sha,
        input,
        (FLAKY_VALIDATION_EVENT, "flaky"),
        FlakyValidation::from_text,
        |record| ValidationRecord {
            command: &record.command,
            run_id: &record.run_id,
            base_sha: &record.base_sha,
            tested_head: &record.tested_head,
            valid: record.valid(),
        },
    );
    let body = append_section(body, super::body::render_flaky_section(&flaky));
    let body = bound_pr_body(with_review_fixes(body, input), &context.tasks);
    // A failed create may already have taken effect. Reconcile by head before
    // every retry; the private create operation must remain single-shot.
    let mut attempt = 1;
    loop {
        match find_pr_by_head(host, &context.workspace_path, head) {
            Ok(Some((pr_number, pr_url))) => {
                return Ok(pr_output(PrOutput {
                    decision: "reused",
                    pr_created: false,
                    pr_reused: true,
                    pr_number,
                    pr_url,
                    base,
                    head,
                    base_ref,
                    base_sha,
                    freshness: &freshness,
                }));
            }
            Ok(None) => {
                tracing::info!(
                    constructed_body_bytes = body.len(),
                    allowed_body_bytes = GITHUB_PR_BODY_BYTE_LIMIT,
                    "creating pull request with bounded body projection"
                );
                let created = match host.run_private_vcs_operation(
                    operations::PR_CREATE,
                    json!({
                        "title": title,
                        "body": body,
                        "base": base,
                        "head": head,
                        "workspace_path": context.workspace_path,
                    }),
                ) {
                    Ok(created) => created,
                    Err(error)
                        if attempt < operations::GITHUB_TRANSIENT_ATTEMPTS
                            && operations::is_transient_github_failure(&error.to_string()) =>
                    {
                        tracing::warn!(
                            attempt,
                            head,
                            "reconciling pull request creation after a transient GitHub failure"
                        );
                        super::super::remote_probe::fixed_wait(
                            operations::GITHUB_TRANSIENT_RETRY_DELAY,
                        );
                        attempt += 1;
                        continue;
                    }
                    Err(error) => return Err(Box::new((FailedHandoffPhase::PrCreate, error))),
                };
                let pr_url = created
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        Box::new((
                            FailedHandoffPhase::PrCreate,
                            OrbitError::Execution(
                                "private automation VCS PR create did not return a PR url"
                                    .to_string(),
                            ),
                        ))
                    })?;
                let (pr_number, viewed_url) = view_pr(host, &context.workspace_path, &pr_url)
                    .map_err(|error| Box::new((FailedHandoffPhase::PrView, error)))?;
                return Ok(pr_output(PrOutput {
                    decision: "performed",
                    pr_created: true,
                    pr_reused: false,
                    pr_number,
                    pr_url: viewed_url.or(Some(pr_url)),
                    base,
                    head,
                    base_ref,
                    base_sha,
                    freshness: &freshness,
                }));
            }
            Err(error) => return Err(Box::new((FailedHandoffPhase::PrLookup, error))),
        }
    }
}

/// [ORB-15308] Close the pull request an earlier claim opened for the
/// candidate this run resumed (`candidate_resume`'s `prior_pull_request`, on
/// its `prior_pull_request_head` [ORB-15570], else its `source_branch`) when
/// this run published through another one, with a comment naming it, so the
/// task never has two open pull requests. A pull request that is no longer
/// open, or whose head is not that branch, is left alone. The PR this run
/// opened stands either way: a failed close is reported in the output and
/// logged, never a step failure. `Null` when there was no earlier pull
/// request or this run reused it.
fn supersede_prior_pull_request<H: RuntimeHost + ?Sized>(
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
            orbit_types::task::names_task_delivery_pull_request(branch, body, &task.id)
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

/// Create or reuse a PR for an already-pushed recovery branch without the
/// normal freshness/success gate. The caller owns candidate validation and
/// must block, never promote, the associated task.
pub(in crate::executor::automation::vcs) fn open_or_reuse_unchecked<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<(String, Option<String>, bool), OrbitError> {
    let body = bound_pr_body(body.to_string(), &[]);
    if let Some((number, url)) = find_pr_by_head(host, workspace_path, head)? {
        return Ok((number, url, false));
    }
    tracing::info!(
        constructed_body_bytes = body.len(),
        allowed_body_bytes = GITHUB_PR_BODY_BYTE_LIMIT,
        "creating unchecked pull request with bounded body projection"
    );
    let created = host.run_private_vcs_operation(
        operations::PR_CREATE,
        json!({
            "title": title,
            "body": body,
            "base": base,
            "head": head,
            "workspace_path": workspace_path,
        }),
    )?;
    let url = created
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR create did not return a PR url".to_string(),
            )
        })?;
    let (number, viewed_url) = view_pr(host, workspace_path, url)?;
    Ok((number, viewed_url.or_else(|| Some(url.to_string())), true))
}

/// Borrowed fields used to admit and deduplicate either kind of evidence.
struct ValidationRecord<'a> {
    command: &'a str,
    run_id: &'a str,
    base_sha: &'a str,
    tested_head: &'a str,
    valid: bool,
}

/// Keep the latest matching record per command, from delivered tasks' history
/// followed by a claimed leaf's handed validation output. Claimed leaves cannot
/// write the owner's task history.
fn collect_validation_records<H: RuntimeHost + ?Sized, T: DeserializeOwned>(
    host: &H,
    context: &HandoffContext,
    base_sha: &str,
    input: &Value,
    (event, key): (&str, &str),
    parse: fn(&str) -> Option<T>,
    fields: impl Fn(&T) -> ValidationRecord<'_>,
) -> Vec<T> {
    let kind = key.replace('_', "-");
    let head = match git_output(&context.workspace_path, &["rev-parse", "HEAD"]) {
        Ok(head) => head,
        Err(error) => {
            tracing::warn!("pr_open could not read HEAD for {kind} validation: {error}");
            return Vec::new();
        }
    };
    let mut records = BTreeMap::new();
    let mut insert_matching = |record| {
        let evidence = fields(&record);
        if evidence.valid
            && evidence.run_id == context.batch_id
            && evidence.base_sha == base_sha
            && evidence.tested_head == head
        {
            records.insert(evidence.command.to_string(), record);
        }
    };
    for task in &context.tasks {
        let history = match host.get_task_history(&task.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = task.id,
                    "pr_open could not read task history for {kind} validation: {error}"
                );
                continue;
            }
        };
        for record in history
            .iter()
            .filter(|entry| entry.event == event)
            .filter_map(|entry| entry.note.as_deref().and_then(parse))
        {
            insert_matching(record);
        }
    }
    if let Some(handed) = input
        .get("claim_validation")
        .and_then(|validation| validation.get(key))
        .filter(|value| !value.is_null())
    {
        match serde_json::from_value::<Vec<T>>(handed.clone()) {
            Ok(handed) => {
                for record in handed {
                    insert_matching(record);
                }
            }
            Err(error) => tracing::warn!("pr_open ignored a malformed {key} input: {error}"),
        }
    }
    records.into_values().collect()
}

fn append_section(body: String, section: Option<String>) -> String {
    match section {
        Some(section) => format!("{}\n\n{section}\n", body.trim_end()),
        None => body,
    }
}

/// Append the settled review's sections — fixes [ORB-13989], raw validation
/// evidence, and validation limits [ORB-14192] — to the PR body, generated or
/// supplied, so the reviewer commit and what the review established are
/// visible where it is published. Without any applicable sections the body
/// is unchanged.
fn with_review_fixes(body: String, input: &Value) -> String {
    append_section(
        body,
        input_string_field(input, "review_fixes")
            .map(|section| section.trim().to_string())
            .filter(|section| !section.is_empty()),
    )
}

/// Refuse to publish when the checked-out head or the pinned base differ from
/// the candidate the review gate settled. An empty `reviewed_head_sha` means
/// no gate applied to this run.
pub(in crate::executor::automation::vcs) fn ensure_reviewed_candidate(
    workspace_path: &std::path::Path,
    input: &Value,
    base_sha: &str,
) -> Result<(), OrbitError> {
    let Some(reviewed_head) = input_string_field(input, "reviewed_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head_sha != reviewed_head {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: checked-out head {head_sha} is not the reviewed candidate \
             {reviewed_head}; the gate must settle the current candidate before a PR is opened"
        )));
    }
    if let Some(reviewed_base) = input_string_field(input, "reviewed_base_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        && reviewed_base != base_sha
    {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: base {base_sha} is not the reviewed base {reviewed_base}; a \
             candidate rebased onto a different base needs a fresh review"
        )));
    }
    Ok(())
}

fn invalid_prepare(error: OrbitError) -> Box<(FailedHandoffPhase, OrbitError)> {
    Box::new((FailedHandoffPhase::PrLookup, error))
}

fn view_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    selector: &str,
) -> Result<(String, Option<String>), OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_VIEW,
        json!({ "pr": selector, "workspace_path": workspace_path }),
    )?;
    let pull_request = PullRequestStatus::from_view_response(&response)?;
    let pr_number = pull_request.number_selector.ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS PR view did not return a PR number".to_string(),
        )
    })?;
    Ok((pr_number, pull_request.url))
}

fn find_pr_by_head<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    head: &str,
) -> Result<Option<(String, Option<String>)>, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_LIST,
        json!({ "head": head, "state": "open", "workspace_path": workspace_path }),
    )?;
    let pull_requests = PullRequestSummary::list_from_response(&response)?;

    let mut matching_pr_number = None;
    for pull_request in pull_requests {
        let listed_head = pull_request.head_branch.as_deref().ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR list returned a pull request without headRefName"
                    .to_string(),
            )
        })?;
        if listed_head != head {
            continue;
        }
        let pr_number = pull_request.number.ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR list returned a matching pull request without a number"
                    .to_string(),
            )
        })?;
        if matching_pr_number.replace(pr_number).is_some() {
            return Err(OrbitError::Execution(format!(
                "private automation VCS PR list returned multiple open pull requests for head branch '{head}'"
            )));
        }
    }

    matching_pr_number
        .map(|pr_number| view_pr(host, workspace_path, &pr_number))
        .transpose()
}

struct PrOutput<'a> {
    decision: &'a str,
    pr_created: bool,
    pr_reused: bool,
    pr_number: String,
    pr_url: Option<String>,
    base: &'a str,
    head: &'a str,
    base_ref: &'a str,
    base_sha: &'a str,
    freshness: &'a super::super::freshness::BranchFreshness,
}

fn pr_output(output: PrOutput<'_>) -> Value {
    json!({
        "phase": "pr_open",
        "decision": output.decision,
        "pr_created": output.pr_created,
        "pr_reused": output.pr_reused,
        "pr_number": output.pr_number,
        "pr_url": output.pr_url,
        "base": output.base,
        "head": output.head,
        "base_ref": output.base_ref,
        "base_sha": output.base_sha,
        "commits_behind": output.freshness.commits_behind,
        "commits_ahead": output.freshness.commits_ahead,
    })
}
