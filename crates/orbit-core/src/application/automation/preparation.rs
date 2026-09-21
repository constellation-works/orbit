//! Authoritative task/source inputs for the shared material fingerprint.

use super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::routines::loader::declared_routine_names;
use orbit_automation::{AutomationError, members::preparation};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::RegisteredTaskResolution;
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::{MemberAttempt, PreparationEligibility};
use serde_json::{Value, json};

/// The landing-branch head commit the material fingerprint is bound to.
/// Tests bind an expected fingerprint to the same revision the evaluator saw.
#[cfg(test)]
pub(crate) fn head_revision(runtime: &OrbitRuntime, branch: &str) -> Result<String, OrbitError> {
    Source::new(&runtime.paths().repo_root)
        .head(branch)
        .map(|(_, revision)| revision.commit)
        .map_err(orbit_automation::automation_error_to_orbit)
}

/// Repository instructions captured from one immutable source revision.
///
/// The serialized form deliberately remains the fingerprint input used before
/// this cache existed, so sharing it changes work performed, not evidence.
#[derive(Clone)]
pub(crate) struct InstructionSnapshot(String);

/// The eligibility the state consumer `consumer` evaluates: the
/// `trigger.state.eligibility` of the routine its key names, read from this
/// workspace's routine catalog [ORB-12745]. Every consumer of an assessment
/// — scheduling, the prepare/apply fingerprint check and promotion — resolves
/// the predicate this way so one definition governs all of them.
///
/// A consumer whose routine no longer exists, or is no longer a state
/// routine, resolves to the default predicate. That is fail-closed: a
/// fingerprint computed under the wrong predicate never matches an accepted
/// assessment, so stale evidence is withheld rather than trusted.
pub(crate) fn consumer_eligibility(
    runtime: &OrbitRuntime,
    consumer: &str,
) -> Result<PreparationEligibility, OrbitError> {
    let Some((_, name)) = consumer.rsplit_once("/routine/") else {
        return Ok(PreparationEligibility::default());
    };
    let Some(path) = declared_routine_names(&runtime.shared_root()).remove(name) else {
        return Ok(PreparationEligibility::default());
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("read routine '{}': {error}", path.display())))?;
    let definition = parse_routine_yaml(&raw)?;
    Ok(definition
        .trigger
        .state
        .map(|trigger| trigger.eligibility)
        .unwrap_or_default())
}

/// The eligibility a task-pilot run evaluates: its state claim's consumer
/// predicate, or the default for an explicit or manual run that carries no
/// claim.
pub(crate) fn claim_eligibility(
    runtime: &OrbitRuntime,
    claim: Option<&MemberAttempt>,
) -> Result<PreparationEligibility, OrbitError> {
    claim
        .map(|claim| consumer_eligibility(runtime, &claim.consumer))
        .unwrap_or_else(|| Ok(PreparationEligibility::default()))
}

pub(crate) fn fingerprint(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    eligibility: &PreparationEligibility,
) -> Result<String, AutomationError> {
    let instructions = instructions(runtime, revision)?;
    fingerprint_with_instructions(runtime, task, revision, &instructions, eligibility)
}

pub(crate) fn instructions(
    runtime: &OrbitRuntime,
    revision: &str,
) -> Result<InstructionSnapshot, AutomationError> {
    let source = Source::new(&runtime.paths().repo_root);

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

pub(crate) fn fingerprint_with_instructions(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    instructions: &InstructionSnapshot,
    eligibility: &PreparationEligibility,
) -> Result<String, AutomationError> {
    let mut dependencies = Vec::new();

    for id in task.dependencies().iter().take(51) {
        if dependencies.len() == 50 {
            return Err(AutomationError::Deferred("dependency_scan_budget".into()));
        }
        match runtime.resolve_dependency_task(id)? {
            // The serialized shape of a resolved dependency is unchanged, so
            // following ownership changes which prerequisites can be read, not
            // the fingerprint of any task that already prepared.
            RegisteredTaskResolution::Resolved(dependency) => {
                dependencies.push(json!({"id": id, "status": dependency.status,
                    "relations": dependency.relations, "criteria": dependency.acceptance_criteria,
                    "description": dependency.description, "plan": dependency.plan,
                    "refs": dependency.external_refs, "pr_status": dependency.pr_status}));
            }
            // Another host's authority. This machine cannot read the body and
            // must not invent one, so the reference is recorded as explicitly
            // unverified rather than resolved or dropped. That matches the
            // readiness contract, which treats a reference it cannot verify
            // here as not blocking rather than as satisfied
            // (`TaskReferenceIndex::is_not_verifiable_here`); preparation
            // never upgrades it to a status.
            RegisteredTaskResolution::ForeignAuthority => {
                dependencies.push(json!({"id": id, "resolution": "not_verifiable_here"}));
            }
            // A prefix this machine does own, with nothing bound to it: the
            // prerequisite is gone, not elsewhere. Fail closed and name it.
            RegisteredTaskResolution::Missing => {
                let reason = format!(
                    "task '{task_id}' depends on '{id}', which no workspace registered on this machine owns; restore that task, or remove the dependency, before preparing '{task_id}'",
                    task_id = task.id
                );
                return Err(OrbitError::InvalidInput(reason).into());
            }
        }
    }

    dependencies.sort_by_key(|value| value["id"].as_str().unwrap_or_default().to_string());

    // The crew a task would actually run under is part of its material input.
    let assignment = runtime.resolve_crew_for_task(None, task.crew.as_deref())?;
    dependencies.push(json!({"effective_assignment": {"crew": assignment.name,
        "model": assignment.assignment.model, "provider": assignment.assignment.provider}}));

    preparation::fingerprint(
        task,
        revision,
        &Value::Array(dependencies),
        &instructions.0,
        eligibility,
    )
}
