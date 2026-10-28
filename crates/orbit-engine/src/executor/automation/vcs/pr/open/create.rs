use orbit_common::OrbitError;
use orbit_types::workflow::{
    FLAKY_VALIDATION_EVENT, FlakyValidation, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation,
    MissingSectionPolicy,
};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::input::{input_string_field, required_input_string};
use super::super::super::base::obsolescence::ensure_base_can_still_land;
use super::super::super::freshness::branch_freshness_against_ref;
use super::super::super::git::git_output;
use super::super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, record_failed_handoff,
};
use super::super::super::operations;
use super::super::body::{
    BodyInputs, GITHUB_PR_BODY_BYTE_LIMIT, bound_pr_body, build_batch_pr_body, pr_title,
    render_known_red_section,
};
use super::{evidence, lookup, superseded, template};

pub(in crate::executor::automation) fn pr_open<H: RuntimeHost + Sync + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "pr_open")?;
    match open_or_reuse_pr(host, input, &context) {
        Ok(mut output) => {
            super::super::labels::apply_task_labels(host, &context, &mut output);
            output["superseded_pull_request"] = superseded::supersede_prior_pull_request(
                host,
                &context.workspace_path,
                input,
                &output,
            );
            // A reused pull request was opened by this lineage, whose create
            // already closed the tasks' other ones.
            output["closed_stale_pull_requests"] = if output["pr_created"] == true {
                superseded::close_earlier_task_pull_requests(
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
    evidence::ensure_reviewed_candidate(&context.workspace_path, input, base_sha)
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

    let publication =
        super::super::super::candidate_resume::source_repair::publication_admission(host, input)
            .map_err(|e| Box::new((FailedHandoffPhase::PrLookup, e)))?;
    if let Some(binding) = &publication {
        super::super::source_identity::verify_publication(host, context, binding, head, base)
            .map_err(|e| Box::new((FailedHandoffPhase::PrLookup, e)))?;
    }

    let pr_config = host.pr_config();
    let title = input_string_field(input, "title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| pr_title(&context.tasks, &pr_config));
    let pr_opener_model = host.actor_model_identity();
    let (body, missing_required) =
        match input_string_field(input, "body").filter(|value| !value.trim().is_empty()) {
            Some(body) => (body, Vec::new()),
            None => {
                let template = template::body_template_text(
                    &context.workspace_path,
                    &context.tasks,
                    &pr_config,
                    base_sha,
                )
                .map_err(|error| Box::new((FailedHandoffPhase::PrBody, error)))?;
                // The evidence commit is pushed before the body is built, so
                // every image link can pin its sha.
                let screenshots = super::super::screenshots::publish_screenshots(
                    host,
                    &context.workspace_path,
                    &context.tasks,
                    &pr_config,
                )
                .map_err(|error| Box::new((FailedHandoffPhase::PrBody, error)))?;
                let inputs = BodyInputs {
                    template: template.as_deref(),
                    filled: template::filled_sections(host, &context.tasks, &pr_config),
                    screenshots,
                };
                let rendered = build_batch_pr_body(
                    &context.tasks,
                    &freshness,
                    &changed_files,
                    &pr_config,
                    pr_opener_model.as_deref(),
                    &inputs,
                )
                .map_err(|error| Box::new((FailedHandoffPhase::PrBody, error)))?;
                (rendered.body, rendered.missing_required)
            }
        };
    let draft = pr_config.conventions.draft
        || (!missing_required.is_empty()
            && pr_config.conventions.missing_section == MissingSectionPolicy::Draft);
    if !missing_required.is_empty() {
        template::record_section_gap(host, context, &missing_required, &pr_config)
            .map_err(|error| Box::new((FailedHandoffPhase::PrBody, error)))?;
        if pr_config.conventions.missing_section == MissingSectionPolicy::Refuse {
            return Err(Box::new((
                FailedHandoffPhase::PrBody,
                OrbitError::Execution(format!(
                    "pr_required_section_missing: {}; pr.missing_required_section = \"refuse\" \
                     holds the pull request",
                    template::section_gaps(&missing_required)
                )),
            )));
        }
    }
    let known_red = evidence::collect_validation_records(
        host,
        context,
        base_sha,
        input,
        (KNOWN_RED_VALIDATION_EVENT, "known_red"),
        KnownRedValidation::from_entry,
        |record| evidence::ValidationRecord {
            command: &record.command,
            run_id: &record.run_id,
            base_sha: &record.base_sha,
            tested_head: &record.tested_head,
            valid: true,
        },
    );
    let body = evidence::append_section(body, render_known_red_section(&known_red));
    let flaky = evidence::collect_validation_records(
        host,
        context,
        base_sha,
        input,
        (FLAKY_VALIDATION_EVENT, "flaky"),
        FlakyValidation::from_entry,
        |record| evidence::ValidationRecord {
            command: &record.command,
            run_id: &record.run_id,
            base_sha: &record.base_sha,
            tested_head: &record.tested_head,
            valid: record.valid(),
        },
    );
    let body = evidence::append_section(body, super::super::body::render_flaky_section(&flaky));
    let body = bound_pr_body(evidence::with_review_fixes(body, input), &context.tasks);
    if let Some(binding) = &publication {
        let (_, _, pr) = lookup::view_pr(host, &context.workspace_path, &binding.pull_request)
            .map_err(|e| Box::new((FailedHandoffPhase::PrView, e)))?;
        super::super::draft_recovery::recover_section_gap_draft(
            host,
            context,
            &pr,
            &body,
            &missing_required,
            &pr_config,
        )
        .map_err(|e| Box::new((FailedHandoffPhase::PrBody, e)))?;
        return Ok(lookup::pr_output(lookup::PrOutput {
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

    // A failed create may already have taken effect. Reconcile by head before
    // every retry; the private create operation must remain single-shot.
    let mut attempt = 1;
    loop {
        match lookup::find_pr_by_head(host, &context.workspace_path, head) {
            Ok(Some((pr_number, pr_url, pr))) => {
                super::super::draft_recovery::recover_section_gap_draft(
                    host,
                    context,
                    &pr,
                    &body,
                    &missing_required,
                    &pr_config,
                )
                .map_err(|e| Box::new((FailedHandoffPhase::PrBody, e)))?;
                return Ok(lookup::pr_output(lookup::PrOutput {
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
                        "draft": draft,
                        "labels": pr_config.conventions.labels,
                        "reviewers": pr_config.conventions.reviewers,
                    }),
                ) {
                    Ok(created) => created,
                    Err(error)
                        if attempt < operations::GITHUB_TRANSIENT_ATTEMPTS
                            && operations::is_transient(&error) =>
                    {
                        tracing::warn!(
                            attempt,
                            head,
                            "reconciling pull request creation after a transient GitHub failure"
                        );
                        super::super::super::remote_probe::fixed_wait(
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
                let (pr_number, viewed_url, _) =
                    lookup::view_pr(host, &context.workspace_path, &pr_url)
                        .map_err(|error| Box::new((FailedHandoffPhase::PrView, error)))?;
                let mut output = lookup::pr_output(lookup::PrOutput {
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
                });
                output["draft"] = json!(draft);
                output["missing_required_sections"] = json!(missing_required);
                return Ok(output);
            }
            Err(error) => return Err(Box::new((FailedHandoffPhase::PrLookup, error))),
        }
    }
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
    if let Some((number, url, _)) = lookup::find_pr_by_head(host, workspace_path, head)? {
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
    let (number, viewed_url, _) = lookup::view_pr(host, workspace_path, url)?;
    Ok((number, viewed_url.or_else(|| Some(url.to_string())), true))
}

fn invalid_prepare(error: OrbitError) -> Box<(FailedHandoffPhase, OrbitError)> {
    Box::new((FailedHandoffPhase::PrLookup, error))
}
