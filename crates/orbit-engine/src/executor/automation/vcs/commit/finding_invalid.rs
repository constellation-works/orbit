//! An implementer's report that the review finding it picked up does not
//! exist at its pinned HEAD. The run changed nothing and ran no workspace
//! gates; its `path:line` citations at that HEAD are its evidence. Once Git
//! state and every citation check out, the host records a REJECTED finding
//! verdict and moves the task to `rejected`, and nothing lands.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::handoff::FindingInvalidReport;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::git::{git_command_success, git_output_raw};
use super::already_landed::run_in_lineage;
use super::checkpoint::pinned_object_id;
use super::clean_tree::{Specification, Verifier};

pub(super) const ARTIFACT: &str = "finding-invalid.json";

/// Checkpoint decision for a verified finding-invalid report.
pub(super) const DECISION: &str = "verified_finding_invalid";

pub(super) struct FindingInvalid;

impl Verifier for FindingInvalid {
    const SPEC: Specification = Specification {
        decision: DECISION,
        evidence_key: "finding_invalid",
        refusal_prefix: "finding_invalid_unverified",
        head_changed: "tested HEAD is not the pinned current HEAD; cite the finding at it",
        dirty_tree: "worktree is not clean; a finding-invalid report changes nothing",
        missing_artifact_suffix: "",
        task_count: "a finding-invalid report covers exactly one task",
        evidence_changed: "accepted finding-invalid report changed; rerun the commit step",
    };

    fn verify_at_revision<H: RuntimeHost + ?Sized>(
        host: &H,
        task: &Task,
        workspace: &Path,
        run_id: &str,
        tested_head: &str,
    ) -> Result<Value, OrbitError> {
        let report = read_report(host, task)?
            .ok_or_else(|| Self::refused(format!("missing task artifact {ARTIFACT}")))?;
        if report.schema_version != 1 || report.task_id != task.id {
            return Err(Self::refused("report must identify this task"));
        }
        if report.reasoning.trim().is_empty() {
            return Err(Self::refused(
                "reasoning must state why the defect does not exist",
            ));
        }
        if !run_in_lineage(host, run_id, &report.run_id)? {
            return Err(Self::refused(
                "report belongs to a different run; judge the finding in this run",
            ));
        }
        let head = pinned_object_id(&report.tested_head)?;
        if head != tested_head {
            return Err(Self::refused("tested HEAD is not the owner-observed base"));
        }
        if report.evidence.is_empty() {
            return Err(Self::refused(
                "evidence must cite at least one `path:line` at the tested HEAD",
            ));
        }
        for citation in &report.evidence {
            if citation.note.trim().is_empty() {
                return Err(Self::refused(format!(
                    "evidence {:?} must say what the line shows",
                    citation.location
                )));
            }
            verify_citation(workspace, &head, &citation.location)?;
        }
        Ok(json!({
            "phase": "commit",
            "decision": DECISION,
            "committed": false,
            "skipped_no_diff_expected": true,
            "task_id": task.id,
            "job_run_id": run_id,
            "base_sha": head,
            "finding_invalid": report,
        }))
    }
}

/// Whether the task's report claims this run: it names this run or one in
/// its retry lineage. An unreadable report claims this run only when its
/// `run_id` still decodes and names this run or its lineage; a report from
/// an earlier, unrelated run — or text that names no run — never shadows
/// this run's no-diff or already-landed evidence. A report that does name
/// this run still fails verification, and the refusal names the parse error.
pub(super) fn claims_run<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    run_id: &str,
) -> Result<bool, OrbitError> {
    let artifact = match host.get_task_artifact(&task.id, ARTIFACT) {
        Ok(artifact) => artifact,
        Err(OrbitError::Execution(_)) => return Ok(true),
        Err(error) => return Err(error),
    };
    let Some(artifact) = artifact else {
        return Ok(false);
    };
    match serde_json::from_slice::<FindingInvalidReport>(&artifact.content) {
        Ok(report) => run_in_lineage(host, run_id, &report.run_id),
        Err(_) => {
            let claimed = serde_json::from_slice::<Value>(&artifact.content)
                .ok()
                .and_then(|value| {
                    value
                        .get("run_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            if claimed.is_empty() {
                return Ok(false);
            }
            run_in_lineage(host, run_id, &claimed)
        }
    }
}

/// Verify the report against the clean pinned HEAD, then have the host
/// record the REJECTED verdict. The checkpoint skips every delivery step.
pub(super) fn reject<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    workspace: &Path,
    run_id: &str,
    base_sha: &str,
    base_branch: &str,
) -> Result<Value, OrbitError> {
    let checkpoint = FindingInvalid::verify(host, task, workspace, run_id, base_sha)?;
    let report: FindingInvalidReport =
        serde_json::from_value(checkpoint["finding_invalid"].clone())
            .map_err(|error| FindingInvalid::refused(format!("invalid {ARTIFACT}: {error}")))?;
    host.reject_invalid_finding(&report, base_branch)
        .map_err(|error| FindingInvalid::refused(format!("verdict not recorded: {error}")))?;
    Ok(checkpoint)
}

/// A claimed leaf cannot write the owner's task, so it cannot reject it.
pub(super) fn claimed_refusal() -> OrbitError {
    FindingInvalid::refused(format!(
        "a claimed run cannot hand off {ARTIFACT}; return `result.blocker` with kind \
         `finding_invalid` and the `path:line` evidence so the owner rejects the finding"
    ))
}

/// The task's report, `None` when it has none. A report that does not
/// parse is refused.
fn read_report<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
) -> Result<Option<FindingInvalidReport>, OrbitError> {
    let Some(artifact) = host.get_task_artifact(&task.id, ARTIFACT)? else {
        return Ok(None);
    };
    serde_json::from_slice(&artifact.content)
        .map(Some)
        .map_err(|error| FindingInvalid::refused(format!("invalid {ARTIFACT}: {error}")))
}

/// `location` is a repository-relative `path:line` naming a line of a file
/// at `head`.
fn verify_citation(workspace: &Path, head: &str, location: &str) -> Result<(), OrbitError> {
    let malformed = || {
        FindingInvalid::refused(format!(
            "evidence location {location:?} is not a repository-relative `path:line`"
        ))
    };
    let (path, line) = location.trim().rsplit_once(':').ok_or_else(malformed)?;
    let line = line
        .parse::<usize>()
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(malformed)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(malformed());
    }
    let object = format!("{head}:{path}");
    if !git_command_success(workspace, &["cat-file", "-e", &object])? {
        return Err(FindingInvalid::refused(format!(
            "evidence location {location:?} names a path absent at the tested HEAD"
        )));
    }
    if git_output_raw(workspace, &["show", &object])?
        .lines()
        .count()
        < line
    {
        return Err(FindingInvalid::refused(format!(
            "evidence location {location:?} is past the end of its file at the tested HEAD"
        )));
    }
    Ok(())
}
