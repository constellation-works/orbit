//! Authenticate head transitions between base synchronization and publication.

mod validation_repair;

use super::*;

pub(super) use validation_repair::ValidationEvidence;

/// The before-landing settlement whose `incomplete` verdict the failure
/// handoff preserves by pushing the reviewer's commit.
const LANDING_SETTLE: &str = "landing_review_gate_settle";

/// Preserved non-accept before-landing reviewer commit `R` on a durable
/// candidate ref (`refs/orbit/candidates/<task>/<run>`), leaving `origin`
/// PR branch and PR head at `published`.
#[derive(Clone, Debug)]
pub(super) struct PreservedReviewTip {
    pub(super) head: String,
    pub(super) durable_ref: String,
}

/// The settlement, revalidation and publication step ids of one review layer.
fn review_steps(landing: bool) -> (&'static str, &'static str, &'static str) {
    if landing {
        (
            "landing_review_gate_settle",
            "landing_review_validate",
            "landing_push",
        )
    } else {
        ("review_gate_settle", "review_validate", "push")
    }
}

/// The reviewer correction a successful host settlement attributes to
/// `implementation`: `None` when the settlement leaves `implementation`
/// standing, otherwise the exact advancing reviewer head. The settlement
/// alone certifies nothing about that head; its host revalidation does.
fn settled_correction<'a>(
    state: &'a PipelineState,
    checkpoint: &impl Fn(&str, &str) -> Result<&'a Value, OrbitError>,
    landing: bool,
    implementation: &'a str,
    base: &str,
    repo: &std::path::Path,
) -> Result<Option<&'a str>, OrbitError> {
    if !observation::is_exact_sha(implementation) {
        return Err(refuse(
            "review implementation is not an exact commit identity",
        ));
    }
    let (settle_id, _, _) = review_steps(landing);
    let Some(output) = state
        .pipeline
        .get(settle_id)
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    let settle = checkpoint(settle_id, "review_gate_settle")?;
    if output["gate"] == "not_required" && output["reviewer_fixed"] == false {
        return Ok(None);
    }
    if settle["gate"] != "passed"
        || field(settle, "implementation_head_sha")? != implementation
        || field(settle, "reviewed_base_sha")? != base
    {
        return Err(refuse(
            "review settlement does not bind its implementation and base",
        ));
    }
    let reviewed = field(settle, "reviewed_head_sha")?;
    if settle["reviewer_fixed"] == false && reviewed == implementation {
        return Ok(None);
    }
    if settle["reviewer_fixed"] != true
        || reviewed == implementation
        || !observation::is_exact_sha(reviewed)
        || !observation::is_git_ancestor(repo, implementation, reviewed)?
    {
        return Err(refuse(
            "reviewer head is not an authenticated advancing correction",
        ));
    }
    Ok(Some(reviewed))
}

/// The definition revalidates a settled correction against its
/// implementation before publishing it, in that order.
fn correction_definition(job: &JobV2, landing: bool) -> Result<(), OrbitError> {
    let (settle_id, validate_id, publication_id) = review_steps(landing);
    require_input(
        job,
        validate_id,
        "ownership_base_sha",
        &format!("{{{{ steps.{settle_id}.output.implementation_head_sha }}}}"),
    )?;
    let position = |id: &str| job.steps.iter().position(|step| step.id == id);
    let predecessor = if landing { "pr_open" } else { "sync_base" };
    let (Some(before), Some(settle), Some(validate), Some(push)) = (
        position(predecessor),
        position(settle_id),
        position(validate_id),
        position(publication_id),
    ) else {
        return Err(refuse("review publication definition is incomplete"));
    };
    if !(before < settle && settle < validate && validate < push) {
        return Err(refuse("review publication steps are out of order"));
    }
    Ok(())
}

/// A host settlement authorizes an advancing reviewer head only after the host
/// validated that exact head and attributed its changes to the implementation.
pub(super) fn reviewed_head<'a>(
    job: &JobV2,
    state: &'a PipelineState,
    checkpoint: &impl Fn(&str, &str) -> Result<&'a Value, OrbitError>,
    landing: bool,
    implementation: &'a str,
    identity: (&str, &str),
    repo: &std::path::Path,
) -> Result<&'a str, OrbitError> {
    let (branch, base) = identity;
    let (_, validate_id, _) = review_steps(landing);
    let Some(reviewed) =
        settled_correction(state, checkpoint, landing, implementation, base, repo)?
    else {
        return Ok(implementation);
    };
    let validation = checkpoint(validate_id, "candidate_validate")?;
    if field(validation, "phase")? != "validate"
        || !matches!(
            field(validation, "decision")?,
            "passed" | "passed_with_known_red" | "passed_with_flaky_tests"
        )
        || field(validation, "tested_head")? != reviewed
        || field(validation, "branch")? != branch
    {
        return Err(refuse(
            "reviewer validation does not bind the published head",
        ));
    }
    correction_definition(job, landing)?;
    Ok(reviewed)
}

fn require_input(job: &JobV2, id: &str, key: &str, expected: &str) -> Result<(), OrbitError> {
    let input = job
        .steps
        .iter()
        .find(|step| step.id == id)
        .and_then(|step| match &step.body {
            JobV2StepBody::Target(target) => target.default_input.as_ref(),
            _ => None,
        });
    if input
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
        != Some(expected)
    {
        return Err(refuse(format!(
            "{id} does not bind {key} to its authenticated predecessor"
        )));
    }
    Ok(())
}

/// Whether `landing_push` ran and left a record. A step its `when:` skipped
/// (for instance under review policy `none` or when a review made no fix) is
/// checkpointed as `Success` with `Value::Null` and listed in `skipped_steps`,
/// and terminal compaction clears `skipped_steps` on `success` and `cancelled`.
/// Either shape means nothing was published; a `Success` step whose output is
/// missing altogether or non-null still counts as recorded so tampering or
/// unauthenticated later publications stay refused.
fn landing_push_recorded(job: &JobV2, state: &PipelineState) -> bool {
    if state
        .pipeline
        .get("landing_push")
        .is_some_and(|v| !v.is_null())
    {
        return true;
    }
    let Some(index) = job
        .steps
        .iter()
        .position(|step| step.id == "landing_push")
        .map(|idx| idx as u32)
    else {
        return false;
    };
    if state.step_output(index).is_some_and(|v| !v.is_null()) {
        return true;
    }
    if state.skipped_steps.contains(&index) {
        return false;
    }
    if state.step_states.get(&index) != Some(&JobRunState::Success) {
        return false;
    }
    let null_skip = state.step_output(index) == Some(&Value::Null)
        || state.pipeline.get("landing_push") == Some(&Value::Null);
    !null_skip
}

pub(super) fn published_head<'a>(
    job: &JobV2,
    state: &'a PipelineState,
    checkpoint: &impl Fn(&str, &str) -> Result<&'a Value, OrbitError>,
    source_state: JobRunState,
    (implementation, base, push): (&'a str, &str, &'a Value),
    branch: &str,
    repo: &std::path::Path,
) -> Result<&'a str, OrbitError> {
    let initial = reviewed_head(
        job,
        state,
        checkpoint,
        false,
        implementation,
        (branch, base),
        repo,
    )?;
    if field(push, "local_sha")? != initial {
        return Err(refuse(
            "initial publication lacks authenticated review provenance",
        ));
    }
    let landing_push_done = landing_push_recorded(job, state);
    // A stopped run that published nothing after `initial` left the PR and
    // origin branch there. Its failure receipt names the landing step that
    // failed; `preserved_landing_review_head` authenticates any repair tip the
    // handoff preserved separately, and the successor validates it afresh.
    let failed_landing_step = (source_state != JobRunState::Success && !landing_push_done)
        .then(|| {
            state
                .failure_activity_checkpoint
                .as_ref()
                .map(|receipt| receipt.failed_step_id.as_str())
        })
        .flatten();
    // A failed `landing_push` recorded no output or step success.
    let failed_landing_push = failed_landing_step == Some("landing_push");
    // A settled reviewer correction whose host revalidation failed was never
    // certified, so no `landing_review_validate` success can be demanded of
    // it; the settlement itself must still authenticate.
    if failed_landing_step == Some("landing_review_validate") {
        if settled_correction(state, checkpoint, true, initial, base, repo)?.is_none() {
            return Err(refuse(
                "failed review revalidation lacks a settled reviewer correction",
            ));
        }
        correction_definition(job, true)?;
        let validate = job
            .steps
            .iter()
            .position(|step| step.id == "landing_review_validate");
        if validate.is_some_and(|index| {
            state.step_states.get(&(index as u32)) == Some(&JobRunState::Success)
        }) {
            return Err(refuse(
                "failed review revalidation is recorded as successful",
            ));
        }
        return Ok(initial);
    }
    let head = reviewed_head(job, state, checkpoint, true, initial, (branch, base), repo)?;
    // A later publication must never be accepted without a settled correction,
    // even if a corrupted output happens to repeat the initial published head.
    if landing_push_done || (head != initial && !failed_landing_push) {
        if head == initial {
            return Err(refuse(
                "later publication lacks an authenticated reviewer correction",
            ));
        }
        let publication = checkpoint("landing_push", "git_push")?;
        require_input(
            job,
            "landing_push",
            "lease_remote_sha",
            "{{ steps.push.output.local_sha }}",
        )?;
        require_input(
            job,
            "landing_push",
            "pr_number",
            "{{ steps.pr_open.output.pr_number }}",
        )?;
        if field(publication, "phase")? != "push"
            || field(publication, "branch")? != branch
            || field(publication, "local_sha")? != head
            || field(publication, "remote_sha_before")? != initial
        {
            return Err(refuse("review publication branch, head or lease disagrees"));
        }
        return Ok(head);
    }
    Ok(if failed_landing_push { initial } else { head })
}

/// [ORB-25671] The reviewer commit an incomplete before-landing review left
/// on the published head, when this run's own failure handoff attests that
/// it pushed exactly that commit onto `published`: the head the branch and
/// pull request now carry. The push certifies nothing about the commit, so
/// the successor delivery validates it. Any fresh independent review follows
/// that delivery's selected review policy, which may be off.
///
/// `None` when the handoff pushed nothing (no receipt, no reviewer commit,
/// or a push it could not make), which keeps `published` and every refusal
/// for a branch that moved anyway. A receipt that claims a push but does not
/// bind this run, task, branch, failed settlement and published head is
/// refused rather than ignored.
pub(super) fn incomplete_review_pushed_head<'a>(
    job: &JobV2,
    state: &'a PipelineState,
    source: (&str, &Task),
    branch: &str,
    published: &str,
    repo: &std::path::Path,
) -> Result<Option<&'a str>, OrbitError> {
    let Some(receipt) = state.failure_activity_checkpoint.as_ref() else {
        return Ok(None);
    };
    let output = &receipt.output;
    let Some(push) = output.get("reviewer_push").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if matches!(
        push.get("decision").and_then(Value::as_str),
        Some("no_reviewer_commit" | "push_failed")
    ) {
        return Ok(None);
    }
    let disagrees = |what: &str| {
        refuse_operator_decision(format!(
            "the failure handoff's reviewer push attestation disagrees ({what}); the retained \
             branch is not an attested reviewer correction of the published head"
        ))
    };
    let (source_id, task) = source;
    let settle = job.steps.iter().position(|step| step.id == LANDING_SETTLE);
    let opened = job.steps.iter().position(|step| step.id == "pr_open");
    if job.failure_activity.as_deref() != Some("pr_failure_handoff")
        || !job.resolved_failure_activity.as_ref().is_some_and(|a| {
            matches!(&a.spec, ActivityV2Spec::Deterministic(s) if s.action == "pr_failure_handoff")
        })
        || receipt.activity_name != "pr_failure_handoff"
    {
        return Err(disagrees("not the host failure handoff"));
    }
    if receipt.failed_step_id != LANDING_SETTLE
        || output["failed_step_id"] != LANDING_SETTLE
        || output["phase"] != "failure_handoff"
        || output["decision"] != "landing_review_failure"
        || output["candidate_preserved"] != true
        || output["handoff_run_id"] != source_id
        || output["task_id"] != task.id.as_str()
        || !output["task_spec_digest"]
            .as_str()
            .is_some_and(|digest| task.spec_digest_matches(digest))
    {
        return Err(disagrees(
            "not this run's and task's incomplete before-landing review",
        ));
    }
    let (Some(settle), Some(opened)) = (settle, opened) else {
        return Err(disagrees("the definition has no before-landing settlement"));
    };
    if settle <= opened
        || state.step_states.get(&(settle as u32)) == Some(&JobRunState::Success)
        || [LANDING_SETTLE, "landing_push"]
            .iter()
            .any(|id| state.pipeline.get(*id).is_some_and(|v| !v.is_null()))
    {
        return Err(disagrees("the before-landing review settled or published"));
    }
    let pushed = push.get("local_sha").and_then(Value::as_str).unwrap_or("");
    if push["phase"] != "push"
        || push["decision"] != "performed_fast_forward"
        || push["branch"] != branch
        || push["published_head_sha"] != published
        || push["remote_sha_before"] != published
        || !observation::is_exact_sha(pushed)
        || pushed == published
        || !observation::is_git_ancestor(repo, published, pushed)?
    {
        return Err(disagrees(
            "it is not a fast-forward of the published head on this branch",
        ));
    }
    Ok(Some(pushed))
}

/// Read and authenticate `reviewer_preservation` recorded by `pr_failure_handoff`
/// for a non-accept or failed-push before-landing review (`reject`,
/// `landing_review` timeout, `landing_review_validate` failure, or failed
/// `landing_push`).
pub(super) fn preserved_landing_review_head(
    job: &JobV2,
    state: &PipelineState,
    source: (&str, &Task),
    published: &str,
    repo: &std::path::Path,
) -> Result<Option<PreservedReviewTip>, OrbitError> {
    let Some(receipt) = state.failure_activity_checkpoint.as_ref() else {
        return Ok(None);
    };
    let output = &receipt.output;
    let Some(preserved) = output.get("reviewer_preservation").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    if matches!(
        preserved.get("decision").and_then(Value::as_str),
        Some("no_reviewer_commit" | "commit_refused" | "preserve_failed" | "unavailable")
    ) {
        return Ok(None);
    }
    let disagrees = |what: &str| {
        refuse_operator_decision(format!(
            "the failure handoff's reviewer preservation attestation disagrees ({what}); the \
             preserved commit is not an attested reviewer repair of the published head"
        ))
    };
    let (source_id, task) = source;
    if job.failure_activity.as_deref() != Some("pr_failure_handoff")
        || !job.resolved_failure_activity.as_ref().is_some_and(|a| {
            matches!(&a.spec, ActivityV2Spec::Deterministic(s) if s.action == "pr_failure_handoff")
        })
        || receipt.activity_name != "pr_failure_handoff"
    {
        return Err(disagrees("not the host failure handoff"));
    }
    const LANDING_STEPS: &[&str] = &[
        "landing_review_gate_admit",
        "landing_review",
        "landing_review_gate_settle",
        "landing_review_validate",
        "landing_push",
    ];
    if !LANDING_STEPS.contains(&receipt.failed_step_id.as_str())
        || output["failed_step_id"] != receipt.failed_step_id
        || output["phase"] != "failure_handoff"
        || output["decision"] != "landing_review_failure"
        || output["candidate_preserved"] != true
        || output["handoff_run_id"] != source_id
        || output["task_id"] != task.id.as_str()
        || !output["task_spec_digest"]
            .as_str()
            .is_some_and(|digest| task.spec_digest_matches(digest))
    {
        return Err(disagrees(
            "not this run's and task's non-accept before-landing review",
        ));
    }
    let opened = job.steps.iter().position(|step| step.id == "pr_open");
    let failed_step_pos = job
        .steps
        .iter()
        .position(|step| step.id == receipt.failed_step_id);
    let Some((open_idx, fail_idx)) = opened.zip(failed_step_pos) else {
        return Err(disagrees("the definition places the review before PR open"));
    };
    if fail_idx <= open_idx {
        return Err(disagrees("the definition places the review before PR open"));
    }
    if receipt.failed_step_id == "landing_push"
        && !matches!(&job.steps[fail_idx].body, JobV2StepBody::Target(t)
            if matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "git_push"))
    {
        return Err(disagrees("landing_push is not the host git_push action"));
    }
    if landing_push_recorded(job, state) {
        return Err(disagrees("the before-landing review already landed"));
    }
    let commit_sha = preserved
        .get("commit_sha")
        .and_then(Value::as_str)
        .unwrap_or("");
    let durable_ref = preserved.get("ref").and_then(Value::as_str).unwrap_or("");
    let expected_ref = format!("refs/orbit/candidates/{}/{source_id}", task.id);
    if preserved["decision"] != "preserved"
        || preserved["published_head_sha"] != published
        || durable_ref != expected_ref
        || !observation::is_exact_sha(commit_sha)
        || commit_sha == published
        || !observation::resolves_to_commit(repo, commit_sha)?
        || !observation::is_git_ancestor(repo, published, commit_sha)?
    {
        return Err(disagrees(
            "it is not an attested preservation commit descending from the published head",
        ));
    }
    // A settled correction that failed revalidation or publication is
    // preserved exactly as the settlement attributed it.
    if matches!(
        receipt.failed_step_id.as_str(),
        "landing_review_validate" | "landing_push"
    ) && state
        .pipeline
        .get(LANDING_SETTLE)
        .and_then(|v| v.get("reviewed_head_sha"))
        .and_then(Value::as_str)
        != Some(commit_sha)
    {
        return Err(disagrees(
            "it is not the settled before-landing reviewer repair",
        ));
    }
    Ok(Some(PreservedReviewTip {
        head: commit_sha.to_string(),
        durable_ref: durable_ref.to_string(),
    }))
}
