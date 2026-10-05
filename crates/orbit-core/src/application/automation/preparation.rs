//! Authoritative task/source inputs for the shared material fingerprint.

use super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::AutomationError;
use orbit_automation::members::preparation::{self, MaterialEvidence};
use orbit_automation::routines::loader::declared_routine_names;
use orbit_common::OrbitError;
use orbit_common::fs::selector::Selector;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_store::RegisteredTaskResolution;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::Task;
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::automation::members::{
    MaterialField, MemberAttempt, PreparationEligibility, PreparationPolicy, SourceSensitivity,
    StateTrigger,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Repository instructions captured from one immutable source revision.
///
/// The serialized form deliberately remains the fingerprint input used before
/// this cache existed, so sharing it changes work performed, not evidence.
#[derive(Clone)]
pub(crate) struct InstructionSnapshot(String);

/// The policy a `preparation_eligible` consumer evaluates: its trigger's
/// eligibility, and its freshness resolved routine > `config.toml` > built-in
/// default per key [ORB-12745, ORB-13638]. A run with no trigger evaluates
/// the default eligibility under the configured freshness.
pub(crate) fn resolve_policy(
    runtime: &OrbitRuntime,
    trigger: Option<&StateTrigger>,
) -> PreparationPolicy {
    let configured = runtime.task_pilot_freshness();
    match trigger {
        Some(trigger) => PreparationPolicy {
            eligibility: trigger.eligibility.clone(),
            freshness: configured.overridden_by(&trigger.freshness),
        },
        None => PreparationPolicy {
            eligibility: PreparationEligibility::default(),
            freshness: configured.normalized(),
        },
    }
}

/// The policy the state consumer `consumer` evaluates, from the
/// `trigger.state` of the routine its key names in this workspace's routine
/// catalog. Every consumer of an assessment — scheduling, the prepare/apply
/// fingerprint check and promotion — resolves it this way so one definition
/// governs all of them.
///
/// A consumer whose routine no longer exists, or is no longer a state
/// routine, resolves as if it had no trigger. That is fail-closed: a
/// fingerprint computed under the wrong policy never matches an accepted
/// assessment, so stale evidence is withheld rather than trusted.
pub(crate) fn consumer_policy(
    runtime: &OrbitRuntime,
    consumer: &str,
) -> Result<PreparationPolicy, OrbitError> {
    let Some((_, name)) = consumer.rsplit_once("/routine/") else {
        return Ok(resolve_policy(runtime, None));
    };
    let Some(path) = declared_routine_names(&runtime.shared_root()).remove(name) else {
        return Ok(resolve_policy(runtime, None));
    };
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| OrbitError::Io(format!("read routine '{}': {error}", path.display())))?;
    let definition = parse_routine_yaml(&raw)?;
    Ok(resolve_policy(runtime, definition.trigger.state.as_ref()))
}

/// The policy a task-pilot run evaluates: its state claim's consumer policy,
/// or the unrouted one for an explicit or manual run that carries no claim.
pub(crate) fn claim_policy(
    runtime: &OrbitRuntime,
    claim: Option<&MemberAttempt>,
) -> Result<PreparationPolicy, OrbitError> {
    claim
        .map(|claim| consumer_policy(runtime, &claim.consumer))
        .unwrap_or_else(|| Ok(resolve_policy(runtime, None)))
}

pub(crate) fn fingerprint(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    policy: &PreparationPolicy,
) -> Result<String, AutomationError> {
    fingerprint_with_instructions(
        runtime,
        task,
        revision,
        &|revision| instructions(runtime, revision),
        policy,
    )
}

/// Task-pilot can assess a local workspace without a Git source snapshot.
/// Keep its no-target freshness check on the same material contract while
/// recording explicitly that no immutable source or instructions were pinned.
pub(crate) fn pilot_fingerprint(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: Option<&str>,
    policy: &PreparationPolicy,
) -> Result<String, AutomationError> {
    if let Some(revision) = revision {
        return fingerprint(runtime, task, revision, policy);
    }
    let evidence = evidence(
        runtime,
        task,
        None,
        &|_| Ok(InstructionSnapshot("[]".into())),
        policy,
    )?;
    preparation::fingerprint(task, &evidence, policy)
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

/// Reads instructions through `instructions` — so a caller can share one
/// snapshot per revision — and only when the policy makes them material.
pub(crate) fn fingerprint_with_instructions(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    instructions: &dyn Fn(&str) -> Result<InstructionSnapshot, AutomationError>,
    policy: &PreparationPolicy,
) -> Result<String, AutomationError> {
    let evidence = evidence(runtime, task, Some(revision), instructions, policy)?;
    preparation::fingerprint(task, &evidence, policy)
}

/// Both task-pilot hashes and the per-field digests behind a `material_changed`
/// refusal, from one instruction and dependency read. The hashes differ only
/// in the normalized status fields even when a dependency is updated
/// concurrently with preparation.
pub(crate) struct PilotFingerprints {
    pub(crate) material: String,
    pub(crate) status_neutral: String,
    pub(crate) components: BTreeMap<String, String>,
}

pub(crate) fn fingerprints(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    policy: &PreparationPolicy,
) -> Result<PilotFingerprints, AutomationError> {
    let evidence = read_evidence(runtime, task, revision, policy)?;
    Ok(PilotFingerprints {
        material: preparation::fingerprint(task, &evidence, policy)?,
        status_neutral: preparation::fingerprint_ignoring_status(task, &evidence, policy)?,
        components: preparation::component_digests(task, &evidence, policy)?,
    })
}

/// Re-read the freshness components at refusal time. Apply uses this only
/// when the status-neutral retry cannot absorb a material drift.
pub(crate) fn component_digests(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    policy: &PreparationPolicy,
) -> Result<BTreeMap<String, String>, AutomationError> {
    let evidence = read_evidence(runtime, task, revision, policy)?;
    preparation::component_digests(task, &evidence, policy)
}

fn read_evidence(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    policy: &PreparationPolicy,
) -> Result<MaterialEvidence, AutomationError> {
    evidence(
        runtime,
        task,
        Some(revision),
        &|revision| instructions(runtime, revision),
        policy,
    )
}

/// The `material_v1` fingerprint of `task` as an assessment pinned at
/// `revision` would have certified it, for carrying such an assessment
/// forward across the contract change [ORB-13638].
pub(crate) fn legacy_fingerprint(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
    instructions: &InstructionSnapshot,
    eligibility: &PreparationEligibility,
) -> Result<String, AutomationError> {
    let mut dependencies = dependency_evidence(runtime, task)?;
    dependencies.push(assignment_evidence(runtime, task)?);
    preparation::legacy_fingerprint(
        task,
        revision,
        &Value::Array(dependencies),
        &instructions.0,
        eligibility,
    )
}

/// Read only the evidence `policy` makes material: an unread input can
/// neither invalidate an assessment nor cost a git or registry lookup.
fn evidence(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: Option<&str>,
    instructions: &dyn Fn(&str) -> Result<InstructionSnapshot, AutomationError>,
    policy: &PreparationPolicy,
) -> Result<MaterialEvidence, AutomationError> {
    let freshness = &policy.freshness;
    let mut evidence = MaterialEvidence::default();
    if freshness.includes(MaterialField::Dependencies) {
        evidence.dependencies = Value::Array(dependency_evidence(runtime, task)?);
    }
    if freshness.includes(MaterialField::Crew) {
        evidence.assignment = assignment_evidence(runtime, task)?;
    }
    if freshness.includes(MaterialField::Instructions) {
        let snapshot = match revision {
            Some(revision) => instructions(revision)?,
            None => InstructionSnapshot("[]".into()),
        };
        evidence.instructions = json!(snapshot.0);
    }
    evidence.source = match (freshness.source_sensitivity, revision) {
        (SourceSensitivity::Ignore, _) => Value::Null,
        (_, None) => json!("no_git_source"),
        (SourceSensitivity::Any, Some(revision)) => json!(revision),
        (SourceSensitivity::ContextFiles, Some(revision)) => {
            selector_objects(runtime, revision, &task.context_files)?
        }
    };
    Ok(evidence)
}

/// The git object id each selector path names at `revision`, `null` where the
/// path does not exist there [ORB-13638]. A directory's tree id changes with
/// anything beneath it, so the head going stale for a selector is exactly a
/// commit touching a path under it (`file:`/`dir:` by prefix, `symbol:`
/// through its file). Module and command selectors name no repository path.
fn selector_objects(
    runtime: &OrbitRuntime,
    revision: &str,
    selectors: &[String],
) -> Result<Value, AutomationError> {
    let mut paths = BTreeSet::new();
    let mut root_selected = false;
    for selector in selectors {
        let Ok(parsed) = selector.parse::<Selector>() else {
            continue;
        };
        let path = match parsed {
            Selector::Dir { path } if path == "." => {
                root_selected = true;
                continue;
            }
            Selector::Dir { path } | Selector::File { path } | Selector::Symbol { path, .. } => {
                path
            }
            _ => continue,
        };
        if !path.is_empty()
            && path != "."
            && !path.starts_with('/')
            && !path.starts_with("../")
            && path != ".."
        {
            paths.insert(path);
        }
    }
    if root_selected {
        // The root tree covers every narrower selector, including paths added
        // after the assessment. Git does not list `.` as an ls-tree path.
        let tree = Source::new(&runtime.paths().repo_root).git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{tree}}"),
        ])?;
        return Ok(json!({".": tree}));
    }
    if paths.len() > 50 {
        return Err(AutomationError::Deferred("selector_scan_budget".into()));
    }
    // A path under another selected directory is already covered by that
    // directory's tree id; listing both would make git expand the directory.
    let covered = |path: &String| {
        paths
            .iter()
            .any(|other| other != path && path.starts_with(&format!("{other}/")))
    };
    let mut objects = paths
        .iter()
        .filter(|path| !covered(path))
        .map(|path| (path.clone(), Value::Null))
        .collect::<BTreeMap<_, _>>();
    if objects.is_empty() {
        return Ok(json!(objects));
    }
    let mut args = vec![
        "--literal-pathspecs",
        "ls-tree",
        "-z",
        "--full-tree",
        revision,
        "--",
    ];
    args.extend(objects.keys().map(String::as_str));
    let listing = Source::new(&runtime.paths().repo_root).git(&args)?;
    for entry in listing.split('\0') {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        if let (Some(slot), Some(object)) = (objects.get_mut(path), meta.split(' ').nth(2)) {
            *slot = json!(object);
        }
    }
    Ok(json!(objects))
}

fn dependency_evidence(runtime: &OrbitRuntime, task: &Task) -> Result<Vec<Value>, AutomationError> {
    let mut dependencies = Vec::new();
    let mut resolved = Vec::new();

    for id in task.dependencies().iter().take(51) {
        if dependencies.len() + resolved.len() == 50 {
            return Err(AutomationError::Deferred("dependency_scan_budget".into()));
        }
        match runtime.resolve_dependency_task(id)? {
            RegisteredTaskResolution::Resolved(dependency) => resolved.push(dependency),
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

    // Report status under the rule readiness and dispatch apply, so a
    // dependency archived after it reached `done` reads as `done` here too.
    let mut status_by_id = resolved
        .iter()
        .map(|dependency| (dependency.id.clone(), dependency.status))
        .collect::<BTreeMap<_, _>>();
    runtime.satisfy_completed_archived_dependencies(&mut status_by_id, [task]);
    // The serialized shape of a resolved dependency is unchanged, so following
    // ownership changes which prerequisites can be read, not the fingerprint
    // of any task that already prepared.
    for dependency in resolved {
        let status = status_by_id
            .get(&dependency.id)
            .copied()
            .unwrap_or(dependency.status);
        dependencies.push(json!({"id": dependency.id, "status": status,
            "relations": dependency.relations, "criteria": dependency.acceptance_criteria,
            "description": dependency.description, "plan": dependency.plan,
            "refs": dependency.external_refs, "pr_status": dependency.pr_status}));
    }

    dependencies.sort_by_key(|value| value["id"].as_str().unwrap_or_default().to_string());
    Ok(dependencies)
}

/// The crew a task would actually run under. Looked up, not dispatch-checked:
/// enabling or disabling the crew does not change what the task would be
/// prepared against, and dispatch refuses a disabled crew on its own.
fn assignment_evidence(runtime: &OrbitRuntime, task: &Task) -> Result<Value, AutomationError> {
    let assignment = runtime.lookup_crew_for_task(None, task.crew.as_deref())?;
    Ok(json!({"effective_assignment": {"crew": assignment.name,
        "model": assignment.assignment.model, "provider": assignment.assignment.provider}}))
}

/// Tasks held by successful preparation checkpoints of active pilot runs.
/// Reconcile stale owners before treating their checkpoints as holds; a
/// terminal run no longer prevents another pilot from preparing its tasks.
pub(crate) fn active_task_pilot_preparations(
    runtime: &OrbitRuntime,
) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
    let workspace_root = runtime.paths().repo_root.canonicalize()?;
    let mut prepared_by_task = BTreeMap::<String, BTreeSet<String>>::new();
    for state in [
        JobRunState::Pending,
        JobRunState::Running,
        JobRunState::Retrying,
    ] {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some("task_pilot_pipeline".to_string()),
                state: Some(state),
                terminal_only: false,
                created_since: None,
                limit: None,
                ..Default::default()
            })?;
        for run in runs {
            let run = runtime.show_job_run(&run.run_id)?;
            if run.state.is_terminal() {
                continue;
            }
            let Some(state) = runtime.read_run_state(&run.run_id)? else {
                continue;
            };
            let Some(task_ids) = state.step_outputs.iter().find_map(|(step_index, output)| {
                (state.step_states.get(step_index) == Some(&JobRunState::Success))
                    .then(|| prepared_task_ids(output, &workspace_root))
                    .flatten()
            }) else {
                continue;
            };
            for task_id in task_ids {
                prepared_by_task
                    .entry(task_id)
                    .or_default()
                    .insert(run.run_id.clone());
            }
        }
    }
    Ok(prepared_by_task)
}

fn prepared_task_ids(output: &Value, workspace_root: &Path) -> Option<Vec<String>> {
    let object = output.as_object()?;
    let prepared_workspace = object.get("workspace_path")?.as_str()?;
    if Path::new(prepared_workspace) != workspace_root
        || !object.get("partitions")?.is_array()
        || !object.get("tasks")?.is_array()
        || !matches!(object.get("mode")?.as_str(), Some("automatic" | "explicit"))
    {
        return None;
    }
    object
        .get("task_ids")?
        .as_array()?
        .iter()
        .map(|task_id| task_id.as_str().map(ToOwned::to_owned))
        .collect()
}
