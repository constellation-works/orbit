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
    if !exact_sha(implementation) {
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
        || !exact_sha(reviewed)
        || !run_git(
            repo,
            &["merge-base", "--is-ancestor", implementation, reviewed],
        )?
        .success
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

fn exact_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}
