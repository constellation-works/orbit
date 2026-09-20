//! Executable steps of a claimed distributed leaf [ORB-12616].
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
//! - [`claim_handoff`] re-observes the same identity, builds the typed
//!   [`TaskHandoff`], and records it as this claim's durable pending
//!   settlement before anything is sent to the owner.
//!
//! Neither activity trusts its input. Task, claim, machine and bound run come
//! from [`RuntimeHost::claim_execution_context`], which the runtime derives
//! from the process worker binding; a payload that disagrees with what Git and
//! that binding say is a refusal, never an override.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::workflow::ReviewTiming;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffReview, HandoffReviewDisposition,
    HandoffValidationLog, TaskHandoff,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::context::{ClaimExecutionContext, RuntimeHost};
use crate::executor::automation::input::input_string_field;

use super::git::{
    BaseSyncMode, git_command_success, git_output, git_success, resolve_worktree_start_point,
};
use super::pr::{DeliveryPin, PrMergeState, classify_pr_state};
use super::review_gate::revision;

/// Ceiling for one required validation command. Long enough for a real
/// repository check suite, short enough that a wedged command settles the
/// claim instead of holding it open indefinitely.
pub(super) const VALIDATION_TIMEOUT_MS: u64 = 45 * 60 * 1000;
/// Captured output kept per command. The log is owner-read evidence, not a
/// build log archive, so a runaway command cannot balloon the task bundle.
pub(super) const MAX_CAPTURED_OUTPUT_BYTES: usize = 256 * 1024;

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
pub(super) fn pull_request_number(value: &Value) -> Option<u64> {
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
            &["fetch", "--quiet", "origin", &accepted.commit],
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
pub(in crate::executor::automation) fn claim_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    if context.required_commands.is_empty() {
        return Err(refused(
            "this owner declares no required validation commands \
             (`workflow.required_validation_commands`), so no claimed handoff can be accepted",
        ));
    }
    let candidate = observe(&workspace_path, &context, input)?;

    // A repository check suite is not a Git invocation: it gets the same
    // allow-listed child environment an agent subprocess would, so a command
    // that needs a configured toolchain variable can still find it.
    let environment = host.agent_subprocess_environment(&[]);
    let mut references = Vec::new();
    let mut commands = Vec::new();
    for (index, command) in context.required_commands.iter().enumerate() {
        let command = command.trim();
        if command.is_empty() {
            return Err(refused(
                "owner required validation contains an empty command",
            ));
        }
        let outcome = run_process(
            &ExecRequest {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), command.to_string()],
                current_dir: Some(workspace_path.to_string_lossy().into_owned()),
                timeout_ms: Some(VALIDATION_TIMEOUT_MS),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::ClearAndSet(environment.clone()),
                debug: false,
            },
            &NoSandbox,
        )?;
        let output = capture(&outcome.stdout, &outcome.stderr);
        if outcome.timed_out || !outcome.success {
            return Err(OrbitError::Execution(format!(
                "required validation '{command}' did not pass on candidate {}: {output}",
                candidate.candidate.commit
            )));
        }
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: context.workspace_id.clone(),
            task_id: context.task_id.clone(),
            claim_id: context.claim_id.clone(),
            machine_id: context.machine_id.clone(),
            run_id: context.run_id.clone(),
            candidate: candidate.clone(),
            tested_head: candidate.candidate.commit.clone(),
            command: command.to_string(),
            exit_code: 0,
            output,
        };
        let content = serde_json::to_vec(&log)
            .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        let path = format!("validation/{}/{index}.json", context.claim_id);
        host.attach_claim_validation_log(&path, content.clone())?;
        references.push(HandoffArtifactRef {
            path,
            sha256: format!("{:x}", Sha256::digest(&content)),
        });
        commands.push(command.to_string());
    }

    Ok(json!({
        "candidate": serde_json::to_value(&candidate)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation": serde_json::to_value(&references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "commands": commands,
        "tested_head": candidate.candidate.commit,
        "validated_base": candidate.base.commit,
    }))
}

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
    let validation: Vec<HandoffArtifactRef> = input
        .get("validation")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("invalid validation references: {error}"))
        })?
        .unwrap_or_default();
    if validation.is_empty() {
        return Err(refused(
            "a typed handoff carries its captured required validation; none was supplied",
        ));
    }
    let execution_summary = input_string_field(input, "execution_summary").unwrap_or_else(|| {
        format!(
            "Claimed execution delivered candidate {} on base {}; required validation passed on \
             the exact candidate and the owner holds every captured log.",
            candidate.candidate.commit, candidate.base.commit
        )
    });

    let handoff = TaskHandoff {
        schema_version: 1,
        workspace_id: context.workspace_id.clone(),
        task_id: context.task_id.clone(),
        claim_id: context.claim_id.clone(),
        machine_id: context.machine_id.clone(),
        run_id: context.run_id.clone(),
        candidate: candidate.clone(),
        // Only `review_policy = none` is admitted, so there is no reviewed SHA,
        // verdict or reviewer artifact to report and none is invented here.
        review: HandoffReview {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        },
        execution_summary,
        validation,
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

/// Interleave what the command said, bounded. Truncation is reported inside
/// the captured text so a reader never mistakes a clipped log for the whole
/// output.
pub(super) fn capture(stdout: &str, stderr: &str) -> String {
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim_end());
    }
    if combined.len() <= MAX_CAPTURED_OUTPUT_BYTES {
        return combined;
    }
    let mut cut = MAX_CAPTURED_OUTPUT_BYTES;
    while cut > 0 && !combined.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "[truncated to {cut} of {} bytes]\n{}",
        combined.len(),
        &combined[..cut]
    )
}
