//! `orbit run auto --auto-fix`: the completion watcher's repair hand-off.
//!
//! `--auto-fix` authorizes repair, never merging. Completion keeps watching
//! the pull request it already watches and reads its merge state the same
//! way; only a `BLOCKED` state is read more finely here:
//!
//! - a required review, an unavailable review decision or an unreadable
//!   check rollup is an administrative blocker: the watcher stops and no
//!   agent runs;
//! - a conclusive required check still running waits, within the ordinary
//!   wait budget;
//! - failed checks are filtered to the ones branch
//!   protection requires on that head (an optional failure alone leaves the
//!   state `UNSTABLE`, which is mergeable, and beside a `BLOCKED` state it is
//!   not the blocker); none required, or an unreadable answer, stops. The
//!   required failures' jobs and logs are read on the exact head the rollup
//!   reports ([`head_check_evidence`]); a check no job answers, an unreadable
//!   job or log, or a runner or network failure stops without an agent. A
//!   required check run outside a workflow (a provider app's aggregate) is
//!   repaired only from its authenticated source failure annotations, which
//!   are retained with the task before the repair is admitted [ORB-16185].
//!   A failure the landing classifies as inherited from the base commit, while
//!   that base branch has not moved past it, spends no repair attempt
//!   ([`red_base_wait`]): the watcher keeps polling until the completion
//!   deadline and then stops with a base-red reason. The same hold covers a
//!   base that has moved to a commit whose same job has not concluded: that
//!   poll admits no repair. A moved tip whose same job has concluded, or that
//!   has no such job, is not this hold. A concluded success is the landed
//!   refresh, decided before the wait. A candidate-caused failure is still
//!   admitted from the same budget.
//!
//! An optional-only failure first passes the narrowly evidenced prerequisite
//! recovery in `infrastructure`: one durable rerun, no crew or source repair.
//!
//! A `DIRTY` pull request takes the existing conflict path (pinned rebase,
//! `pr_conflict_recovery`, lease-checked push, or a re-review round for a
//! reviewed head).
//!
//! Every repair is admitted by the host first ([`admit`]): the run's crew is
//! drawn once from `workflow.low_complexity_crews` and the pull request's
//! durable ledger dedupes the head and blocker and bounds the attempts. A
//! check repair is then handed to the step's repair activity as a failure
//! led by [`PR_AUTO_FIX_CHECK_REPAIR_MARKER`]; the executor routes it to the
//! job's `step_failure_recovery`, commits the repair and reruns completion.
//! That rerun finds the repair on the branch ([`publish_local_repair`]),
//! runs the workspace's required validation on it, and publishes it on the
//! pull request (or, for a reviewed head, hands it to the re-review round)
//! before it watches the new head. The published head is recorded in the
//! ledger, so a later round or a resume pins the head this run published.
//!
//! [`head_check_evidence`]: super::super::landing_checks::head_check_evidence

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    PR_AUTO_FIX_CHECK_REPAIR_MARKER, PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixAttempt, PrAutoFixBlocker,
    PrAutoFixDecision, PrAutoFixPush, pr_auto_fix_attempts, pr_auto_fix_pushes,
};
use serde_json::{Value, json};

use crate::context::{PrAutoFixAdmission, PrAutoFixRequest, RuntimeHost, TaskAutomationUpdate};

use super::super::super::input::input_string_field;
use super::super::candidate_validate::candidate_validate;
use super::super::freshness::{commit_sha, remote_branch_sha};
use super::super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::super::landing_checks::{HeadCheckEvidence, head_check_evidence, unmoved_red_base};
use super::super::push::push_batch_changes;
use super::answers::{CheckState, PullRequestStatus, RequiredChecks, read_required_checks};
use super::delivery::{DeliveryPin, blocked_checks};

/// Polls a `BLOCKED` head with no failed and no running check is waited on
/// before it counts as blocked: a freshly published head can report no
/// checks until the provider queues them.
pub(super) const UNSETTLED_BLOCKED_POLLS: u32 = 4;

/// What an auto-fix run may do, read once per completion step.
pub(super) struct AutoFix {
    pub(super) run_id: String,
    /// Current execution and authenticated retry ancestors, never siblings.
    ledger_runs: BTreeSet<String>,
    pub(super) task_ids: Vec<String>,
    allowed_crews: Value,
    /// `--complete` was granted too: completion may merge once mergeable.
    merge_authorized: bool,
}

impl AutoFix {
    /// `Some` when the step's input carries `auto_fix: true`.
    pub(super) fn from_input<H: RuntimeHost + ?Sized>(
        host: &H,
        input: &Value,
        task_ids: &[String],
        batch_id: &str,
    ) -> Result<Option<Self>, OrbitError> {
        if input.get("auto_fix").and_then(Value::as_bool) != Some(true) {
            return Ok(None);
        }
        let run_id = input_string_field(input, "run_id").unwrap_or_else(|| batch_id.to_string());
        let ledger_runs = repair_ledger_runs(host, &run_id)?;
        Ok(Some(Self {
            run_id,
            ledger_runs,
            task_ids: task_ids.to_vec(),
            allowed_crews: input.get("allowed_crews").cloned().unwrap_or(Value::Null),
            merge_authorized: input.get("completion").and_then(Value::as_str) == Some("done"),
        }))
    }

    /// Repair only: never request a merge or complete a task.
    pub(super) fn watch_only(&self) -> bool {
        !self.merge_authorized
    }

    pub(super) fn primary_task(&self) -> Result<&str, OrbitError> {
        self.task_ids
            .iter()
            .min()
            .map(String::as_str)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "auto_fix: the pull request names no task to keep its repair ledger on"
                        .to_string(),
                )
            })
    }
}

/// A resume has a new execution identity while successful push checkpoints
/// retain the source head. Only the host's durable, same-job retry ancestry
/// transfers repair authority; input/checkpoint claims do not supply it.
/// Direct activity calls without a stored run retain current-run authority.
fn repair_ledger_runs<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
) -> Result<BTreeSet<String>, OrbitError> {
    let mut runs = BTreeSet::from([run_id.to_string()]);
    let Some(mut current) = host.get_job_run(run_id)? else {
        return Ok(runs);
    };
    if current.run_id != run_id {
        return Err(OrbitError::Execution(
            "auto_fix: execution identity differs; refusing inherited repair authority".into(),
        ));
    }
    let job_id = current.job_id.clone();
    for _ in 0..64 {
        let Some(parent_id) = current.retry_source_run_id.take() else {
            return Ok(runs);
        };
        if !runs.insert(parent_id.clone()) {
            return Err(OrbitError::Execution(
                "auto_fix: cyclic retry lineage; refusing inherited repair authority".into(),
            ));
        }
        current = host.get_job_run(&parent_id)?.ok_or_else(|| {
            OrbitError::Execution(
                "auto_fix: retry ancestor is missing; refusing inherited repair authority".into(),
            )
        })?;
        if current.run_id != parent_id || current.job_id != job_id {
            return Err(OrbitError::Execution(
                "auto_fix: retry ancestor identity differs; refusing inherited repair authority"
                    .into(),
            ));
        }
    }
    Err(OrbitError::Execution(
        "auto_fix: retry lineage exceeds its bound; refusing inherited repair authority".into(),
    ))
}

/// What a `BLOCKED` pull request means to an auto-fix watcher.
pub(super) enum BlockedVerdict {
    /// Checks are still running, or the head has not settled yet.
    Wait,
    /// Required checks failed: repair them.
    RepairChecks(Vec<(Option<String>, String)>),
    /// An administrative blocker no code repair clears.
    Stop(String),
}

/// Read a `BLOCKED` pull request for auto-fix. A review gate or an
/// unreadable rollup stops before any check is considered, as completion
/// already does; only policy-bound pending evidence authorizes a check wait.
pub(super) fn blocked_verdict(
    status: &PullRequestStatus,
    unsettled_polls: u32,
    pending_required: bool,
) -> BlockedVerdict {
    let checks = match blocked_checks(status) {
        Ok(checks) => checks,
        Err(reason) => return BlockedVerdict::Stop(reason),
    };
    if pending_required {
        return BlockedVerdict::Wait;
    }
    let failed = checks
        .iter()
        .filter(|check| check.state == Some(CheckState::Failed))
        .filter_map(|check| Some((check.workflow_name.clone(), check.name.clone()?)))
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        return BlockedVerdict::RepairChecks(failed);
    }
    if unsettled_polls < UNSETTLED_BLOCKED_POLLS {
        return BlockedVerdict::Wait;
    }
    BlockedVerdict::Stop("required reviews or checks are not satisfied".into())
}

/// The failure that stops an auto-fix watcher without an agent.
pub(super) fn stopped(pr_number: &str, reason: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "auto_fix_stopped: pull request #{pr_number} cannot be repaired automatically \
         ({reason}); no repair agent runs, the pull request stays open and the task stays in \
         review"
    ))
}

/// Ask the host to admit one repair of `head_sha`, returning the attempt
/// the repair runs as. Every refusal is a stop with its diagnostics.
pub(super) fn admit<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<PrAutoFixAttempt, OrbitError> {
    let admission = host.admit_pr_auto_fix(&PrAutoFixRequest {
        run_id: auto_fix.run_id.clone(),
        task_ids: auto_fix.task_ids.clone(),
        pr_number: pr_number.to_string(),
        head_sha: head_sha.to_string(),
        blocker,
        allowed_crews: auto_fix.allowed_crews.clone(),
    })?;
    let decision = match admission {
        PrAutoFixAdmission::Refused(reason) => return Err(stopped(pr_number, &reason)),
        PrAutoFixAdmission::Decided(decision) => decision,
    };
    match decision {
        PrAutoFixDecision::Admit(attempt) | PrAutoFixDecision::Continue(attempt) => {
            tracing::info!(
                pr_number,
                head_sha,
                blocker = blocker.as_str(),
                attempt = attempt.attempt,
                budget = attempt.budget,
                crew = %attempt.crew,
                crew_source = %attempt.crew_source,
                "auto_fix: repair admitted"
            );
            Ok(attempt)
        }
        PrAutoFixDecision::Duplicate(previous) => Err(stopped(
            pr_number,
            &format!(
                "a {} repair was already dispatched for head {head_sha} as attempt {} of {} by \
                 run {}, and that head is still the one blocked",
                blocker.as_str(),
                previous.attempt,
                previous.budget,
                previous.run_id
            ),
        )),
        PrAutoFixDecision::Exhausted(attempts) => {
            let spent = attempts
                .iter()
                .map(|attempt| {
                    format!(
                        "{} on {} by run {} with crew '{}'",
                        attempt.blocker.as_str(),
                        attempt.head_sha,
                        attempt.run_id,
                        attempt.crew
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            Err(stopped(
                pr_number,
                &format!(
                    "auto_fix_budget_exhausted: its {} repairs are spent ({spent})",
                    attempts.len()
                ),
            ))
        }
    }
}

/// The failure that hands an admitted check repair to the step's repair
/// activity, with the head-bound evidence it repairs from.
pub(super) fn check_repair_handoff(
    pr_number: &str,
    head_sha: &str,
    attempt: &PrAutoFixAttempt,
    evidence: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "{PR_AUTO_FIX_CHECK_REPAIR_MARKER}: pull request #{pr_number} failed required checks on \
         head {head_sha}; auto-fix repair attempt {} of {} runs as crew '{}' (drawn from {}). \
         Repair the candidate in the assigned worktree so these checks pass and leave the \
         repair uncommitted: Orbit commits it, reruns required validation, publishes it on the \
         pull request and watches the new head.\n{evidence}",
        attempt.attempt, attempt.budget, attempt.crew, attempt.crew_source
    ))
}

/// A required failure the base's own job already fails the same way, while
/// that failure is not yet shown to be gone: the base has not moved past
/// that commit, or it has moved to a commit whose same job has not concluded.
///
/// `Some(reason)` waits for the base and spends no repair attempt. `None`
/// keeps the ordinary repair path, including a candidate-caused failure, a
/// moved tip whose same job has concluded, a moved tip with no such job, and
/// any git state or evidence this classification could not read, such as a
/// head object that is not local.
pub(super) fn red_base_wait<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> Option<String> {
    let head = status.head_sha.as_deref()?;
    let base_name = input_string_field(input, "base")?;
    let workspace = Path::new(workspace_path);
    let base_ref = resolve_worktree_start_point(
        workspace,
        &base_name,
        base_sync_mode_from_input(input).ok()?,
    )
    .ok()?;
    let tip = commit_sha(workspace, &base_ref).ok()?;
    let base = git_output(workspace, &["merge-base", head, &tip]).ok()?;
    // An unreadable answer is not itself a red base. The repair read still
    // stops when the head evidence is missing.
    unmoved_red_base(host, workspace, head, &base, &tip, failed)
        .ok()
        .flatten()
}

/// Read the failed checks on the pinned head and admit their repair. Returns
/// the hand-off failure once admitted, or the stop.
pub(super) fn repair_failed_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    workspace_path: &str,
    pr_number: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> OrbitError {
    let Some(head_sha) = status.head_sha.as_deref() else {
        return stopped(pr_number, "the pull request reported no head commit");
    };
    let (failed, required) =
        match required_failures(host, workspace_path, pr_number, head_sha, failed) {
            Ok(found) => found,
            Err(reason) => return stopped(pr_number, &reason),
        };
    let (evidence, annotations) = match head_check_evidence(
        host,
        Path::new(workspace_path),
        pr_number,
        head_sha,
        &failed,
        &required,
    ) {
        HeadCheckEvidence::Repair {
            evidence,
            annotations,
        } => (evidence, annotations),
        HeadCheckEvidence::Stop(reason) => return stopped(pr_number, &reason),
    };
    if !annotations.is_empty()
        && let Err(error) =
            record_check_annotations(host, auto_fix, pr_number, head_sha, annotations)
    {
        return stopped(
            pr_number,
            &format!("the check annotations a repair would rely on were not retained: {error}"),
        );
    }
    match admit(
        host,
        auto_fix,
        pr_number,
        head_sha,
        PrAutoFixBlocker::FailedChecks,
    ) {
        Ok(attempt) => check_repair_handoff(pr_number, head_sha, &attempt, &evidence),
        Err(stop) => stop,
    }
}

/// Recognize a landed repair using all required failures and exact base jobs.
/// This is only diagnosis: the caller still admits an attempt, validates,
/// reviews and lease-publishes the refreshed candidate.
pub(super) fn landed_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    status: &PullRequestStatus,
    failed: &[(Option<String>, String)],
) -> Option<Value> {
    let workspace = Path::new(workspace_path);
    let head = status.head_sha.as_deref()?;
    let (failed, _) = required_failures(host, workspace_path, pr_number, head, failed).ok()?;
    let base = input_string_field(input, "base")?;
    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input).ok()?)
            .ok()?;
    let tip = commit_sha(workspace, &base_ref).ok()?;
    let old = git_output(workspace, &["merge-base", head, &tip]).ok()?;
    super::super::landing_checks::landed_check_repair(host, workspace, head, &old, &tip, &failed)
}

/// Retain the failed and passing exact-head evidence before refreshing.
pub(super) fn record_landed_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let head = evidence["failed_head"]
        .as_str()
        .ok_or_else(|| stopped("", "missing failed head"))?;
    retain(
        host,
        auto_fix,
        &format!("auto-fix/{}/landed-{head}.json", auto_fix.run_id),
        evidence,
    )
}

/// Retain the authenticated check annotations a repair of `head` relies on,
/// before it is admitted. A repeated read of the same head replaces them.
fn record_check_annotations<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    head: &str,
    annotations: Vec<Value>,
) -> Result<(), OrbitError> {
    retain(
        host,
        auto_fix,
        &format!("auto-fix/{}/check-annotations-{head}.json", auto_fix.run_id),
        &json!({"pr": pr_number, "failed_head": head, "checks": annotations}),
    )
}

fn retain<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    path: &str,
    evidence: &Value,
) -> Result<(), OrbitError> {
    let owner_run = host
        .get_task(auto_fix.primary_task()?)?
        .job_run_id
        .ok_or_else(|| stopped("", "missing owning run"))?;
    let evidence = orbit_common::security::redaction::redact_all_json(evidence.clone());
    host.attach_task_validation_log(
        auto_fix.primary_task()?,
        &owner_run,
        path,
        serde_json::to_vec(&evidence).map_err(|error| OrbitError::Execution(error.to_string()))?,
    )
}

/// The required failures on a head, with the head-bound read naming them.
type RequiredFailures = (Vec<(Option<String>, String)>, RequiredChecks);

/// The failed checks branch protection requires on `head_sha`. The rollup
/// does not say which checks are required, and a `BLOCKED` state can come
/// from a rule the rollup does not show, so an optional failure beside it
/// would otherwise read as the blocker. Nothing required failing, or an
/// answer that cannot show it, is a stop. The head-bound read is returned
/// with the failures, for the identities of the required checks.
fn required_failures<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    head_sha: &str,
    failed: &[(Option<String>, String)],
) -> Result<RequiredFailures, String> {
    let required = read_required_checks(host, Path::new(workspace_path), pr_number)
        .map_err(|error| format!("its required checks could not be read: {error}"))?;
    if required.head_sha != head_sha {
        return Err(format!(
            "its required checks were read on head {} rather than {head_sha}",
            required.head_sha
        ));
    }
    let failed = failed
        .iter()
        .filter(|(workflow, name)| required.requires(workflow.as_deref(), name))
        .cloned()
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Err("only optional checks failed; the merge is blocked by something else".into());
    }
    Ok((failed, required))
}

/// Pin the contiguous heads this execution or its authenticated retry
/// ancestors published for the same pull request. Branch and review pins
/// remain enforced by completion on every provider read.
pub(super) fn adopt_published_repairs<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    pin: &mut DeliveryPin,
) -> Result<(), OrbitError> {
    let history = host.get_task_history(auto_fix.primary_task()?)?;
    for push in pr_auto_fix_pushes(&history, pr_number) {
        if auto_fix.ledger_runs.contains(&push.run_id)
            && pin.candidate_sha() == Some(&push.from_head_sha)
        {
            pin.adopt_refreshed_candidate(Some(&push.to_head_sha));
        }
    }
    Ok(())
}

/// Record a head this run published on the pull request.
pub(super) fn record_published_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    from_head_sha: &str,
    to_head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<(), OrbitError> {
    let push = PrAutoFixPush {
        pr_number: pr_number.to_string(),
        from_head_sha: from_head_sha.to_string(),
        to_head_sha: to_head_sha.to_string(),
        blocker,
        run_id: auto_fix.run_id.clone(),
    };
    let note = serde_json::to_string(&push).map_err(|error| {
        OrbitError::Execution(format!("auto_fix: published head not recorded: {error}"))
    })?;
    host.apply_task_automation_update(
        auto_fix.primary_task()?,
        TaskAutomationUpdate {
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: "system".to_string(),
                event: PR_AUTO_FIX_PUSHED_EVENT.to_string(),
                note: Some(note),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..TaskAutomationUpdate::default()
        },
    )
}

/// What completion found on the branch before it read the pull request.
pub(super) enum LocalRepair {
    /// Nothing to publish.
    None,
    /// The repair was validated and published as this head.
    Published(String),
    /// The repair sits on a reviewed head: validated, unpublished, and
    /// handed to the re-review round as this checkpoint.
    ForReview(Value),
}

/// Publish a check repair the executor committed on top of the pinned head.
///
/// Acts only when this execution or a retry ancestor holds an admitted
/// check repair of exactly the pinned head and the checkout is that branch,
/// strictly ahead of it.
/// Anything else (an interrupted rebase, a detached checkout, another
/// writer's commit) is left to the ordinary completion path, which refuses
/// what it does not pin. The repair passes the workspace's required
/// validation before it moves anything, and the push is a fast-forward of
/// the exact published head.
pub(super) fn publish_local_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    auto_fix: &AutoFix,
    workspace_path: &str,
    pr_number: &str,
    pin: &DeliveryPin,
    reviewed: bool,
) -> Result<LocalRepair, OrbitError> {
    let (Some(authorized), Some(head)) = (pin.candidate_sha(), input_string_field(input, "head"))
    else {
        return Ok(LocalRepair::None);
    };
    let workspace = Path::new(workspace_path);
    let history = host.get_task_history(auto_fix.primary_task()?)?;
    let holds_repair = pr_auto_fix_attempts(&history, pr_number)
        .iter()
        .any(|attempt| {
            auto_fix.ledger_runs.contains(&attempt.run_id)
                && attempt.head_sha == authorized
                && attempt.blocker == PrAutoFixBlocker::FailedChecks
        });
    if !holds_repair {
        return Ok(LocalRepair::None);
    }
    let local = commit_sha(workspace, "HEAD")?;
    let branch = git_output(workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    if local == authorized
        || branch.as_deref() != Some(head.as_str())
        || !git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", authorized, &local],
        )?
    {
        return Ok(LocalRepair::None);
    }
    let remote = remote_branch_sha(workspace, &head)?;
    if remote.as_deref() != Some(authorized) {
        return Err(stopped(
            pr_number,
            &format!(
                "published branch 'origin/{head}' moved away from {authorized} while its repair \
                 ran, so the repair is not published over it"
            ),
        ));
    }
    let base = input_string_field(input, "base").ok_or_else(|| {
        stopped(
            pr_number,
            "the completion step carries no base to validate against",
        )
    })?;
    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input)?)?;
    let base_sha = git_output(workspace, &["merge-base", authorized, &base_ref])?;
    candidate_validate(
        host,
        &json!({
            "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
            "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
            "workspace_path": workspace_path,
            "base_sha": base_sha,
            "base_ref": base_ref,
        }),
    )?;
    if reviewed {
        return Ok(LocalRepair::ForReview(json!({
            "head": head,
            "head_sha": local,
            "head_sha_before": authorized,
            "remote_sha_before": authorized,
            "base": base,
            "base_ref": base_ref,
            "base_sha": base_sha,
            "rewritten": false,
            "auto_fix": true,
        })));
    }
    push_batch_changes(
        host,
        &json!({
            "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
            "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
            "workspace_path": workspace_path,
            "branch": head,
            "expected_remote_sha": authorized,
        }),
    )?;
    record_published_repair(
        host,
        auto_fix,
        pr_number,
        authorized,
        &local,
        PrAutoFixBlocker::FailedChecks,
    )?;
    Ok(LocalRepair::Published(local))
}
