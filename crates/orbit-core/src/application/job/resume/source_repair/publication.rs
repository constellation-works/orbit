//! Authenticate reviewer transitions between base synchronization and publication.
use super::*;

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
    if !observation::is_exact_sha(implementation) {
        return Err(refuse(
            "review implementation is not an exact commit identity",
        ));
    }
    let (branch, base) = identity;
    let (settle_id, validate_id, publication_id) = if landing {
        (
            "landing_review_gate_settle",
            "landing_review_validate",
            "landing_push",
        )
    } else {
        ("review_gate_settle", "review_validate", "push")
    };
    let Some(output) = state
        .pipeline
        .get(settle_id)
        .filter(|value| !value.is_null())
    else {
        return Ok(implementation);
    };
    let settle = checkpoint(settle_id, "review_gate_settle")?;
    if output["gate"] == "not_required" && output["reviewer_fixed"] == false {
        return Ok(implementation);
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
        return Ok(implementation);
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

pub(super) fn published_head<'a>(
    job: &JobV2,
    state: &'a PipelineState,
    checkpoint: &impl Fn(&str, &str) -> Result<&'a Value, OrbitError>,
    sync: &'a Value,
    push: &'a Value,
    branch: &str,
    repo: &std::path::Path,
) -> Result<&'a str, OrbitError> {
    let base = field(sync, "base_sha")?;
    let initial = reviewed_head(
        job,
        state,
        checkpoint,
        false,
        field(sync, "head_sha")?,
        (branch, base),
        repo,
    )?;
    if field(push, "local_sha")? != initial {
        return Err(refuse(
            "initial publication lacks authenticated review provenance",
        ));
    }
    let head = reviewed_head(job, state, checkpoint, true, initial, (branch, base), repo)?;
    // A later publication must never be accepted without a settled correction,
    // even if a corrupted output happens to repeat the initial published head.
    if head != initial
        || state
            .pipeline
            .get("landing_push")
            .is_some_and(|value| !value.is_null())
    {
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
    }
    Ok(head)
}

/// The before-landing settlement whose `incomplete` verdict the failure
/// handoff preserves by pushing the reviewer's commit.
const LANDING_SETTLE: &str = "landing_review_gate_settle";

/// [ORB-25671] The reviewer commit an incomplete before-landing review left
/// on the published head, when this run's own failure handoff attests that
/// it pushed exactly that commit onto `published`: the head the branch and
/// pull request now carry. The push certifies nothing about the commit, so
/// the run that adopts it validates and reviews it afresh.
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
