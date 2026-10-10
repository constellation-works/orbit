use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::input::{
    input_string_field, json_number_to_string, required_input_string,
};
use super::super::base_obsolescence::ensure_base_can_still_land;
use super::super::freshness::branch_freshness_against_ref;
use super::super::git::git_output;
use super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, record_failed_handoff,
};
use super::super::operations;
use super::body::{
    GITHUB_PR_BODY_BYTE_LIMIT, bound_pr_body, build_batch_pr_body, default_pr_title,
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
                        std::thread::sleep(operations::GITHUB_TRANSIENT_RETRY_DELAY);
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
/// its `source_branch`) when this run published through another one, with a
/// comment naming it, so the task never has two open pull requests. A pull request that is no longer
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
    let prior_branch = input_string_field(resumed, "source_branch").unwrap_or_default();
    let status = match host.run_private_vcs_operation(
        operations::PR_STATUS,
        json!({"pr": prior, "workspace_path": workspace_path}),
    ) {
        Ok(status) => status,
        Err(error) => return close_failed(&prior, number, error),
    };
    let pull_request = &status["pull_request"];
    let state = pull_request["state"].as_str().unwrap_or_default();
    let head = pull_request["headRefName"].as_str().unwrap_or_default();
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

/// Append the settled review's sections — fixes [ORB-13989], raw validation
/// evidence, and validation limits [ORB-14192] — to the PR body, generated or
/// supplied, so the reviewer commit and what the review established are
/// visible where it is published. Without any applicable sections the body
/// is unchanged.
fn with_review_fixes(body: String, input: &Value) -> String {
    match input_string_field(input, "review_fixes")
        .map(|section| section.trim().to_string())
        .filter(|section| !section.is_empty())
    {
        Some(section) => format!("{}\n\n{section}\n", body.trim_end()),
        None => body,
    }
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
    let value = host.run_private_vcs_operation(
        operations::PR_VIEW,
        json!({ "pr": selector, "workspace_path": workspace_path }),
    )?;
    let pull_request = value.get("pull_request").ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS PR view did not return pull_request metadata".to_string(),
        )
    })?;
    let pr_number = pull_request
        .get("number")
        .and_then(json_number_to_string)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR view did not return a PR number".to_string(),
            )
        })?;
    let pr_url = pull_request
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok((pr_number, pr_url))
}

fn find_pr_by_head<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &std::path::Path,
    head: &str,
) -> Result<Option<(String, Option<String>)>, OrbitError> {
    let value = host.run_private_vcs_operation(
        operations::PR_LIST,
        json!({ "head": head, "state": "open", "workspace_path": workspace_path }),
    )?;
    let pull_requests = value
        .get("pull_requests")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR list did not return pull_requests metadata".to_string(),
            )
        })?;

    let mut matching_pr_number = None;
    for pull_request in pull_requests {
        let listed_head = pull_request
            .get("headRefName")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "private automation VCS PR list returned a pull request without headRefName"
                        .to_string(),
                )
            })?;
        if listed_head != head {
            continue;
        }
        let pr_number = pull_request
            .get("number")
            .and_then(json_number_to_string)
            .ok_or_else(|| {
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
