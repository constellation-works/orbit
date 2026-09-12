//! Authoritative task/source inputs for the shared material fingerprint.

use crate::host::AutomationHost;
use crate::source::Source;
use crate::{AutomationError, automation_error_to_orbit, members::preparation};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::MemberAssessment;
use serde_json::{Value, json};

/// Repository instructions captured from one immutable source revision.
///
/// The serialized form deliberately remains the fingerprint input used before
/// this cache existed, so sharing it changes work performed, not evidence.
#[derive(Clone)]
pub struct InstructionSnapshot(String);

/// Bound on the consumer states one lookup reads.
const MAX_CONSUMER_STATES: usize = 50;

/// The landing-branch head commit the material fingerprint is bound to.
pub fn head_revision<H: AutomationHost>(host: &H, branch: &str) -> Result<String, OrbitError> {
    Source::new(host.repo_root())
        .head(branch)
        .map(|(_, revision)| revision.commit)
        .map_err(automation_error_to_orbit)
}

/// The accepted preparation assessment for `task_id`, from this machine's
/// state consumers for this workspace. Read-only: callers consume the record
/// the shared evaluator accepted and never derive readiness themselves
/// [ORB-11332]. `None` without a registered machine identity, because no
/// consumer can have been evaluated here.
pub fn accepted_assessment<H: AutomationHost>(
    host: &H,
    task_id: &str,
) -> Result<Option<MemberAssessment>, OrbitError> {
    let Some(machine) = host.machine_identity() else {
        return Ok(None);
    };
    let prefix = format!("{machine}/{}/routine/", host.workspace_id()?);
    let states = host
        .automation_store()?
        .automation_states(&prefix, MAX_CONSUMER_STATES)?;
    Ok(states
        .into_iter()
        .filter_map(|state| state.members)
        .filter_map(|members| members.assessed.get(task_id).cloned())
        .max_by_key(|assessment| assessment.receipt_id.clone()))
}

/// The material fingerprint of one task at `revision`, including repository
/// instructions read from that pinned tree.
pub fn fingerprint<H: AutomationHost>(
    host: &H,
    task: &Task,
    revision: &str,
) -> Result<String, AutomationError> {
    let instructions = instructions(host, revision)?;
    fingerprint_with_instructions(host, task, revision, &instructions)
}

/// Repository instructions captured from one immutable source revision.
pub fn instructions<H: AutomationHost>(
    host: &H,
    revision: &str,
) -> Result<InstructionSnapshot, AutomationError> {
    let source = Source::new(host.repo_root());

    // The pinned tree includes every repository instruction, including nested
    // selectors. Dirty local instructions cannot certify this pinned source.
    let paths = source.git(&[
        "ls-tree",
        "-r",
        "--name-only",
        revision,
        "--",
        "**/AGENTS.md",
        "**/CLAUDE.md",
    ])?;

    let mut instructions = Vec::new();

    for path in paths
        .lines()
        .filter(|path| matches!(path.rsplit('/').next(), Some("AGENTS.md" | "CLAUDE.md")))
    {
        if instructions.len() >= 50 {
            return Err(AutomationError::Deferred("instruction_scan_budget".into()));
        }
        instructions.push((
            path.to_string(),
            source.git(&["show", &format!("{revision}:{path}")])?,
        ));
    }

    Ok(InstructionSnapshot(
        serde_json::to_string(&instructions)
            .map_err(|error| AutomationError::Evidence(error.to_string()))?,
    ))
}

/// As [`fingerprint`], reusing an instruction snapshot already read for this
/// revision.
pub fn fingerprint_with_instructions<H: AutomationHost>(
    host: &H,
    task: &Task,
    revision: &str,
    instructions: &InstructionSnapshot,
) -> Result<String, AutomationError> {
    let mut dependencies = Vec::new();

    for id in task.dependencies().iter().take(51) {
        if dependencies.len() == 50 {
            return Err(AutomationError::Deferred("dependency_scan_budget".into()));
        }
        let dependency = host.get_task(id)?;
        dependencies.push(json!({"id": id, "status": dependency.status,
            "relations": dependency.relations, "criteria": dependency.acceptance_criteria,
            "description": dependency.description, "plan": dependency.plan,
            "refs": dependency.external_refs, "pr_status": dependency.pr_status}));
    }

    dependencies.sort_by_key(|value| value["id"].as_str().unwrap_or_default().to_string());

    // The crew a task would actually run under is part of its material input.
    let assignment = host.effective_crew(task.crew.as_deref())?;
    dependencies.push(json!({"effective_assignment": {"crew": assignment.name,
        "model": assignment.assignment.model, "provider": assignment.assignment.provider}}));

    preparation::fingerprint(task, revision, &Value::Array(dependencies), &instructions.0)
}
