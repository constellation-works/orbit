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
    AlreadyLandedEvidence as Evidence, already_landed_scope as scope, already_landed_scope_matches,
};
use orbit_types::workflow::{DeliveryRefusal, ValidationOutcome, ValidationRole};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::delivery_marker::delivery_markers;
use super::super::git::{git_output, git_output_paths, git_success};
use super::super::retry_lineage::{LineageStep, LineageStop, RETRY_LINEAGE_MAX_HOPS, RetryLineage};
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
        Self::verify_at_revision_with_supplied_regression_from(
            host,
            task,
            workspace,
            run_id,
            tested_head,
            &[],
        )
    }

    fn verify_at_revision_with_supplied_regression_from<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
        supplied_regression_from: &[String],
    ) -> Result<Value, OrbitError> {
        Self::verify_evidence(
            host,
            task,
            workspace,
            run_id,
            tested_head,
            supplied_regression_from,
            ScopeChange::Fails,
        )
    }

    /// The owner's recheck of a handoff's pinned evidence. A task whose scope
    /// changed after the evidence was captured is a deliberate, typed
    /// [`DeliveryRefusal::EvidenceStale`], so the owner's landing holds the
    /// candidate in review; every other refusal keeps its plain failure.
    fn verify_handoff_at_revision_with_supplied_regression_from<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        checkpoint: &Value,
        supplied_regression_from: &[String],
    ) -> Result<(), OrbitError> {
        let head = checkpoint["base_sha"].as_str().unwrap_or_default();
        let checked = Self::verify_evidence(
            host,
            task,
            workspace,
            run_id,
            head,
            supplied_regression_from,
            ScopeChange::StaleEvidence,
        )?;
        Self::compare_checkpoint(checkpoint, &checked)
    }
}

/// What a task scope that differs from the recorded evidence's scope raises.
#[derive(Clone, Copy)]
enum ScopeChange {
    /// The implement-side commit step: the agent's evidence is wrong, so the
    /// step fails.
    Fails,
    /// The owner's recheck at acceptance or landing: the evidence went stale,
    /// a refusal that parks the run.
    StaleEvidence,
}

impl AlreadyLanded {
    fn verify_evidence<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
        supplied_regression_from: &[String],
        scope_change: ScopeChange,
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
        let comments = host.get_task_comments(&task.id)?;
        let expected_scope = scope(task, &comments);
        if !already_landed_scope_matches(&evidence.scope, task, &comments, supplied_regression_from)
        {
            let refused = Self::refused(scope_changed(&evidence.scope, &expected_scope));
            return Err(match (scope_change, refused) {
                (ScopeChange::StaleEvidence, OrbitError::Execution(message)) => {
                    OrbitError::DeliveryRefused {
                        refusal: DeliveryRefusal::EvidenceStale,
                        message,
                    }
                }
                (_, refused) => refused,
            });
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
        hints.push_str("; validation[] role must be one of required, expected_failure, excluded, superseded, diagnostic (a workspace-required command is covered by a passed, exit-0 entry labelled required or diagnostic)");
    }
    let unknown_outcome = report["validation"].as_array().is_some_and(|checks| {
        checks.iter().any(|check| {
            check.get("outcome").is_some_and(|outcome| {
                serde_json::from_value::<ValidationOutcome>(outcome.clone()).is_err()
            })
        })
    });
    if unknown_outcome {
        hints.push_str("; validation[] outcome must be one of passed, failed, denied, not_run (use failed, with role expected_failure, for a negative control)");
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
    let is_evidence = |id: &str| !evidence_run.is_empty() && id == evidence_run;
    if is_evidence(run_id) {
        return Ok(true);
    }
    let Some(run) = host.get_job_run(run_id)? else {
        return Ok(false);
    };
    // Matches the current run and at most `RETRY_LINEAGE_MAX_HOPS - 1`
    // ancestors by the id their descendant names, stored or not.
    let mut lineage = RetryLineage::any_job(&run);
    for _ in 1..RETRY_LINEAGE_MAX_HOPS {
        match lineage.step(host)? {
            LineageStep::Ancestor(source, _) if is_evidence(&source) => return Ok(true),
            LineageStep::Ancestor(..) => {}
            LineageStep::Stop(LineageStop::Missing(source)) => return Ok(is_evidence(&source)),
            LineageStep::Stop(_) => return Ok(false),
        }
    }
    Ok(false)
}

/// Check captured validation against the workspace's required commands.
///
/// `evidence.required_commands` is not the requirement. The requirement is
/// `RuntimeHost::required_validation_commands`: the list final validation
/// runs (`workflow.required_validation_commands`, or the owner's captured
/// gates on a claimed leaf). An empty report list is accepted only when that
/// set is empty. Passing, exit-0 evidence covers a command whichever accepted
/// role labelled it. A missing command is refused by name.
fn verify_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    evidence: &Evidence,
    task: &Task,
) -> Result<Vec<Value>, OrbitError> {
    if task.acceptance_criteria.is_empty()
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
    let declared: BTreeSet<_> = evidence.required_commands.iter().collect();
    if declared.len() != evidence.required_commands.len()
        || declared.iter().any(|command| command.trim().is_empty())
    {
        return Err(AlreadyLanded::refused(
            "required_commands must list each command once, without blanks",
        ));
    }
    let required = workspace_required_commands(host);
    let mut covered = BTreeSet::new();
    let mut required_labels = BTreeSet::new();
    let mut logs = Vec::new();
    for check in &evidence.validation {
        let validation = &check.validation;
        match validation.role {
            ValidationRole::Required => {
                if validation.outcome != ValidationOutcome::Passed
                    || validation.command.trim().is_empty()
                    || !required_labels.insert(validation.command.as_str())
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
        if validation.outcome == ValidationOutcome::Passed
            && exit_code == Some(0)
            && required
                .iter()
                .any(|command| command == &validation.command)
        {
            covered.insert(validation.command.as_str());
        }
        logs.push(provenance);
    }
    let missing = required
        .iter()
        .filter(|command| !covered.contains(command.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(AlreadyLanded::refused(missing_required_commands(&missing)));
    }
    Ok(logs)
}

/// `host.required_validation_commands`, trimmed, with blanks and repeats dropped.
fn workspace_required_commands<H: RuntimeHost + ?Sized>(host: &H) -> Vec<String> {
    let mut commands = Vec::new();
    for command in host.required_validation_commands() {
        let command = command.trim();
        if command.is_empty() || commands.iter().any(|seen| seen == command) {
            continue;
        }
        commands.push(command.to_string());
    }
    commands
}

fn missing_required_commands(missing: &[&str]) -> String {
    let listed = missing
        .iter()
        .map(|command| format!("`{command}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if missing.len() == 1 {
        format!("missing required command {listed}")
    } else {
        format!("missing required commands {listed}")
    }
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
