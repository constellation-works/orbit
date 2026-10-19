//! One task-pilot preparation pass assesses siblings whose stored crews differ,
//! as separate partitions. A hand-assembled mixed list is still refused before
//! any provider runs.

use std::os::unix::fs::PermissionsExt;

use orbit_core::application::automation::pin_attempt_source;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::KeyedJobRunAdmission;
use orbit_types::resource::ExecutorResource;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, JobRunState};
use serde_json::json;

use super::*;

const CREWS: &str = "\
[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
[crews.haiku]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
[crews.sol]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
[workflow]\ndefault_crew = \"fixture\"\n";

fn workspace() -> Workspace {
    Workspace::with_config(CREWS)
}

fn task(workspace: &Workspace, title: &str, crew: &str) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: format!("Prepare {title}."),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            crew: Some(crew.into()),
            ..Default::default()
        })
        .unwrap()
}

/// Scripted codex CLI. Each invocation answers the partition whose `task_ids`
/// appear last in its prompt, and appends one line to `calls`.
fn install_provider(workspace: &Workspace) -> std::path::PathBuf {
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let calls = workspace.root.path().join("provider-calls");
    let provider = workspace.root.path().join("codex");
    std::fs::write(&calls, "").unwrap();
    std::fs::write(
        &provider,
        format!(
            r#"#!/usr/bin/env python3
import json, re, sys
raw = sys.stdin.read()
found = []
for blob in re.findall(r'"task_ids"\s*:\s*(\[[^\]]*\])', raw):
    try:
        parsed = json.loads(blob)
    except json.JSONDecodeError:
        continue
    ids = [item for item in parsed if isinstance(item, str) and re.fullmatch(r"ORB-\d+", item)]
    if ids:
        found = ids
indexes = [int(value) for value in re.findall(r'"partition_index"\s*:\s*(\d+)', raw)]
partition = indexes[-1] if indexes else 0
if not found:
    sys.stderr.write("provider saw no task ids\n")
    sys.exit(1)
tasks = [{{
    "task_id": task_id,
    "context_files_after": ["file:README.md"],
    "disposition": "selectors",
    "recommended_crew": "fixture",
    "recommended_complexity": "low",
    "confidence": "high",
    "assessment_rationale": "README.md contains the affected material.",
    "validation_approach": "Inspect the persisted scope.",
    "evidence_gaps": [],
    "reassessment_triggers": [],
    "blocked_by": [],
    "adr_conflicts": [],
    "utility_warnings": [],
    "surface_warnings": [],
    "duplicate_of": None,
    "already_landed": None,
}} for task_id in found]
sys.stdout.write(json.dumps({{
    "schemaVersion": 1,
    "status": "success",
    "error": None,
    "result": {{"partition_index": partition, "task_ids": found, "tasks": tasks}},
}}))
with open({calls:?}, "a", encoding="utf-8") as handle:
    handle.write("call\n")
"#,
            calls = calls,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut executor: ExecutorResource = serde_yaml::from_str(
        &std::fs::read_to_string(assets.join("executors/codex.yaml")).unwrap(),
    )
    .unwrap();
    executor.spec.command = Some(provider.display().to_string());
    executor.spec.sandbox = Some(ExecutorSandboxKind::Off);
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef::from_resource_spec(
            executor.metadata.name,
            executor.spec,
            None,
            None,
        ))
        .unwrap();
    calls
}

/// The preparation run that owns this batch. Its input is the production
/// claim: `state_automation` plus every member's task id. Prepare accepts
/// that claim only from this run.
fn claimed_preparation_run(workspace: &Workspace, tasks: &[&Task]) -> String {
    let store = workspace.runtime.automation_store().unwrap();
    let state = workspace.routine_state();
    let members = tasks
        .iter()
        .map(|task| state.members.as_ref().unwrap().pending[&task.id].clone())
        .collect::<Vec<_>>();
    let keys = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    let id = super::digest(format!("attempt:{}", keys.join(",")).as_bytes());
    let now = Utc::now();
    let mut attempt = MemberAttempt {
        consumer: state.consumer.clone(),
        kind: StateTriggerKind::PreparationEligible,
        action_key: format!("automation:{id}:1"),
        id,
        member: members[0].clone(),
        members,
        attempt: 1,
        max_attempts: 2,
        deadline: now + Duration::minutes(90),
        retry_after: now,
        action_id: None,
        exhausted: false,
    };
    let mut claimed = state.clone();
    claimed.generation += 1;
    claimed.members.as_mut().unwrap().active = Some(attempt.clone());
    assert!(store.automation_commit(&state, &claimed, None).unwrap());

    let input = json!({
        "state_automation": attempt,
        "task_ids": attempt.task_ids(),
        "source_revision": attempt.member.source.commit,
        "base_branch": "main",
        "workspace_path": workspace.repo,
        "max_tasks": tasks.len(),
        "promotion_authorized": false,
    });
    let KeyedJobRunAdmission::Admitted(run) = workspace
        .jobs
        .insert_automation_job_run("task_pilot_pipeline", input, &attempt.action_key)
        .unwrap()
    else {
        panic!("a fresh fixture attempt must admit its own run");
    };
    attempt.action_id = Some(run.run_id.clone());
    let mut acknowledged = claimed.clone();
    acknowledged.generation += 1;
    acknowledged.members.as_mut().unwrap().active = Some(attempt.clone());
    assert!(
        store
            .automation_commit(&claimed, &acknowledged, None)
            .unwrap()
    );
    pin_attempt_source(&workspace.runtime, &attempt).unwrap();
    run.run_id
}

fn install_routine(workspace: &Workspace) {
    let routines = workspace.runtime.shared_root().join("routines");
    std::fs::create_dir_all(&routines).unwrap();
    std::fs::write(
        routines.join("fixture-pilot.yaml"),
        serde_yaml::to_string(&pilot_routine()).unwrap(),
    )
    .unwrap();
}

fn a_preparation_pass_assesses_two_crews_as_separate_partitions() {
    let workspace = workspace();
    let calls = install_provider(&workspace);
    install_routine(&workspace);
    let haiku = task(&workspace, "haiku sibling", "haiku");
    let sol = task(&workspace, "sol sibling", "sol");
    assert_eq!(haiku.crew.as_deref(), Some("haiku"));
    assert_eq!(sol.crew.as_deref(), Some("sol"));
    let routine = pilot_routine();
    let now = Utc::now();
    let pending = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    assert_eq!(pending.reason, "debouncing", "{pending:?}");
    let due = evaluate_routine(
        &workspace.runtime,
        &routine,
        true,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(due.reason, "would_fire", "{due:?}");
    let bundled = due
        .batch
        .iter()
        .flat_map(|member| member.task_ids.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        bundled,
        vec![haiku.id.clone(), sol.id.clone()],
        "one pass bundles both crews: {due:?}"
    );

    workspace.install_pilot_job();
    let run_id = claimed_preparation_run(&workspace, &[&haiku, &sol]);
    workspace
        .runtime
        .execute_pipeline_run_worker(&run_id)
        .unwrap_or_else(|error| panic!("mixed preparation bundle must not be refused: {error}"));
    let run = workspace.runtime.show_job_run(&run_id).unwrap();
    assert_eq!(
        run.state,
        JobRunState::Success,
        "one pass assesses both crews: {run:?}"
    );
    assert_eq!(
        run.resolved_crew.as_deref(),
        Some("haiku"),
        "the pass records the first task's crew: {run:?}"
    );
    let pipeline = workspace
        .runtime
        .read_run_state(&run_id)
        .unwrap()
        .unwrap()
        .pipeline;
    let prepared = &pipeline["prepare"];
    assert_eq!(
        prepared["task_ids"],
        json!([haiku.id, sol.id]),
        "{prepared}"
    );
    assert_eq!(prepared["partition_count"], 2, "{prepared}");
    assert_eq!(
        prepared["partitions"],
        json!([
            {"partition_index": 0, "task_ids": [haiku.id]},
            {"partition_index": 1, "task_ids": [sol.id]},
        ]),
        "{prepared}"
    );
    assert_eq!(pipeline["apply"]["applied_count"], 2, "{pipeline}");
    for id in [&haiku.id, &sol.id] {
        assert_eq!(
            workspace.runtime.get_task(id).unwrap().context_files,
            ["file:README.md"],
            "{id} was not assessed"
        );
    }
    assert_eq!(
        std::fs::read_to_string(calls).unwrap().lines().count(),
        2,
        "each crew partition is assessed once"
    );
}

fn an_operator_mixed_crew_bundle_is_still_refused() {
    let workspace = workspace();
    let calls = install_provider(&workspace);
    let haiku = task(&workspace, "haiku sibling", "haiku");
    let sol = task(&workspace, "sol sibling", "sol");
    let before = [&haiku.id, &sol.id].map(|id| workspace.runtime.get_task(id).unwrap());
    let job = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/jobs/task_pilot_pipeline.yaml");
    let error = workspace
        .runtime
        .run_job_v2_from_yaml(
            &job,
            json!({
                "task_ids": [haiku.id, sol.id],
                "workspace_path": workspace.repo,
                "base_branch": "main",
            }),
        )
        .expect_err("a hand-assembled mixed bundle must be refused")
        .to_string();
    assert!(
        error.contains(
            "task bundle mixes crews `haiku` and `sol`; split the bundle or assign one crew instead of inheriting workflow.default_crew"
        ),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "", "{error}");
    for task in before {
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), task);
    }
}

crate::scenario_table!(
    mixed_crew_task_pilot_bundles_scenarios,
    a_preparation_pass_assesses_two_crews_as_separate_partitions,
    an_operator_mixed_crew_bundle_is_still_refused
);
