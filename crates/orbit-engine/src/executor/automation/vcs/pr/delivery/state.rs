use super::super::answers::{CheckState, PullRequestStatus, RequiredChecks};
use super::super::auto_fix::UNSETTLED_BLOCKED_POLLS;
use super::super::merge_state::{BlockedCheckRefusal, blocked_checks, has_failed_pr_check};
use super::{PrMergeState, classify_pr_state};
use crate::context::RuntimeHost;
use crate::executor::automation::vcs::operations;
use orbit_common::{OrbitError, PrivateVcsFailureClass};
use orbit_types::workflow::DeliveryRefusal;
use serde_json::{Value, json};
use std::path::Path;

/// Which consumer may refine a failed rollup into a bounded required-check wait.
#[derive(Clone, Copy)]
pub(in crate::executor::automation::vcs) enum BlockedCheckWait<'a> {
    /// Owner landing retains its failed-rollup refusal and captured owed checks.
    Owner(&'a [String]),
    /// Completion without auto-fix shares the bounded policy graces, but a
    /// running check current policy does not require never waits: the head
    /// stays BLOCKED and stops as requirements unsatisfied.
    Completion,
    /// Auto-fix classifies failures against policy before deciding to recover.
    AutoFix,
}

/// [ORB-16339] Why the last observation of a BLOCKED head waited although the
/// head reported nothing pending and nothing failed.
enum UnreportedWait {
    None,
    /// Current branch policy names these checks, and the head has not
    /// reported them yet.
    Missing(Vec<String>),
    /// Current branch policy names these checks, and the head has reported
    /// them still running (never failed).
    Pending(Vec<String>),
    /// Branch policy could not be read, so a bounded grace stands in for it.
    Unsettled(String),
    /// Branch policy names nothing missing or pending, but the rollup still
    /// reports checks running. Owner landing waits within its deadline.
    RollupPending(Vec<String>),
    /// Branch policy names nothing missing or pending, but the provider
    /// still reports BLOCKED: its merge state may lag the checks that just
    /// passed, so a bounded grace re-polls it.
    LaggingMergeState,
}

/// Required checks a freshly published head has not reported yet, tracked
/// across one completion or landing wait. A head can read BLOCKED for a few
/// seconds before the provider queues its first check run.
pub(in crate::executor::automation::vcs) struct UnreportedChecks {
    last: UnreportedWait,
    /// Latest required-check observation; diagnostic only, never authority.
    pub(in crate::executor::automation::vcs) required_observation: Value,
    policy_observation: Value,
    /// Observations waited on while branch policy was unreadable.
    unsettled_polls: u32,
    /// Observations waited on while the provider merge state lagged
    /// behind checks that had already passed.
    lagging_blocked_polls: u32,
    /// [ORB-23812] The last observation found no run, queued or otherwise,
    /// of any check current branch policy requires on the head.
    no_required_check_run: bool,
    /// [ORB-24457] The forge adapter reported a workflow run for this head,
    /// in any state. A missing answer is false, so a scripted host that omits
    /// the field keeps the dropped-event path.
    head_workflow_present: bool,
    /// Adapter-verified workflow failures whose protected checks never ran.
    pub(in crate::executor::automation::vcs::pr) failed_workflow_runs:
        Vec<orbit_types::workflow::PrInfrastructureEvidence>,
    /// Last head-verified required-check answer, reused only to answer
    /// "a required check is still pending" on a later BLOCKED poll of the
    /// same head. Never consulted on a Mergeable head, so the fresh
    /// failed-history guard before a merge request is unaffected.
    cached_required_checks: Option<RequiredChecks>,
    /// Observation captured with `cached_required_checks`; diagnostic
    /// only, restored (marked reused) when the cache answers a poll.
    cached_required_observation: Value,
}

impl UnreportedChecks {
    pub(in crate::executor::automation::vcs) fn new() -> Self {
        Self {
            last: UnreportedWait::None,
            required_observation: json!({"state":"not_collected"}),
            policy_observation: json!({"state":"not_collected"}),
            unsettled_polls: 0,
            lagging_blocked_polls: 0,
            no_required_check_run: false,
            head_workflow_present: false,
            failed_workflow_runs: Vec::new(),
            cached_required_checks: None,
            cached_required_observation: json!({"state":"not_collected"}),
        }
    }

    /// [ORB-23812] Whether the last observation of a BLOCKED head found
    /// readable branch policy requiring checks, and no report at all (not
    /// even queued or pending) of any of them. A head whose required checks
    /// are pending, queued, failed or reported skipped is never this state.
    pub(in crate::executor::automation::vcs) fn no_required_check_run(&self) -> bool {
        self.no_required_check_run
    }

    /// [ORB-24457] Whether the last current-gates answer reported a workflow
    /// run for this head. Any state counts: a queued run has no check runs
    /// yet, and that is checks still pending, not a dropped event.
    pub(in crate::executor::automation::vcs) fn head_workflow_present(&self) -> bool {
        self.head_workflow_present
    }

    /// Whether the last observation waits for checks readable policy names.
    /// Absent-context recovery may still diagnose such a head.
    pub(in crate::executor::automation::vcs) fn awaiting_policy_checks(&self) -> bool {
        matches!(self.last, UnreportedWait::Missing(_))
    }

    /// The existing branch-policy refinement, if it ran. This retains only
    /// what this wait already parsed; it is not a policy payload or emission proof.
    pub(in crate::executor::automation::vcs) fn diagnostic(&self) -> Value {
        json!({"awaiting_policy_checks":self.awaiting_policy_checks(),
            "policy_read":self.policy_observation})
    }

    /// What a wait that ends here was still waiting for, for its stop reason.
    pub(in crate::executor::automation::vcs) fn timeout_note(&self) -> String {
        match &self.last {
            UnreportedWait::None => String::new(),
            UnreportedWait::Missing(names) => format!(
                " (required checks the head has not reported: {})",
                names.join(", ")
            ),
            UnreportedWait::Pending(names) => {
                format!(" (required checks still pending: {})", names.join(", "))
            }
            UnreportedWait::RollupPending(names) => {
                format!(" (checks still running: {})", names.join(", "))
            }
            UnreportedWait::Unsettled(why) => format!(
                " (the head reported no pending required check and branch policy is \
                 unreadable: {why})"
            ),
            UnreportedWait::LaggingMergeState => " (the provider still reports the pull \
                 request BLOCKED although no required check is missing or pending)"
                .to_string(),
        }
    }
}

/// Why a BLOCKED head's check wait could not be refined. Displays as its
/// reason; `cause` is the provider read that failed, when one did, so a stop
/// built from the reason keeps that failure's class.
pub(in crate::executor::automation::vcs) struct CheckWaitRefusal {
    reason: String,
    cause: Option<Box<OrbitError>>,
    refusal: Option<DeliveryRefusal>,
}

impl CheckWaitRefusal {
    /// The error `wrap` renders from the reason, carrying the class of the
    /// failed provider read behind it.
    pub(in crate::executor::automation::vcs) fn into_error(
        self,
        wrap: impl FnOnce(&str) -> OrbitError,
    ) -> OrbitError {
        let wrapper = wrap(&self.reason);
        let refusal = self.refusal.or(match self.cause.as_deref() {
            Some(OrbitError::DeliveryRefused { refusal, .. }) => Some(*refusal),
            _ => None,
        });
        if let Some(refusal) = refusal {
            return match wrapper {
                OrbitError::Execution(message) => OrbitError::DeliveryRefused { refusal, message },
                other => other,
            };
        }
        match &self.cause {
            Some(cause) => operations::carry_class(cause, wrapper),
            None => wrapper,
        }
    }
}

impl From<String> for CheckWaitRefusal {
    fn from(reason: String) -> Self {
        Self {
            reason,
            cause: None,
            refusal: None,
        }
    }
}

impl From<BlockedCheckRefusal> for CheckWaitRefusal {
    fn from(reason: BlockedCheckRefusal) -> Self {
        Self {
            reason: reason.to_string(),
            cause: None,
            refusal: reason.delivery_refusal(),
        }
    }
}

impl From<&str> for CheckWaitRefusal {
    fn from(reason: &str) -> Self {
        reason.to_string().into()
    }
}

impl std::fmt::Display for CheckWaitRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Require green required checks even on a provider-reported mergeable head.
/// Refine BLOCKED from authenticated current policy on the observed head,
/// an actually running owner-owed check, or an owner rollup still running
/// after policy rules out failed gates. A completed neutral/skipped result
/// is never pending. An absent check is pending only
/// while current branch policy names it as required [ORB-16339]; when that
/// policy is unreadable, `Owner` and `Completion` consumers get the bounded
/// unsettled-poll grace auto-fix already applies in its own verdict. When
/// readable policy names nothing missing or pending either and every
/// reported check explicitly succeeded, `Owner` and `Completion` consumers
/// get the same bounded grace, because the
/// provider's merge state can lag behind checks that have already passed.
/// This grants only another bounded observation
/// within the caller's deadline; only the provider's mergeable state can merge.
/// Owner landing also waits on a running rollup when neither required-check
/// view identifies a pending check, provided current policy reports no failure.
/// Completion without auto-fix never takes that wait: a running check that
/// policy does not require leaves the head BLOCKED, so it stops at once and
/// never reaches the pending dispatch that could arm auto-merge.
/// `None` performs the bounded immediate read used by zero-wait completion;
/// a watch always passes its original absolute deadline.
pub(in crate::executor::automation::vcs) fn checked_pr_state<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    status: &PullRequestStatus,
    wait: BlockedCheckWait<'_>,
    unreported: &mut UnreportedChecks,
    deadline_ms: Option<u64>,
) -> Result<PrMergeState, CheckWaitRefusal> {
    unreported.last = UnreportedWait::None;
    unreported.no_required_check_run = false;
    unreported.head_workflow_present = false;
    unreported.failed_workflow_runs.clear();
    unreported.required_observation = json!({"state":"not_collected"});
    unreported.policy_observation = json!({"state":"not_collected"});
    let state = classify_pr_state(status);
    // A draft is already refused. Refining BLOCKED into a check wait would
    // hand it to auto-merge or repair [ORB-16342].
    if status.is_draft
        || !(matches!(state, PrMergeState::Mergeable)
            || (status.merge_state_status.as_deref() == Some("BLOCKED")
                && matches!(state, PrMergeState::Blocked(_))))
    {
        return Ok(state);
    }
    if status.state.as_deref() != Some("OPEN") {
        return Err("pull request state is unavailable for a blocked check wait".into());
    }
    let checks = blocked_checks(status).map_err(|reason| {
        let mut refusal = CheckWaitRefusal::from(reason);
        if has_failed_pr_check(status) {
            refusal.refusal = None;
        }
        refusal
    })?;
    let owed: &[String] = match wait {
        BlockedCheckWait::Owner(owed) => owed,
        BlockedCheckWait::Completion | BlockedCheckWait::AutoFix => &[],
    };
    if matches!(
        wait,
        BlockedCheckWait::Owner(_) | BlockedCheckWait::Completion
    ) && !matches!(state, PrMergeState::Mergeable)
        && checks
            .iter()
            .any(|check| check.state == Some(CheckState::Failed))
    {
        return Ok(state);
    }
    // [ORB-24403] A required check still pending on the same head answers
    // this BLOCKED poll from the last head-verified answer: which checks
    // are required for a head does not change between polls, and Pending
    // authorizes nothing but another wait. Mergeable heads, a changed
    // head, and polls whose rollup no longer shows a cached-required
    // pending check all read fresh below.
    if matches!(state, PrMergeState::Blocked(_)) {
        if unreported
            .cached_required_checks
            .as_ref()
            .is_some_and(|cached| status.head_sha.as_deref() != Some(cached.head_sha.as_str()))
        {
            unreported.cached_required_checks = None;
            unreported.cached_required_observation = json!({"state":"not_collected"});
        }
        let cache_answers_pending =
            unreported
                .cached_required_checks
                .as_ref()
                .is_some_and(|cached| {
                    checks.iter().any(|check| {
                        check.state == Some(CheckState::Pending)
                            && check.name.as_deref().is_some_and(|name| {
                                cached.requires(check.workflow_name.as_deref(), name)
                            })
                    })
                });
        if cache_answers_pending {
            let mut observation = unreported.cached_required_observation.clone();
            observation["reused"] = json!(true);
            unreported.required_observation = observation;
            return Ok(PrMergeState::Pending);
        }
    }
    let mut required_input = json!({"pr":pr,"workspace_path":workspace.to_string_lossy()});
    if let Some(deadline_ms) = deadline_ms {
        required_input["deadline_ms"] = json!(deadline_ms);
    }
    let response = host.run_private_vcs_operation(operations::PR_REQUIRED_CHECKS, required_input);
    let required = RequiredChecks::observe_response(response, &mut unreported.required_observation)
        .map_err(|error| {
            // [ORB-21412] A persistently unavailable required-checks read is
            // inconclusive evidence, not a verdict on the PR: auto-fix holds
            // (retryable by the next pass) instead of stopping for good. A
            // transient transport failure keeps its class so completion's
            // observation hold (ForgeObservationUnavailable) still applies,
            // and a cause carrying its own typed refusal (for example a
            // completion-budget expiry) keeps that refusal [ORB-21436].
            let refusal = match wait {
                BlockedCheckWait::AutoFix
                    if !matches!(error, OrbitError::DeliveryRefused { .. })
                        && !error.private_vcs_failure().is_some_and(|failure| {
                            failure.class == PrivateVcsFailureClass::TransientObservation
                        }) =>
                {
                    Some(DeliveryRefusal::CheckEvidenceInconclusive)
                }
                _ => None,
            };
            CheckWaitRefusal {
                reason: format!("required-check evidence is unavailable: {error}"),
                cause: Some(Box::new(error)),
                refusal,
            }
        })?;
    if status.head_sha.as_deref() != Some(required.head_sha.as_str())
        || required.reported_head_sha.as_deref() != Some(required.head_sha.as_str())
    {
        unreported.required_observation["outcome"] = json!("head_mismatch");
        return Err("required-check evidence does not identify the current head".into());
    }
    unreported.required_observation["head_matches"] = json!(true);
    // Only a head-verified answer may answer a later poll.
    unreported.cached_required_checks = Some(required.clone());
    unreported.cached_required_observation = unreported.required_observation.clone();
    let mut pending = false;
    let mut failed = None;
    for check in &required.checks {
        match check.state {
            Some(CheckState::Pending) => pending = true,
            Some(CheckState::Passed)
                if matches!(state, PrMergeState::Mergeable)
                    && check.owed_state != Some(CheckState::Passed)
                    && !required.never_failed(check) =>
            {
                // Branch protection accepts skipped/neutral conclusions, and
                // so does this gate for a check that never failed on this
                // head (a docs-only skip). Neither is a passing rerun of a
                // failed required check, and unreadable history cannot show
                // the check never failed. A refusal also prevents delegating
                // this head to auto-merge.
                failed = Some(format!(
                    "required check '{}' has no successful run on the current head",
                    check.name.as_deref().unwrap_or_default()
                ));
            }
            Some(CheckState::Passed) => {}
            Some(CheckState::Failed) => {
                failed = Some(format!(
                    "required check '{}' failed",
                    check.name.as_deref().unwrap_or_default()
                ));
            }
            None => {
                unreported.required_observation["outcome"] = json!("unreadable_state");
                return Err("required-check evidence is unreadable".into());
            }
        }
    }
    pending |= checks.iter().any(|check| {
        check.state == Some(CheckState::Pending)
            && owed.iter().any(|name| {
                check.name.as_deref() == Some(name.as_str())
                    || check
                        .workflow_name
                        .as_ref()
                        .zip(check.name.as_ref())
                        .is_some_and(|(workflow, own)| name == &format!("{workflow} / {own}"))
            })
    });
    if pending {
        return Ok(PrMergeState::Pending);
    }
    if let Some(reason) = failed {
        return Ok(PrMergeState::Blocked(reason.into()));
    }
    if matches!(state, PrMergeState::Mergeable) {
        return Ok(state);
    }
    // A failed check keeps its existing recovery or stop.
    if checks
        .iter()
        .any(|check| check.state == Some(CheckState::Failed))
    {
        return Ok(state);
    }
    let missing = unreported_required_checks(host, workspace, pr, status, deadline_ms);
    if let Ok(assessment) = &missing
        && !assessment.failed_workflow_runs.is_empty()
    {
        unreported.failed_workflow_runs = assessment.failed_workflow_runs.clone();
        return Ok(PrMergeState::Blocked(
            super::super::workflow_failure::failed_workflow_diagnosis(
                &assessment.failed_workflow_runs[0],
            )
            .into(),
        ));
    }
    unreported.policy_observation = match &missing {
        Ok(assessment) => json!({"state":"collected", "missing_checks":
            super::super::diagnostics::collection(&assessment.missing, |name| super::super::diagnostics::text(name)),
            "pending_checks":
            super::super::diagnostics::collection(&assessment.pending, |name| super::super::diagnostics::text(name))}),
        Err(why) => json!({"state":"unreadable", "reason":super::super::diagnostics::text(why)}),
    };
    match missing {
        Ok(assessment) if assessment.missing.is_empty() && assessment.pending.is_empty() => {
            // The rollup can report checks still running before either policy
            // view identifies them as required. Owner landing may keep
            // observing within its existing deadline, but BLOCKED grants no
            // merge authority. Completion without auto-fix stops instead.
            // A failed policy gate still keeps its refusal.
            if matches!(wait, BlockedCheckWait::Owner(_))
                && assessment.failed.is_empty()
                && checks
                    .iter()
                    .any(|check| check.state == Some(CheckState::Pending))
            {
                let running: Vec<String> = checks
                    .iter()
                    .filter(|check| check.state == Some(CheckState::Pending))
                    .map(|check| {
                        let name = check.name.as_deref().unwrap_or_default();
                        check
                            .workflow_name
                            .as_deref()
                            .filter(|workflow| !workflow.is_empty())
                            .map(|workflow| format!("{workflow} / {name}"))
                            .unwrap_or_else(|| name.to_string())
                    })
                    .collect();
                unreported.last = UnreportedWait::RollupPending(running);
                return Ok(PrMergeState::Pending);
            }
            // Nothing is owed and every reported check explicitly
            // succeeded, yet the provider still reads BLOCKED: its
            // merge state can lag seconds behind the last required check
            // going green [ORB-22350]. Re-poll it for the same bounded
            // grace an unreadable policy gets, within the caller's
            // existing deadline (this branch is only reached with one);
            // a genuine block still returns BLOCKED after the bound and
            // is never bypassed. Auto-fix keeps its own unsettled grace.
            match wait {
                BlockedCheckWait::Owner(_) | BlockedCheckWait::Completion
                    if assessment.failed.is_empty()
                        && !required.checks.is_empty()
                        && required
                            .checks
                            .iter()
                            .all(|check| check.owed_state == Some(CheckState::Passed))
                        && !checks.is_empty()
                        && checks
                            .iter()
                            .all(|check| check.owed_state == Some(CheckState::Passed))
                        && unreported.lagging_blocked_polls < UNSETTLED_BLOCKED_POLLS =>
                {
                    unreported.lagging_blocked_polls += 1;
                    unreported.last = UnreportedWait::LaggingMergeState;
                    Ok(PrMergeState::Pending)
                }
                _ => Ok(state),
            }
        }
        Ok(assessment) => {
            unreported.head_workflow_present = assessment.head_workflow_present;
            unreported.no_required_check_run = assessment.every_required_missing
                && assessment.pending.is_empty()
                && assessment.failed.is_empty()
                && required.checks.is_empty();
            unreported.last = if assessment.missing.is_empty() {
                UnreportedWait::Pending(assessment.pending)
            } else {
                UnreportedWait::Missing(assessment.missing)
            };
            Ok(PrMergeState::Pending)
        }
        // Auto-fix applies the same grace in its own BLOCKED verdict.
        Err(why) => match wait {
            BlockedCheckWait::Owner(_) | BlockedCheckWait::Completion
                if unreported.unsettled_polls < UNSETTLED_BLOCKED_POLLS =>
            {
                unreported.unsettled_polls += 1;
                unreported.last = UnreportedWait::Unsettled(why);
                Ok(PrMergeState::Pending)
            }
            _ => Ok(state),
        },
    }
}

/// What current branch policy still owes on the observed head, from the
/// same authenticated current-gates read a base update is held to.
struct PolicyRequiredAssessment {
    failed_workflow_runs: Vec<orbit_types::workflow::PrInfrastructureEvidence>,
    /// Policy-required checks the head has not reported at all.
    missing: Vec<String>,
    /// Policy-required checks the head reported still running, with no
    /// failed report. The required-checks GraphQL read can settle before
    /// such a check is reported as required (or name it differently), while
    /// branch policy and the check-runs read already agree it is required
    /// and running [ORB-21458]: that is a wait, not an unsatisfied refusal.
    pending: Vec<String>,
    /// Policy-required checks the head reported failed. A failed gate
    /// keeps its existing recovery or stop, never a merge-state wait.
    failed: Vec<String>,
    /// Policy requires at least one check, and the head reported none of
    /// them in any state [ORB-23812].
    every_required_missing: bool,
    /// The forge adapter reported a workflow run for this head [ORB-24457].
    head_workflow_present: bool,
}

/// The checks current branch policy requires that the observed head has not
/// reported at all, and those it reported still running, from the same
/// authenticated current-gates read a base update is held to. A check the
/// head reported, in any state, is not missing.
fn unreported_required_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    pr: &str,
    status: &PullRequestStatus,
    deadline_ms: Option<u64>,
) -> Result<PolicyRequiredAssessment, String> {
    let deadline_ms =
        deadline_ms.ok_or("no completion wait budget remains for required-check policy")?;
    let (Some(head), Some(branch), Some(base)) = (
        status.head_sha.as_deref(),
        status.head_branch.as_deref(),
        status.base_branch.as_deref(),
    ) else {
        return Err("the pull request did not report its head and base".into());
    };
    let gates = host
        .run_private_vcs_operation(
            operations::CI_CURRENT_GATES,
            json!({
                "workspace_path":workspace.to_string_lossy(),"pr":pr,"head_sha":head,
                "head_branch":branch,"base_branch":base,"deadline_ms":deadline_ms,
            }),
        )
        .map_err(|error| error.to_string())?;
    let missing: Vec<String> = gates["missing"]
        .as_array()
        .ok_or_else(|| "the current-gates answer named no missing checks".to_string())?
        .iter()
        .map(|gate| {
            gate["context"]
                .as_str()
                .filter(|context| !context.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| "the current-gates answer named an unreadable check".to_string())
        })
        .collect::<Result<_, _>>()?;
    let (pending, failed) = pending_policy_required_checks(&gates, &missing);
    let required: Vec<&str> = gates["required"]
        .as_array()
        .map(|gates| {
            gates
                .iter()
                .filter_map(|gate| gate["context"].as_str())
                .filter(|context| !context.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let every_required_missing = !required.is_empty()
        && required
            .iter()
            .all(|context| missing.iter().any(|name| name == context));
    // A missing field is "no run": scripted hosts that omit it keep the
    // dropped-event path. The engine does not read provider status strings.
    let head_workflow_present = gates
        .pointer("/head_workflow_run/present")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(PolicyRequiredAssessment {
        failed_workflow_runs: gates
            .get("failed_workflow_runs")
            .map(|value| {
                serde_json::from_value(value.clone())
                    .map_err(|error| format!("unreadable failed workflow evidence: {error}"))
            })
            .transpose()?
            .unwrap_or_default(),
        missing,
        pending,
        failed,
        every_required_missing,
        head_workflow_present,
    })
}

/// Policy-required gates the head reported still running, and those it
/// reported failed, in the same current-gates answer. A gate that also
/// reported a failure is failed, never pending: a failed gate keeps its
/// existing recovery or stop, never a wait.
fn pending_policy_required_checks(gates: &Value, missing: &[String]) -> (Vec<String>, Vec<String>) {
    // The current-gates answer carries the provider's raw list objects:
    // `checks` holds a `check_runs` list and `statuses` a `statuses` list.
    let checks = gates["checks"]
        .as_array()
        .or_else(|| gates["checks"]["check_runs"].as_array());
    let (Some(required), Some(checks)) = (gates["required"].as_array(), checks) else {
        return (Vec::new(), Vec::new());
    };
    let statuses = gates["statuses"]
        .as_array()
        .or_else(|| gates["statuses"]["statuses"].as_array());
    let mut pending = Vec::new();
    let mut failed = Vec::new();
    for gate in required {
        let Some(context) = gate["context"]
            .as_str()
            .filter(|context| !context.is_empty())
        else {
            continue;
        };
        if missing.iter().any(|name| name == context)
            || pending.iter().any(|name| name == context)
            || failed.iter().any(|name| name == context)
        {
            continue;
        }
        let app_id = gate["app_id"].as_u64();
        let mut present = false;
        let mut running = false;
        let mut gate_failed = false;
        for check in checks {
            if check["name"].as_str() != Some(context)
                || app_id.is_some_and(|app| check["app"]["id"].as_u64() != Some(app))
            {
                continue;
            }
            present = true;
            match check["status"].as_str() {
                Some("completed") => {
                    if let Some(
                        "failure" | "cancelled" | "timed_out" | "action_required"
                        | "startup_failure" | "stale",
                    ) = check["conclusion"].as_str()
                    {
                        gate_failed = true;
                    }
                }
                Some(_) => running = true,
                None => {}
            }
        }
        if app_id.is_none()
            && let Some(statuses) = statuses
        {
            for status in statuses {
                if status["context"].as_str() != Some(context) {
                    continue;
                }
                present = true;
                match status["state"].as_str() {
                    Some("pending" | "expected") => running = true,
                    Some("failure" | "error") => gate_failed = true,
                    _ => {}
                }
            }
        }
        if present && gate_failed {
            failed.push(context.to_string());
        } else if present && running {
            pending.push(context.to_string());
        }
    }
    (pending, failed)
}

/// [ORB-15677] Where the owner-owed hosted checks stand on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) enum OwedHostedChecks {
    /// Every named check reported an explicit success, and none is still
    /// running or finished without one.
    Passed,
    /// A named check has not reported yet, is still running, or completed
    /// without an explicit success (skipped or neutral).
    Waiting(Vec<String>),
    /// A named check failed, or its state is unreadable.
    Failed(String),
}

/// Judge the hosted checks `names` against the pull request's status rollup.
/// A rollup entry answers a name by its own name or as `<workflow> / <job>`,
/// as the forge displays it. A name no entry answers has not reported yet.
///
/// Only an explicit success satisfies a name: a CheckRun conclusion of
/// `SUCCESS`, or a StatusContext state of `SUCCESS`. A completed `SKIPPED`
/// or `NEUTRAL` conclusion is not that analysis, so the owner keeps waiting
/// [ORB-15812]. Branch protection still classifies those conclusions as
/// satisfied.
pub(in crate::executor::automation::vcs) fn owed_hosted_checks(
    pull_request: &PullRequestStatus,
    names: &[String],
) -> OwedHostedChecks {
    if names.is_empty() {
        return OwedHostedChecks::Passed;
    }
    let checks = pull_request.status_checks.as_deref().unwrap_or_default();
    let mut waiting = Vec::new();
    for name in names {
        let mut answered = false;
        let mut pending = false;
        for check in checks {
            let own = check.name.as_deref().unwrap_or_default();
            let qualified = check
                .workflow_name
                .as_deref()
                .map(|workflow| format!("{workflow} / {own}"));
            if own != name && qualified.as_deref() != Some(name.as_str()) {
                continue;
            }
            answered = true;
            match check.owed_state {
                Some(CheckState::Passed) => {}
                Some(CheckState::Pending) => pending = true,
                Some(CheckState::Failed) => {
                    return OwedHostedChecks::Failed(format!("hosted check '{name}' failed"));
                }
                None => {
                    return OwedHostedChecks::Failed(format!(
                        "hosted check '{name}' reports an unreadable state"
                    ));
                }
            }
        }
        if !answered || pending {
            waiting.push(name.clone());
        }
    }
    if waiting.is_empty() {
        OwedHostedChecks::Passed
    } else {
        OwedHostedChecks::Waiting(waiting)
    }
}
