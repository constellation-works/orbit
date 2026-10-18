//! Conservative revalidation of a task's existing delivery. The worker's
//! structured report is a claim; Git identity, scope, and captured checks must
//! agree before the pipeline can skip creating a new delivery.
//!
//! `covering_task_id` names this task (same-task retry) or a sibling whose
//! covering commit already landed the required change on the pinned HEAD.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{anchor_path, overlaps};
use orbit_types::task::Task;
use orbit_types::workflow::handoff::{
    AlreadyLandedEvidence as Evidence, already_landed_scope as scope,
};
use orbit_types::workflow::{ValidationOutcome, ValidationRole};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::delivery_marker::delivery_markers;
use super::super::git::{git_output, git_output_paths, git_success};
use super::checkpoint::pinned_object_id;
use super::clean_tree::{Specification, Verifier};
use super::no_diff;

pub(super) const ARTIFACT: &str = "already-landed.json";

/// Checkpoint decision for a verified covering delivery.
pub(super) const DECISION: &str = "verified_already_landed";

pub(super) struct AlreadyLanded;

impl Verifier for AlreadyLanded {
    const SPEC: Specification = Specification {
        decision: DECISION,
        evidence_key: "already_landed",
        refusal_prefix: "already_landed_unverified",
        head_changed: "tested HEAD changed; rerun required validation on the pinned current HEAD",
        dirty_tree: "worktree is no longer clean; deliver or reconcile the pending changes",
        missing_artifact_suffix: "; attach structured landing proof and required validation logs",
        task_count: "already-landed completion requires exactly one task",
        evidence_changed: "accepted landing evidence changed; rerun the commit verification step",
    };

    fn check_artifacts<H: RuntimeHost + ?Sized>(host: &H, task: &Task) -> Result<(), OrbitError> {
        if host.get_task_artifact(&task.id, ARTIFACT)?.is_none()
            && host
                .get_task_artifact(&task.id, no_diff::ARTIFACT)?
                .is_none()
        {
            return Err(Self::refused(format!(
                "missing task artifact {ARTIFACT} or {no_diff}; for a task that correctly changed nothing, attach {no_diff} naming this task, its run or retry lineage, the pinned HEAD as tested_head, a reason, and zero-exit validation logs carrying run_id, tested_head, command, exit_code and output; for work a covering commit delivered, attach {ARTIFACT} with structured landing proof and required validation logs",
                no_diff = no_diff::ARTIFACT,
            )));
        }
        Ok(())
    }

    fn verify_at_revision<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
    ) -> Result<Value, OrbitError> {
        let report = Self::artifact(host, &task.id, ARTIFACT)?;
        let evidence: Evidence = serde_json::from_slice(&report.content).map_err(|error| {
            let diagnostic = error.to_string();
            let hints = shape_hints(&diagnostic, &report.content);
            Self::refused(format!("invalid {ARTIFACT}: {diagnostic}{hints}"))
        })?;
        if evidence.schema_version != 1
            || evidence.task_id != task.id
            || evidence.covering_task_id.trim().is_empty()
        {
            return Err(Self::refused(
                "evidence must identify this task and a covering delivery",
            ));
        }
        let expected_scope = scope(task, &host.get_task_comments(&task.id)?);
        if evidence.scope != expected_scope {
            return Err(Self::refused(scope_changed(
                &evidence.scope,
                &expected_scope,
            )));
        }
        if !run_in_lineage(host, run_id, &evidence.run_id)? {
            return Err(Self::refused(
                "validation belongs to a different run; capture checks in this run or its recorded retry lineage",
            ));
        }
        let head = pinned_object_id(&evidence.tested_head)?;
        let covering = pinned_object_id(&evidence.covering_commit)?;
        if head != tested_head {
            return Err(Self::refused("tested HEAD is not the owner-observed base"));
        }

        git_success(
            workspace,
            &["merge-base", "--is-ancestor", &covering, &head],
        )
        .map_err(|error| {
            Self::refused(format!(
                "covering commit is not a verified ancestor of tested HEAD: {error}"
            ))
        })?;
        let message = git_output(workspace, &["show", "-s", "--format=%B", &covering])?;
        if !delivery_markers(&message).contains(&format!("[{}]", evidence.covering_task_id)) {
            return Err(Self::refused(
                "covering commit has no matching task delivery marker",
            ));
        }
        verify_covering_scope(task, workspace, &covering, &head)?;
        let validation_provenance = verify_checks(host, &evidence, task)?;

        Ok(json!({
            "phase": "commit",
            "decision": DECISION,
            "committed": false,
            "skipped_no_diff_expected": true,
            "task_id": task.id,
            "job_run_id": run_id,
            "base_sha": head,
            "already_landed": evidence,
            "validation_provenance": validation_provenance,
        }))
    }
}

/// Refusal for a report whose `scope` is not the task's current scope. It
/// names the first differing key (sorted) so a report copied from `task.show`
/// is not mistaken for a substantive task edit. Only key names appear; task
/// content never does.
fn scope_changed(reported: &Value, expected: &Value) -> String {
    let (reported, expected) = (reported.as_object(), expected.as_object());
    let mut keys: Vec<&String> = reported
        .into_iter()
        .chain(expected)
        .flat_map(|scope| scope.keys())
        .collect();
    keys.sort();
    keys.dedup();
    let differing = keys
        .into_iter()
        .find(|key| reported.and_then(|s| s.get(*key)) != expected.and_then(|s| s.get(*key)));
    let mut message = String::from("task scope changed");
    match differing {
        Some(key) => message.push_str(&format!(" at scope.{key}")),
        None => message.push_str("; scope must be a JSON object"),
    }
    message.push_str("; revalidate the current requirements, selectors and comments");
    if differing.is_some_and(|key| key == "comments") {
        message.push_str(
            "; scope.comments must list the task's comments with only `at`, `by` and `message` per comment (drop receipt fields such as `operation_id` that task.show adds)",
        );
    }
    message
}

/// Field-contract reminders for a report that failed to decode. Type and role
/// mistakes are located structurally, since serde's message names neither the
/// field nor, under `flatten`, the accepted role vocabulary.
fn shape_hints(diagnostic: &str, content: &[u8]) -> String {
    let mut hints = String::new();
    if diagnostic.contains("missing field `command`") {
        hints.push_str("; each validation[] element must flatten command, outcome, role, and log_artifact as sibling fields");
    }
    let Ok(report) = serde_json::from_slice::<Value>(content) else {
        return hints;
    };
    if report["criteria_evidence"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| !item.is_string()))
    {
        hints.push_str("; criteria_evidence must be an array of non-empty strings, one per acceptance criterion in order");
    }
    let unknown_role = report["validation"].as_array().is_some_and(|checks| {
        checks.iter().any(|check| {
            check
                .get("role")
                .is_some_and(|role| serde_json::from_value::<ValidationRole>(role.clone()).is_err())
        })
    });
    if unknown_role {
        hints.push_str("; validation[] role must be one of required, expected_failure, excluded, superseded, diagnostic (use required, with outcome passed, for each required command)");
    }
    hints
}

fn verify_covering_scope(
    task: &Task,
    workspace: &Path,
    covering: &str,
    head: &str,
) -> Result<(), OrbitError> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| AlreadyLanded::refused("workspace is unavailable"))?;
    for selector in &task.context_files {
        let anchor =
            anchor_path(selector).map_err(|error| AlreadyLanded::refused(error.to_string()))?;
        let resolved = canonical_workspace
            .join(anchor)
            .canonicalize()
            .map_err(|_| {
                AlreadyLanded::refused("scope anchor is unavailable; reconcile the task selectors")
            })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(AlreadyLanded::refused(
                "scope anchor is outside the tested workspace",
            ));
        }
    }
    let changed = git_output_paths(
        workspace,
        &["diff", "--name-only", "-z", covering, head, "--"],
    )?;
    if task.context_files.is_empty()
        || changed.iter().any(|path| {
            task.context_files
                .iter()
                .any(|selector| overlaps(selector, path))
        })
    {
        return Err(AlreadyLanded::refused(
            "covering scope is empty or changed since landing; reconcile and revalidate before delivery",
        ));
    }
    let delivered = git_output_paths(
        workspace,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--first-parent",
            "-m",
            "-z",
            covering,
            "--",
        ],
    )?;
    if !delivered.iter().any(|path| {
        task.context_files
            .iter()
            .any(|selector| overlaps(selector, path))
    }) {
        return Err(AlreadyLanded::refused(
            "covering commit did not change the task's declared scope",
        ));
    }
    Ok(())
}

/// Whether `evidence_run` is the current run or a run in its recorded retry
/// lineage. Shared with the no-diff verifier.
pub(super) fn run_in_lineage<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    evidence_run: &str,
) -> Result<bool, OrbitError> {
    let mut current = run_id.to_string();
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        if !evidence_run.is_empty() && current == evidence_run {
            return Ok(true);
        }
        if !seen.insert(current.clone()) {
            break;
        }
        let Some(source) = host
            .get_job_run(&current)?
            .and_then(|run| run.retry_source_run_id)
        else {
            break;
        };
        current = source;
    }
    Ok(false)
}

fn verify_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    evidence: &Evidence,
    task: &Task,
) -> Result<Vec<Value>, OrbitError> {
    let commands: BTreeSet<_> = evidence.required_commands.iter().collect();
    if commands.is_empty()
        || commands.len() != evidence.required_commands.len()
        || commands.iter().any(|command| command.trim().is_empty())
        || task.acceptance_criteria.is_empty()
        || evidence.criteria_evidence.len() != task.acceptance_criteria.len()
        || evidence
            .criteria_evidence
            .iter()
            .any(|item| item.trim().is_empty())
    {
        return Err(AlreadyLanded::refused(
            "list every required workspace/task check and concrete evidence for every acceptance criterion",
        ));
    }
    let mut checked = BTreeSet::new();
    let mut logs = Vec::new();
    for check in &evidence.validation {
        let validation = &check.validation;
        match validation.role {
            ValidationRole::Required => {
                if validation.outcome != ValidationOutcome::Passed
                    || !commands.contains(&validation.command)
                    || !checked.insert(&validation.command)
                {
                    return Err(AlreadyLanded::refused(
                        "required validation is missing, duplicated, failed, denied or not run",
                    ));
                }
            }
            ValidationRole::ExpectedFailure | ValidationRole::Diagnostic => {}
            ValidationRole::Excluded | ValidationRole::Superseded => {
                return Err(AlreadyLanded::refused(
                    "already-landed validation accepts only required, expected_failure or diagnostic checks",
                ));
            }
        }
        let log = AlreadyLanded::artifact(host, &task.id, &check.log_artifact)?;
        let provenance: Value = serde_json::from_slice(&log.content).map_err(|error| {
            AlreadyLanded::refused(format!(
                "invalid validation log {}: {error}",
                check.log_artifact
            ))
        })?;
        let exit_code = provenance["exit_code"].as_i64();
        let exit_matches = match validation.role {
            ValidationRole::Required => exit_code == Some(0),
            ValidationRole::ExpectedFailure => exit_code.is_some_and(|code| code != 0),
            ValidationRole::Diagnostic => exit_code.is_some(),
            ValidationRole::Excluded | ValidationRole::Superseded => false,
        };
        if validation.command.trim().is_empty()
            || !exit_matches
            || !log_has_provenance(
                &provenance,
                &evidence.run_id,
                &evidence.tested_head,
                &validation.command,
            )
        {
            return Err(AlreadyLanded::refused(
                "validation log must capture the exact run, tested HEAD, command, integer exit code and output; required checks must exit zero and expected_failure checks must exit nonzero",
            ));
        }
        logs.push(provenance);
    }
    if checked != commands {
        return Err(AlreadyLanded::refused("required validation is missing"));
    }
    Ok(logs)
}

/// A captured validation log names the exact run, tested HEAD and command,
/// with a zero exit code and captured (possibly empty) output.
pub(super) fn log_matches(
    provenance: &Value,
    run_id: &str,
    tested_head: &str,
    command: &str,
) -> bool {
    log_has_provenance(provenance, run_id, tested_head, command) && provenance["exit_code"] == 0
}

fn log_has_provenance(provenance: &Value, run_id: &str, tested_head: &str, command: &str) -> bool {
    provenance["run_id"] == run_id
        && provenance["tested_head"] == tested_head
        && provenance["command"] == command
        && provenance["output"].as_str().is_some()
}
