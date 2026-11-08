use std::path::PathBuf;

#[cfg(doc)]
use orbit_types::workflow::activity_job::ActivityV2;
use orbit_types::workflow::{JobV2, JobV2Step, JobV2StepBody, LoopBlock, TargetRef, TargetStep};

use super::{
    ACTIVITY_REF_PREFIX, AUTO_TASK_REF_PREFIX, ResolveError, SCRIPT_REF_PREFIX, V2ActivityCatalog,
};

/// Walk `job` and rewrite every `activity:` [`JobV2StepBody::TargetRef`]
/// into a [`JobV2StepBody::Target`] using the named [`ActivityV2`] from
/// `catalog`. A ref with an unknown name is a hard error; silently
/// succeeding would leave a `TargetRef` lurking past dispatch where the
/// executor would ignore it.
///
/// A `script:` ref is a load error here: resolving one needs the
/// workspace's script catalog ([`resolve_job_target_refs_with_scripts`]),
/// and a caller without one must not silently admit a script step.
pub fn resolve_job_target_refs(
    job: &mut JobV2,
    catalog: &V2ActivityCatalog,
) -> Result<(), ResolveError> {
    let scripts =
        super::super::script_catalog::V2ScriptCatalog::new(PathBuf::new(), PathBuf::new());
    resolve_job_target_refs_with_scripts(job, catalog, &scripts)
}

/// [`resolve_job_target_refs`], plus `script:` refs: each one is checked
/// against the workspace's script catalog (whose loader has already
/// validated the manifest and resolved its entry inside the workspace),
/// and a missing script is a load error naming the manifest file it
/// would live at. A resolved script ref is *retained* as a `TargetRef`:
/// the host re-reads and snapshots the script, checks its grant and runs
/// it at dispatch, so a script edited after load is still caught.
pub fn resolve_job_target_refs_with_scripts(
    job: &mut JobV2,
    catalog: &V2ActivityCatalog,
    scripts: &super::super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    job.resolved_recovery_activity = match job.recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job recovery_activity: ".to_string(),
                    || ResolveError::RecoveryActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };
    job.resolved_failure_activity = match job.failure_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job failure_activity: ".to_string(),
                    || ResolveError::FailureActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };
    job.resolved_final_recovery_activity = match job.final_recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || "job final_recovery_activity: ".to_string(),
                    || ResolveError::FinalRecoveryActivityNotInCatalog {
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };

    for step in &mut job.steps {
        resolve_step(step, catalog, scripts)?;
    }
    Ok(())
}

fn resolve_step(
    step: &mut JobV2Step,
    catalog: &V2ActivityCatalog,
    scripts: &super::super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    step.resolved_recovery_activity = match step.recovery_activity.as_deref() {
        Some(name) => Some(
            catalog
                .resolve_activity(
                    name,
                    || format!("step `{}`: recovery_activity: ", step.id),
                    || ResolveError::StepRecoveryActivityNotInCatalog {
                        step_id: step.id.clone(),
                        name: name.to_string(),
                    },
                )?
                .clone(),
        ),
        None => None,
    };

    match &mut step.body {
        JobV2StepBody::Target(_) => Ok(()),
        JobV2StepBody::TargetRef(reference) if reference.target.starts_with(SCRIPT_REF_PREFIX) => {
            resolve_script_ref(&step.id, &reference.target, scripts)
        }
        // An auto-task ref is retained: Core resolves the named definition
        // for inspection, and `validate_job` refuses to run it.
        JobV2StepBody::TargetRef(reference)
            if reference.target.starts_with(AUTO_TASK_REF_PREFIX) =>
        {
            Ok(())
        }
        JobV2StepBody::TargetRef(_) => {
            // Swap the body out so we can own the ref without cloning; the
            // replacement is a throwaway `Target` that we immediately
            // overwrite with the resolved one.
            let old = std::mem::replace(
                &mut step.body,
                JobV2StepBody::TargetRef(TargetRef {
                    target: String::new(),
                    default_input: None,
                    timeout_seconds: 0,
                    session: None,
                }),
            );
            let JobV2StepBody::TargetRef(r) = old else {
                unreachable!("checked above");
            };
            let resolved = resolve_ref(&step.id, r, catalog)?;
            step.body = JobV2StepBody::Target(resolved);
            Ok(())
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &mut parallel.branches {
                resolve_step(branch, catalog, scripts)?;
            }
            Ok(())
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            resolve_step(&mut fan_out.worker, catalog, scripts)
        }
        JobV2StepBody::Loop { loop_ } => resolve_loop(loop_, catalog, scripts),
    }
}

fn resolve_loop(
    block: &mut LoopBlock,
    catalog: &V2ActivityCatalog,
    scripts: &super::super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    for step in &mut block.steps {
        resolve_step(step, catalog, scripts)?;
    }
    Ok(())
}

/// Check one `script:<name>` ref against the script catalog. The catalog
/// records invalid manifests as unavailable, with their diagnostic.
fn resolve_script_ref(
    step_id: &str,
    target: &str,
    scripts: &super::super::script_catalog::V2ScriptCatalog,
) -> Result<(), ResolveError> {
    let name = target.strip_prefix(SCRIPT_REF_PREFIX).unwrap_or(target);
    if let Some(reason) = scripts.unavailable_script(name) {
        return Err(ResolveError::ScriptUnavailable {
            step_id: step_id.to_string(),
            name: name.to_string(),
            reason: reason.to_string(),
        });
    }
    if scripts.get(name).is_some() {
        return Ok(());
    }
    Err(ResolveError::ScriptNotInCatalog {
        step_id: step_id.to_string(),
        name: name.to_string(),
        manifest: scripts.manifest_path_for(name),
    })
}

fn resolve_ref(
    step_id: &str,
    r: TargetRef,
    catalog: &V2ActivityCatalog,
) -> Result<TargetStep, ResolveError> {
    let name =
        r.target
            .strip_prefix(ACTIVITY_REF_PREFIX)
            .ok_or_else(|| ResolveError::UnknownRefKind {
                step_id: step_id.to_string(),
                target: r.target.clone(),
            })?;
    let activity = catalog.resolve_activity(
        name,
        || format!("step `{step_id}`: "),
        || ResolveError::ActivityNotInCatalog {
            step_id: step_id.to_string(),
            name: name.to_string(),
        },
    )?;
    Ok(TargetStep {
        spec: activity.spec.clone(),
        activity_name: Some(name.to_string()),
        input_schema_json: Some(activity.input_schema_json.clone()),
        fs_profile: activity.fs_profile.clone(),
        default_input: r.default_input,
        timeout_seconds: r.timeout_seconds,
        session: r.session,
    })
}
