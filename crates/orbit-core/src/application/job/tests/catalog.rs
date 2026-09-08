use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_engine::activity_job::{V2ActivityCatalog, resolve_job_target_refs};
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{inject_system_crew_input, resolve_crew_settings};
use orbit_types::workflow::{
    ActivityV2Spec, JobRunState, JobV2, JobV2Step, JobV2StepBody, PipelineState,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::super::catalog::{DEFAULT_JOB_FILES, JobCatalogFilter, seed_default_jobs};
use crate::OrbitRuntime;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, global_root, workspace_root)
}

fn write_job(path: &Path, name: &str, action: &str, max_active_runs: u32) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: {max_active_runs}
  steps:
    - id: marker
      spec:
        type: deterministic
        action: {action}
        config: {{}}
"#
    );
    std::fs::create_dir_all(path.parent().expect("job path has parent")).expect("create job dir");
    std::fs::write(path, yaml).expect("write job yaml");
}

fn write_empty_job(path: &Path, name: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  steps: []
"#
    );
    std::fs::create_dir_all(path.parent().expect("job path has parent")).expect("create job dir");
    std::fs::write(path, yaml).expect("write job yaml");
}

/// A workspace-local shadow job whose single step dispatches an unregistered
/// deterministic action. If catalog resolution incorrectly executed this
/// shadow instead of the builtin, the dispatch would fail
/// (`DeterministicActionNotRegistered`) and the run would land in `Failed` —
/// so a `Success` run proves the shadow's steps never ran.
fn write_failing_shadow_job(path: &Path, name: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  max_active_runs: 1
  steps:
    - id: exploit
      spec:
        type: deterministic
        description: Unregistered action; must never be dispatched.
        action: __shadow_should_not_run__
        config: {{}}
"#
    );
    std::fs::create_dir_all(path.parent().expect("job path has parent")).expect("create job dir");
    std::fs::write(path, yaml).expect("write job yaml");
}

#[test]
fn fresh_job_seeding_copies_every_canonical_asset() {
    let root = tempdir().expect("create tempdir");
    let jobs_dir = root.path().join("resources/jobs");
    seed_default_jobs(&jobs_dir, false).expect("seed canonical jobs");

    for (name, yaml) in DEFAULT_JOB_FILES {
        let seeded = std::fs::read_to_string(jobs_dir.join(format!("{name}.yaml")))
            .expect("read seeded job");
        assert_eq!(
            seeded, *yaml,
            "freshly seeded {name} must match its canonical asset"
        );
        load_job_asset(&seeded).expect("seeded canonical job must parse");
    }
}

#[test]
fn job_reseeding_preserves_local_concurrency_override() {
    let root = tempdir().expect("create tempdir");
    let jobs_dir = root.path().join("resources/jobs");
    seed_default_jobs(&jobs_dir, false).expect("seed canonical jobs");

    let path = jobs_dir.join("task_gate_pipeline.yaml");
    let seeded = std::fs::read_to_string(&path).expect("read seeded gate job");
    let original_limit = load_job_asset(&seeded)
        .expect("parse gate job")
        .spec
        .max_active_runs;
    let override_limit = original_limit + 5;
    let modified = seeded.replacen(
        &format!("  max_active_runs: {original_limit}\n"),
        &format!("  max_active_runs: {override_limit}\n"),
        1,
    );
    assert_eq!(
        load_job_asset(&modified)
            .expect("parse modified gate job")
            .spec
            .max_active_runs,
        override_limit
    );
    std::fs::write(&path, &modified).expect("write local override");

    seed_default_jobs(&jobs_dir, false).expect("reseed jobs without overwriting overrides");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read preserved override"),
        modified
    );
}

#[test]
fn workspace_default_named_job_does_not_run_when_workflow_invoked_by_name() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let job_name = "task_auto_pipeline";
    let global_job = global_root.join("resources/jobs/task_auto_pipeline.yaml");
    let workspace_job = workspace_root.join("resources/jobs/task_auto_pipeline.yaml");
    write_empty_job(&global_job, job_name);
    write_failing_shadow_job(&workspace_job, job_name);

    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_name, 1, Utc::now(), Some(json!({})), None)
        .expect("insert named pipeline run");
    runtime
        .stores()
        .jobs()
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({})),
        )
        .expect("write initial pipeline state");

    runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("execute named pipeline run");

    let finished = runtime
        .show_job_run(&run.run_id)
        .expect("show finished run");
    assert_eq!(
        finished.state,
        JobRunState::Success,
        "workspace-local job shadow must not execute; the empty builtin should run instead"
    );
}

fn default_activity_catalog() -> V2ActivityCatalog {
    let mut catalog = V2ActivityCatalog::new();
    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|err| panic!("default activity {name} should parse: {err}"));
        assert_eq!(&asset.name, name);
        catalog.insert(*name, asset.spec);
    }
    catalog
}

/// The CI-failure sweep quarantines first, then delegates read-only inspection
/// to the existing pilot job. It must never invoke an implementation pipeline
/// or build a worktree itself.
#[test]
fn ci_failure_sweep_pipeline_pilots_proposed_findings_before_authorized_admission() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "ci_failure_sweep_pipeline").then_some(*yaml))
        .expect("CI-failure sweep job default exists");
    let asset = load_job_asset(yaml).expect("parse CI-failure sweep pipeline");
    let catalog = default_activity_catalog();

    assert_eq!(asset.spec.max_active_runs, 1);

    let step_ids: Vec<&str> = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect();
    assert_eq!(
        step_ids,
        ["collect", "file", "pilots", "require_pilot_success"],
        "proposed filing must precede pilot admission"
    );

    for step in &asset.spec.steps[..2] {
        let JobV2StepBody::TargetRef(target) = &step.body else {
            panic!("sweep step `{}` must reference an activity", step.id);
        };
        assert!(
            matches!(
                target.target.as_str(),
                "activity:collect_ci_evidence" | "activity:file_ci_failure_tasks"
            ),
            "unexpected pre-pilot step {}",
            target.target
        );
    }
    let JobV2StepBody::FanOut { fan_out, fan_in } = &asset.spec.steps[2].body else {
        panic!("new CI findings must fan out into independent pilots");
    };
    assert_eq!(fan_out.items, "{{ steps.file.output.pilot_candidates }}");
    assert_eq!(fan_out.max_workers, 3);
    assert_eq!(fan_in.collect.as_deref(), Some("pilot_results"));
    let JobV2StepBody::TargetRef(pilot) = &fan_out.worker.body else {
        panic!("pilot worker must invoke the existing task-pilot job");
    };
    assert_eq!(pilot.target, "activity:invoke_and_wait");
    let input = pilot
        .default_input
        .as_ref()
        .expect("pilot invocation input");
    assert_eq!(input["job_name"], "task_pilot_pipeline");
    assert_eq!(
        input["run_input"]["task_ids"],
        json!(["{{ item.task_id }}"])
    );
    assert_eq!(input["run_input"]["ci_sweep_filing"], "{{ item }}");
    assert_eq!(input["run_input"]["promotion_authorized"], true);

    let JobV2StepBody::TargetRef(require_success) = &asset.spec.steps[3].body else {
        panic!("CI sweep must guard collected pilot child results");
    };
    assert_eq!(require_success.target, "activity:pipeline_success_guard");
    assert_eq!(
        asset.spec.steps[3].when.as_deref(),
        Some("{{ steps.file.output.pilot_candidate_count }} != 0")
    );
    let guard_input = require_success
        .default_input
        .as_ref()
        .expect("pilot success guard input");
    assert_eq!(guard_input["results"], "{{ steps.pilot_results.output }}");
    assert_eq!(guard_input["context"], "ci-failure sweep pilot child");

    let mut resolved = asset.clone();
    resolve_job_target_refs(&mut resolved.spec, &catalog).expect("resolve sweep target refs");
    assert!(
        !yaml.contains("worktree_setup"),
        "the sweep never implements, so it must never build a worktree"
    );
    assert!(!yaml.contains("job_name: task_auto_pipeline"));
}

#[test]
fn dependabot_sweep_pipeline_is_two_deterministic_steps_and_single_flight() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "dependabot_alert_sweep_pipeline").then_some(*yaml))
        .expect("Dependabot sweep job default exists");
    let mut asset = load_job_asset(yaml).expect("parse Dependabot sweep pipeline");
    let catalog = default_activity_catalog();
    resolve_job_target_refs(&mut asset.spec, &catalog).expect("resolve sweep target refs");
    assert_eq!(asset.spec.max_active_runs, 1);
    assert_eq!(
        asset
            .spec
            .steps
            .iter()
            .map(|step| step.id.as_str())
            .collect::<Vec<_>>(),
        ["collect", "file"]
    );
    assert!(
        asset
            .spec
            .steps
            .iter()
            .all(|step| matches!(step.body, JobV2StepBody::Target(_)))
    );
    assert!(!yaml.contains("agent_loop"));
    assert!(!yaml.contains("worktree_setup"));
    for expected in [
        "max_alerts: 100",
        "max_pull_requests: 100",
        "max_code_scanning_alerts: 100",
        "max_secret_scanning_alerts: 100",
        "max_secret_locations: 20",
        "max_tasks: 10",
        "min_severity: high",
        "skip_when_dependabot_pr_open: true",
    ] {
        assert!(yaml.contains(expected), "missing default {expected}");
    }

    let collect = DEFAULT_ACTIVITY_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "collect_dependabot_alerts").then_some(*yaml))
        .expect("collect activity default exists");
    for field in [
        "max_code_scanning_alerts:",
        "max_secret_scanning_alerts:",
        "max_secret_locations:",
        "code_scanning:",
        "secret_scanning:",
        "collection_status:",
    ] {
        assert!(collect.contains(field), "collect schema missing {field}");
    }
    let file = DEFAULT_ACTIVITY_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "file_dependabot_alert_tasks").then_some(*yaml))
        .expect("file activity default exists");
    for field in [
        "collection_outcome:",
        "family_outcomes:",
        "skipped_over_cap:",
        "match_kind:",
        "match_evidence:",
    ] {
        assert!(file.contains(field), "file schema missing {field}");
    }

    let ci_file = DEFAULT_ACTIVITY_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "file_ci_failure_tasks").then_some(*yaml))
        .expect("CI file activity default exists");
    for field in ["match_kind:", "match_evidence:"] {
        assert!(ci_file.contains(field), "CI file schema missing {field}");
    }
}

#[test]
fn seeded_recovery_assets_stay_aligned_without_retired_role() {
    for name in ["step_failure_recovery", "pr_conflict_recovery"] {
        let seeded = DEFAULT_ACTIVITY_FILES
            .iter()
            .find_map(|(activity_name, yaml)| (*activity_name == name).then_some(*yaml))
            .unwrap_or_else(|| panic!("seeded {name} activity"));
        let dogfood = match name {
            "step_failure_recovery" => include_str!(
                "../../../../../../.orbit/resources/activities/step_failure_recovery.yaml"
            ),
            "pr_conflict_recovery" => include_str!(
                "../../../../../../.orbit/resources/activities/pr_conflict_recovery.yaml"
            ),
            _ => unreachable!("fixed recovery activity list"),
        };
        assert_eq!(
            seeded, dogfood,
            "dogfood and seeded {name} assets must remain behaviorally aligned"
        );

        let asset = load_activity_asset(seeded).expect("parse recovery activity");
        let ActivityV2Spec::AgentLoop(_) = asset.spec.spec else {
            panic!("{name} must remain an agent loop");
        };
        assert!(!seeded.contains("\n  role:"));
    }
}

fn assert_condition_tokens_are_paths(condition: &str) {
    let mut remaining = condition;
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let end = after_start
            .find("}}")
            .unwrap_or_else(|| panic!("unterminated template token in {condition:?}"));
        let token = after_start[..end].trim();
        assert!(
            !["==", "!=", "&&", "||", ">", "<"]
                .iter()
                .any(|op| token.contains(op)),
            "template token {token:?} in condition {condition:?} must be a path; put comparisons outside the braces",
        );
        remaining = &after_start[end + 2..];
    }
}

fn assert_step_condition_tokens_are_paths(step: &orbit_types::workflow::JobV2Step) {
    if let Some(when) = &step.when {
        assert_condition_tokens_are_paths(when);
    }
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                assert_step_condition_tokens_are_paths(branch);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            assert_step_condition_tokens_are_paths(&fan_out.worker);
        }
        JobV2StepBody::Loop { loop_ } => {
            if let Some(break_when) = &loop_.break_when {
                assert_condition_tokens_are_paths(break_when);
            }
            for child in &loop_.steps {
                assert_step_condition_tokens_are_paths(child);
            }
        }
        JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
    }
}

#[test]
fn default_job_target_refs_resolve_against_default_activities() {
    let catalog = default_activity_catalog();

    for (job_name, yaml) in DEFAULT_JOB_FILES {
        let mut asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));
        resolve_job_target_refs(&mut asset.spec, &catalog)
            .unwrap_or_else(|err| panic!("default job {job_name} refs resolve: {err}"));
    }
}

/// [ORB-10877] The pilots must resolve their crew per machine. A literal crew
/// name in this asset only exists on a host where the matching agent CLI was
/// detected at `orbit init`; everywhere else it is a hard dispatch failure,
/// and the `all` join turns that into a whole-run failure.
#[test]
fn task_pilot_pipeline_resolves_system_crew_and_bounded_partial_join_partitions() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_pilot_pipeline").then_some(*yaml))
        .expect("task pilot pipeline exists");
    let asset = load_job_asset(yaml).expect("task pilot pipeline parses");
    let defaults = asset.spec.default_input.as_ref().expect("default input");
    assert_eq!(defaults["task_ids"], json!([]));
    assert!(
        defaults.get("base_branch").is_none(),
        "branch fallback belongs to the prepare activity, not the job defaults"
    );
    assert_eq!(defaults["max_partition_size"], 5);
    assert_eq!(defaults["promotion_authorized"], false);
    assert_eq!(defaults["ci_sweep_filing"], Value::Null);
    assert!(
        defaults.get("crew").is_none(),
        "a job-input crew would be dead: the system-crew marker overwrites it before resolution"
    );
    assert_eq!(asset.spec.steps.len(), 4);

    let JobV2StepBody::TargetRef(prepare) = &asset.spec.steps[0].body else {
        panic!("task pilot preparation must be deterministic activity reference");
    };
    assert_eq!(prepare.target, "activity:prepare_task_pilot");
    let prepare_input = prepare.default_input.as_ref().expect("prepare input");
    assert_eq!(prepare_input["task_ids"], "{{ input.task_ids }}");
    assert_eq!(prepare_input["base_branch"], "{{ input.base_branch }}");

    let JobV2StepBody::FanOut { fan_out, fan_in } = &asset.spec.steps[1].body else {
        panic!("task pilot agent work must fan out");
    };
    assert_eq!(fan_out.items, "{{ steps.prepare.output.partitions }}");
    assert_eq!(fan_out.max_workers, 5);
    assert_eq!(
        fan_in.join,
        orbit_types::workflow::activity_job::JoinMode::Any
    );
    assert_eq!(fan_in.collect.as_deref(), Some("pilot_results"));
    let JobV2StepBody::TargetRef(pilot) = &fan_out.worker.body else {
        panic!("task pilot worker must reference agent activity");
    };
    assert_eq!(pilot.target, "activity:task_pilot");
    let pilot_input = pilot.default_input.as_ref().expect("pilot input");
    assert_eq!(pilot_input["task_ids"], "{{ item.task_ids }}");
    assert_eq!(
        pilot_input["base_branch"],
        "{{ steps.prepare.output.source.base_branch }}"
    );
    assert_eq!(
        pilot_input["source_revision"],
        pilot_input["inspection_revision"]
    );
    assert_eq!(
        pilot_input["inspection_revision"],
        "{{ steps.prepare.output.source.source_revision }}"
    );
    assert_eq!(
        pilot_input["crew"],
        json!("system"),
        "the pilot worker must name the system crew so the definition states who runs it"
    );
    assert!(
        pilot_input.get("system_crew").is_none(),
        "naming the crew directly replaces the system-crew marker"
    );

    let JobV2StepBody::TargetRef(apply) = &asset.spec.steps[2].body else {
        panic!("task pilot apply must be deterministic activity reference");
    };
    assert_eq!(apply.target, "activity:apply_task_pilot_results");
    let apply_input = apply.default_input.as_ref().expect("apply input");
    assert_eq!(apply_input["prepared"], "{{ steps.prepare.output }}");
    assert_eq!(apply_input["results"], "{{ steps.pilot_results.output }}");
    assert_eq!(
        apply_input["promotion_authorized"],
        "{{ input.promotion_authorized }}"
    );
    assert_eq!(
        apply_input["ci_sweep_filing"],
        "{{ input.ci_sweep_filing }}"
    );
    assert!(
        apply_input.get("crew").is_none() && apply_input.get("system_crew").is_none(),
        "the deterministic apply step must carry no crew key: it cannot receive the \
         system-crew injection, which only runs for agent-loop targets"
    );

    let JobV2StepBody::TargetRef(require_success) = &asset.spec.steps[3].body else {
        panic!("task pilot must guard the durable partition apply result");
    };
    assert_eq!(require_success.target, "activity:pipeline_success_guard");
    assert_eq!(
        require_success.default_input.as_ref().expect("guard input")["result"],
        "{{ steps.apply.output }}"
    );
    assert!(
        !yaml.contains("crew: luna") && !yaml.contains("{{ input.crew }}"),
        "no shipped job may pin a family-specific crew"
    );
    assert!(
        yaml.contains("Schedulable task-pilot pipeline"),
        "task pilot must document scheduled zero-input support"
    );
    assert!(
        !yaml.contains("Invoked-only"),
        "task pilot is no longer restricted to explicit invocation"
    );

    let mut resolved = load_job_asset(yaml).expect("task pilot pipeline parses for resolution");
    resolve_job_target_refs(&mut resolved.spec, &default_activity_catalog())
        .expect("task pilot activity references resolve");
    let JobV2StepBody::FanOut { fan_out, .. } = &resolved.spec.steps[1].body else {
        panic!("resolved task pilot agent work must remain a fan-out");
    };
    let JobV2StepBody::Target(pilot) = &fan_out.worker.body else {
        panic!("task pilot worker must resolve to an activity target");
    };
    assert_eq!(
        pilot.fs_profile.as_deref(),
        Some("reviewer"),
        "resolved task-pilot worker must preserve the read-only filesystem profile"
    );
}

/// [ORB-10877] The regression this pipeline shipped with: on a host whose
/// `[crews]` table has no codex crew, the literal `crew: luna` made every
/// pilot partition fail dispatch with "explicit activity crew `luna` cannot be
/// resolved or used", and the `all` join turned that into a whole-run failure.
///
/// The crew table here is synthetic on purpose — the outcome must not depend
/// on which agent CLIs happen to be installed on the machine running the test.
#[test]
fn task_pilot_dispatch_resolves_a_crew_with_no_codex_crew_configured() {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    // What `orbit init` seeds where only the claude CLI was detected: no
    // `[crews.luna]`, and `workflow.system_crew` pointing at the claude `system`.
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "opus"
system_crew = "system"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"

[crews.system]
provider = "claude"
model = "claude-sonnet-4-6"
backend = "cli"
"#,
    )
    .expect("write claude-only crew config");
    let runtime =
        OrbitRuntime::from_roots(&global, &workspace).expect("build claude-only crew runtime");

    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_pilot_pipeline").then_some(*yaml))
        .expect("task pilot pipeline exists");
    let mut asset = load_job_asset(yaml).expect("task pilot pipeline parses");
    resolve_job_target_refs(&mut asset.spec, &default_activity_catalog())
        .expect("task pilot activity references resolve");
    let JobV2StepBody::FanOut { fan_out, .. } = &asset.spec.steps[1].body else {
        panic!("task pilot agent work must fan out");
    };
    let JobV2StepBody::Target(pilot) = &fan_out.worker.body else {
        panic!("task pilot worker must resolve to an activity target");
    };
    let ActivityV2Spec::AgentLoop(spec) = &pilot.spec else {
        panic!("task pilot worker must be an agent loop");
    };
    let pilot_input = pilot.default_input.clone().expect("pilot input");

    // Exactly what `crew_overridden_spec` does at dispatch, in order.
    let dispatched_input =
        inject_system_crew_input(&runtime, &pilot_input).expect("inject configured system crew");
    let resolved = resolve_crew_settings(&runtime, spec, &dispatched_input, &json!({}))
        .expect("task pilot dispatch must not fail without a codex crew")
        .expect("task pilot dispatch must resolve a crew");
    assert_eq!(resolved.provider.as_str(), "claude");
    assert_eq!(resolved.model.as_deref(), Some("claude-sonnet-4-6"));

    // The old shape still fails on this host, so the assertion above is load-bearing.
    let error = resolve_crew_settings(&runtime, spec, &json!({ "crew": "luna" }), &json!({}))
        .expect_err("a literal codex crew must still fail where that crew is unconfigured");
    assert!(
        error
            .to_string()
            .contains("explicit activity crew `luna` cannot be resolved or used"),
        "unexpected error: {error}"
    );
}

/// [ORB-11242] The restriction is only as good as its weakest hand-off. Every
/// job on the auto-drain's dispatch chain has to declare the input, and every
/// one that dispatches a child has to forward it, or a nested run would resolve
/// its crew with no restriction in sight. Asserted on the loaded assets so a
/// job added to the chain without the forwarding line fails here.
#[test]
fn auto_drain_dispatch_chain_declares_and_forwards_the_crew_allowlist() {
    /// job name -> does it dispatch a child that must inherit the window?
    const CHAIN: &[(&str, bool)] = &[
        ("workspace_auto_pipeline", true),
        ("task_auto_pipeline", true),
        ("task_gate_pipeline", true),
        ("epic_pipeline", true),
        // Chain termini: the agent activities in these jobs resolve their crew
        // against this run input, which is where the gate reads it.
        ("task_local_pipeline", false),
        ("task_pr_pipeline", false),
    ];

    for (job_name, forwards) in CHAIN {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("{job_name} ships as a default job"));
        let asset = load_job_asset(yaml).unwrap_or_else(|error| panic!("{job_name}: {error}"));
        let default_input = asset
            .spec
            .default_input
            .as_ref()
            .unwrap_or_else(|| panic!("{job_name} declares a default input"));
        assert_eq!(
            default_input.get("allowed_crews"),
            Some(&json!([])),
            "{job_name} must default to an unrestricted window"
        );

        let rendered = serde_json::to_string(&asset.spec).expect("serialize job spec");
        assert_eq!(
            rendered.contains("{{ input.allowed_crews }}"),
            *forwards,
            "{job_name} forwarding expectation mismatch"
        );
    }
}

/// [ORB-11242] The failure that motivated the run-scoped allowlist: a system
/// activity resolved `workflow.system_crew` on its own, so it launched a
/// provider the operator had excluded even though the run's own crew was a
/// permitted one. Driven through the shipped `task_pilot_pipeline` asset and
/// the real dispatch order (`inject_system_crew_input` then
/// `resolve_crew_settings`), because the injection is exactly what made the
/// override invisible to the run input.
#[test]
fn system_crew_dispatch_is_refused_when_the_run_window_excludes_it() {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    // `luna` is the permitted wrapper the run selected; `system` resolves to a
    // *different* model, which is the provider usage the window excluded.
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "luna"
system_crew = "system"

[crews.luna]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"

[crews.system]
provider = "claude"
model = "claude-fable-5-1"
backend = "cli"
"#,
    )
    .expect("write crew config");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_pilot_pipeline").then_some(*yaml))
        .expect("task pilot pipeline exists");
    let mut asset = load_job_asset(yaml).expect("task pilot pipeline parses");
    resolve_job_target_refs(&mut asset.spec, &default_activity_catalog())
        .expect("task pilot activity references resolve");
    let JobV2StepBody::FanOut { fan_out, .. } = &asset.spec.steps[1].body else {
        panic!("task pilot agent work must fan out");
    };
    let JobV2StepBody::Target(pilot) = &fan_out.worker.body else {
        panic!("task pilot worker must resolve to an activity target");
    };
    let ActivityV2Spec::AgentLoop(spec) = &pilot.spec else {
        panic!("task pilot worker must be an agent loop");
    };
    let pilot_input = pilot.default_input.clone().expect("pilot input");
    let dispatched_input =
        inject_system_crew_input(&runtime, &pilot_input).expect("inject configured system crew");

    // The permitted control: an unrestricted run dispatches the system crew as
    // it always has, so the refusal below is the allowlist and nothing else.
    let unrestricted = resolve_crew_settings(&runtime, spec, &dispatched_input, &json!({}))
        .expect("an unrestricted run still dispatches the system crew")
        .expect("system crew resolves");
    assert_eq!(unrestricted.model.as_deref(), Some("claude-fable-5-1"));

    // The run permits only `luna`. The system override names its own crew, so
    // without the run input's allowlist travelling with it this would launch
    // `claude-fable-5-1` regardless of the window.
    let error = resolve_crew_settings(
        &runtime,
        spec,
        &dispatched_input,
        &json!({ "allowed_crews": ["luna"] }),
    )
    .expect_err("an excluded system crew must not reach a provider");
    let message = error.to_string();
    assert!(
        message.contains("`system`") && message.contains("claude-fable-5-1"),
        "the refusal must name the effective configured identity: {message}"
    );
    assert!(message.contains("luna"), "{message}");

    // The other system route: `system_crew: true`, which the recovery
    // dispatcher injects at runtime rather than declaring in an asset. It
    // resolves `workflow.system_crew` itself, so it is the route that could
    // reach an excluded provider without the run input ever naming it.
    let injected = inject_system_crew_input(&runtime, &json!({ "system_crew": true }))
        .expect("inject configured system crew");
    let injected_error = resolve_crew_settings(
        &runtime,
        spec,
        &injected,
        &json!({ "allowed_crews": ["luna"] }),
    )
    .expect_err("an excluded `workflow.system_crew` must not reach a provider");
    let injected_message = injected_error.to_string();
    assert!(
        injected_message.contains("workflow.system_crew"),
        "the refusal must name where the crew came from: {injected_message}"
    );
    assert!(
        injected_message.contains("claude-fable-5-1"),
        "{injected_message}"
    );

    // A wrapper is not provider usage: naming `luna` is what the run selected,
    // and it still dispatches under the same restriction.
    let allowed = resolve_crew_settings(
        &runtime,
        spec,
        &json!({ "crew": "luna" }),
        &json!({ "allowed_crews": ["luna"] }),
    )
    .expect("a permitted crew must still dispatch")
    .expect("permitted crew resolves");
    assert_eq!(allowed.model.as_deref(), Some("claude-opus-4-6"));
}

/// [ORB-10385] Every deterministic action reachable from a shipped job —
/// including terminal `failure_activity` hooks — must be registered in
/// this binary's v2 dispatch table. `pr_failure_handoff` shipped as a
/// catalog asset bound to `task_pr_pipeline` while orbit-core's dispatch
/// arm still omitted it, so the hook fired as "deterministic action not
/// registered" on three runs, each after a task had been admitted,
/// implemented, and validated. `worktree_gc` had the same gap.
#[test]
fn default_jobs_only_reference_registered_deterministic_actions() {
    let (_root, runtime, _global_root, _workspace_root) = test_runtime();
    let catalog = default_activity_catalog();

    for (job_name, yaml) in DEFAULT_JOB_FILES {
        let mut asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));
        resolve_job_target_refs(&mut asset.spec, &catalog)
            .unwrap_or_else(|err| panic!("default job {job_name} refs resolve: {err}"));
        orbit_engine::validate_job_deterministic_actions(&asset.spec, &runtime).unwrap_or_else(
            |err| panic!("default job {job_name} references an unregistered action: {err}"),
        );
    }
}

/// [ORB-11325] No shipped job's `when:` / `break_when:` may read
/// `steps.<id>.output` for a step that may be skipped — by its own `when:`
/// or, since [ORB-11346], by a `when:` on any enclosing step, whose false
/// branch skips the whole nested body. A skipped step records nothing, and
/// `condition::evaluate_bool_expr` renders the whole expression before
/// parsing it, so the reference fails with
/// `template.rs`'s "no data recorded for step" error on exactly the branch
/// where the referenced step would have been skipped. `validate_job` is the
/// catalog-load gate; no shipped job is exempted from it.
#[test]
fn default_jobs_only_read_step_output_from_always_run_steps() {
    let catalog = default_activity_catalog();

    for (job_name, yaml) in DEFAULT_JOB_FILES {
        let mut asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));
        resolve_job_target_refs(&mut asset.spec, &catalog)
            .unwrap_or_else(|err| panic!("default job {job_name} refs resolve: {err}"));
        orbit_engine::validate_job(&asset.spec).unwrap_or_else(|err| {
            panic!(
                "default job {job_name} reads a conditional step's output from a when/break_when: {err}"
            )
        });
    }
}

/// Companion to the job sweep above: a seeded deterministic activity that
/// no shipped job targets yet must still be dispatchable, or the first job
/// to bind it inherits the same skew.
#[test]
fn default_deterministic_activities_are_registered_in_the_runtime() {
    let (_root, runtime, _global_root, _workspace_root) = test_runtime();

    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let asset = load_activity_asset(yaml)
            .unwrap_or_else(|err| panic!("default activity {name} should parse: {err}"));
        let ActivityV2Spec::Deterministic(spec) = &asset.spec.spec else {
            continue;
        };
        assert!(
            orbit_engine::RuntimeHost::has_deterministic_action(&runtime, &spec.action),
            "seeded activity `{name}` names deterministic action `{}`, which this runtime cannot dispatch",
            spec.action
        );
    }
}

#[test]
fn local_task_pipeline_commits_before_merge_and_reconciles_with_local_base() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_local_pipeline").then_some(*yaml))
        .expect("task local pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task local pipeline");
    let root_step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();

    let commit_index = root_step_ids
        .iter()
        .position(|id| *id == "commit")
        .expect("task local pipeline has commit step");
    let merge_index = root_step_ids
        .iter()
        .position(|id| *id == "merge")
        .expect("task local pipeline has merge step");
    assert!(
        commit_index < merge_index,
        "task local pipeline must commit before merge"
    );

    let merge = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "merge")
        .expect("task local pipeline has merge step");
    let JobV2StepBody::TargetRef(merge) = &merge.body else {
        panic!("task local pipeline merge must reference git_merge");
    };
    let merge_input = merge.default_input.as_ref().expect("merge input");
    assert_eq!(
        merge_input["base_sync"], "local",
        "an unpublished earlier merge must be a valid base for the next local merge"
    );

    assert_eq!(
        asset.spec.default_input.as_ref().unwrap()["terminal_status"],
        "review"
    );
    let mark_review = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "mark_review")
        .expect("task local pipeline has terminal task update loop");
    let JobV2StepBody::Loop { loop_ } = &mark_review.body else {
        panic!("task local terminal update must be a loop");
    };
    let JobV2StepBody::TargetRef(update) = &loop_.steps[0].body else {
        panic!("task local terminal update must reference update_task");
    };
    assert_eq!(
        update.default_input.as_ref().unwrap()["status"],
        "{{ input.terminal_status }}"
    );
}

#[test]
fn task_shipment_implementers_pin_workspace_and_repo_roots_to_the_worktree() {
    for job_name in ["task_local_pipeline", "task_pr_pipeline"] {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (*name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {job_name} exists"));
        let asset =
            load_job_asset(yaml).unwrap_or_else(|error| panic!("parse {job_name}: {error}"));
        let implement_bundle = asset
            .spec
            .steps
            .iter()
            .find(|step| step.id == "implement_bundle")
            .expect("implement bundle");
        let JobV2StepBody::Loop { loop_ } = &implement_bundle.body else {
            panic!("{job_name} implement bundle must be a loop");
        };
        let JobV2StepBody::TargetRef(implement) = &loop_.steps[0].body else {
            panic!("{job_name} implement step must reference agent_implement");
        };
        let input = implement.default_input.as_ref().expect("implement input");
        for field in ["workspace_path", "repo_root"] {
            assert_eq!(
                input[field], "{{ steps.worktree.output.workspace_path }}",
                "{job_name} must pin {field} to the exact assigned worktree"
            );
        }
    }
}

#[test]
fn task_shipment_commit_steps_use_the_worktree_base_checkpoint() {
    for job_name in ["task_local_pipeline", "task_pr_pipeline"] {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (*name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {job_name} exists"));
        let asset =
            load_job_asset(yaml).unwrap_or_else(|error| panic!("parse {job_name}: {error}"));
        let commit = asset
            .spec
            .steps
            .iter()
            .find(|step| step.id == "commit")
            .expect("commit step");
        let JobV2StepBody::TargetRef(commit) = &commit.body else {
            panic!("{job_name} commit step must reference git_commit");
        };
        let input = commit.default_input.as_ref().expect("commit input");
        assert_eq!(
            input["base_ref"], "{{ steps.worktree.output.base_ref }}",
            "{job_name} must pass the exact worktree start-point ref"
        );
        // ORB-10380: the commit step reconciles history against the commit
        // pinned at setup, never the moving ref name.
        assert_eq!(
            input["base_sha"], "{{ steps.worktree.output.base_sha }}",
            "{job_name} must pin the commit step to the setup-time base commit"
        );
    }
}

#[test]
fn pr_pipeline_models_handoff_phases_as_ordered_activity_checkpoints() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_pr_pipeline").then_some(*yaml))
        .expect("task pr pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task pr pipeline");
    let phases = asset
        .spec
        .steps
        .iter()
        .filter_map(|step| match &step.body {
            JobV2StepBody::TargetRef(target) => Some((
                step.id.as_str(),
                target.target.as_str(),
                step.recovery_activity.as_deref(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        phases,
        vec![
            ("worktree", "activity:worktree_setup", None),
            (
                "commit",
                "activity:git_commit",
                Some("step_failure_recovery")
            ),
            (
                "prepare_branch",
                "activity:pr_prepare",
                Some("step_failure_recovery")
            ),
            (
                "sync_base",
                "activity:git_rebase",
                Some("pr_conflict_recovery")
            ),
            // ORB-11333: the before-PR review gate always runs between the
            // final base sync and publication; a non-pass fails the settle
            // step so the failure handoff preserves the candidate.
            ("review_gate_admit", "activity:review_gate_admit", None),
            ("review", "activity:agent_review_repair", None),
            ("review_gate_settle", "activity:review_gate_settle", None),
            ("push", "activity:git_push", Some("step_failure_recovery")),
            ("pr_open", "activity:pr_open", Some("step_failure_recovery")),
            (
                "promote_tasks",
                "activity:pr_promote",
                Some("step_failure_recovery")
            ),
            (
                "promote_no_diff",
                "activity:pr_promote",
                Some("step_failure_recovery")
            ),
            // ORB-11187: completion is two more ordered checkpoints after the
            // review handoff, reached only under explicit authorization.
            (
                "complete_pr",
                "activity:pr_complete",
                Some("pr_conflict_recovery")
            ),
            (
                "complete_no_diff",
                "activity:pr_complete",
                Some("step_failure_recovery")
            ),
        ]
    );

    let pr_open = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "pr_open")
        .expect("PR open phase");
    let JobV2StepBody::TargetRef(target) = &pr_open.body else {
        panic!("PR open must reference a focused activity");
    };
    let input = target.default_input.as_ref().expect("PR open input");
    for hidden_phase in ["scope", "rewrite_performed", "expected_remote_sha"] {
        assert!(
            input.get(hidden_phase).is_none(),
            "pr_open must not embed earlier {hidden_phase} phase input"
        );
    }

    let complete_pr = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "complete_pr")
        .expect("PR completion phase");
    let JobV2StepBody::TargetRef(target) = &complete_pr.body else {
        panic!("PR completion must reference a focused activity");
    };
    let input = target.default_input.as_ref().expect("PR completion input");
    assert_eq!(input["completion"], "{{ input.completion }}");
    assert_eq!(input["head"], "{{ steps.sync_base.output.head }}");
    assert_eq!(
        input["published_head_sha"],
        "{{ steps.push.output.local_sha }}"
    );
    assert_eq!(input["base"], "{{ steps.sync_base.output.base }}");
    assert_eq!(input["base_sync"], "{{ input.base_sync }}");
}

#[test]
fn gate_pipeline_releases_reservation_before_child_success_guard() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_gate_pipeline").then_some(*yaml))
        .expect("task gate pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task gate pipeline");
    let root_step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();

    let dispatch_index = root_step_ids
        .iter()
        .position(|id| *id == "dispatch_child")
        .expect("task gate pipeline has child dispatch step");
    let release_index = root_step_ids
        .iter()
        .position(|id| *id == "release_reservation")
        .expect("task gate pipeline has reservation release step");
    let guard_index = root_step_ids
        .iter()
        .position(|id| *id == "require_child_success")
        .expect("task gate pipeline has child success guard step");
    assert!(
        dispatch_index < release_index,
        "reservation must release only after invoke_and_wait returns"
    );
    assert!(
        release_index < guard_index,
        "reservation must release before the child success guard can fail the run"
    );

    let dispatch = &asset.spec.steps[dispatch_index];
    // No `when:` of its own (ORB-11325): `starvation_check`'s complementary
    // `when: reserved == false` always fails and halts the run before this
    // step would otherwise be reached on that branch, which is what lets
    // `release_reservation` safely read `steps.dispatch_child.output`.
    assert_eq!(dispatch.when.as_deref(), None);
    match &dispatch.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:invoke_and_wait");
            let input = target.default_input.as_ref().expect("dispatch input");
            assert_eq!(
                input["job_name"],
                Value::String("task_{{ input.mode }}_pipeline".to_string())
            );
            assert_eq!(
                input["admission_task_ids"],
                Value::String("{{ input.task_ids }}".to_string())
            );
            assert_eq!(
                input["admission_workflow"],
                Value::String("worktree_setup".to_string())
            );
        }
        other => panic!("expected dispatch target ref, got {other:?}"),
    }

    let release = &asset.spec.steps[release_index];
    assert_eq!(
        release.when.as_deref(),
        Some(
            "{{ steps.dispatch_child.output.status }} != timeout && {{ steps.dispatch_child.output.status }} != pending && {{ steps.dispatch_child.output.status }} != running"
        )
    );
    match &release.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:release_locks");
            let input = target.default_input.as_ref().expect("release input");
            assert_eq!(
                input["reservation_id"],
                Value::String("{{ steps.reserve.output.reservation_id }}".to_string())
            );
        }
        other => panic!("expected release target ref, got {other:?}"),
    }

    let guard = &asset.spec.steps[guard_index];
    // No `when:` of its own: reaching this step already implies
    // `reserve.output.reserved == true`, since `starvation_check`'s
    // complementary `when: reserved == false` always fails and halts the run
    // on the other branch (ORB-11325).
    assert_eq!(guard.when.as_deref(), None);
    match &guard.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:pipeline_success_guard");
            let input = target.default_input.as_ref().expect("guard input");
            assert_eq!(
                input["result"],
                Value::String("{{ steps.dispatch_child.output }}".to_string())
            );
        }
        other => panic!("expected guard target ref, got {other:?}"),
    }
}

#[test]
fn auto_pipeline_checks_gate_results_after_fan_in() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_auto_pipeline").then_some(*yaml))
        .expect("task auto pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task auto pipeline");
    let root_step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();

    let dispatch_index = root_step_ids
        .iter()
        .position(|id| *id == "dispatch")
        .expect("task auto pipeline has dispatch fan-out");
    let guard_index = root_step_ids
        .iter()
        .position(|id| *id == "require_gate_success")
        .expect("task auto pipeline has gate success guard");
    assert!(
        dispatch_index < guard_index,
        "gate results must be collected before the success guard runs"
    );

    let dispatch = &asset.spec.steps[dispatch_index];
    match &dispatch.body {
        JobV2StepBody::FanOut { fan_out, .. } => {
            assert_eq!(fan_out.max_workers, 5);
        }
        other => panic!("expected dispatch fan-out, got {other:?}"),
    }

    let guard = &asset.spec.steps[guard_index];
    assert_eq!(
        guard.when.as_deref(),
        Some("{{ steps.validate_bundles.output.bundle_count }} != 0")
    );
    match &guard.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:pipeline_success_guard");
            let input = target.default_input.as_ref().expect("guard input");
            assert_eq!(
                input["results"],
                Value::String("{{ steps.gate_results.output }}".to_string())
            );
        }
        other => panic!("expected guard target ref, got {other:?}"),
    }
}

/// [ORB-11268] `allowed_crews` reaching the `dispatch` fan-out is not enough:
/// the generic escape hatch (`orbit run job task_auto_pipeline --input
/// allowed_crews=<crew>` with `task_ids` left empty) forces the discovery
/// branch in `list_backlog_tasks`, which reads the allowlist off
/// `list_backlog`'s own rendered step input. If that step never forwards
/// `input.allowed_crews`, discovery-mode backlog listing silently ignores the
/// run's crew restriction even though the later gate still enforces it.
#[test]
fn auto_pipeline_list_backlog_step_forwards_allowed_crews() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_auto_pipeline").then_some(*yaml))
        .expect("task auto pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task auto pipeline");

    let list_backlog = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "list_backlog")
        .expect("task auto pipeline has a list_backlog step");
    match &list_backlog.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:list_backlog_tasks");
            let input = target.default_input.as_ref().expect("list_backlog input");
            assert_eq!(
                input["allowed_crews"],
                Value::String("{{ input.allowed_crews }}".to_string()),
                "list_backlog must forward the job's top-level allowed_crews input, \
                 not just the later dispatch step"
            );
        }
        other => panic!("expected list_backlog target ref, got {other:?}"),
    }
}

#[test]
fn gate_pipeline_default_reservation_ttl_covers_child_wait_budget() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_gate_pipeline").then_some(*yaml))
        .expect("task gate pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task gate pipeline");
    let default_input = asset
        .spec
        .default_input
        .as_ref()
        .expect("task gate pipeline default input");
    let ttl_seconds = default_input["ttl_seconds"]
        .as_u64()
        .expect("numeric ttl_seconds");
    let dispatch_timeout_seconds = default_input["dispatch_timeout_seconds"]
        .as_u64()
        .expect("numeric dispatch_timeout_seconds");

    assert!(
        ttl_seconds >= dispatch_timeout_seconds,
        "reservation TTL must cover the child dispatch wait budget"
    );
}

/// [ORB-10129] Structural invariants of the triage pipeline: it is
/// single-flight (`max_active_runs: 1` — one half of the overlap
/// guarantee, the routine's `overlap: forbid` is the other), an empty
/// candidate list skips both downstream steps (clean no-op), and the
/// lifecycle write is the deterministic `apply_dispositions` step, not
/// the agent.
#[test]
fn triage_pipeline_is_single_flight_and_gates_on_candidates() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_triage_pipeline").then_some(*yaml))
        .expect("task triage pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task triage pipeline");
    assert_eq!(asset.spec.max_active_runs, 1);

    let step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        step_ids,
        ["list_candidates", "triage", "apply_dispositions"]
    );

    for step_id in ["triage", "apply_dispositions"] {
        let step = asset
            .spec
            .steps
            .iter()
            .find(|step| step.id == step_id)
            .expect("triage pipeline step");
        assert_eq!(
            step.when.as_deref(),
            Some("{{ steps.list_candidates.output.candidate_count }} != 0"),
            "step {step_id} must be skipped on an empty candidate list"
        );
    }

    let apply = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "apply_dispositions")
        .expect("apply step");
    match &apply.body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:apply_triage_dispositions");
            let input = target.default_input.as_ref().expect("apply input");
            assert_eq!(
                input["dispositions"],
                Value::String("{{ steps.triage.output.dispositions }}".to_string())
            );
            assert_eq!(
                input["candidates"],
                Value::String("{{ steps.list_candidates.output.candidates }}".to_string())
            );
        }
        other => panic!("expected apply target ref, got {other:?}"),
    }
}

#[test]
fn workspace_ship_pipeline_waits_for_workspace_auto_sequencer() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "workspace_ship_pipeline").then_some(*yaml))
        .expect("workspace ship pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse workspace ship pipeline");
    assert_eq!(asset.spec.max_active_runs, 1);
    assert_eq!(asset.spec.steps.len(), 2);
    assert_eq!(asset.spec.steps[0].id, "auto");
    assert_eq!(asset.spec.steps[1].id, "require_auto_success");

    match &asset.spec.steps[0].body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:invoke_and_wait");
            let input = target.default_input.as_ref().expect("ship input");
            assert_eq!(input["job_name"], "workspace_auto_pipeline");
            // [ORB-10819] The wrapper blocks on its child, so the window it
            // passes is a window `ship-sweep-orbit`'s `overlap: forbid` holds.
            // It must stay comfortably inside that routine's 30-minute period.
            let for_seconds = input["run_input"]["for_seconds"]
                .as_u64()
                .expect("wrapper drain window");
            assert!(
                (0..=1_500).contains(&for_seconds),
                "wrapper window {for_seconds}s leaves too little slack before the next sweep fire"
            );
        }
        other => panic!("expected invoke-and-wait target ref, got {other:?}"),
    }
    match &asset.spec.steps[1].body {
        JobV2StepBody::TargetRef(target) => {
            assert_eq!(target.target, "activity:pipeline_success_guard");
        }
        other => panic!("expected success guard target ref, got {other:?}"),
    }
    // The v1 wrapper shelled out to the retired sweep. Check the definition,
    // not the prose: the comment names `ship-sweep-orbit` deliberately,
    // because that routine's period is why the window above is what it is.
    let definition = yaml_without_comments(yaml);
    assert!(!definition.contains("auto_ship"));
    assert!(!definition.contains("ship-sweep"));
    assert!(!definition.contains("type: shell"));
}

/// A job asset's YAML with whole-line comments removed, for assertions about
/// what a definition *does* rather than about what its header explains.
fn yaml_without_comments(yaml: &str) -> String {
    yaml.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn workspace_auto_pipeline_is_single_flight_and_conditionally_dispatches() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "workspace_auto_pipeline").then_some(*yaml))
        .expect("workspace auto pipeline exists");
    let asset = load_job_asset(yaml).expect("workspace auto pipeline parses");
    assert_eq!(asset.spec.max_active_runs, 1);
    assert_eq!(asset.spec.steps[0].id, "resolve_ship_input");

    // [ORB-10819] The window is stamped once, before the loop, and re-read
    // inside it. `break_when` is evaluated after the body, so a zero window
    // still yields exactly one iteration.
    assert_eq!(asset.spec.steps[1].id, "open_window");
    let JobV2StepBody::TargetRef(open_window) = &asset.spec.steps[1].body else {
        panic!("open_window step must use the deterministic activity");
    };
    assert_eq!(open_window.target, "activity:drain_window");
    assert_eq!(
        open_window.default_input.as_ref().expect("window input")["for_seconds"],
        "{{ input.for_seconds }}"
    );

    let JobV2StepBody::Loop { loop_: drain } = &asset.spec.steps[2].body else {
        panic!("the drain must be a loop, not a single tick");
    };
    assert_eq!(asset.spec.steps[2].id, "drain");
    assert_eq!(
        drain.break_when.as_deref(),
        Some("{{ steps.window.output.expired }} == true")
    );
    let body_ids = drain
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        body_ids,
        vec![
            "admissible",
            "ship_leaves",
            "start_epic",
            "window",
            "idle_wait",
        ],
    );

    // Re-listing inside the loop is what lets a task that entered `backlog`
    // after the run started still ship.
    let JobV2StepBody::TargetRef(admissible) = &drain.steps[0].body else {
        panic!("admissible step must use the deterministic activity");
    };
    assert_eq!(admissible.target, "activity:classify_workspace_auto_tasks");

    let ship = &drain.steps[1];
    assert_eq!(
        ship.when.as_deref(),
        Some("{{ steps.admissible.output.has_leaves }} == true")
    );
    let JobV2StepBody::FanOut { fan_out, fan_in } = &ship.body else {
        panic!("ship step must fan out the admitted leaf dispatches");
    };
    assert_eq!(
        fan_out.items,
        "{{ steps.admissible.output.loose_task_dispatches }}"
    );
    assert_eq!(fan_out.max_workers, 5);
    assert_eq!(fan_in.collect.as_deref(), Some("leaf_dispatches"));
    // Detached, like the epic: waiting on the whole fan-out held every other
    // slot closed for as long as its slowest member ran. The classifier caps
    // the dispatches at the free slots instead, and the loop tops them up.
    let JobV2StepBody::TargetRef(ship_target) = &fan_out.worker.body else {
        panic!("each leaf must be dispatched by an activity");
    };
    assert_eq!(ship_target.target, "activity:invoke_detached");
    let ship_input = ship_target.default_input.as_ref().expect("ship input");
    assert_eq!(ship_input["job_name"], "task_auto_pipeline");
    assert_eq!(ship_input["run_input"]["task_ids"], "{{ item.task_ids }}");

    // Nothing aggregates leaf outcomes any more: a detached child records its
    // own result, and a terminal leaf was already explicitly not a failure of
    // this sequencer.
    let definition_body = yaml_without_comments(yaml);
    assert!(!definition_body.contains("pipeline_success_guard"));
    assert!(!definition_body.contains("record_leaf_outcomes"));

    // The epic must NOT be waited on: blocking on a multi-hour epic would
    // consume the window and starve the conflict-free leaves behind it.
    let epic = &drain.steps[2];
    assert_eq!(
        epic.when.as_deref(),
        Some("{{ steps.admissible.output.has_epic }} == true")
    );
    let JobV2StepBody::TargetRef(epic_target) = &epic.body else {
        panic!("epic step must dispatch detached");
    };
    assert_eq!(epic_target.target, "activity:invoke_detached");
    assert_eq!(
        epic_target.default_input.as_ref().expect("epic input")["job_name"],
        "epic_pipeline"
    );

    let JobV2StepBody::TargetRef(window) = &drain.steps[3].body else {
        panic!("window step must use the deterministic activity");
    };
    assert_eq!(window.target, "activity:drain_window");
    assert_eq!(
        window.default_input.as_ref().expect("window input")["deadline"],
        "{{ steps.open_window.output.deadline }}"
    );

    // Sleeping only when idle keeps a busy window re-listing immediately, and
    // an expired one from paying a final sleep it will not use.
    assert_eq!(
        drain.steps[4].when.as_deref(),
        Some(
            "{{ steps.admissible.output.idle }} == true && \
             {{ steps.window.output.expired }} == false"
        )
    );
    // The classifier picks the wait, so a saturated drain refills a freed slot
    // in seconds while an empty workspace still waits the long idle.
    let JobV2StepBody::TargetRef(idle_wait) = &drain.steps[4].body else {
        panic!("idle wait must use the deterministic activity");
    };
    assert_eq!(
        idle_wait.default_input.as_ref().expect("idle input")["seconds"],
        "{{ steps.admissible.output.sleep_seconds }}"
    );
    // The four-way `ship`/`hold`/`epic`/`empty` decision is gone; the loop
    // reads an admissible set instead.
    let definition = yaml_without_comments(yaml);
    assert!(!definition.contains("decision"));
    assert!(!definition.contains("hold"));
}

#[test]
fn default_jobs_template_only_declared_agent_loop_handoffs() {
    let agent_activity_names = DEFAULT_ACTIVITY_FILES
        .iter()
        .filter_map(|(name, yaml)| {
            let asset = load_activity_asset(yaml).ok()?;
            matches!(asset.spec.spec, ActivityV2Spec::AgentLoop(_)).then_some(*name)
        })
        .collect::<BTreeSet<_>>();
    let allowed_handoffs = BTreeSet::from([
        // [ORB-10129] The triage agent's dispositions flow into the
        // deterministic `apply_triage_dispositions` step, which bounds
        // them (candidates-only, environmental-only re-backlog, durable
        // budget) instead of trusting them.
        (
            "task_triage_pipeline",
            "triage",
            "steps.triage.output.dispositions",
        ),
    ]);

    for (job_name, yaml) in DEFAULT_JOB_FILES {
        let asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));
        let mut agent_step_ids = BTreeSet::new();
        for step in &asset.spec.steps {
            collect_agent_loop_step_ids(step, &agent_activity_names, &mut agent_step_ids);
        }

        if agent_step_ids.is_empty() {
            continue;
        }

        let mut template_strings = Vec::new();
        for step in &asset.spec.steps {
            collect_template_strings(step, &mut template_strings);
        }

        for agent_step_id in agent_step_ids {
            let forbidden = format!("steps.{agent_step_id}.output");
            for template in &template_strings {
                let allowed =
                    allowed_handoffs
                        .iter()
                        .any(|(allowed_job, allowed_step, allowed_path)| {
                            *allowed_job == *job_name
                                && *allowed_step == agent_step_id
                                && template.contains(allowed_path)
                        });
                assert!(
                    !template.contains(&forbidden) || allowed,
                    "default job {job_name} templates from agent_loop output: {template}"
                );
            }
        }
    }
}

#[test]
fn default_job_conditions_keep_comparisons_outside_template_tokens() {
    for (name, yaml) in DEFAULT_JOB_FILES {
        let asset = load_job_asset(yaml).unwrap_or_else(|err| {
            panic!("default job {name} should parse before condition checks: {err}")
        });
        for step in &asset.spec.steps {
            assert_step_condition_tokens_are_paths(step);
        }
    }
}

#[test]
fn task_shipment_jobs_resolve_default_recovery_activity() {
    let catalog = default_activity_catalog();

    for job_name in ["task_local_pipeline", "task_pr_pipeline"] {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (*name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {job_name} exists"));
        let mut asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));

        assert_eq!(asset.spec.recovery_activity.as_deref(), None);
        resolve_job_target_refs(&mut asset.spec, &catalog)
            .unwrap_or_else(|err| panic!("default job {job_name} refs resolve: {err}"));
        if job_name == "task_pr_pipeline" {
            assert_eq!(
                asset.spec.failure_activity.as_deref(),
                Some("pr_failure_handoff")
            );
            assert!(
                asset.spec.resolved_failure_activity.is_some(),
                "task PR terminal failure handoff must resolve from the shipped catalog"
            );
        } else {
            assert_eq!(asset.spec.failure_activity, None);
        }
        let recovery_steps = step_recovery_activities(&asset.spec);
        assert!(
            !recovery_steps.is_empty(),
            "default job {job_name} should wire recovery on direct shipment steps"
        );
        for (step_id, recovery_activity, resolved) in recovery_steps {
            let expected = if job_name == "task_pr_pipeline"
                && matches!(step_id, "sync_base" | "complete_pr")
            {
                "pr_conflict_recovery"
            } else {
                "step_failure_recovery"
            };
            assert_eq!(
                recovery_activity.as_deref(),
                Some(expected),
                "step {step_id} should use default recovery activity"
            );
            assert!(
                resolved,
                "step {step_id} should cache its recovery activity"
            );
        }
    }
}

#[test]
fn orchestration_jobs_do_not_enable_generic_recovery() {
    for job_name in [
        "epic_pipeline",
        "task_auto_pipeline",
        "task_gate_pipeline",
        "task_triage_pipeline",
        "workspace_ship_pipeline",
        "workspace_auto_pipeline",
    ] {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (*name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {job_name} exists"));
        let asset = load_job_asset(yaml)
            .unwrap_or_else(|err| panic!("default job {job_name} should parse: {err}"));

        assert_eq!(
            asset.spec.recovery_activity, None,
            "default job {job_name} should not generically recover child orchestration"
        );
    }
}

#[test]
fn epic_pipeline_opens_one_stable_worktree_and_drains_children_serially() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "epic_pipeline").then_some(*yaml))
        .expect("epic pipeline exists");
    let asset = load_job_asset(yaml).expect("epic pipeline parses");
    assert_eq!(asset.spec.max_active_runs, 1);
    // ORB-11187 added the two authorized PR completion steps; ORB-11333 added
    // the three before-PR review gate steps.
    assert_eq!(asset.spec.steps.len(), 18);
    let root_step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        root_step_ids,
        [
            "resolve_ship_input",
            "worktree",
            "assemble",
            "require_empty",
            "commit_delivery",
            "prepare_branch",
            "sync_base",
            "review_gate_admit",
            "review",
            "review_gate_settle",
            "push",
            "pr_open",
            "promote_pr",
            "promote_pr_no_diff",
            // ORB-11187: authorized PR completion, between the review handoff
            // and the local-mode delivery steps.
            "complete_pr",
            "complete_pr_no_diff",
            "merge",
            "mark_done",
        ]
    );

    let JobV2StepBody::TargetRef(worktree) = &asset.spec.steps[1].body else {
        panic!("epic pipeline must set up its worktree before draining children");
    };
    assert_eq!(worktree.target, "activity:worktree_setup");
    let worktree_input = worktree.default_input.as_ref().expect("worktree input");
    assert_eq!(worktree_input["run_id"], "epic-{{ input.epic_task_id }}");
    assert_eq!(worktree_input["branch_prefix"], "epic");
    assert_eq!(worktree_input["base_sync"], "remote");

    let JobV2StepBody::Loop { loop_ } = &asset.spec.steps[2].body else {
        panic!("epic pipeline must assemble descendants and the finisher in one loop");
    };
    assert_eq!(loop_.max_iterations, 8);
    assert_eq!(
        loop_.break_when.as_deref(),
        Some("{{ steps.remaining.output.empty }} == true")
    );
    assert_eq!(loop_.steps.len(), 5);

    let JobV2StepBody::Target(descendants) = &loop_.steps[0].body else {
        panic!("assemble loop must list descendants deterministically");
    };
    let ActivityV2Spec::Deterministic(descendants) = &descendants.spec else {
        panic!("epic descendant listing must be deterministic");
    };
    assert_eq!(descendants.action, "list_epic_descendants");

    let JobV2StepBody::Loop { loop_: drain } = &loop_.steps[1].body else {
        panic!("assemble loop must drain descendants through a sequential loop");
    };
    assert_eq!(
        drain.items.as_deref(),
        Some("{{ steps.descendants.output.task_ids }}")
    );
    assert_eq!(drain.max_iterations, 256);
    assert_eq!(drain.steps.len(), 2);
    let JobV2StepBody::TargetRef(land_child) = &drain.steps[0].body else {
        panic!("first drain body must invoke one child pipeline");
    };
    assert_eq!(land_child.target, "activity:invoke_and_wait");
    let child_input = &land_child.default_input.as_ref().expect("child input")["run_input"];
    assert_eq!(
        child_input["base_branch"],
        "{{ steps.worktree.output.head_ref }}"
    );
    assert_eq!(child_input["base_sync"], "local");
    assert_eq!(child_input["auto_push"], false);
    assert_eq!(child_input["terminal_status"], "done");
    assert_eq!(
        child_input["landing_branch"],
        "{{ steps.resolve_ship_input.output.base_branch }}"
    );

    let JobV2StepBody::TargetRef(finish) = &loop_.steps[2].body else {
        panic!("assemble loop must invoke the epic finisher");
    };
    assert_eq!(finish.target, "activity:epic_orchestrator");
    let finish_input = finish.default_input.as_ref().expect("finish input");
    assert_eq!(finish_input["task_id"], "{{ input.epic_task_id }}");
    for field in ["workspace_path", "repo_root"] {
        assert_eq!(
            finish_input[field], "{{ steps.worktree.output.workspace_path }}",
            "epic finisher must pin {field} to the assigned epic worktree"
        );
    }

    let commit_finish = &loop_.steps[3];
    assert_eq!(commit_finish.id, "commit_finish");
    assert_eq!(
        commit_finish.when.as_deref(),
        Some("{{ steps.descendants.output.empty }} == true")
    );
    let JobV2StepBody::TargetRef(commit_finish) = &commit_finish.body else {
        panic!("empty-descendant finish must commit through git_commit");
    };
    assert_eq!(commit_finish.target, "activity:git_commit");
    let commit_input = commit_finish.default_input.as_ref().expect("commit input");
    assert_eq!(
        commit_input["workspace_path"],
        "{{ steps.worktree.output.workspace_path }}"
    );
    assert_eq!(
        commit_input["base_sha"],
        "{{ steps.worktree.output.base_sha }}"
    );

    let JobV2StepBody::Target(remaining) = &loop_.steps[4].body else {
        panic!("assemble loop must re-list descendants after the finisher");
    };
    let ActivityV2Spec::Deterministic(remaining) = &remaining.spec else {
        panic!("remaining descendant listing must be deterministic");
    };
    assert_eq!(remaining.action, "list_epic_descendants");

    let require_empty = &asset.spec.steps[3];
    assert_eq!(require_empty.id, "require_empty");
    let JobV2StepBody::Target(require_empty_step) = &require_empty.body else {
        panic!("post-loop gate must list descendants deterministically");
    };
    let ActivityV2Spec::Deterministic(require_empty_spec) = &require_empty_step.spec else {
        panic!("post-loop descendant gate must be deterministic");
    };
    assert_eq!(require_empty_spec.action, "list_epic_descendants");
    assert_eq!(
        require_empty_step
            .default_input
            .as_ref()
            .expect("require_empty input")["fail_if_nonempty"],
        true
    );

    let commit_delivery = &asset.spec.steps[4];
    let JobV2StepBody::TargetRef(commit_delivery) = &commit_delivery.body else {
        panic!("delivery commit must reference git_commit");
    };
    assert_eq!(commit_delivery.target, "activity:git_commit");
    let commit_input = commit_delivery
        .default_input
        .as_ref()
        .expect("commit_delivery input");
    assert_eq!(commit_input["allow_empty"], true);
    assert_eq!(commit_input["allow_moved_head"], true);
    assert_eq!(
        commit_input["workspace_path"],
        "{{ steps.worktree.output.workspace_path }}"
    );

    let pr_when = Some(
        "{{ steps.resolve_ship_input.output.mode }} == pr && {{ steps.commit_delivery.output.skipped_no_diff_expected }} != true",
    );
    // ORB-11333: the three review-gate steps sit between sync_base and push
    // and run unconditionally, so only the PR handoff steps carry `pr_when`.
    for (index, id) in [
        ("prepare_branch", 5),
        ("sync_base", 6),
        ("push", 10),
        ("pr_open", 11),
        ("promote_pr", 12),
    ]
    .into_iter()
    .map(|(id, index)| (index, id))
    {
        let step = &asset.spec.steps[index];
        assert_eq!(step.id, id);
        assert_eq!(step.when.as_deref(), pr_when);
        let JobV2StepBody::TargetRef(target) = &step.body else {
            panic!("{id} must be an activity reference");
        };
        assert!(
            !target.target.contains("task_pr_pipeline"),
            "delivery must compose activities, not invoke task_pr_pipeline"
        );
        if let Some(input) = target.default_input.as_ref() {
            if let Some(workspace_path) = input.get("workspace_path") {
                assert_eq!(
                    workspace_path, "{{ steps.worktree.output.workspace_path }}",
                    "{id} must reuse the epic worktree"
                );
            }
            assert!(
                input.get("job_name") != Some(&json!("task_pr_pipeline")),
                "{id} must not dispatch task_pr_pipeline"
            );
        }
    }

    let promote_no_diff = &asset.spec.steps[13];
    assert_eq!(
        promote_no_diff.when.as_deref(),
        Some(
            "{{ steps.resolve_ship_input.output.mode }} == pr && {{ steps.commit_delivery.output.skipped_no_diff_expected }} == true"
        )
    );
    let JobV2StepBody::TargetRef(promote_no_diff) = &promote_no_diff.body else {
        panic!("no-diff PR path must update the epic root");
    };
    assert_eq!(promote_no_diff.target, "activity:update_task");
    assert_eq!(
        promote_no_diff
            .default_input
            .as_ref()
            .expect("no-diff input")["status"],
        "review"
    );

    let merge = &asset.spec.steps[16];
    assert_eq!(
        merge.when.as_deref(),
        Some("{{ steps.resolve_ship_input.output.mode }} == local")
    );
    let JobV2StepBody::TargetRef(merge) = &merge.body else {
        panic!("local delivery must merge the epic branch");
    };
    assert_eq!(merge.target, "activity:git_merge");
    let merge_input = merge.default_input.as_ref().expect("merge input");
    assert_eq!(
        merge_input["workspace_path"],
        "{{ steps.worktree.output.workspace_path }}"
    );
    assert_eq!(
        merge_input["base"],
        "{{ steps.resolve_ship_input.output.base_branch }}"
    );

    let mark_done = &asset.spec.steps[17];
    assert_eq!(
        mark_done.when.as_deref(),
        Some("{{ steps.resolve_ship_input.output.mode }} == local")
    );
    let JobV2StepBody::TargetRef(mark_done) = &mark_done.body else {
        panic!("local delivery must mark the epic done");
    };
    assert_eq!(mark_done.target, "activity:update_task");
    assert_eq!(
        mark_done.default_input.as_ref().expect("mark_done input")["status"],
        "done"
    );
}

fn collect_agent_loop_step_ids<'a>(
    step: &'a JobV2Step,
    agent_activity_names: &BTreeSet<&str>,
    out: &mut BTreeSet<&'a str>,
) {
    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            if let Some(activity_name) = target.target.strip_prefix("activity:")
                && agent_activity_names.contains(activity_name)
            {
                out.insert(step.id.as_str());
            }
        }
        JobV2StepBody::Target(target) => {
            if matches!(target.spec, ActivityV2Spec::AgentLoop(_)) {
                out.insert(step.id.as_str());
            }
        }
        JobV2StepBody::Parallel { parallel } => {
            for child in &parallel.branches {
                collect_agent_loop_step_ids(child, agent_activity_names, out);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            collect_agent_loop_step_ids(&fan_out.worker, agent_activity_names, out);
        }
        JobV2StepBody::Loop { loop_ } => {
            for child in &loop_.steps {
                collect_agent_loop_step_ids(child, agent_activity_names, out);
            }
        }
    }
}

fn step_recovery_activities(job: &JobV2) -> Vec<(&str, &Option<String>, bool)> {
    let mut out = Vec::new();
    for step in &job.steps {
        collect_step_recovery_activities(step, &mut out);
    }
    out
}

fn collect_step_recovery_activities<'a>(
    step: &'a JobV2Step,
    out: &mut Vec<(&'a str, &'a Option<String>, bool)>,
) {
    if step.recovery_activity.is_some() {
        out.push((
            step.id.as_str(),
            &step.recovery_activity,
            step.resolved_recovery_activity.is_some(),
        ));
    }
    match &step.body {
        JobV2StepBody::Parallel { parallel } => {
            for child in &parallel.branches {
                collect_step_recovery_activities(child, out);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            collect_step_recovery_activities(&fan_out.worker, out);
        }
        JobV2StepBody::Loop { loop_ } => {
            for child in &loop_.steps {
                collect_step_recovery_activities(child, out);
            }
        }
        JobV2StepBody::TargetRef(_) | JobV2StepBody::Target(_) => {}
    }
}

fn collect_template_strings<'a>(step: &'a JobV2Step, out: &mut Vec<&'a str>) {
    if let Some(when) = &step.when {
        out.push(when);
    }

    match &step.body {
        JobV2StepBody::TargetRef(target) => {
            collect_value_strings(target.default_input.as_ref(), out);
        }
        JobV2StepBody::Target(target) => {
            collect_value_strings(target.default_input.as_ref(), out);
        }
        JobV2StepBody::Parallel { parallel } => {
            for child in &parallel.branches {
                collect_template_strings(child, out);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            out.push(&fan_out.items);
            collect_template_strings(&fan_out.worker, out);
        }
        JobV2StepBody::Loop { loop_ } => {
            if let Some(items) = &loop_.items {
                out.push(items);
            }
            if let Some(break_when) = &loop_.break_when {
                out.push(break_when);
            }
            for child in &loop_.steps {
                collect_template_strings(child, out);
            }
        }
    }
}

fn collect_value_strings<'a>(value: Option<&'a Value>, out: &mut Vec<&'a str>) {
    match value {
        Some(Value::String(text)) => out.push(text),
        Some(Value::Array(items)) => {
            for item in items {
                collect_value_strings(Some(item), out);
            }
        }
        Some(Value::Object(map)) => {
            for item in map.values() {
                collect_value_strings(Some(item), out);
            }
        }
        _ => {}
    }
}

#[test]
fn workspace_job_overrides_global_default_in_catalog_listing() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let global_job = global_root.join("resources/jobs/task_auto_pipeline.yaml");
    let workspace_job = workspace_root.join("resources/jobs/task_auto_pipeline.yaml");
    write_job(&global_job, "task_auto_pipeline", "global_action", 1);
    write_job(&workspace_job, "task_auto_pipeline", "workspace_action", 7);

    let jobs = runtime
        .list_job_catalog_with_last_run(true, JobCatalogFilter::All)
        .expect("list job catalog");
    let matches = jobs
        .iter()
        .filter(|(entry, _)| entry.job_id == "task_auto_pipeline")
        .collect::<Vec<_>>();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].0.path, workspace_job);
    assert_eq!(matches[0].0.spec.max_active_runs, 7);
}

#[test]
fn job_listing_prefers_workspace_over_global() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let workspace_dir = workspace_root.join("resources/jobs");
    let global_dir = global_root.join("resources/jobs");
    write_job(
        &workspace_dir.join("layered.yaml"),
        "layered",
        "workspace",
        7,
    );
    write_job(&global_dir.join("layered.yaml"), "layered", "global", 1);

    let entry = runtime
        .show_job_catalog_entry("layered")
        .expect("layered job");
    assert_eq!(entry.path, workspace_dir.join("layered.yaml"));
    assert_eq!(entry.spec.max_active_runs, 7);
}

#[test]
fn job_execution_prefers_global_over_workspace() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let workspace_dir = workspace_root.join("resources/jobs");
    let global_dir = global_root.join("resources/jobs");
    write_job(&workspace_dir.join("custom.yaml"), "custom", "workspace", 7);
    write_job(&global_dir.join("custom.yaml"), "custom", "global", 1);
    write_job(
        &workspace_dir.join("task_auto_pipeline.yaml"),
        "task_auto_pipeline",
        "workspace",
        7,
    );
    write_job(
        &global_dir.join("task_auto_pipeline.yaml"),
        "task_auto_pipeline",
        "global",
        1,
    );

    let custom = runtime
        .load_v2_job_asset_by_name("custom")
        .expect("load custom catalog");
    assert_eq!(custom.0, global_dir.join("custom.yaml"));

    let default = runtime
        .load_v2_job_asset_by_name("task_auto_pipeline")
        .expect("load default catalog");
    assert_eq!(default.0, global_dir.join("task_auto_pipeline.yaml"));
}

#[test]
fn workspace_job_overrides_global_default_in_catalog_lookup_but_not_execution_lookup() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    let global_job = global_root.join("resources/jobs/task_auto_pipeline.yaml");
    let workspace_job = workspace_root.join("resources/jobs/task_auto_pipeline.yaml");
    write_job(&global_job, "task_auto_pipeline", "global_action", 1);
    write_job(&workspace_job, "task_auto_pipeline", "workspace_action", 7);

    let entry = runtime
        .show_job_catalog_entry("task_auto_pipeline")
        .expect("catalog entry");
    assert_eq!(entry.path, workspace_job);
    assert_eq!(entry.spec.max_active_runs, 7);

    let (path, spec) = runtime
        .load_v2_job_asset_by_name("task_auto_pipeline")
        .expect("job lookup");
    assert_eq!(path, global_job);
    assert_eq!(spec.max_active_runs, 1);
}

#[test]
fn duplicate_jobs_within_one_catalog_directory_remain_invalid() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let jobs_dir = workspace_root.join("resources/jobs");
    write_job(&jobs_dir.join("first.yaml"), "duplicate_job", "first", 1);
    write_job(
        &jobs_dir.join("nested/second.yaml"),
        "duplicate_job",
        "second",
        1,
    );

    let err = runtime
        .show_job_catalog_entry("duplicate_job")
        .expect_err("duplicate job name should fail");
    assert!(
        err.to_string()
            .contains("duplicate v2 job name 'duplicate_job'"),
        "{err}"
    );
}

#[test]
fn malformed_job_assets_remain_hard_catalog_errors() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let malformed = workspace_root.join("resources/jobs/malformed.yaml");
    std::fs::create_dir_all(malformed.parent().expect("job path has parent"))
        .expect("create jobs dir");
    std::fs::write(&malformed, "schemaVersion: 2\nkind: Job\nspec: [")
        .expect("write malformed job");

    let err = runtime
        .list_job_catalog_with_last_run(true, JobCatalogFilter::All)
        .expect_err("malformed job should fail catalog loading");
    assert!(err.to_string().contains("malformed.yaml"), "{err}");
    assert!(err.to_string().contains("parse"), "{err}");
}

/// [ORB-11187] The completion policy is one shared input threaded through every
/// job boundary rather than parallel per-surface behavior. This pins both ends:
/// each pipeline defaults it to `review`, and each dispatching pipeline forwards
/// its own `input.completion` to its children.
#[test]
fn completion_policy_defaults_to_review_and_propagates_across_job_boundaries() {
    fn job(name: &str) -> JobV2 {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(job_name, yaml)| (*job_name == name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {name} exists"));
        load_job_asset(yaml)
            .unwrap_or_else(|error| panic!("parse {name}: {error}"))
            .spec
    }

    // Every pipeline that participates must default to the review-ending
    // behavior, so an omitted policy can never be read as authorization.
    for name in [
        "workspace_auto_pipeline",
        "task_auto_pipeline",
        "task_gate_pipeline",
        "task_local_pipeline",
        "task_pr_pipeline",
        "epic_pipeline",
    ] {
        assert_eq!(
            job(name).default_input.as_ref().expect("default input")["completion"],
            "review",
            "{name} must default to ending successful work at review"
        );
    }

    // workspace auto -> task auto (detached leaves) and -> epic. Both are read
    // from `input`, not from a step output captured once, so every drain
    // iteration forwards the same authorization to newly discovered work.
    let workspace_auto = job("workspace_auto_pipeline");
    let drain = workspace_auto
        .steps
        .iter()
        .find(|step| step.id == "drain")
        .expect("workspace auto drain loop");
    let JobV2StepBody::Loop { loop_ } = &drain.body else {
        panic!("workspace auto drain must be a loop");
    };
    let ship_leaves = loop_
        .steps
        .iter()
        .find(|step| step.id == "ship_leaves")
        .expect("ship_leaves step");
    let JobV2StepBody::FanOut { fan_out, .. } = &ship_leaves.body else {
        panic!("ship_leaves must be a fan-out");
    };
    let JobV2StepBody::TargetRef(leaf_invoke) = &fan_out.worker.body else {
        panic!("leaf worker must reference invoke_detached");
    };
    assert_eq!(
        leaf_invoke.default_input.as_ref().expect("leaf input")["run_input"]["completion"],
        "{{ input.completion }}",
        "detached leaves must inherit the drain's completion authorization"
    );

    let start_epic = loop_
        .steps
        .iter()
        .find(|step| step.id == "start_epic")
        .expect("start_epic step");
    let JobV2StepBody::TargetRef(start_epic) = &start_epic.body else {
        panic!("start_epic must reference invoke_detached");
    };
    assert_eq!(
        start_epic.default_input.as_ref().expect("epic input")["run_input"]["completion"],
        "{{ input.completion }}"
    );

    // task auto -> gate.
    let task_auto = job("task_auto_pipeline");
    let dispatch = task_auto
        .steps
        .iter()
        .find(|step| step.id == "dispatch")
        .expect("task auto dispatch");
    let JobV2StepBody::FanOut { fan_out, .. } = &dispatch.body else {
        panic!("task auto dispatch must be a fan-out");
    };
    let JobV2StepBody::TargetRef(gate_invoke) = &fan_out.worker.body else {
        panic!("gate worker must reference invoke_and_wait");
    };
    assert_eq!(
        gate_invoke.default_input.as_ref().expect("gate input")["run_input"]["completion"],
        "{{ input.completion }}"
    );

    // gate -> leaf.
    let gate = job("task_gate_pipeline");
    let dispatch_child = gate
        .steps
        .iter()
        .find(|step| step.id == "dispatch_child")
        .expect("gate dispatch_child");
    let JobV2StepBody::TargetRef(dispatch_child) = &dispatch_child.body else {
        panic!("gate dispatch_child must reference invoke_and_wait");
    };
    assert_eq!(
        dispatch_child.default_input.as_ref().expect("child input")["run_input"]["completion"],
        "{{ input.completion }}"
    );
}

/// [ORB-11187] Local completion must be unreachable unless every publication
/// step this invocation required already succeeded, so the terminal transition
/// is ordered after both the merge and the push.
#[test]
fn local_pipeline_completes_tasks_only_after_merge_and_push() {
    let yaml = DEFAULT_JOB_FILES
        .iter()
        .find_map(|(name, yaml)| (*name == "task_local_pipeline").then_some(*yaml))
        .expect("task local pipeline default exists");
    let asset = load_job_asset(yaml).expect("parse task local pipeline");
    let step_ids = asset
        .spec
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    let index = |id: &str| {
        step_ids
            .iter()
            .position(|candidate| *candidate == id)
            .unwrap_or_else(|| panic!("task local pipeline has a {id} step"))
    };

    assert!(
        index("merge") < index("complete_tasks"),
        "a failed merge must fail the run before any task can reach done"
    );
    assert!(
        index("push") < index("complete_tasks"),
        "a failed push must fail the run before any task can reach done"
    );

    let complete = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "complete_tasks")
        .expect("complete_tasks step");
    assert_eq!(
        complete.when.as_deref(),
        Some("{{ input.completion }} == done"),
        "completion must be gated on the explicit authorization"
    );
    let JobV2StepBody::Loop { loop_ } = &complete.body else {
        panic!("complete_tasks must be a loop over the bundle");
    };
    let JobV2StepBody::TargetRef(complete_one) = &loop_.steps[0].body else {
        panic!("complete_tasks must reference task_complete");
    };
    assert_eq!(complete_one.target, "activity:task_complete");
}

/// [ORB-11187] PR-mode completion runs after the PR is opened, is gated on the
/// authorization, and routes no-diff work down a path that needs no PR.
#[test]
fn pr_pipelines_complete_only_when_authorized_and_handle_no_diff_without_a_pr() {
    for job_name in ["task_pr_pipeline", "epic_pipeline"] {
        let yaml = DEFAULT_JOB_FILES
            .iter()
            .find_map(|(name, yaml)| (*name == job_name).then_some(*yaml))
            .unwrap_or_else(|| panic!("default job {job_name} exists"));
        let asset =
            load_job_asset(yaml).unwrap_or_else(|error| panic!("parse {job_name}: {error}"));

        let no_diff_id = if job_name == "task_pr_pipeline" {
            "complete_no_diff"
        } else {
            "complete_pr_no_diff"
        };

        let complete = asset
            .spec
            .steps
            .iter()
            .find(|step| step.id == "complete_pr")
            .unwrap_or_else(|| panic!("{job_name} has a complete_pr step"));
        let when = complete.when.as_deref().unwrap_or_default();
        assert!(
            when.contains("{{ input.completion }} == done"),
            "{job_name} completion must be gated on the authorization: {when}"
        );
        let JobV2StepBody::TargetRef(complete) = &complete.body else {
            panic!("{job_name} complete_pr must reference pr_complete");
        };
        assert_eq!(complete.target, "activity:pr_complete");

        let no_diff = asset
            .spec
            .steps
            .iter()
            .find(|step| step.id == no_diff_id)
            .unwrap_or_else(|| panic!("{job_name} has a {no_diff_id} step"));
        let when = no_diff.when.as_deref().unwrap_or_default();
        assert!(
            when.contains("{{ input.completion }} == done"),
            "{job_name} no-diff completion must be gated on the authorization: {when}"
        );

        // task_pr_pipeline's `skipped_no_diff_expected` is tag-derived (its
        // `commit` step passes no `allow_empty`), so its no-diff completion
        // still asserts the tag through `pr_complete`, exactly like
        // `pr_promote`'s equivalent guard. epic_pipeline's `commit_delivery`
        // passes `allow_empty: true` unconditionally, so the same signal does
        // NOT imply the tag there; it must route through the tag-agnostic
        // `task_complete` instead, matching `promote_pr_no_diff` routing
        // around `pr_promote`'s guard on the same branch.
        if job_name == "task_pr_pipeline" {
            let JobV2StepBody::TargetRef(no_diff) = &no_diff.body else {
                panic!("{job_name} {no_diff_id} must reference pr_complete");
            };
            assert_eq!(no_diff.target, "activity:pr_complete");
            let input = no_diff.default_input.as_ref().expect("no-diff input");
            assert_eq!(input["no_diff_expected"], true);
            assert_eq!(
                input["already_landed_checkpoint"],
                "{{ steps.commit.output }}"
            );
            assert!(
                input.get("pr_number").is_none(),
                "{job_name} no-diff completion must not require a nonexistent PR"
            );
        } else {
            let JobV2StepBody::TargetRef(no_diff) = &no_diff.body else {
                panic!("{job_name} {no_diff_id} must reference task_complete");
            };
            assert_eq!(
                no_diff.target, "activity:task_complete",
                "epic no-diff completion must not carry pr_complete's tag guard"
            );
            let input = no_diff.default_input.as_ref().expect("no-diff input");
            assert!(
                input.get("no_diff_expected").is_none(),
                "task_complete has no tag guard to gate"
            );
            assert!(
                input.get("pr_number").is_none(),
                "{job_name} no-diff completion must not require a nonexistent PR"
            );
        }
    }
}
