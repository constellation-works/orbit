//! A task-pilot pool member written `<crew>@auto` runs each partition at the
//! effort its filed tier's band chooses. A pilot that raises the task to a
//! tier mapped to a higher effort, or reports confidence below `high`, is
//! re-piloted once at that effort, and that assessment is the one applied.
//!
//! Each scenario asserts its bands' efforts, so it runs with effort
//! exploration off: a fixed share of seeded draws would otherwise run another
//! effort the bands name, on a seed that changes from run to run.

use std::os::unix::fs::PermissionsExt;

use orbit_types::resource::ExecutorResource;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, JobRunState};
use serde_json::json;

use super::*;

const CONFIG: &str = "\
[crews.fixture]\nmodel = \"fixture-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n\
[workflow]\ndefault_crew = \"fixture\"\n\
[workflow.purpose_crews]\ntask_pilot = [\"fixture@auto\"]\n\
[[workflow.auto_effort.bands.fixture]]\neffort = \"low\"\ncomplexity = [\"low\"]\n\
[[workflow.auto_effort.bands.fixture]]\neffort = \"high\"\ncomplexity = [\"hard\"]\n";

fn filed_low_task(workspace: &Workspace) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "auto effort subject".into(),
            description: "Prepare the auto effort subject.".into(),
            acceptance_criteria: vec!["Selectors identify the implementation scope.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Proposed),
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .unwrap()
}

/// Scripted codex CLI answering every task it is shown with `complexity` and
/// `confidence`. Each invocation appends the reasoning effort it was run at
/// to the returned file.
fn install_provider(workspace: &Workspace, complexity: &str, confidence: &str) -> PathBuf {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let calls = workspace.root.path().join("provider-efforts");
    let provider = workspace.root.path().join("codex");
    std::fs::write(&calls, "").unwrap();
    std::fs::write(
        &provider,
        format!(
            r#"#!/usr/bin/env python3
import json, re, subprocess, sys
raw = sys.stdin.read()
effort = "unset"
for arg in sys.argv:
    match = re.fullmatch(r'model_reasoning_effort="?(\w+)"?', arg)
    if match:
        effort = match.group(1)
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
    "recommended_complexity": {complexity:?},
    "confidence": {confidence:?},
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
head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
tree = subprocess.check_output(["git", "rev-parse", "HEAD^{{tree}}"], text=True).strip()
sys.stdout.write(json.dumps({{
    "schemaVersion": 1,
    "status": "success",
    "error": None,
    "result": {{"partition_index": partition, "observed_revision": head, "observed_tree": tree, "task_ids": found, "tasks": tasks}},
}}))
with open({calls:?}, "a", encoding="utf-8") as handle:
    handle.write(effort + "\n")
"#,
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

/// Pilot `task` through the production job; returns the run id.
fn pilot(workspace: &Workspace, task: &Task) -> String {
    workspace.install_pilot_job();
    let run = workspace
        .jobs
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task.id],
                "workspace_path": workspace.repo,
                "base_branch": "main",
            })),
            None,
        )
        .unwrap();
    workspace
        .runtime
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), "task_pilot_pipeline".into(), json!({})),
        )
        .unwrap();
    workspace
        .runtime
        .execute_pipeline_run_worker(&run.run_id)
        .unwrap_or_else(|error| panic!("the pilot run must finish: {error}"));
    run.run_id
}

fn efforts(calls: &Path) -> Vec<String> {
    std::fs::read_to_string(calls)
        .unwrap()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn partition_crew(workspace: &Workspace, run_id: &str, round: &str) -> Option<String> {
    workspace
        .runtime
        .read_run_state(run_id)
        .unwrap()
        .unwrap()
        .activity_crew_draws
        .get(&format!("workflow.purpose_crews.task_pilot#{round}:0"))
        .map(|draw| draw.crew.clone())
}

fn a_raised_tier_is_repiloted_once_at_its_higher_effort() {
    orbit_core::test_support::without_auto_effort_exploration(|| {
        let workspace = Workspace::with_config(CONFIG);
        let calls = install_provider(&workspace, "hard", "high");
        let task = filed_low_task(&workspace);

        let run_id = pilot(&workspace, &task);

        let run = workspace.runtime.show_job_run(&run_id).unwrap();
        assert_eq!(run.state, JobRunState::Success, "{run:?}");
        assert_eq!(
            efforts(&calls),
            ["low", "high"],
            "the filed tier pilots at low, the raised tier re-pilots once at high"
        );
        assert_eq!(
            partition_crew(&workspace, &run_id, "pilots").as_deref(),
            Some("fixture@low")
        );
        assert_eq!(
            partition_crew(&workspace, &run_id, "repair_pilots").as_deref(),
            Some("fixture@high")
        );
        let pipeline = workspace
            .runtime
            .read_run_state(&run_id)
            .unwrap()
            .unwrap()
            .pipeline;
        assert_eq!(
            pipeline["apply"]["task_outcomes"][0]["outcome"], "escalated",
            "{pipeline}"
        );
        assert_eq!(pipeline["apply"]["applied_count"], 0, "{pipeline}");
        assert_eq!(pipeline["apply_repairs"]["applied_count"], 1, "{pipeline}");
        assert_eq!(
            pipeline["apply_repairs"]["tasks"][0]["pilot_crew"], "fixture@high",
            "the applied assessment records the effort it ran at: {pipeline}"
        );
        let assessed = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(assessed.complexity, Some(TaskComplexity::Hard));
        assert_eq!(assessed.context_files, ["file:README.md"]);
    });
}

fn a_doubtful_pilot_is_repiloted_at_the_highest_band_effort() {
    orbit_core::test_support::without_auto_effort_exploration(|| {
        let workspace = Workspace::with_config(CONFIG);
        let calls = install_provider(&workspace, "low", "medium");
        let task = filed_low_task(&workspace);

        let run_id = pilot(&workspace, &task);

        let run = workspace.runtime.show_job_run(&run_id).unwrap();
        assert_eq!(run.state, JobRunState::Success, "{run:?}");
        assert_eq!(
            efforts(&calls),
            ["low", "high"],
            "a medium-confidence pilot re-pilots once; the repair never escalates again"
        );
    });
}

fn a_confident_pilot_at_its_filed_tier_applies_without_escalation() {
    orbit_core::test_support::without_auto_effort_exploration(|| {
        let workspace = Workspace::with_config(CONFIG);
        let calls = install_provider(&workspace, "low", "high");
        let task = filed_low_task(&workspace);

        let run_id = pilot(&workspace, &task);

        let run = workspace.runtime.show_job_run(&run_id).unwrap();
        assert_eq!(run.state, JobRunState::Success, "{run:?}");
        assert_eq!(efforts(&calls), ["low"]);
        let pipeline = workspace
            .runtime
            .read_run_state(&run_id)
            .unwrap()
            .unwrap()
            .pipeline;
        assert_eq!(pipeline["apply"]["applied_count"], 1, "{pipeline}");
        assert_eq!(
            pipeline["apply"]["tasks"][0]["pilot_crew"], "fixture@low",
            "{pipeline}"
        );
    });
}

crate::scenario_table!(
    auto_effort_task_pilot_scenarios,
    a_raised_tier_is_repiloted_once_at_its_higher_effort,
    a_doubtful_pilot_is_repiloted_at_the_highest_band_effort,
    a_confident_pilot_at_its_filed_tier_applies_without_escalation
);
