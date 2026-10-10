use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::claim::carry_to_durable_ref;
use super::super::commit::commit_failure_candidate;
use super::super::git::git_output;
use super::{MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES, VALIDATION_ENVIRONMENT_EVENT};

/// The spec digest a later run compares before resuming this candidate
/// [ORB-13985], read after this handoff's own selector widening.
pub(super) fn recorded_spec_digest<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
) -> Result<String, OrbitError> {
    Ok(host.get_task(task_id)?.spec_digest())
}

/// [ORB-14905] Push a held, committed candidate to its durable ref on
/// `origin` — the ref a failed claimed leaf carries its candidate to
/// [ORB-14338] — and describe where it is. Under a distributed drain the
/// task's next run may be a claim on another host, which can fetch the
/// candidate only from `origin`. A refused push never stops the hold: the
/// candidate stays on this host, and the record says why.
fn carry_held_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    branch: &str,
    head_sha: &str,
    workspace_path: &Path,
) -> Result<HeldCandidate, OrbitError> {
    let carried = carry_to_durable_ref(host, workspace_path, branch, head_sha, &task.id, run_id);
    if let Err(reason) = &carried {
        tracing::warn!(
            run_id,
            task_id = %task.id,
            head_sha,
            error = %reason,
            "held candidate could not be pushed to a durable ref; it stays on this host"
        );
    }
    Ok(HeldCandidate {
        run_id: run_id.to_string(),
        machine_id: host.local_machine_id(),
        branch: branch.to_string(),
        head_sha: head_sha.to_string(),
        failed_step_id: failed_step_id.to_string(),
        durable_ref: carried.as_ref().ok().cloned(),
        carry_failure: carried.err(),
        task_spec_digest: recorded_spec_digest(host, &task.id)?,
    })
}

/// The history entry admission reads to offer `held` to the task's next
/// claim, on whichever host.
fn held_history(held: &HeldCandidate) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "system".to_string(),
        event: CANDIDATE_HELD_EVENT.to_string(),
        note: Some(held.text()),
        from_status: None,
        to_status: None,
    }
}

/// Where `held` is, for the hold's comment: on `origin`, or only on this
/// host with the push diagnostic.
fn held_place(held: &HeldCandidate) -> String {
    match (&held.durable_ref, &held.carry_failure) {
        (Some(reference), _) => format!(
            "The candidate is on `origin` at `{reference}`, so a later run of the task can fetch \
             it on any host."
        ),
        (None, failure) => {
            let machine = held
                .machine_id
                .as_deref()
                .map_or_else(|| "this host".to_string(), |id| format!("machine `{id}`"));
            format!(
                "The candidate is host-local: pushing it to a durable ref on `origin` failed, so \
                 it exists only on {machine}, and a run of the task on any other host implements \
                 it afresh.\n\n```text\n{}\n```",
                failure.as_deref().unwrap_or("no diagnostic")
            )
        }
    }
}

/// The hold output's record of where `held` is.
fn insert_carry(output: &mut Value, held: &HeldCandidate) {
    output["carry"] = json!(if held.durable_ref.is_some() {
        "durable"
    } else {
        "failed"
    });
    if let Some(reference) = &held.durable_ref {
        output["durable_ref"] = json!(reference);
    }
    if let Some(failure) = &held.carry_failure {
        output["carry_failure"] = json!(failure);
    }
}

/// Keep a worktree whose implementer [ORB-14269] or step recovery
/// [ORB-14268] declared a blocker.
///
/// Unlike a validation-environment failure, the tree may still be dirty:
/// nothing is committed, pushed, or published. The task is blocked under
/// [`TASK_BLOCKED_BY_AGENT_EVENT`](orbit_types::workflow::TASK_BLOCKED_BY_AGENT_EVENT)
/// with the kind in the note. Resume does not undo that block.
///
/// [ORB-14905] Unlike the holds below it publishes no durable candidate ref:
/// it commits nothing, so there is no candidate commit to push, and
/// `candidate_resume` never resumes its decision.
pub(super) fn preserve_agent_blocked_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let kind = orbit_types::workflow::task_blocked_by_agent_kind(error_message)
        .unwrap_or("unspecified")
        .to_string();
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let note = format!(
        "{} kind={kind} an agent declared a blocker: run={run_id}, \
         failed_step={failed_step_id}, candidate={head_sha}, branch={branch}; no PR was opened",
        orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER
    );
    let body = format!(
        "## Agent blocker\n\nThe implementer or step recovery stopped with kind `{kind}` and \
         asked the run not to continue. No final recovery ran, no review budget was spent, \
         nothing was committed or pushed, and no PR was opened. The worktree still holds the \
         candidate as the agent left it, including uncommitted files.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Kind: `{kind}`\n- Branch: `{branch}`\n- Head: `{head_sha}`\n\n\
         Move the task back to `in-progress` once the blocker is gone, then resume the run if the \
         candidate should continue.\n\n## Failure\n\n```text\n{}{}\n```",
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(orbit_types::workflow::TASK_BLOCKED_BY_AGENT_EVENT.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{body}"),
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": "blocked_by_agent",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "blocker_kind": kind,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "blocked",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}

/// Keep a candidate whose required validation could not run because a tool
/// was missing [ORB-13987].
///
/// The validation step only runs on a clean, committed candidate, so there is
/// nothing to commit and nothing to push: the branch and the run's worktree
/// already hold it, and `orbit job resume` reruns validation on that exact
/// head once the environment is fixed. The task is blocked under its own
/// event, which the blocked-task recovery backstop does not treat as a code
/// failure, and no PR is opened.
///
/// [ORB-14905] An operator may return the task to the backlog instead of
/// resuming the run, and a claim on another host may take it next, so the
/// candidate is also pushed to its durable ref on `origin`.
pub(super) fn preserve_validation_environment_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let held = carry_held_candidate(
        host,
        task,
        run_id,
        failed_step_id,
        &branch,
        &head_sha,
        workspace_path,
    )?;
    let note = format!(
        "required validation lacked a tool in its environment: run={run_id}, \
         failed_step={failed_step_id}, candidate={head_sha}, branch={branch}; the candidate was \
         not judged and no PR was opened"
    );
    let body = format!(
        "## Validation environment\n\nA required validation command could not run because a \
         tool it calls is missing from the validation environment. This is the host's \
         environment, not a defect in the candidate, so no repair ran, no review or rework \
         budget was spent, and no PR was opened.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Candidate branch: `{branch}`\n- Candidate head: `{head_sha}`\n\n\
         Make the tool available to the owner's login shell (or set \
         `workflow.validation_env.path`), check with `orbit doctor`, then resume validation on \
         the same candidate with `orbit job resume {run_id}`.\n\n{}\n\n## Failure\n\n\
         ```text\n{}{}\n```",
        held_place(&held),
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(VALIDATION_ENVIRONMENT_EVENT.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{body}"),
            }],
            append_history: vec![held_history(&held)],
            ..TaskAutomationUpdate::default()
        },
    )?;

    let mut output = json!({
        "phase": "failure_handoff",
        "decision": "blocked_validation_environment",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "blocked",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    });
    insert_carry(&mut output, &held);
    Ok(output)
}

/// Keep the candidate of a run its provider failed [ORB-14266].
///
/// Whatever the agent left is committed on the run's branch, attributed like
/// any failure candidate, and no branch or PR is published. The task's
/// status is not written here: run finalization moves it to `backlog` under a
/// provider failure hold, for local and PR pipelines alike. The next run on
/// this host resumes this candidate (`candidate_resume` treats the decision
/// as preserving) on a crew the hold permits.
///
/// [ORB-14905] The next run may be a claim on another host, so the candidate
/// is also pushed to its durable ref on `origin` and recorded in the task's
/// history for admission to offer. A refused push is recorded in a task
/// comment: the candidate is host-local.
pub(super) fn hold_provider_failure_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let (head_sha, committed_files) = commit_failure_candidate(host, run_id, workspace_path, task)?;
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let class = orbit_types::workflow::ProviderFailureClass::of(None, Some(error_message))
        .map_or("provider_failure", |class| class.as_str());
    let held = carry_held_candidate(
        host,
        task,
        run_id,
        failed_step_id,
        &branch,
        &head_sha,
        workspace_path,
    )?;
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            append_history: vec![held_history(&held)],
            append_comments: held
                .carry_failure
                .is_some()
                .then(|| TaskComment {
                    at: Utc::now(),
                    by: "system".to_string(),
                    message: format!(
                        "## Provider failure\n\nRun `{run_id}` failed at `{failed_step_id}` \
                         because of its provider ({class}). Its candidate `{head_sha}` is \
                         committed on branch `{branch}`.\n\n{}",
                        held_place(&held)
                    ),
                })
                .into_iter()
                .collect(),
            ..TaskAutomationUpdate::default()
        },
    )?;
    let mut output = json!({
        "phase": "failure_handoff",
        "decision": "held_provider_failure",
        "provider_failure": class,
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "committed_files": committed_files,
        "candidate_preserved": true,
        "pr_created": false,
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    });
    insert_carry(&mut output, &held);
    Ok(output)
}

/// Keep a candidate whose required command fails on its base exactly as on
/// the candidate, and hold its task [ORB-14258].
///
/// Like a validation-environment failure, the candidate is a clean committed
/// head the run's branch and worktree already hold: nothing is committed, and
/// no branch or PR is published. The task goes back to `backlog` under a
/// [`BASELINE_RED_HOLD_EVENT`](orbit_types::workflow::BASELINE_RED_HOLD_EVENT)
/// whose note carries the hold, so admission withholds it until the base ref
/// moves to a commit where the command may pass.
///
/// [ORB-14905] Any host may run the task next, so the candidate is pushed to
/// its durable ref on `origin` and recorded in the task's history; admission
/// hands it to the task's next claim, and a run on this host finds it in this
/// handoff's output. When the push is refused the hold still happens, and its
/// output and comment say the candidate is host-local.
pub(super) fn hold_baseline_red_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    hold: &orbit_types::workflow::BaselineRedHold,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let held = carry_held_candidate(
        host,
        task,
        run_id,
        failed_step_id,
        &branch,
        &head_sha,
        workspace_path,
    )?;
    let base_ref = if hold.base_ref.is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref)
    };
    let publication = task.github_pr_number().map_or_else(
        || "no PR was opened".to_string(),
        |number| {
            format!(
                "existing PR #{number} remains on its previously published head; this failing \
                 candidate was not pushed to its branch"
            )
        },
    );
    let note = hold.text(&format!(
        "required validation `{}` is red on base {}: run={run_id}, failed_step={failed_step_id}, \
         candidate={head_sha}, branch={branch}; held in the backlog until {base_ref} moves to a \
         base where it passes; {publication}",
        hold.command, hold.base_sha
    ));
    let body = format!(
        "## Red base\n\nRequired validation `{}` fails on base `{}` exactly as it fails on this \
         candidate, so the candidate did not cause it. No repair ran and no rework budget was \
         spent; {publication}.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Candidate branch: `{branch}`\n- Candidate head: `{head_sha}`\n\n\
         The task is back in the backlog and admission skips it while {base_ref} fails \
         the command; fixing the base is the way forward. Once it passes on a new base tip, the \
         next delivery can resume this candidate and validate it again.\n\n{}\n\n## Failure\n\n\
         ```text\n{}{}\n```",
        hold.command,
        hold.base_sha,
        held_place(&held),
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Backlog),
            status_event: Some(orbit_types::workflow::BASELINE_RED_HOLD_EVENT.to_string()),
            status_note: Some(note),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: body,
            }],
            append_history: vec![held_history(&held)],
            ..TaskAutomationUpdate::default()
        },
    )?;

    let mut output = json!({
        "phase": "failure_handoff",
        "decision": "held_baseline_red",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "base_ref": hold.base_ref,
        "base_sha": hold.base_sha,
        "command": hold.command,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "backlog",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    });
    insert_carry(&mut output, &held);
    Ok(output)
}
