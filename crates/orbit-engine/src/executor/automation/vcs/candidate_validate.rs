//! Required validation on the owner's own delivery path [ORB-13915].
//!
//! `workflow.required_validation_commands` is the list of commands a workspace
//! requires every delivered candidate to pass. [`candidate_validate`] runs it
//! after the candidate is synchronized onto its base and before it is
//! published or landed: an agent's report that it ran the commands is not
//! evidence; this step is. It runs each command the way `claim_validate` does
//! (see [`super::required::command`]), so a command that fails for lack of a
//! tool is the validation environment's failure rather than the candidate's
//! [ORB-13987].
//!
//! Managed PR synchronization that ends absorbed uses this same runner on
//! its final clean base before settlement, including a claimed leaf whose
//! logs are routed through its trusted owner binding.
//!
//! A failure that survives the network reruns is compared with the
//! synchronized base [ORB-14258] (see [`super::baseline`]). One the base shares
//! fails as a typed `baseline_red`: no recovery repairs it, and the failure
//! handoff (or run finalization) holds the task. One the base does not share
//! first receives one bounded isolated failing-test replay when it reports
//! a replayable selection. A pass records the flaky tests and proceeds;
//! a repeated failure fails like any other deterministic step, so a workflow can attach
//! `step_failure_recovery` to repair the candidate (for example commit a
//! formatting fix) before the one post-recovery attempt reruns every command.
//!
//! [ORB-15375] A shared failure whose failing tests all fail on the base too
//! ([`BaselineCheck::known_red`]) is not held: the candidate introduced none
//! of them. The step records the known-red tests in its output, its log and
//! each delivered task's history ([`KNOWN_RED_VALIDATION_EVENT`]), where
//! `pr_open` reads them for the pull request, and goes on to the next
//! command. Complete-output host coverage refusals and declared replay
//! execution counts are checked first: shared failed tests cannot waive an
//! unexecuted sandbox path [ORB-15548].
//!
//! [ORB-15287] An implementer whose affected-test gate passed with only
//! Bubblewrap deferrals hands the gate off instead of failing, and the task
//! keeps that record (see [`super::deferred_sandbox`]). The gate then runs
//! here after the required commands, outside any agent sandbox: it is judged
//! like them, a pass that still defers is the validation environment's
//! failure, and one that reports no executed tests is refused.
//!
//! [ORB-16784] A candidate whose tree differs from the last tree this run
//! passed the full list on only in `workflow.unlocked_context_paths` runs
//! `workflow.unlocked_path_validation_commands` instead, when that is set
//! (see [`super::validation_scope`]); every output and log names the scope.
//!
//! [ORB-16855] A candidate whose delivered tasks all carry `skip-validation`
//! and whose every changed path since `base_sha` matches
//! `workflow.light_delivery_paths` runs no command: the decision is
//! `skipped_light_delivery` and each task's history records why (see
//! [`super::light_delivery`]). Any other changed path ignores the tag.

use std::collections::BTreeSet;
use std::time::Instant;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{ContextWideningStep, SKIP_VALIDATION_TAG, TaskDecision, TaskHistoryEntry};
use orbit_types::workflow::handoff::HandoffArtifactRef;
use orbit_types::workflow::{
    BaselineRedFailures, BaselineRedHold, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation,
};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate, ValidatedTree};
use crate::executor::automation::input::{input_string_field, required_job_run_id};

use super::baseline::{
    BaselineCheck, baseline_red_failure, candidate_failure, compare_candidate_with_base,
    run_validation_command,
};
use super::claim::{
    NO_REQUIRED_COMMANDS_NOTE, SKIPPED_LIGHT_DELIVERY, SKIPPED_NO_REQUIRED_COMMANDS,
    require_clean_checkout,
};
use super::commit::attribute_candidate_paths;
use super::deferred_sandbox::{deferred_replays, with_replays};
use super::flaky::rerun_failed_tests;
use super::git::{git_command_success, git_output};
use super::handoff::completed_task_ids_from_input;
use super::light_delivery::{light_delivery_decision, record_light_delivery};
use super::validation_scope::validation_scope;

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
/// failing command fails the step after its log — those of the commands that
/// passed before it, and the base's run of it — is attached, unless its
/// failing tests are all known red on the base [ORB-15375]: then the
/// decision is `passed_with_known_red` and `known_red` names them. A passing
/// base also permits a bounded isolated failing-test replay: tests passing it
/// are recorded in `flaky`, and delivery proceeds with the rerun evidence.
pub(in crate::executor::automation) fn candidate_validate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let owned_paths = match input_string_field(input, "ownership_base_sha") {
        Some(ownership_base) => Some(attribute_reviewed_paths(host, input, &ownership_base)?),
        None => None,
    };
    let replays = match completed_task_ids_from_input(input) {
        Some(task_ids) => deferred_replays(host, &task_ids)?,
        None => Vec::new(),
    };
    let commands = with_replays(host.required_validation_commands(), &replays);
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
    if let Some(base_sha) = base_sha.as_deref()
        && let Some(decision) = light_delivery_decision(
            host,
            &workspace_path,
            base_sha,
            &candidate,
            &task_ids,
            SKIP_VALIDATION_TAG,
        )?
    {
        record_light_delivery(host, &task_ids, &decision)?;
        if decision.applies() {
            let mut output = json!({
                "phase": "validate",
                "base_sha": base_sha,
                "decision": SKIPPED_LIGHT_DELIVERY,
                "note": decision.note(),
                "commands": [],
                "validation": [],
                "branch": branch,
                "tested_head": candidate,
            });
            if let Some(owned_paths) = owned_paths {
                output["owned_paths"] = json!(owned_paths);
            }
            return Ok(output);
        }
    }
    let tree = git_output(
        &workspace_path,
        &["rev-parse", "--verify", &format!("{candidate}^{{tree}}")],
    )?;
    let scope = validation_scope(host, &workspace_path, &run_id, &task_ids, commands, &tree);
    let commands = &scope.commands;

    // Capturing optional reuse evidence must not change ordinary validation.
    let completion_policy = host
        .completion_policy_digest(&run_id, &task_ids)
        .ok()
        .flatten();
    let mut logs = Vec::new();
    let mut passed = Vec::new();
    let mut known_red = Vec::new();
    let mut flaky = Vec::new();
    let mut validation_env = Value::Null;
    for (index, command) in commands.iter().enumerate() {
        let run = run_validation_command(host, &workspace_path, command, None)?;
        validation_env = run.environment_record();
        // [ORB-14258] A conclusive failure that is not a missing tool is rerun on the
        // synchronized base, so a red base is told apart from the candidate.
        let baseline_started = Instant::now();
        let baseline: Option<BaselineCheck> = base_sha
            .as_deref()
            .filter(|_| {
                !run.passed && run.missing_tool.is_none() && run.infrastructure_failure.is_none()
            })
            .map(|base| compare_candidate_with_base(host, &workspace_path, base, &run));
        let baseline_comparison_ms = baseline
            .as_ref()
            .map(|_| baseline_started.elapsed().as_millis());
        let log_path = format!("validation/{run_id}/{index}.json");
        let baseline_path = baseline
            .as_ref()
            .map(|_| format!("validation/{run_id}/{index}.baseline.json"));
        let red = baseline
            .as_ref()
            .is_some_and(|check| check.reproduces(&run));
        let evidence = format!(
            "Candidate log: `{log_path}`; base log: {}.",
            baseline_path
                .as_ref()
                .map_or_else(|| "not run".to_string(), |path| format!("`{path}`"))
        );
        let coverage_failure = run.coverage_failure_with_evidence(
            &candidate,
            replays.contains(&run.command),
            baseline
                .as_ref()
                .map_or("not checked", BaselineCheck::decision),
            &evidence,
        );
        let known = baseline
            .as_ref()
            .filter(|_| coverage_failure.is_none())
            .and_then(|check| {
                check.known_red(&run).map(|failures| KnownRedValidation {
                    command: run.command.clone(),
                    base_sha: check.base_sha.clone(),
                    tested_head: candidate.clone(),
                    run_id: run_id.clone(),
                    failures: BaselineRedFailures::new(failures.iter().cloned(), ""),
                })
            });
        let rerun_started = Instant::now();
        let rerun = if coverage_failure.is_none() {
            rerun_failed_tests(
                host,
                &workspace_path,
                &candidate,
                &run_id,
                &run,
                baseline.as_ref(),
            )?
        } else {
            None
        };
        let candidate_rerun_ms = rerun.as_ref().map(|_| rerun_started.elapsed().as_millis());
        let recovered = rerun.as_ref().and_then(|rerun| rerun.flaky.as_ref());
        let rerun_path = format!("validation/{run_id}/{index}.rerun.json");
        let evidence = if rerun.is_some() {
            format!("{evidence} Candidate rerun log: `{rerun_path}`.")
        } else {
            evidence
        };
        if let Some(rerun) = &rerun {
            logs.push((
                rerun_path.clone(),
                serde_json::to_vec(&json!({
                    "schema_version": 1, "role": "candidate_rerun", "run_id": run_id,
                    "tested_head": candidate, "command": rerun.run.command,
                    "exit_code": rerun.run.exit_code, "timed_out": rerun.run.timed_out,
                    "output": rerun.run.output, "summary": rerun.run.summary,
                    "timing": rerun.run.timing,
                    "validation_env": rerun.run.environment_record(), "flaky": rerun.flaky,
                }))
                .map_err(|error| {
                    OrbitError::Execution(format!("encode candidate rerun: {error}"))
                })?,
            ));
        }
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
            "failures": run.failures,
            "baseline_reproduces": red,
            "validation_env": validation_env,
            "failure_kind": if recovered.is_some() {
                json!("flaky")
            } else if known.is_some() {
                json!("known_red")
            } else if red {
                json!("baseline_red")
            } else if baseline.as_ref().is_some_and(|check| check.infrastructure_failure().is_some()) {
                json!("environment")
            } else {
                run.failure_kind()
            },
            "known_red": known.as_ref().map(|known| &known.failures),
            "flaky": recovered,
            "candidate_rerun": rerun.as_ref().map(|_| &rerun_path),
            "deferred_notice": run.deferred_notice,
            "host_output_refusal": run.host_output_refusal.as_ref().map(|refusal| json!({
                "reason": refusal.reason.as_str(),
                "detail": refusal.detail,
            })),
            "deferred_sandbox_replay": replays.contains(&run.command),
            "missing_tool": run.missing_tool_name(),
            "network_retries": run.network_retries,
            "timing": run.timing,
            "baseline_comparison_ms": baseline_comparison_ms,
            "candidate_rerun_ms": candidate_rerun_ms,
            "summary": run.summary,
            "baseline": baseline
                .as_ref()
                .map(|check| check.record(baseline_path.as_deref())),
            "validation_scope": scope.record,
        }))
        .map_err(|error| OrbitError::Execution(format!("encode validation log: {error}")))?;
        logs.push((log_path.clone(), content));
        if let (Some(check), Some(path)) = (&baseline, &baseline_path) {
            let content = serde_json::to_vec(&check.log(&run_id)).map_err(|error| {
                OrbitError::Execution(format!("encode baseline validation log: {error}"))
            })?;
            logs.push((path.clone(), content));
        }
        if let Some(failure) = coverage_failure {
            attach_logs(host, &task_ids, &run_id, &logs)?;
            return Err(failure);
        }
        if let Some(recovered) = recovered {
            require_clean_checkout(&workspace_path, &branch, &candidate, "the candidate")?;
            attach_logs(host, &task_ids, &run_id, &logs)?;
            for task_id in &task_ids {
                host.record_flaky_validation(task_id, recovered)?;
            }
            flaky.push(recovered.clone());
            passed.push(run.command);
            continue;
        }
        if let Some(known) = known {
            record_known_red(host, &task_ids, &known)?;
            require_clean_checkout(&workspace_path, &branch, &candidate, "the candidate")?;
            known_red.push(known);
            continue;
        }
        if !run.passed {
            // The failing log is the evidence a recovery agent or reader
            // repairs from, so it is attached before the step fails.
            attach_logs(host, &task_ids, &run_id, &logs)?;
            if run.missing_tool.is_some() || run.infrastructure_failure.is_some() {
                return Err(run.failure(
                    &candidate,
                    "not checked: candidate environment failed",
                    &evidence,
                ));
            }
            if let Some(check) = baseline.as_ref().filter(|_| red) {
                let hold = BaselineRedHold {
                    base_ref: input_string_field(input, "base_ref").unwrap_or_default(),
                    base_sha: check.base_sha.clone(),
                    command: run.command.clone(),
                    run_id: run_id.clone(),
                    selection: run.selection().cloned(),
                    failures: check.failures(),
                };
                return Err(baseline_red_failure(&hold, &run, &candidate, &evidence));
            }
            return Err(candidate_failure(
                &run,
                &candidate,
                baseline.as_ref(),
                &evidence,
            ));
        }
        require_clean_checkout(&workspace_path, &branch, &candidate, "the candidate")?;
        passed.push(run.command);
    }

    // A policy or mandate changed while commands ran cannot confer reusable evidence.
    let completion_policy = completion_policy.filter(|before| {
        host.completion_policy_digest(&run_id, &task_ids)
            .ok()
            .flatten()
            .as_ref()
            == Some(before)
    });
    let references = attach_logs(host, &task_ids, &run_id, &logs)?;
    if scope.full {
        // Optional evidence: a host that cannot keep it validates later
        // candidates of this run in full.
        if let Err(error) = host.record_validated_tree(&ValidatedTree {
            run_id: run_id.clone(),
            task_ids: task_ids.clone(),
            workspace_path: workspace_path.to_string_lossy().into_owned(),
            head: candidate.clone(),
            tree: tree.clone(),
            base_sha: base_sha.clone(),
            commands: commands.clone(),
        }) {
            tracing::warn!(
                target: "orbit.engine.validation",
                run_id = %run_id,
                %error,
                "could not record the validated tree; later candidates of this run validate in full"
            );
        }
    }
    let mut output = json!({
        "phase": "validate",
        "base_sha": base_sha,
        "completion_policy": completion_policy,
        "decision": if !known_red.is_empty() { "passed_with_known_red" } else if !flaky.is_empty() { "passed_with_flaky_tests" } else { "passed" },
        "commands": passed,
        "known_red": known_red,
        "flaky": flaky,
        "branch": branch,
        "tested_head": candidate,
        "tested_tree": tree,
        "validation": serde_json::to_value(&references)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
        "validation_env": validation_env,
        "validation_scope": scope.record,
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

/// Record `known` on each delivered task's history, the decision `pr_open`
/// reads for the pull request.
fn record_known_red<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    known: &KnownRedValidation,
) -> Result<(), OrbitError> {
    let note = known.text(&format!(
        "Required validation '{}' fails on candidate {} only with tests that also fail on base \
         {}, so the candidate introduced none of them and delivery proceeds; {}.",
        known.command,
        known.tested_head,
        known.base_sha,
        known
            .failures
            .describe()
            .unwrap_or_else(|| "no failure named".to_string())
    ));
    for task_id in task_ids {
        host.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".to_string(),
                    event: KNOWN_RED_VALIDATION_EVENT.to_string(),
                    note: Some(note.clone()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::KnownRedValidation(known.clone())),
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}

fn attach_logs<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    run_id: &str,
    logs: &[(String, Vec<u8>)],
) -> Result<Vec<HandoffArtifactRef>, OrbitError> {
    // Absorbed PR synchronization also uses this runner on a claimed leaf:
    // there is no PR candidate to pin through claim_validate. Route its
    // diagnostic evidence through the trusted claim, never the local store.
    let claimed = host.worker_invocation().is_some();
    if claimed {
        let claim = host.claim_execution_context()?;
        if task_ids != std::slice::from_ref(&claim.task_id) || run_id != claim.run_id {
            return Err(OrbitError::PolicyDenied(
                "validation evidence does not belong to the bound claim".to_string(),
            ));
        }
    }
    let mut references = Vec::new();
    for (path, content) in logs {
        // Preserve the previous canonical bytes before replacing them. A first
        // observation needs only its canonical record; recovery must not erase
        // an earlier attempt's cost or failure. Archives are diagnostic only.
        let content =
            super::required::evidence::retain(host, task_ids, run_id, path, content, claimed)?;
        let digest = sha256_hex(&content);
        for task_id in task_ids {
            if let Some(previous) = host.get_task_artifact(task_id, path)?
                && previous.content != content
            {
                let archive = format!(
                    "validation/{run_id}/attempts/{}.json",
                    sha256_hex(&previous.content)
                );
                if claimed {
                    host.attach_claim_validation_log(&archive, previous.content)?;
                } else {
                    host.attach_task_validation_log(task_id, run_id, &archive, previous.content)?;
                }
            }
            if claimed {
                host.attach_claim_validation_log(path, content.clone())?;
            } else {
                host.attach_task_validation_log(task_id, run_id, path, content.clone())?;
            }
        }
        references.push(HandoffArtifactRef {
            path: path.clone(),
            sha256: digest,
        });
    }
    Ok(references)
}
