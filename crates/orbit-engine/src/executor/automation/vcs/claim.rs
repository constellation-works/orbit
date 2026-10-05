//! Executable steps of a claimed distributed leaf [ORB-12616], and the
//! required-validation runner it shares with the owner's own delivery path
//! [ORB-13915].
//!
//! A claimed leaf never merges and never completes its task. It implements,
//! commits, publishes where its ship mode requires it, and then stops at a
//! typed handoff the owner alone may act on. These two activities are that
//! stopping point:
//!
//! - [`claim_validate`] resolves the candidate and its base from Git in the
//!   executor's own worktree, refuses a base the candidate does not descend
//!   from, runs the commands the *owner* requires on that exact candidate, and
//!   attaches one captured log per command to the owner's copy of the task.
//!   An empty list runs nothing and records that no required validation
//!   commands are configured, as [`candidate_validate`] does.
//! - [`claim_handoff`] re-observes the same identity, builds the typed
//!   [`TaskHandoff`], and records it as this claim's durable pending
//!   settlement before anything is sent to the owner.
//!
//! Neither activity trusts its input. Task, claim, machine and bound run come
//! from [`RuntimeHost::claim_execution_context`], which the runtime derives
//! from the process worker binding; a payload that disagrees with what Git and
//! that binding say is a refusal, never an override.
//!
//! `workflow.required_validation_commands` is the list of commands a workspace
//! requires every delivered candidate to pass. [`candidate_validate`] runs it
//! on the owner's own delivery path, after the candidate is synchronized onto
//! its base and before it is published or landed: an agent's report that it
//! ran the commands is not evidence; this step is. Both steps run each command
//! through [`run_required_command`], so they see the same shell, environment,
//! timeout, output capture and failure text. That environment is resolved from
//! the owner's login shell rather than the worker's launcher, and a command
//! that fails for lack of a tool is reported as the validation environment's
//! failure rather than the candidate's [ORB-13987]. The owner step fails like any
//! other deterministic step, so a workflow can attach `step_failure_recovery`
//! to repair the candidate (for example commit a formatting fix) before the
//! one post-recovery attempt reruns every command.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_common::text::floor_char_boundary;
use orbit_types::task::ContextWideningStep;
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffValidationLog, TaskHandoff,
};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};
use crate::executor::automation::input::{input_string_field, required_job_run_id};

use super::commit::attribute_candidate_paths;
use super::git::{
    BaseSyncMode, git_command_success, git_output, git_output_raw, git_success,
    resolve_worktree_start_point,
};
use super::handoff::{completed_task_ids_from_input, reports_failure};
use super::pr::{DeliveryPin, PrMergeState, classify_pr_state};
use super::required_command::run_required_command;
use super::review_gate::revision;

fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::PolicyDenied(message.into())
}

fn required_workspace(input: &Value) -> Result<std::path::PathBuf, OrbitError> {
    input_string_field(input, "workspace_path")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| OrbitError::InvalidInput("workspace_path is required".to_string()))
}

/// The delivery this claim's ship mode produces. `pr` needs the number the
/// run's own `pr_open` step observed; `local` publishes nothing.
///
/// `pub(super)` so the sibling unit test can hit this seam with a
/// `pr_open`-shaped string without observing Git [ORB-12640].
pub(super) fn delivery(
    context: &ClaimExecutionContext,
    input: &Value,
) -> Result<HandoffDelivery, OrbitError> {
    match context.ship_mode.as_str() {
        "local" => Ok(HandoffDelivery::LocalCandidate),
        "pr" => {
            let number = input
                .get("pull_request")
                .and_then(pull_request_number)
                .filter(|number| *number > 0)
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "a pr-mode claim hands off a published pull request; pull_request is \
                         required"
                            .to_string(),
                    )
                })?;
            Ok(HandoffDelivery::PullRequest { number })
        }
        other => Err(refused(format!(
            "claimed leaf ship mode '{other}' has no handoff delivery"
        ))),
    }
}

/// The PR number this step was handed, in either shape the run can produce
/// [ORB-12617] [ORB-12640].
///
/// `pr_open` reports `pr_number` as a *string* — a provider selector rather
/// than a quantity — and an exact step-output template forwards the source
/// JSON type unchanged, so the pr-mode leaf receives a string. Reading only
/// `as_u64` here made every published claimed PR fail at its handoff with
/// "pull_request is required" while the number was sitting right there.
fn pull_request_number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Repository identity for the handoff. A checkout with a remote names it; an
/// owner-local checkout has none to observe, so the owner workspace it was
/// claimed from is the identity of record.
fn repository(workspace_path: &Path, fallback: &str) -> String {
    git_output(workspace_path, &["remote", "get-url", "origin"])
        .ok()
        .and_then(|url| slug(&url))
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn slug(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let tail = trimmed.rsplit_once(':').map_or(trimmed, |(_, tail)| tail);
    let mut segments = tail.rsplitn(3, '/');
    let name = segments.next()?;
    let owner = segments.next()?;
    (!name.is_empty() && !owner.is_empty()).then(|| format!("{owner}/{name}"))
}

/// Observe a candidate and the base it sits on, in one checkout.
///
/// This is the single observation rule. The executor runs it on its own
/// worktree to build the handoff; the owner runs it again on its checkout to
/// decide whether the handoff describes anything real. Because both sides
/// derive the same fields the same way, an owner that disagrees is reporting a
/// genuine difference rather than a second implementation's quirk.
///
/// `source` names the branch to read, or `None` for whatever is checked out.
/// `fallback_repository` is used when the checkout has no remote to name.
/// `base_sync` is the run's sync mode (`local` or `remote`): the base *ref*
/// is resolved through [`resolve_worktree_start_point`], the same mapping
/// every other step of the claimed pipeline uses, so a remote-sync claim
/// still fetches `origin/<base>` rather than a lagging local
/// `refs/heads/<base>`. The recorded base is then the merge-base of the
/// candidate and that ref — the tip the candidate sits on — not the live
/// fetched tip. A later advance of `origin/<base>` is therefore not a
/// refusal of a candidate that was synchronized onto the earlier SHA.
pub fn observe_candidate(
    workspace_path: &Path,
    source: Option<&str>,
    base_branch: &str,
    landing_branch: &str,
    delivery: HandoffDelivery,
    fallback_repository: &str,
    base_sync: &str,
) -> Result<HandoffCandidate, OrbitError> {
    let source_branch = match source {
        Some(branch) => branch.to_string(),
        None => git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    if source_branch.trim().is_empty() || source_branch == "HEAD" {
        // A detached HEAD has no name the owner could resolve in its own
        // checkout, so the handoff would pin a branch that means something
        // different on each side.
        return Err(refused(
            "a claimed candidate must sit on a named branch; this checkout has a detached HEAD",
        ));
    }
    let candidate = revision(workspace_path, &source_branch)?;
    let sync_mode = match base_sync.trim() {
        "local" => BaseSyncMode::Local,
        "remote" => BaseSyncMode::Remote,
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "base_sync must be 'local' or 'remote', got '{other}'"
            )));
        }
    };
    let base_ref = resolve_worktree_start_point(workspace_path, base_branch, sync_mode)?;
    let tip = revision(workspace_path, &base_ref)?;
    // Same rule as `synchronized_base`: the candidate is judged against the
    // merge-base it sits on, not against a tip that can move under a fetch.
    if !git_command_success(
        workspace_path,
        &["merge-base", &candidate.commit, &base_ref],
    )? {
        return Err(refused(format!(
            "candidate '{}' does not descend from validated base '{}'",
            candidate.commit, tip.commit
        )));
    }
    let merge_base = git_output(
        workspace_path,
        &["merge-base", &candidate.commit, &base_ref],
    )?;
    let base = revision(workspace_path, &merge_base)?;
    if base.commit == candidate.commit {
        return Err(refused(
            "the candidate is the base itself; a claimed leaf hands off delivered work, and \
             no-diff delivery is not part of this route",
        ));
    }
    let ancestor = git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &base.commit,
            &candidate.commit,
        ],
    )?;
    if !ancestor {
        return Err(refused(format!(
            "candidate '{}' does not descend from validated base '{}'",
            candidate.commit, base.commit
        )));
    }
    let landing_branch = if landing_branch.trim().is_empty() {
        base_branch
    } else {
        landing_branch
    };
    Ok(HandoffCandidate {
        repository: repository(workspace_path, fallback_repository),
        source_branch,
        base_branch: base_branch.to_string(),
        landing_branch: landing_branch.to_string(),
        candidate,
        base,
        delivery,
    })
}

/// The executor's own observation: whatever is checked out, against the base
/// this claim was synchronized onto. A `base_sha` carried from `sync_base`
/// must be contained in the candidate (an ancestor), never compared to the
/// live `origin/<base>` tip: a later advance of that tip is not a refusal.
fn observe(
    workspace_path: &Path,
    context: &ClaimExecutionContext,
    input: &Value,
) -> Result<HandoffCandidate, OrbitError> {
    let candidate = observe_candidate(
        workspace_path,
        None,
        &context.base_branch,
        &context.landing_branch,
        delivery(context, input)?,
        &context.workspace_id,
        &claimed_base_sync(context, input)?,
    )?;
    if let Some(declared) = input_string_field(input, "base_sha") {
        let contains_declared = git_command_success(
            workspace_path,
            &[
                "merge-base",
                "--is-ancestor",
                "--end-of-options",
                &declared,
                &candidate.candidate.commit,
            ],
        )?;
        if !contains_declared {
            return Err(refused(format!(
                "candidate '{}' does not descend from validated base '{declared}'",
                candidate.candidate.commit
            )));
        }
    }
    Ok(candidate)
}

/// The claimed candidate must still be the clean checkout it was observed as.
fn require_clean_candidate(
    workspace_path: &Path,
    candidate: &HandoffCandidate,
) -> Result<(), OrbitError> {
    require_clean_checkout(
        workspace_path,
        &candidate.source_branch,
        &candidate.candidate.commit,
        "the claimed candidate",
    )
}

/// The run's sync mode, which every other claimed-leaf step already honors.
///
/// An explicit `base_sync` on the activity input wins. When it is absent, ship
/// mode is the durable proxy the admission transaction used to pin the run
/// (`local` → local ref, `pr` → `origin/<base>`). That fallback is required:
/// `base_sync_mode_from_input` treats a missing field as remote, which would
/// send the owner-local route looking for an origin it does not have.
fn claimed_base_sync(context: &ClaimExecutionContext, input: &Value) -> Result<String, OrbitError> {
    if let Some(value) = input_string_field(input, "base_sync") {
        return Ok(value);
    }
    match context.ship_mode.as_str() {
        "local" => Ok("local".to_string()),
        "pr" => Ok("remote".to_string()),
        other => Err(refused(format!(
            "claimed leaf ship mode '{other}' has no base sync mode"
        ))),
    }
}

/// Resolve one accepted revision in the *owner's* checkout, fetching it first
/// when the object only exists on the remote [ORB-12500].
///
/// The tree comparison is what makes this an observation rather than a
/// restatement: a commit id the owner can read but whose tree disagrees with
/// the accepted identity is not the object the evidence was produced against.
/// Both owner-side consumers — handoff acceptance here and the landing
/// attempt — resolve accepted revisions through this one rule.
pub(in crate::executor::automation::vcs) fn observe_accepted_revision(
    workspace_path: &Path,
    accepted: &SourceRevision,
    label: &str,
    context: &str,
) -> Result<SourceRevision, OrbitError> {
    if revision(workspace_path, &accepted.commit).is_err() {
        let _ = git_success(
            workspace_path,
            &[
                "fetch",
                "--quiet",
                "--end-of-options",
                "origin",
                &accepted.commit,
            ],
        );
    }
    let observed = revision(workspace_path, &accepted.commit).map_err(|error| {
        OrbitError::Execution(format!(
            "{context}: {label} commit {} is not readable in the owner checkout: {error}",
            accepted.commit
        ))
    })?;
    if observed.tree != accepted.tree {
        return Err(OrbitError::Execution(format!(
            "{context}: {label} commit {} has tree {} but the accepted handoff recorded {}",
            accepted.commit, observed.tree, accepted.tree
        )));
    }
    Ok(observed)
}

/// The owner's independent observation of a *published* candidate [ORB-12500].
///
/// A follower's word that it opened a pull request is not evidence that one
/// exists, points at the candidate it validated, or belongs to this
/// repository. This is the owner reading all three for itself before its claim
/// journal is allowed to promote anything:
///
/// 1. **The provider names the delivery.** The pull request's head branch,
///    base branch and head commit are read from the provider and pinned
///    against the submitted candidate, so a branch repointed after the handoff
///    was written is a refusal rather than an acceptance of stale identity.
/// 2. **A closed or self-contradictory pull request delivers nothing.** A
///    merged one is observable: the external write already happened and
///    refusing it here would only strand the settlement — completion still
///    requires separately recorded authority.
/// 3. **The objects are resolved in the owner's own checkout**, with the same
///    tree-identity and ancestry rules the executor applied, so the owner is
///    never quoting the worker's arithmetic back to itself.
///
/// `landing_branch` is the one field carried from the submitted candidate: it
/// is owner-resolved ship configuration captured at admission, not anything a
/// pull request reports.
pub fn observe_published_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    submitted: &HandoffCandidate,
) -> Result<HandoffCandidate, OrbitError> {
    let HandoffDelivery::PullRequest { number } = submitted.delivery else {
        return Err(refused(
            "owner observation of a published candidate requires a pull-request delivery",
        ));
    };
    let pr_number = number.to_string();
    let response = host.run_private_vcs_operation(
        super::operations::PR_STATUS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path.to_string_lossy(),
        }),
    )?;
    let status = response.get("pull_request").cloned().unwrap_or(Value::Null);

    match classify_pr_state(&status) {
        PrMergeState::Closed => {
            return Err(refused(format!(
                "pull request #{pr_number} was closed without merging; it delivers no \
                 candidate for this handoff"
            )));
        }
        PrMergeState::Contradictory(reason) => {
            return Err(refused(format!(
                "pull request #{pr_number} reports a contradictory merge state ({reason}); \
                 the owner accepts a delivery it can read unambiguously"
            )));
        }
        // Merged, mergeable, blocked, conflicted and pending all describe a
        // live delivery. Whether it may *land* is the landing attempt's
        // question, asked again against authority this acceptance does not
        // grant.
        PrMergeState::Merged
        | PrMergeState::Blocked(_)
        | PrMergeState::Conflict
        | PrMergeState::Mergeable
        | PrMergeState::Pending => {}
    }

    DeliveryPin::pinned(
        &submitted.source_branch,
        &submitted.base_branch,
        &submitted.candidate.commit,
    )
    .ensure_pinned_candidate(&status, &pr_number)?;

    // `gh` resolves a bare PR number against the *checkout's* own remote, so
    // the delivery is already scoped to this repository. Comparing the URL it
    // reports is still worth doing when the owner's origin is a provider
    // remote whose slug is comparable: that catches a handoff pointing at a
    // fork's pull request.
    let origin_url = git_output(workspace_path, &["remote", "get-url", "origin"]).ok();
    let repository = origin_url
        .as_deref()
        .and_then(slug)
        .unwrap_or_else(|| submitted.repository.clone());
    if origin_url
        .as_deref()
        .is_some_and(|url| url.contains("github.com"))
        && let Some(published) = published_repository(&status)
        && published != repository
    {
        return Err(refused(format!(
            "pull request #{pr_number} belongs to repository '{published}', not this \
             owner's '{repository}'"
        )));
    }

    let context = format!("handoff acceptance for pull request #{pr_number}");
    let candidate =
        observe_accepted_revision(workspace_path, &submitted.candidate, "candidate", &context)?;
    let base = observe_accepted_revision(workspace_path, &submitted.base, "base", &context)?;
    if !git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &base.commit,
            &candidate.commit,
        ],
    )? {
        return Err(refused(format!(
            "{context}: candidate '{}' does not descend from validated base '{}' in \
             the owner checkout",
            candidate.commit, base.commit
        )));
    }

    Ok(HandoffCandidate {
        repository,
        source_branch: reported(&status, "headRefName").ok_or_else(|| {
            refused(format!(
                "pull request #{pr_number} did not report its head branch"
            ))
        })?,
        base_branch: reported(&status, "baseRefName").ok_or_else(|| {
            refused(format!(
                "pull request #{pr_number} did not report its base branch"
            ))
        })?,
        landing_branch: submitted.landing_branch.clone(),
        candidate,
        base,
        delivery: submitted.delivery.clone(),
    })
}

fn reported(status: &Value, field: &str) -> Option<String> {
    status
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// The repository a pull request URL names, when the provider reported one.
fn published_repository(status: &Value) -> Option<String> {
    let url = reported(status, "url")?;
    let trimmed = url.trim_end_matches('/');
    let (repository, _) = trimmed.rsplit_once("/pull/")?;
    slug(repository)
}

/// Run the owner's required commands on the exact candidate and attach one
/// captured log per command to the owner's copy of the task.
///
/// An empty requirement list runs no command: the candidate is still observed
/// and pinned, and the output records `skipped_no_required_commands` with no
/// validation references.
pub(in crate::executor::automation) fn claim_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    let candidate = observe(&workspace_path, &context, input)?;
    require_clean_candidate(&workspace_path, &candidate)?;

    let mut logs = Vec::new();
    let mut commands = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in context.required_commands.iter().enumerate() {
        let run = run_required_command(host, &workspace_path, command)?;
        validation_env = run.environment_record();
        if !run.passed {
            return Err(run.failure(&candidate.candidate.commit));
        }
        require_clean_candidate(&workspace_path, &candidate)?;
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: context.workspace_id.clone(),
            task_id: context.task_id.clone(),
            claim_id: context.claim_id.clone(),
            machine_id: context.machine_id.clone(),
            run_id: context.run_id.clone(),
            candidate: candidate.clone(),
            tested_head: candidate.candidate.commit.clone(),
            command: run.command.clone(),
            exit_code: 0,
            output: run.output,
        };
        let content = serde_json::to_vec(&log)
            .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        let path = format!("validation/{}/{index}.json", context.claim_id);
        logs.push((path, content));
        commands.push(run.command);
    }

    // A later required command must not invalidate logs from an earlier one.
    // Publish them only after the whole suite has kept the candidate intact.
    require_clean_candidate(&workspace_path, &candidate)?;
    let mut references = Vec::new();
    for (path, content) in logs {
        host.attach_claim_validation_log(&path, content.clone())?;
        references.push(HandoffArtifactRef {
            path,
            sha256: sha256_hex(&content),
        });
    }

    let mut output = json!({
        "decision": "passed",
        "candidate": serde_json::to_value(&candidate)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation": serde_json::to_value(&references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "commands": commands,
        "tested_head": candidate.candidate.commit,
        "validated_base": candidate.base.commit,
        "validation_env": validation_env,
    });
    if context.required_commands.is_empty() {
        output["decision"] = json!(SKIPPED_NO_REQUIRED_COMMANDS);
        output["note"] = json!(NO_REQUIRED_COMMANDS_NOTE);
    }
    Ok(output)
}

/// The decision a validation step records when the workspace requires no
/// command, on the owner's delivery path and a claimed leaf alike.
const SKIPPED_NO_REQUIRED_COMMANDS: &str = "skipped_no_required_commands";

/// Why that decision ran nothing, in the step's own output.
const NO_REQUIRED_COMMANDS_NOTE: &str = "no required validation commands configured \
     (`workflow.required_validation_commands` is empty); no check ran";

/// Build the typed handoff and record it as this claim's durable settlement.
pub(in crate::executor::automation) fn claim_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    let candidate = observe(&workspace_path, &context, input)?;
    let validated: HandoffCandidate = input
        .get("candidate")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| OrbitError::InvalidInput(format!("invalid validated candidate: {error}")))?
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "claim_handoff requires the candidate its validation ran on".to_string(),
            )
        })?;
    if validated != candidate {
        return Err(refused(
            "the worktree moved after validation; the owner accepts only a handoff whose \
             evidence pins the candidate that is still checked out",
        ));
    }
    require_clean_candidate(&workspace_path, &candidate)?;
    let validation: Vec<HandoffArtifactRef> = input
        .get("validation")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("invalid validation references: {error}"))
        })?
        .unwrap_or_default();
    if validation.is_empty() && !context.required_commands.is_empty() {
        return Err(refused(
            "a typed handoff carries its captured required validation; none was supplied",
        ));
    }
    let execution_summary = handoff_execution_summary(input, &candidate, !validation.is_empty())?;

    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: context.workspace_id.clone(),
        task_id: context.task_id.clone(),
        claim_id: context.claim_id.clone(),
        machine_id: context.machine_id.clone(),
        run_id: context.run_id.clone(),
        candidate: candidate.clone(),
        // Only `review.before_pr = false` is admitted, so there is no reviewed SHA,
        // verdict or reviewer artifact to report and none is invented here.
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary,
        validation,
        footprint_widening: super::commit::validate_claim_new_paths(
            &workspace_path,
            &context.footprint,
            &candidate.base.commit,
            &candidate.candidate.commit,
        )?
        .1,
    };
    host.record_claim_handoff(&handoff)?;

    Ok(json!({
        "handed_off": true,
        "merged": false,
        "task_id": context.task_id,
        "candidate": candidate.candidate.commit,
        "base": candidate.base.commit,
        "delivery": match candidate.delivery {
            HandoffDelivery::LocalCandidate => "local_candidate",
            HandoffDelivery::PullRequest { .. } => "pull_request",
            HandoffDelivery::AlreadyLanded { .. } => "already_landed",
        },
    }))
}

/// Largest implementer summary a handoff carries. The summary is prose for a
/// reader, and the handoff travels to the owner in one coordination call.
const MAX_HANDOFF_SUMMARY_BYTES: usize = 64 * 1024;

/// The `execution_summary` the typed handoff carries to the owner.
///
/// Acceptance writes the handoff's summary over the owner's
/// `execution_summary`, and an implementer in claimed mode writes no owner
/// task state itself (distributed-drain design §3, "Claimed-mode
/// implementation"): it returns its summary in the implement step's output,
/// which the claimed pipelines pass here as `implementation`. In order:
///
/// 1. an explicit `execution_summary` input;
/// 2. `implementation.execution_summary`, else the step's short
///    `implementation.summary`;
/// 3. a generic delivery statement.
///
/// An implementer summary keeps its own words, gains the implementer's
/// `comment` and any `context_files_added` it reported (recorded as prose;
/// typed widening is recomputed from the candidate rather than this output), and ends with a delivery line naming the candidate. One whose
/// first line reports `Outcome: failed` is refused here, as the owner would
/// refuse it, so a failed implementation is never handed off as delivered
/// work.
pub(super) fn handoff_execution_summary(
    input: &Value,
    candidate: &HandoffCandidate,
    validated: bool,
) -> Result<String, OrbitError> {
    let validation = if validated {
        "required validation passed on the exact candidate and the owner holds every captured log"
    } else {
        "no required validation commands are configured, so no check ran"
    };
    let delivered = format!(
        "Claimed execution delivered candidate {} on base {}; {validation}.",
        candidate.candidate.commit, candidate.base.commit
    );
    let implementation = implementation_output(input);
    let Some(summary) = implementer_summary(input) else {
        return Ok(delivered);
    };
    if reports_failure(&summary) {
        return Err(refused(
            "the implementer's execution summary reports `Outcome: failed`; a claimed leaf \
             hands off only delivered work",
        ));
    }
    let mut composed = bounded_summary(&summary);
    if let Some(comment) = text(implementation.and_then(|output| output.get("comment"))) {
        composed.push_str("\n\nImplementer comment:\n");
        composed.push_str(&bounded_summary(&comment));
    }
    let added = implementation
        .and_then(|output| output.get("context_files_added"))
        .and_then(Value::as_array)
        .map(|selectors| {
            selectors
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !added.is_empty() {
        composed.push_str(
            "\n\nContext selectors the implementer reported (advisory; owner-validated widening \
             is derived from Git):",
        );
        for selector in added {
            composed.push_str("\n- ");
            composed.push_str(selector);
        }
    }
    composed.push_str("\n\n");
    composed.push_str(&delivered);
    Ok(composed)
}

/// The implement step's output, when the pipeline passed it as `implementation`.
fn implementation_output(input: &Value) -> Option<&Value> {
    input
        .get("implementation")
        .filter(|value| value.is_object())
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

/// Whether this step was handed this run's implementer output at all: an
/// explicit `execution_summary` or the implement step's `implementation`.
pub(super) fn carries_implementer_output(input: &Value) -> bool {
    input_string_field(input, "execution_summary").is_some()
        || implementation_output(input).is_some()
}

/// This run's implementer summary, trimmed, in the precedence
/// [`handoff_execution_summary`] documents (items 1 and 2). The claimed
/// delivery gate
/// ([`reject_failed_attempt`](super::handoff::reject_failed_attempt)) judges
/// the same text the handoff carries [ORB-13755].
pub(super) fn implementer_summary(input: &Value) -> Option<String> {
    let implementation = implementation_output(input);
    input_string_field(input, "execution_summary")
        .or_else(|| text(implementation.and_then(|output| output.get("execution_summary"))))
        .or_else(|| text(implementation.and_then(|output| output.get("summary"))))
}

/// `text`, cut to [`MAX_HANDOFF_SUMMARY_BYTES`] with the cut reported.
fn bounded_summary(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_HANDOFF_SUMMARY_BYTES {
        return text.to_string();
    }
    let cut = floor_char_boundary(text, MAX_HANDOFF_SUMMARY_BYTES);
    format!(
        "{}\n\n[summary truncated to {cut} of {} bytes]",
        &text[..cut],
        text.len()
    )
}

/// The tree a validation result describes is the committed HEAD only while
/// the named branch still points there and no tracked or untracked input
/// differs from it. Ignored build output is intentionally outside this check.
fn require_clean_checkout(
    workspace_path: &Path,
    branch: &str,
    commit: &str,
    subject: &str,
) -> Result<(), OrbitError> {
    let observed = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if observed != branch {
        return Err(OrbitError::PolicyDenied(format!(
            "the checked-out source branch moved from '{branch}' to '{observed}'; rerun validation"
        )));
    }
    let head = git_output(workspace_path, &["rev-parse", "HEAD"])?;
    if head != commit {
        return Err(OrbitError::PolicyDenied(format!(
            "the checked-out HEAD moved from validated candidate {commit} to {head}; rerun \
             validation"
        )));
    }
    if !git_output_raw(
        workspace_path,
        &["status", "--porcelain=v1", "--untracked-files=all", "-z"],
    )?
    .is_empty()
    {
        return Err(OrbitError::PolicyDenied(format!(
            "{subject} has staged, tracked, or untracked changes; commit or remove them and \
             rerun validation on the exact candidate"
        )));
    }
    Ok(())
}

/// Run the workspace's required commands on the owner's committed candidate
/// and attach one captured log per command to every task the run delivers.
///
/// With `ownership_base_sha` — the implementation head a before-PR reviewer
/// commit sits on [ORB-13989] — every path the candidate changed since that
/// commit is first attributed to the delivered tasks, widening their
/// selectors with review provenance over any path none of them covers,
/// whatever the requirement list holds.
///
/// An empty requirement list runs no command. Otherwise the candidate must be
/// a clean checkout of a named branch that contains the `base_sha` this run
/// synchronized onto, and must stay exactly that while the suite runs. A
/// failing command fails the step with its output after its log — and those
/// of the commands that passed before it — is attached.
pub(in crate::executor::automation) fn candidate_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let owned_paths = match input_string_field(input, "ownership_base_sha") {
        Some(ownership_base) => Some(attribute_reviewed_paths(host, input, &ownership_base)?),
        None => None,
    };
    let commands = host.required_validation_commands();
    if commands.is_empty() {
        let mut output = json!({
            "phase": "validate",
            "decision": SKIPPED_NO_REQUIRED_COMMANDS,
            "note": NO_REQUIRED_COMMANDS_NOTE,
            "commands": [],
            "validation": [],
        });
        if let Some(owned_paths) = owned_paths {
            output["owned_paths"] = json!(owned_paths);
        }
        return Ok(output);
    }
    let run_id = required_job_run_id(input, "candidate_validate")?.to_string();
    let task_ids = completed_task_ids_from_input(input).ok_or_else(|| {
        OrbitError::InvalidInput(
            "candidate_validate requires the run's completed_task_ids to attach its logs to"
                .to_string(),
        )
    })?;
    let workspace_path = input_string_field(input, "workspace_path")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| OrbitError::InvalidInput("workspace_path is required".to_string()))?;

    let branch = git_output(&workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.trim().is_empty() || branch == "HEAD" {
        return Err(OrbitError::PolicyDenied(
            "required validation runs on a named candidate branch; this checkout has a \
             detached HEAD"
                .to_string(),
        ));
    }
    let candidate = git_output(&workspace_path, &["rev-parse", "HEAD"])?;
    let base_sha = input_string_field(input, "base_sha");
    if let Some(base_sha) = base_sha.as_deref()
        && !git_command_success(
            &workspace_path,
            &[
                "merge-base",
                "--is-ancestor",
                "--end-of-options",
                base_sha,
                &candidate,
            ],
        )?
    {
        return Err(OrbitError::PolicyDenied(format!(
            "candidate '{candidate}' does not descend from synchronized base '{base_sha}'"
        )));
    }
    require_clean_checkout(&workspace_path, &branch, &candidate, "the candidate")?;

    let mut logs = Vec::new();
    let mut passed = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in commands.iter().enumerate() {
        let run = run_required_command(host, &workspace_path, command)?;
        validation_env = run.environment_record();
        let content = serde_json::to_vec(&json!({
            "schema_version": 1,
            "run_id": run_id,
            "task_ids": task_ids,
            "branch": branch,
            "tested_head": candidate,
            "base_sha": base_sha,
            "command": run.command,
            "exit_code": run.exit_code,
            "timed_out": run.timed_out,
            "output": run.output,
            "validation_env": validation_env,
            "failure_kind": run.failure_kind(),
            "missing_tool": run.missing_tool_name(),
        }))
        .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        logs.push((format!("validation/{run_id}/{index}.json"), content));
        if !run.passed {
            // The failing log is the evidence a recovery agent or reader
            // repairs from, so it is attached before the step fails.
            attach_logs(host, &task_ids, &run_id, &logs)?;
            return Err(run.failure(&candidate));
        }
        require_clean_checkout(&workspace_path, &branch, &candidate, "the candidate")?;
        passed.push(run.command);
    }

    let references = attach_logs(host, &task_ids, &run_id, &logs)?;
    let mut output = json!({
        "phase": "validate",
        "decision": "passed",
        "commands": passed,
        "branch": branch,
        "tested_head": candidate,
        "validation": serde_json::to_value(&references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation_env": validation_env,
    });
    if let Some(owned_paths) = owned_paths {
        output["owned_paths"] = json!(owned_paths);
    }
    Ok(output)
}

/// Attribute every path the candidate changed since `ownership_base` to the
/// delivered tasks. A reviewer may change any path a fix requires: a path
/// none of the tasks' selectors covers widens the first task's selectors with
/// review provenance rather than refusing. Shared ownership is accepted: a
/// reviewer fix may touch a path two batched tasks both declare.
fn attribute_reviewed_paths<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    ownership_base: &str,
) -> Result<Vec<String>, OrbitError> {
    let task_ids = completed_task_ids_from_input(input).ok_or_else(|| {
        OrbitError::InvalidInput(
            "candidate_validate requires the run's completed_task_ids to attribute ownership"
                .to_string(),
        )
    })?;
    let workspace_path = input_string_field(input, "workspace_path")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| OrbitError::InvalidInput("workspace_path is required".to_string()))?;
    let changed = git_output(
        &workspace_path,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "--end-of-options",
            &format!("{ownership_base}..HEAD"),
        ],
    )?;
    let changed = changed
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let tasks = task_ids
        .iter()
        .map(|task_id| host.get_task(task_id))
        .collect::<Result<Vec<_>, _>>()?;
    let run_id = required_job_run_id(input, "candidate_validate")?;
    attribute_candidate_paths(
        host,
        run_id,
        ContextWideningStep::Review,
        "candidate_validate",
        &changed,
        &workspace_path,
        &tasks,
        false,
    );
    Ok(changed.into_iter().collect())
}

fn attach_logs<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    run_id: &str,
    logs: &[(String, Vec<u8>)],
) -> Result<Vec<HandoffArtifactRef>, OrbitError> {
    let mut references = Vec::new();
    for (path, content) in logs {
        for task_id in task_ids {
            host.attach_task_validation_log(task_id, run_id, path, content.clone())?;
        }
        references.push(HandoffArtifactRef {
            path: path.clone(),
            sha256: sha256_hex(content),
        });
    }
    Ok(references)
}
