//! Required validation on the owner's own delivery path [ORB-13915].
//!
//! `workflow.required_validation_commands` is the list of commands a workspace
//! requires every delivered candidate to pass. [`candidate_validate`] runs it
//! after the candidate is synchronized onto its base and before it is
//! published or landed: an agent's report that it ran the commands is not
//! evidence; this step is. It runs each command the way `claim_validate` does
//! (see [`super::required_command`]), so a command that fails for lack of a
//! tool is the validation environment's failure rather than the candidate's
//! [ORB-13987].
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

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{ContextWideningStep, TaskHistoryEntry};
use orbit_types::workflow::handoff::HandoffArtifactRef;
use orbit_types::workflow::{
    BaselineRedFailures, BaselineRedHold, KNOWN_RED_VALIDATION_EVENT, KnownRedValidation,
};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{input_string_field, required_job_run_id};

use super::baseline::{
    BaselineCheck, baseline_red_failure, candidate_failure, compare_with_base,
    run_validation_command,
};
use super::claim::{
    NO_REQUIRED_COMMANDS_NOTE, SKIPPED_NO_REQUIRED_COMMANDS, require_clean_checkout,
};
use super::commit::attribute_candidate_paths;
use super::deferred_sandbox::{deferred_replays, with_replays};
use super::flaky::rerun_failed_tests;
use super::git::{git_command_success, git_output};
use super::handoff::completed_task_ids_from_input;

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
        let baseline: Option<BaselineCheck> = base_sha
            .as_deref()
            .filter(|_| {
                !run.passed && run.missing_tool.is_none() && run.infrastructure_failure.is_none()
            })
            .map(|base| compare_with_base(host, &workspace_path, base, command, run.selection()));
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
            "summary": run.summary,
            "baseline": baseline
                .as_ref()
                .map(|check| check.record(baseline_path.as_deref())),
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

    let references = attach_logs(host, &task_ids, &run_id, &logs)?;
    let mut output = json!({
        "phase": "validate",
        "decision": if !known_red.is_empty() { "passed_with_known_red" } else if !flaky.is_empty() { "passed_with_flaky_tests" } else { "passed" },
        "commands": passed,
        "known_red": known_red,
        "flaky": flaky,
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

/// Record `known` on each delivered task's history, the note `pr_open` reads
/// for the pull request.
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
