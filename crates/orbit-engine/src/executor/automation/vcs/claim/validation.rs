use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{LightDeliveryDecision, SKIP_VALIDATION_TAG};
use orbit_types::workflow::handoff::{
    HandoffArtifactRef, HandoffCandidate, HandoffDelivery, HandoffKnownRed, HandoffValidationLog,
};
use orbit_types::workflow::{
    BaselineRedFailures, BaselineRedHold, FlakyValidation, KnownRedValidation,
    TRANSIENT_FAILURE_MARKER,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context::{ClaimExecutionContext, RuntimeHost};
use crate::executor::automation::input::input_string_field;

use super::super::baseline::{
    baseline_red_failure, candidate_failure, compare_candidate_with_base, run_validation_command,
};
use super::super::deferred_sandbox::{claimed_deferred_replays, with_replays};
use super::super::flaky::rerun_failed_tests;
use super::super::git::{BaseSyncMode, normalize_base_branch};
use super::super::light_delivery::light_delivery_decision;
use super::super::required::command::{RequiredCommandRun, bounded_validation_text};
use super::input::{refused, required_workspace};
use super::observe::{claimed_base_sync, observe, observe_with, require_clean_candidate};
use super::{already_resolved, no_diff};

/// Run the owner's required commands on the exact candidate and attach one
/// captured log per command to the owner's copy of the task.
///
/// The pull-request route validates before anything is published
/// [ORB-14258]. With no `pull_request` yet, the step runs the commands and
/// returns their results as `publication: pending`, attaching no typed handoff
/// logs: these name the delivery, which does not exist yet. Diagnostic logs
/// for an initial failure and its base/replay may already be attached. After
/// `pr_open`, the same activity given that output as `prevalidated` runs
/// nothing. It re-observes the candidate with its pull request, refuses one
/// that is not the commit and base the commands passed on, and attaches the
/// logs. A candidate whose validation failed is therefore never published.
///
/// A failing command is compared with the base (see [`super::super::baseline`]):
/// its log and the base's are attached to the owner's task, and a failure
/// the base shares fails as a typed `baseline_red`, which the leaf's
/// settlement turns into a held release rather than a failed claim.
///
/// [ORB-15427] Unless every test the candidate fails also fails on the base
/// ([`BaselineCheck::known_red`](super::super::baseline::BaselineCheck::known_red)),
/// as on the owner's own delivery path: the command's log then carries its
/// exit status and a known-red report ([`HandoffKnownRed`]), the decision is
/// `passed_with_known_red`, `known_red` names the tests for the pull request,
/// and the next command runs. The owner accepts that log only after its own
/// run of the base fails the same tests.
///
/// A comparable passing base also permits one bounded isolated failed-test
/// candidate replay. A pass carries `flaky` evidence through publication and
/// pinning; the owner records the tests and deduplicates their friction at
/// handoff acceptance. A repeated failure is refused as before.
///
/// An empty requirement list runs no command: the candidate is still observed
/// and pinned, and the output records `skipped_no_required_commands` with no
/// validation references.
///
/// [ORB-14849] A before-landing review runs after `pr_open`. Its
/// revalidation step passes the pre-publication output as `carry`: unless
/// `revalidate` is true — the reviewer committed a fix — that output is
/// returned unchanged, so the pin step always reads one step. With a fix the
/// commands run on the new head, still unpublished, before it is pushed.
///
/// [ORB-15287] When this attempt's implementer, handed in as
/// `implementation`, deferred Bubblewrap tests in a gate, that gate runs
/// after the required commands, here outside the agent sandbox, and must
/// report executed tests. The pre-publication output names it in
/// `replayed`, which the pin step expects among the results.
///
/// [ORB-15747] An implementer's already-resolved outcome belongs to the
/// NoDiff route, which replays its named tests on the base
/// ([`super::already_resolved`]); a candidate that changes the base is
/// refused for one.
///
/// [ORB-16855] When the owner's task carries `skip-validation` and the
/// owner's captured `workflow.light_delivery_paths` covers every path the
/// candidate changes on its base, no command or replay runs and the decision
/// is `skipped_light_delivery`; the pin step re-judges the published
/// candidate before attaching nothing.
///
/// [ORB-15764] A `no-diff-expected` skip checkpoint on that route runs no
/// command or replay at all and records `skipped_no_diff_expected` with no
/// validation references ([`super::no_diff`]).
pub(in crate::executor::automation) fn claim_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = host.claim_execution_context()?;
    let workspace_path = required_workspace(input)?;
    if input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return no_diff::validate(host, &context, &workspace_path, input);
    }
    if let Some(carried) = input.get("carry").filter(|value| !value.is_null())
        && input.get("revalidate").and_then(Value::as_bool) != Some(true)
    {
        return Ok(carried.clone());
    }
    if let Some(prevalidated) = input.get("prevalidated").filter(|value| !value.is_null()) {
        return pin_prevalidated(host, &context, &workspace_path, input, prevalidated);
    }
    if already_resolved::declared(input)?.is_some() {
        return Err(refused(
            "an already-resolved outcome hands off its unchanged base, but this candidate \
             changes it; deliver the change without `already_resolved`, or revert it",
        ));
    }
    let pending = context.ship_mode == "pr"
        && input
            .get("pull_request")
            .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty));
    let candidate = if pending {
        // Observed for its commit and base only; the delivery is pinned by
        // the `prevalidated` step once the pull request exists.
        observe_with(
            &workspace_path,
            &context,
            input,
            HandoffDelivery::LocalCandidate,
        )?
    } else {
        observe(&workspace_path, &context, input)?
    };
    require_clean_candidate(&workspace_path, &candidate)?;

    let light = light_validation(host, &context, &workspace_path, &candidate)?;
    let (replays, commands) = if light.is_some() {
        (Vec::new(), Vec::new())
    } else {
        let replays = claimed_deferred_replays(host, input)?;
        let commands = with_replays(context.required_commands.clone(), &replays);
        (replays, commands)
    };
    let (results, validation_env) = run_claimed_commands(
        host,
        &context,
        &workspace_path,
        input,
        &candidate,
        &commands,
        &replays,
    )?;

    // A later required command must not invalidate logs from an earlier one.
    // Publish them only after the whole suite has kept the candidate intact.
    require_clean_candidate(&workspace_path, &candidate)?;
    if pending {
        let known_red = known_red_records(&context, &candidate, &results);
        let mut output = json!({
            "decision": decision(&known_red, &results),
            "publication": "pending",
            "candidate": null,
            "validation": [],
            "commands": passed_commands(&results),
            "known_red": known_red,
        "flaky": results.iter().filter_map(|result| result.flaky.as_ref()).collect::<Vec<_>>(),
            "results": results,
            "tested_head": candidate.candidate.commit,
            "validated_base": candidate.base.commit,
            "validation_env": validation_env,
            "replayed": replays,
        });
        if commands.is_empty() {
            output["decision"] = json!(SKIPPED_NO_REQUIRED_COMMANDS);
            output["note"] = json!(NO_REQUIRED_COMMANDS_NOTE);
        }
        mark_light(&mut output, light.as_ref());
        return Ok(output);
    }
    let references = attach_handoff_logs(host, &context, &candidate, &results)?;
    let mut output = passed_output(&context, &candidate, &references, results, validation_env)?;
    mark_light(&mut output, light.as_ref());
    Ok(output)
}

/// The owner task's `skip-validation` decision for this claimed candidate
/// when the tag applies [ORB-16855]: the owner's captured allow-list
/// covers every path the candidate changes on its base, so no required
/// command or deferral replay runs. The owner re-judges the candidate, and
/// records the decision in history, when it accepts the handoff.
fn light_validation<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace_path: &Path,
    candidate: &HandoffCandidate,
) -> Result<Option<LightDeliveryDecision>, OrbitError> {
    Ok(light_delivery_decision(
        host,
        workspace_path,
        &candidate.base.commit,
        &candidate.candidate.commit,
        std::slice::from_ref(&context.task_id),
        SKIP_VALIDATION_TAG,
    )?
    .filter(LightDeliveryDecision::applies))
}

/// Record an applied `skip-validation` decision as the step's decision.
fn mark_light(output: &mut Value, light: Option<&LightDeliveryDecision>) {
    if let Some(light) = light {
        output["decision"] = json!(SKIPPED_LIGHT_DELIVERY);
        output["note"] = json!(light.note());
    }
}

/// One required command's result on a claimed candidate: it passed, or it
/// passed an isolated flaky-test replay, or failed only with tests its base
/// fails too, as `known_red` reports
/// [ORB-15427]. A pre-publication output carries these as `results`, and the
/// pin attaches them as they were.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClaimedResult {
    command: String,
    output: String,
    /// Absent from outputs recorded before known red applied to claims,
    /// which carried only passed commands.
    #[serde(default)]
    exit_code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    known_red: Option<HandoffKnownRed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flaky: Option<FlakyValidation>,
}

impl ClaimedResult {
    /// A command that passed outright with `output`.
    pub(super) fn passed(command: String, output: String) -> Self {
        Self {
            command,
            output,
            exit_code: 0,
            known_red: None,
            flaky: None,
        }
    }
}

/// Run `commands` on the observed candidate, in order, and return each
/// command's result with the last run's validation environment. A failing
/// command is judged by [`claim_failure`], which refuses it unless its
/// failing tests are known red on the base or pass an isolated candidate
/// replay, with host coverage executed. A
/// passing one still must not have deferred a sandbox-confined path, and a
/// command in `replays` must report executed tests before either continuation.
/// Shared by the ordinary route and clean-base NoDiff
/// validation so neither can attach a passing log the other would refuse.
pub(super) fn run_claimed_commands<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace_path: &Path,
    input: &Value,
    candidate: &HandoffCandidate,
    commands: &[String],
    replays: &[String],
) -> Result<(Vec<ClaimedResult>, Value), OrbitError> {
    let mut results = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in commands.iter().enumerate() {
        let run = run_validation_command(host, workspace_path, command, None)?;
        validation_env = run.environment_record();
        let commit = &candidate.candidate.commit;
        let replay = replays.contains(&run.command);
        let (known_red, flaky) = if run.passed && run.coverage_failure(commit, replay).is_none() {
            (None, None)
        } else {
            claim_failure(host, context, input, candidate, index, &run, replay)?
        };
        require_clean_candidate(workspace_path, candidate)?;
        results.push(ClaimedResult {
            command: run.command,
            output: flaky
                .as_ref()
                .map_or_else(|| run.output.clone(), |flaky| flaky.rerun_output.clone()),
            exit_code: if flaky.is_some() { 0 } else { run.exit_code },
            known_red,
            flaky,
        });
    }
    Ok((results, validation_env))
}

/// Pin a pre-publication validation onto the published candidate: the
/// commands already passed on this exact commit and base, so the logs are
/// built from those results with the pull request as their delivery.
fn pin_prevalidated<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    workspace_path: &Path,
    input: &Value,
    prevalidated: &Value,
) -> Result<Value, OrbitError> {
    let candidate = observe(workspace_path, context, input)?;
    require_clean_candidate(workspace_path, &candidate)?;
    if input_string_field(prevalidated, "publication").as_deref() != Some("pending") {
        return Err(OrbitError::InvalidInput(
            "prevalidated must be the output of a pre-publication claim_validate step".to_string(),
        ));
    }
    let tested_head = input_string_field(prevalidated, "tested_head");
    let validated_base = input_string_field(prevalidated, "validated_base");
    if tested_head.as_deref() != Some(candidate.candidate.commit.as_str())
        || validated_base.as_deref() != Some(candidate.base.commit.as_str())
    {
        return Err(refused(format!(
            "required validation passed on candidate {} over base {}, but the published \
             candidate is {} over base {}; rerun validation",
            tested_head.as_deref().unwrap_or("<none>"),
            validated_base.as_deref().unwrap_or("<none>"),
            candidate.candidate.commit,
            candidate.base.commit
        )));
    }
    let results: Vec<ClaimedResult> = prevalidated
        .get("results")
        .cloned()
        .filter(|results| !results.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!(
                "prevalidated results must each carry a command and its output: {error}"
            ))
        })?
        .unwrap_or_default();
    let replayed = prevalidated
        .get("replayed")
        .and_then(Value::as_array)
        .map(|replayed| {
            replayed
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let light = if input_string_field(prevalidated, "decision").as_deref()
        == Some(SKIPPED_LIGHT_DELIVERY)
    {
        let light = light_validation(host, context, workspace_path, &candidate)?;
        if light.is_none() {
            return Err(refused(
                "required validation was skipped for `skip-validation`, but the published \
                 candidate no longer keeps the tag; rerun validation",
            ));
        }
        light
    } else {
        None
    };
    let expected = if light.is_some() {
        Vec::new()
    } else {
        with_replays(context.required_commands.clone(), &replayed)
    };
    if results
        .iter()
        .map(|result| result.command.as_str())
        .ne(expected.iter().map(|command| command.trim()))
    {
        return Err(refused(
            "the prevalidated commands are not the owner's required commands and the \
             deferrals they replay; rerun validation",
        ));
    }
    // Each result passed, or failed known red on the base the published
    // candidate names [ORB-15427].
    if results.iter().any(|result| match &result.known_red {
        None => result.exit_code != 0,
        Some(known) => result.exit_code == 0 || known.base_sha != candidate.base.commit,
    }) {
        return Err(refused(
            "a prevalidated result is neither a pass nor known red on the published \
             candidate's base; rerun validation",
        ));
    }
    if results.iter().any(|result| {
        result.flaky.as_ref().is_some_and(|record| {
            !record.valid()
                || result.known_red.is_some()
                || result.exit_code != 0
                || record.command != result.command
                || record.base_sha != candidate.base.commit
                || record.tested_head != candidate.candidate.commit
                || record.run_id != context.run_id
                || record.rerun_output != result.output
        })
    }) {
        return Err(refused(
            "prevalidated flaky evidence does not match the pinned candidate",
        ));
    }
    let references = attach_handoff_logs(host, context, &candidate, &results)?;
    let validation_env = prevalidated
        .get("validation_env")
        .cloned()
        .unwrap_or(Value::Null);
    let mut output = passed_output(context, &candidate, &references, results, validation_env)?;
    mark_light(&mut output, light.as_ref());
    Ok(output)
}

/// Attach one typed log per command to the owner's task: a passed command's,
/// or a known-red command's with its exit status and report [ORB-15427].
pub(super) fn attach_handoff_logs<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    candidate: &HandoffCandidate,
    results: &[ClaimedResult],
) -> Result<Vec<HandoffArtifactRef>, OrbitError> {
    let mut references = Vec::new();
    for (index, result) in results.iter().enumerate() {
        let log = HandoffValidationLog {
            schema_version: 1,
            workspace_id: context.workspace_id.clone(),
            task_id: context.task_id.clone(),
            claim_id: context.claim_id.clone(),
            machine_id: context.machine_id.clone(),
            run_id: context.run_id.clone(),
            candidate: candidate.clone(),
            tested_head: candidate.candidate.commit.clone(),
            command: result.command.clone(),
            exit_code: result.exit_code,
            output: result.output.clone(),
            known_red: result.known_red.clone(),
            flaky: result.flaky.clone(),
        };
        let content = serde_json::to_vec(&log)
            .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        let path = format!("validation/{}/{index}.json", context.claim_id);
        host.attach_claim_validation_log(&path, content.clone())?;
        references.push(HandoffArtifactRef {
            path,
            sha256: sha256_hex(&content),
        });
    }
    Ok(references)
}

pub(super) fn passed_output(
    context: &ClaimExecutionContext,
    candidate: &HandoffCandidate,
    references: &[HandoffArtifactRef],
    results: Vec<ClaimedResult>,
    validation_env: Value,
) -> Result<Value, OrbitError> {
    let skipped = results.is_empty();
    let known_red = known_red_records(context, candidate, &results);
    let mut output = json!({
        "decision": decision(&known_red, &results),
        "candidate": serde_json::to_value(candidate)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation": serde_json::to_value(references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "commands": passed_commands(&results),
        "known_red": known_red,
        "flaky": results.iter().filter_map(|result| result.flaky.as_ref()).collect::<Vec<_>>(),
        "tested_head": candidate.candidate.commit,
        "validated_base": candidate.base.commit,
        "validation_env": validation_env,
        "no_diff_evidence": null,
    });
    if skipped {
        output["decision"] = json!(SKIPPED_NO_REQUIRED_COMMANDS);
        output["note"] = json!(NO_REQUIRED_COMMANDS_NOTE);
    }
    Ok(output)
}

/// The commands that passed outright.
fn passed_commands(results: &[ClaimedResult]) -> Vec<&str> {
    results
        .iter()
        .filter(|result| result.known_red.is_none())
        .map(|result| result.command.as_str())
        .collect()
}

/// The known-red record of each known-red result, for the pull request
/// `pr_open` publishes [ORB-15427].
fn known_red_records(
    context: &ClaimExecutionContext,
    candidate: &HandoffCandidate,
    results: &[ClaimedResult],
) -> Vec<KnownRedValidation> {
    results
        .iter()
        .filter_map(|result| {
            let known = result.known_red.as_ref()?;
            Some(KnownRedValidation {
                command: result.command.clone(),
                base_sha: known.base_sha.clone(),
                tested_head: candidate.candidate.commit.clone(),
                run_id: context.run_id.clone(),
                failures: BaselineRedFailures::new(known.failures.iter().cloned(), ""),
            })
        })
        .collect()
}

/// A pass, with known-red or flaky-test evidence when applicable.
fn decision(known_red: &[KnownRedValidation], results: &[ClaimedResult]) -> &'static str {
    if !known_red.is_empty() {
        "passed_with_known_red"
    } else if results.iter().any(|result| result.flaky.is_some()) {
        "passed_with_flaky_tests"
    } else {
        "passed"
    }
}

/// Judge a claimed command that did not pass. A failure whose failing tests
/// all fail on the candidate's base is known red [ORB-15427]: the report for
/// its handoff log is returned and validation goes on. A comparable passing
/// base permits one bounded failing-test replay; a pass returns flaky evidence.
/// Other failures are refused. One that is not a missing tool is rerun on the candidate's base,
/// and both logs are attached to the owner's task before the step fails; a
/// failure the base shares is typed `baseline_red`, and one that still could
/// not reach the network after its retries is typed `transient`.
fn claim_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &ClaimExecutionContext,
    input: &Value,
    candidate: &HandoffCandidate,
    index: usize,
    run: &RequiredCommandRun,
    replay: bool,
) -> Result<(Option<HandoffKnownRed>, Option<FlakyValidation>), OrbitError> {
    let commit = &candidate.candidate.commit;
    let workspace_path = required_workspace(input)?;
    let check = (!run.passed && run.missing_tool.is_none() && run.infrastructure_failure.is_none())
        .then(|| compare_candidate_with_base(host, &workspace_path, &candidate.base.commit, run));
    let coverage_failure = run.coverage_failure(commit, replay);
    let known_red = check
        .as_ref()
        .filter(|_| coverage_failure.is_none())
        .and_then(|check| {
            check.known_red(run).map(|failures| HandoffKnownRed {
                base_sha: check.base_sha.clone(),
                selection: run.selection().cloned(),
                failures: failures.clone(),
            })
        });
    let rerun = if coverage_failure.is_none() && known_red.is_none() {
        rerun_failed_tests(
            host,
            &workspace_path,
            commit,
            &context.run_id,
            run,
            check.as_ref(),
        )?
    } else {
        None
    };
    let red = check.as_ref().is_some_and(|check| check.reproduces(run));
    let log_path = format!("validation/{}/{index}.failed.json", context.claim_id);
    let base_path = format!("validation/{}/{index}.baseline.json", context.claim_id);
    let log = json!({
        "schema_version": 1,
        "role": "candidate",
        "workspace_id": context.workspace_id,
        "task_id": context.task_id,
        "claim_id": context.claim_id,
        "machine_id": context.machine_id,
        "run_id": context.run_id,
        "tested_head": commit,
        "base_sha": candidate.base.commit,
        "command": run.command,
        "exit_code": run.exit_code,
        "timed_out": run.timed_out,
        "output": run.output,
        "failures": run.failures,
        "baseline_reproduces": red,
        "host_output_refusal": run.host_output_refusal.as_ref().map(|refusal| json!({
            "reason": refusal.reason.as_str(),
            "detail": refusal.detail,
        })),
        "validation_env": run.environment_record(),
        "flaky": rerun.as_ref().and_then(|rerun| rerun.flaky.as_ref()),
        "failure_kind": if rerun.as_ref().is_some_and(|rerun| rerun.flaky.is_some()) {
            json!("flaky")
        } else if known_red.is_some() {
            json!("known_red")
        } else if red {
            json!("baseline_red")
        } else if check.as_ref().is_some_and(|check| check.infrastructure_failure().is_some()) {
            json!("environment")
        } else if run.network_evidence.is_some() {
            json!("transient")
        } else {
            run.failure_kind()
        },
        "network_retries": run.network_retries,
        "timing": run.timing,
        "summary": run.summary,
        "baseline": check.as_ref().map(|check| check.record(Some(&base_path))),
    });
    // The logs are evidence for whoever reads the owner's task; failing to
    // deliver them must not hide the typed failure itself.
    let rerun_path = format!("validation/{}/{index}.rerun.json", context.claim_id);
    let mut logs = vec![(&log_path, log)];
    if let Some(rerun) = &rerun {
        logs.push((
            &rerun_path,
            json!({
                "schema_version": 1, "role": "candidate_rerun", "run_id": context.run_id,
                "tested_head": commit, "command": rerun.run.command,
                "exit_code": rerun.run.exit_code, "timed_out": rerun.run.timed_out,
                "output": rerun.run.output, "summary": rerun.run.summary,
                "timing": rerun.run.timing,
                "validation_env": rerun.run.environment_record(), "flaky": rerun.flaky,
            }),
        ));
    }
    if let Some(check) = &check {
        logs.push((&base_path, check.log(&context.run_id)));
    }
    for (path, content) in logs {
        if let Err(error) = serde_json::to_vec(&content)
            .map_err(|error| OrbitError::Execution(error.to_string()))
            .and_then(|bytes| {
                super::super::required::evidence::retain(
                    host,
                    std::slice::from_ref(&context.task_id),
                    &context.run_id,
                    path,
                    &bytes,
                    true,
                )
            })
            .and_then(|bytes| host.attach_claim_validation_log(path, bytes))
        {
            tracing::warn!(
                path,
                "could not attach a claimed validation failure log: {error}"
            );
        }
    }
    if let Some(known_red) = known_red {
        return Ok((Some(known_red), None));
    }
    if let Some(record) = rerun.as_ref().and_then(|rerun| rerun.flaky.as_ref()) {
        require_clean_candidate(&workspace_path, candidate)?;
        return Ok((None, Some(record.clone())));
    }
    let evidence = format!(
        "Candidate log: `{log_path}`; base log: {}. Candidate rerun log: {}.",
        check
            .as_ref()
            .map_or_else(|| "not run".to_string(), |_| format!("`{base_path}`")),
        rerun
            .as_ref()
            .map_or_else(|| "not run".to_string(), |_| format!("`{rerun_path}`"))
    );
    let verdict = check
        .as_ref()
        .map_or("not checked", |check| check.decision());
    if let Some(failure) = run.coverage_failure_with_evidence(commit, replay, verdict, &evidence) {
        return Err(failure);
    }
    if run.missing_tool.is_some() || run.infrastructure_failure.is_some() {
        return Err(run.failure(commit, verdict, &evidence));
    }
    if let Some(check) = check.as_ref().filter(|_| red) {
        let hold = BaselineRedHold {
            base_ref: claimed_base_ref(context, input)?,
            base_sha: check.base_sha.clone(),
            command: run.command.clone(),
            run_id: context.run_id.clone(),
            selection: run.selection().cloned(),
            failures: check.failures(),
        };
        return Err(baseline_red_failure(&hold, run, commit, &evidence));
    }
    // Still unable to reach the network after its retries, the command said
    // nothing about the candidate [ORB-14257].
    if let Some(network) = &run.network_evidence {
        return Err(OrbitError::Execution(format!(
            "{TRANSIENT_FAILURE_MARKER} {} Could not reach the network after {} retries ({}); \
             the candidate was not judged.",
            run.failure_summary(commit, verdict, &evidence),
            run.network_retries,
            bounded_validation_text(network, 1024)
        )));
    }
    Err(candidate_failure(run, commit, check.as_ref(), &evidence))
}

/// The base ref this claim synchronized onto, as the owner names it.
fn claimed_base_ref(context: &ClaimExecutionContext, input: &Value) -> Result<String, OrbitError> {
    let branch = normalize_base_branch(&context.base_branch)
        .unwrap_or_else(|_| context.base_branch.trim().to_string());
    Ok(match claimed_base_sync(context, input)? {
        BaseSyncMode::Remote => format!("origin/{branch}"),
        BaseSyncMode::Local => branch,
    })
}

/// The decision a validation step records when the workspace requires no
/// command, on the owner's delivery path and a claimed leaf alike.
pub(in crate::executor::automation::vcs) const SKIPPED_NO_REQUIRED_COMMANDS: &str =
    "skipped_no_required_commands";

/// The decision a validation step records when every delivered task carries
/// `skip-validation` and the candidate changes only
/// `workflow.light_delivery_paths` [ORB-16855].
pub(in crate::executor::automation::vcs) const SKIPPED_LIGHT_DELIVERY: &str =
    "skipped_light_delivery";

/// Why that decision ran nothing, in the step's own output.
pub(in crate::executor::automation::vcs) const NO_REQUIRED_COMMANDS_NOTE: &str = "no required validation commands configured \
     (`workflow.required_validation_commands` is empty); no check ran";

#[cfg(test)]
#[path = "tests/validation.rs"]
mod tests;
