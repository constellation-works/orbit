#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(
    clippy::expect_used,
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::unwrap_used
)]

//! Name-resolution integration coverage — T20260418-2019.
//!
//! Exercises:
//!   A) `V2ActivityCatalog::load_dir` picks up the four new v2 activities
//!      (`agent_assess_diff`, `agent_apply_fixes`, `promote_agent_main`,
//!      `revert_on_red`) and skips v1 assets silently.
//!   B) `resolve_job_target_refs` rewrites `target: activity:<name>` refs
//!      into inline `TargetStep`s using the catalog.
//!   C) A round-trip through backend resolution + §3.2 loader rejection
//!      works on the resolved job — unknown refs surface a structural
//!      error, not a silent no-op.
//!   D) Loading the new `task_pipeline.yaml` sample produces a job with
//!      `TargetRef`s that point at the bundled v2 activities plus the
//!      not-yet-ported activities (which is the expected partial state of
//!      Phase 4). Only the resolvable refs rewrite; unresolved ones are
//!      reported by the resolver.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^v2_name_resolution::/)'`.

use std::path::PathBuf;

use orbit_engine::activity_job::{
    ResolveError, V2ActivityCatalog, load_job_asset, resolve_job_target_refs,
};
use orbit_engine::{DispatchError, resolve_job_catalog_refs_for_execution, validate_job};
use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, FanInSpec, FanOutBlock, JobKind, JobV2, JobV2Step, JobV2StepBody,
    JoinMode, LoopBlock, ParallelBlock, Provider, TargetRef, TargetStep,
    validate_job_retired_sessions,
};

#[test]
fn name_resolution_regressions() -> Result<(), Box<dyn std::error::Error>> {
    scenario_a_catalog_loads_new_activities()?;
    scenario_b_target_ref_resolves()?;
    scenario_c_unknown_ref_is_structural_error()?;
    scenario_d_pipeline_yaml_partial_resolution()?;
    scenario_e_retired_session_rejection_runs_after_resolution()?;

    Ok(())
}

/// [ORB-13890] A shipped job whose `when:` reads a skippable step's output
/// is refused by `validate_job` at dispatch, failing every run of it; the
/// asset smoke only parses, so validate each shipped job as execution does.
#[test]
fn every_shipped_job_resolves_and_passes_execution_validation()
-> Result<(), Box<dyn std::error::Error>> {
    let catalog = load_reference_catalog()?;
    let mut validated = 0;
    for entry in std::fs::read_dir(repo_root().join("crates/orbit-core/assets/jobs"))? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("yaml") {
            continue;
        }
        let mut job = load_job_asset(&std::fs::read_to_string(&path)?)?.spec;
        resolve_job_catalog_refs_for_execution(&mut job, &catalog)
            .and_then(|()| validate_job(&job))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        validated += 1;
    }
    assert!(validated > 0, "no shipped jobs found");
    Ok(())
}

/// Conditions cannot read an output whose producer can be skipped independently
/// of the reader, including fan-in aliases and inherited guards.
#[test]
fn validate_job_checks_output_producer_guards() {
    let flag = "{{ input.flag }} == true";
    let alias_reader = || validation_target("reader", Some("{{ steps.results.output }} != []"));
    let guarded_fan = || validation_fan("fan", Some(flag), "results");
    let plain_fan = || validation_fan("fan", None, "results");
    let break_expr = "{{ steps.results.output }} != []";
    let mut guarded_worker = plain_fan();
    if let JobV2StepBody::FanOut { fan_out, .. } = &mut guarded_worker.body {
        fan_out.worker.when = Some(flag.to_string());
    }

    for (case, steps, diagnostic_names) in [
        (
            "guarded step output",
            vec![
                validation_target("maybe_run", Some(flag)),
                validation_target("reader", Some("{{ steps.maybe_run.output.done }} == true")),
            ],
            vec!["reader", "maybe_run"],
        ),
        (
            "guarded fan-in alias in when",
            vec![guarded_fan(), alias_reader()],
            vec!["reader", "fan", "results"],
        ),
        (
            "guarded fan-in alias in break_when",
            vec![
                guarded_fan(),
                validation_loop("reader", None, vec![], Some(break_expr)),
            ],
            vec!["reader", "fan", "results"],
        ),
        (
            "fan-in alias inherits a parallel guard",
            vec![
                validation_parallel("outer", Some(flag), vec![plain_fan()]),
                alias_reader(),
            ],
            vec!["reader", "fan", "outer", "results"],
        ),
        (
            "shared loop guard covers when and break_when",
            vec![validation_loop(
                "outer",
                Some(flag),
                vec![plain_fan(), alias_reader()],
                Some(break_expr),
            )],
            vec![],
        ),
        (
            "shared guard cannot cover the fan's own guard",
            vec![validation_loop(
                "outer",
                Some(flag),
                vec![guarded_fan(), alias_reader()],
                None,
            )],
            vec!["reader", "fan", "results"],
        ),
        (
            "container when runs before the fan-in alias exists",
            vec![validation_loop(
                "reader",
                Some(break_expr),
                vec![plain_fan()],
                None,
            )],
            vec!["reader", "fan", "results"],
        ),
        (
            "unguarded fan-in alias",
            vec![plain_fan(), alias_reader()],
            vec![],
        ),
        (
            "guarded worker does not guard collection",
            vec![guarded_worker, alias_reader()],
            vec![],
        ),
        (
            "collect alias may equal its own step id",
            vec![validation_fan("results", None, "results"), alias_reader()],
            vec![],
        ),
        (
            "same-id collect alias retains its guard",
            vec![
                validation_fan("results", Some(flag), "results"),
                alias_reader(),
            ],
            vec!["reader", "results"],
        ),
        (
            "later step id cannot overwrite collect alias guards",
            vec![
                guarded_fan(),
                alias_reader(),
                validation_target("results", None),
            ],
            vec!["reader", "fan", "results"],
        ),
        (
            "later collect alias cannot overwrite step id guards",
            vec![
                validation_target("results", Some(flag)),
                alias_reader(),
                plain_fan(),
            ],
            vec!["reader", "results"],
        ),
        (
            "later collect alias cannot overwrite earlier alias guards",
            vec![
                guarded_fan(),
                alias_reader(),
                validation_fan("later", None, "results"),
            ],
            vec!["reader", "fan", "results"],
        ),
    ] {
        let mut job = synthetic_job_using_ref("noop");
        job.steps = steps;
        if diagnostic_names.is_empty() {
            validate_job(&job).unwrap_or_else(|error| panic!("{case}: {error}"));
        } else {
            let error = validate_job(&job).expect_err(case);
            let DispatchError::JobValidation(message) = error else {
                panic!("{case}: expected JobValidation, got {error:?}");
            };
            for name in diagnostic_names {
                assert!(
                    message.contains(name),
                    "{case}: diagnostic must name {name}: {message}"
                );
            }
        }
    }
}

/// Step IDs remain unique across the entire tree, independently of guards or
/// output aliases, so a later declaration cannot erase an earlier guard chain.
#[test]
fn validate_job_rejects_duplicate_step_ids_across_nested_bodies() {
    let duplicate = || validation_target("duplicate", None);
    for (case, steps) in [
        ("top-level", vec![duplicate(), duplicate()]),
        (
            "guard overwrite regression",
            vec![
                validation_target("duplicate", Some("{{ input.flag }} == true")),
                validation_target("reader", Some("{{ steps.duplicate.output }} == true")),
                duplicate(),
            ],
        ),
        (
            "parallel branches",
            vec![validation_parallel(
                "parallel",
                None,
                vec![duplicate(), duplicate()],
            )],
        ),
        (
            "loop body versus top-level",
            vec![
                validation_loop("loop", None, vec![duplicate()], None),
                duplicate(),
            ],
        ),
        (
            "fan-out worker versus top-level",
            vec![
                validation_fan("fan", None, "results"),
                validation_target("fan_worker", None),
            ],
        ),
        (
            "parent versus nested child",
            vec![validation_loop("duplicate", None, vec![duplicate()], None)],
        ),
    ] {
        let mut job = synthetic_job_using_ref("noop");
        job.steps = steps;
        let error = validate_job(&job).expect_err(case);
        let DispatchError::JobValidation(message) = error else {
            panic!("{case}: expected JobValidation, got {error:?}");
        };
        let id = if case == "fan-out worker versus top-level" {
            "fan_worker"
        } else {
            "duplicate"
        };
        assert!(
            message.contains(id),
            "{case}: diagnostic must name duplicate id {id}: {message}"
        );
        assert!(
            message.contains("duplicate step id"),
            "{case}: reject the duplicate before checking guards: {message}"
        );
    }
}

fn validation_step(id: &str, when: Option<&str>, body: JobV2StepBody) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: when.map(str::to_string),
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body,
    }
}

fn validation_target(id: &str, when: Option<&str>) -> JobV2Step {
    validation_step(
        id,
        when,
        JobV2StepBody::Target(TargetStep {
            spec: stub_deterministic_activity("noop").spec,
            activity_name: None,
            input_schema_json: None,
            fs_profile: None,
            default_input: None,
            timeout_seconds: 0,
            session: None,
        }),
    )
}

fn validation_fan(id: &str, when: Option<&str>, collect: &str) -> JobV2Step {
    validation_step(
        id,
        when,
        JobV2StepBody::FanOut {
            fan_out: FanOutBlock {
                items: "{{ input.items }}".to_string(),
                max_workers: 1,
                worker: Box::new(validation_target(&format!("{id}_worker"), None)),
            },
            fan_in: FanInSpec {
                join: JoinMode::All,
                collect: Some(collect.to_string()),
            },
        },
    )
}

fn validation_loop(
    id: &str,
    when: Option<&str>,
    steps: Vec<JobV2Step>,
    break_when: Option<&str>,
) -> JobV2Step {
    validation_step(
        id,
        when,
        JobV2StepBody::Loop {
            loop_: LoopBlock {
                items: None,
                max_iterations: 1,
                break_when: break_when.map(str::to_string),
                steps,
            },
        },
    )
}

fn validation_parallel(id: &str, when: Option<&str>, branches: Vec<JobV2Step>) -> JobV2Step {
    validation_step(
        id,
        when,
        JobV2StepBody::Parallel {
            parallel: ParallelBlock {
                join: JoinMode::All,
                branches,
            },
        },
    )
}

fn scenario_a_catalog_loads_new_activities() -> Result<(), Box<dyn std::error::Error>> {
    println!("  A) catalog retains supported examples and excludes retired promotion");
    let mut catalog = V2ActivityCatalog::new();
    let dir = repo_root().join("crates/orbit-core/assets/activities");
    catalog.load_dir(&dir)?;

    for name in ["agent_assess_diff", "agent_apply_fixes", "revert_on_red"] {
        assert!(
            catalog.get(name).is_some(),
            "catalog missing new activity `{}` (present: {:?})",
            name,
            catalog.names().collect::<Vec<_>>()
        );
    }
    let assessor = catalog.get("agent_assess_diff").expect("present");
    let ActivityV2Spec::AgentLoop(spec) = &assessor.spec else {
        panic!("agent_assess_diff should be agent_loop");
    };
    assert_eq!(spec.provider, Provider::Claude);

    let fixer = catalog.get("agent_apply_fixes").expect("present");
    assert!(matches!(&fixer.spec, ActivityV2Spec::AgentLoop(_)));

    let revert = catalog.get("revert_on_red").expect("present");
    assert!(matches!(&revert.spec, ActivityV2Spec::Deterministic(_)));
    assert!(
        catalog.get("promote_agent_main").is_none(),
        "retired promotion must not remain in the activity catalog"
    );

    println!(
        "    loaded {} activities total (filter selects retained surface)",
        catalog.len()
    );
    Ok(())
}

fn scenario_b_target_ref_resolves() -> Result<(), Box<dyn std::error::Error>> {
    println!("  B) resolve_job_target_refs rewrites named refs to inline specs");
    let catalog = load_reference_catalog()?;

    let mut job = synthetic_job_using_ref("agent_assess_diff");
    resolve_job_target_refs(&mut job, &catalog)?;

    // After resolution the body must be an inline Target, not a TargetRef.
    let JobV2StepBody::Target(t) = &job.steps[0].body else {
        panic!(
            "expected Target after resolution, got {:?}",
            job.steps[0].body
        );
    };
    assert!(matches!(&t.spec, ActivityV2Spec::AgentLoop(_)));
    assert_eq!(t.session.as_deref(), Some("assessor"));
    println!("    resolved ref → inline Target carrying its session binding");
    Ok(())
}

fn scenario_c_unknown_ref_is_structural_error() -> Result<(), Box<dyn std::error::Error>> {
    println!("  C) unknown activity name surfaces ResolveError structurally");
    let catalog = load_reference_catalog()?;
    let mut job = synthetic_job_using_ref("does_not_exist");
    let err = resolve_job_target_refs(&mut job, &catalog).expect_err("expected error");
    match err {
        ResolveError::ActivityNotInCatalog { step_id, name } => {
            assert_eq!(step_id, "the_step");
            assert_eq!(name, "does_not_exist");
            println!("    got ActivityNotInCatalog for `{}`", name);
        }
        other => panic!("wrong error: {other:?}"),
    }
    Ok(())
}

fn scenario_d_pipeline_yaml_partial_resolution() -> Result<(), Box<dyn std::error::Error>> {
    println!("  D) task_pipeline.yaml exposes retired promotion structurally");
    let yaml_path = repo_root().join("crates/orbit-core/assets/jobs/examples/task_pipeline.yaml");
    let yaml = std::fs::read_to_string(&yaml_path)?;
    let asset = load_job_asset(&yaml)?;

    // Confirm the parse produced TargetRefs (not inline specs) throughout.
    let ref_count = count_target_refs(&asset.spec);
    assert!(
        ref_count >= 8,
        "expected at least 8 TargetRefs in pipeline, got {}",
        ref_count
    );

    // The post-sweep catalog resolves the live pipeline surface but leaves
    // retired promotion unresolved rather than silently treating it as live.
    let catalog = load_reference_catalog()?;
    let mut partial = asset.spec.clone();
    let err = resolve_job_target_refs(&mut partial, &catalog);
    match err {
        Err(ResolveError::ActivityNotInCatalog { name, .. }) => {
            assert_eq!(name, "promote_agent_main");
            println!("    retired promotion remains a structural resolution error");
        }
        Ok(_) => panic!("expected retired promotion to remain unresolved"),
        Err(other) => panic!("wrong error: {other:?}"),
    }

    // A synthetic promotion entry proves all remaining targets resolve; it
    // must never be shipped as a real activity until the action exists.
    let mut catalog_with_stubs = catalog;
    catalog_with_stubs.insert(
        "promote_agent_main",
        stub_deterministic_activity("promote_agent_main"),
    );
    let mut full = asset.spec.clone();
    resolve_job_target_refs(&mut full, &catalog_with_stubs)?;
    assert_eq!(
        count_target_refs(&full),
        0,
        "every TargetRef should be resolved after stubs land"
    );
    println!("    all refs resolve with stubs in place");
    Ok(())
}

fn scenario_e_retired_session_rejection_runs_after_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    println!("  E) retired-session rejection operates on resolved specs");
    let catalog = load_reference_catalog()?;
    let mut job = pipeline_with_assessor_loop();
    resolve_job_target_refs(&mut job, &catalog)?;
    // [ORB-10801] The `session:` binding survives ref resolution, so the
    // load-time validator sees it and refuses the job rather than running a
    // loop whose cross-iteration semantics no longer exist.
    let err = validate_job_retired_sessions(&job, "synthetic").expect_err("expected rejection");
    println!("    resolved assessor session rejected: {err}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn load_reference_catalog() -> Result<V2ActivityCatalog, Box<dyn std::error::Error>> {
    let mut catalog = V2ActivityCatalog::new();
    let dir = repo_root().join("crates/orbit-core/assets/activities");
    catalog.load_dir(&dir)?;
    Ok(catalog)
}

fn synthetic_job_using_ref(target_name: &str) -> JobV2 {
    JobV2 {
        state: JobScheduleState::Enabled,
        owns_task_worktree: false,
        task_delivery: None,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        final_recovery_activity: None,
        resolved_final_recovery_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![JobV2Step {
            id: "the_step".to_string(),
            when: None,
            retry: None,
            recovery_activity: None,
            resolved_recovery_activity: None,
            body: JobV2StepBody::TargetRef(TargetRef {
                target: format!("activity:{}", target_name),
                default_input: None,
                timeout_seconds: 0,
                session: Some("assessor".to_string()),
            }),
        }],
    }
}

fn pipeline_with_assessor_loop() -> JobV2 {
    let assess_step = JobV2Step {
        id: "assess".to_string(),
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::TargetRef(TargetRef {
            target: "activity:agent_assess_diff".to_string(),
            default_input: None,
            timeout_seconds: 0,
            session: Some("assessor".to_string()),
        }),
    };
    JobV2 {
        state: JobScheduleState::Enabled,
        owns_task_worktree: false,
        task_delivery: None,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        final_recovery_activity: None,
        resolved_final_recovery_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![JobV2Step {
            id: "assess_fix".to_string(),
            when: None,
            retry: None,
            recovery_activity: None,
            resolved_recovery_activity: None,
            body: JobV2StepBody::Loop {
                loop_: LoopBlock {
                    items: None,
                    max_iterations: 3,
                    break_when: None,
                    steps: vec![assess_step],
                },
            },
        }],
    }
}

fn stub_deterministic_activity(name: &str) -> ActivityV2 {
    ActivityV2 {
        description: format!("stub for `{name}` — pending v1 port"),
        input_schema_json: serde_json::Value::Null,
        output_schema_json: serde_json::Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(
            orbit_types::workflow::activity_job::DeterministicSpec {
                action: "noop".to_string(),
                config: serde_json::Value::Null,
            },
        ),
    }
}

// Walk a job counting remaining TargetRefs — anything >0 after resolution
// means Phase 4 hasn't finished porting that activity.
fn count_target_refs(job: &JobV2) -> usize {
    fn count_step(step: &JobV2Step) -> usize {
        match &step.body {
            JobV2StepBody::TargetRef(_) => 1,
            JobV2StepBody::Target(_) => 0,
            JobV2StepBody::Parallel { parallel } => parallel.branches.iter().map(count_step).sum(),
            JobV2StepBody::FanOut { fan_out, .. } => count_step(&fan_out.worker),
            JobV2StepBody::Loop { loop_ } => loop_.steps.iter().map(count_step).sum(),
        }
    }
    job.steps.iter().map(count_step).sum()
}
