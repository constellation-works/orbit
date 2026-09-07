//! Deterministic admission tests over the real task and run stores.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

use super::exec::test_runtime_with_workspace_config;

fn task(runtime: &OrbitRuntime, complexity: TaskComplexity, crew: Option<&str>) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "Crew pool admission fixture".into(),
            description: "Exercise automatic crew selection".into(),
            plan: "Inspect admission evidence".into(),
            complexity,
            crew: crew.map(ToOwned::to_owned),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task")
}

fn no_draw() -> Result<u64, OrbitError> {
    panic!("this admission must not draw a new random ticket")
}

fn coordinator(runtime: &OrbitRuntime, mut input: Value) -> String {
    runtime
        .install_auto_crew_admission(
            "workspace_auto_pipeline",
            &mut input,
            None,
            false,
            &mut no_draw,
        )
        .expect("capture coordinator policy");
    persist(runtime, "workspace_auto_pipeline", input)
}

fn persist(runtime: &OrbitRuntime, job: &str, input: Value) -> String {
    runtime
        .stores()
        .jobs()
        .insert_job_run(job, 1, Utc::now(), Some(input), None)
        .expect("persist run input")
        .run_id
}

fn admit(runtime: &OrbitRuntime, parent: &str, task: &Task, ticket: u64) -> Value {
    let mut input = json!({"task_ids": [task.id]});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(parent),
            false,
            &mut || Ok(ticket),
        )
        .expect("admit task");
    input
}

#[test]
fn complexity_pools_override_matching_config_and_select_independently_without_weighting() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        r#"
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["astra"]
hard_complexity_crews = ["sol"]
"#,
    );
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["terra", "grok", "terra"]}),
    );
    let first = task(&runtime, TaskComplexity::Medium, None);
    let second = task(&runtime, TaskComplexity::Medium, None);
    let left = admit(&runtime, &parent, &first, 0);
    let right = admit(&runtime, &parent, &second, 1);
    assert_eq!(left["crew"], "grok");
    assert_eq!(right["crew"], "terra");
    assert_eq!(
        left["crew_selection"]["source"],
        "run_input.medium_complexity_crews"
    );
    assert_eq!(
        left["crew_selection"]["eligible_pool"],
        json!(["grok", "terra"])
    );
    assert!(left.get("allowed_crews").is_none());
    assert_eq!(
        runtime.get_task(&first.id).expect("unchanged task").crew,
        None
    );
    for (complexity, crew) in [(TaskComplexity::Low, "luna"), (TaskComplexity::Hard, "sol")] {
        let selected = admit(&runtime, &parent, &task(&runtime, complexity, None), 0);
        assert_eq!(selected["crew"], crew);
        assert_eq!(
            selected["crew_selection"]["source"],
            format!("workflow.{complexity}_complexity_crews")
        );
    }
}

#[test]
fn explicit_task_crews_empty_pools_and_unassessed_tasks_preserve_fallback() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    );
    let parent = coordinator(&runtime, json!({}));
    let assigned = task(&runtime, TaskComplexity::Medium, Some("astra"));
    let mut input = json!({"task_ids": [assigned.id]});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(&parent),
            false,
            &mut no_draw,
        )
        .expect("manual assignment");
    assert_eq!(input["crew"], "astra");
    assert_eq!(input["crew_selection"]["source"], "task.crew");
    let disabled = coordinator(&runtime, json!({"medium_complexity_crews": []}));
    assert_eq!(
        admit(
            &runtime,
            &disabled,
            &task(&runtime, TaskComplexity::Medium, None),
            0
        )["crew"],
        "opus"
    );
    assert_eq!(
        admit(
            &runtime,
            &parent,
            &task(&runtime, TaskComplexity::Unassessed, None),
            0
        )["crew"],
        "opus"
    );
    let pools = runtime
        .auto_crew_pools_for_input(&json!({"run_id": parent}))
        .expect("pools");
    let mut unset = assigned;
    unset.crew = None;
    unset.complexity = None;
    let (candidates, source) = runtime
        .auto_task_crew_candidates(&unset, &pools, None)
        .expect("legacy unset complexity");
    assert_eq!(candidates[0].name, "opus");
    assert_eq!(source, "default");
}

#[test]
fn pool_constraints_filter_candidates_and_diagnose_disjoint_or_explicit_crews() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok", "terra"], "allowed_crews": ["terra"]}),
    );
    let unassigned = task(&runtime, TaskComplexity::Medium, None);
    let mut input = json!({"task_ids": [unassigned.id], "allowed_crews": ["grok"]});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(&parent),
            false,
            &mut no_draw,
        )
        .expect("only permitted member");
    assert_eq!(input["crew"], "terra");
    assert_eq!(input["allowed_crews"], json!(["terra"]));
    let disjoint = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok", "terra"], "allowed_crews": ["astra"]}),
    );
    let mut rejected = json!({"task_ids": [unassigned.id]});
    let error = runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut rejected,
            Some(&disjoint),
            false,
            &mut no_draw,
        )
        .expect_err("disjoint pool");
    assert!(error.to_string().contains("no member permitted"), "{error}");
    let assigned = task(&runtime, TaskComplexity::Medium, Some("grok"));
    let mut rejected = json!({"task_ids": [assigned.id]});
    let error = runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut rejected,
            Some(&parent),
            false,
            &mut no_draw,
        )
        .expect_err("manual excluded");
    assert!(error.to_string().contains("task.crew"), "{error}");
}

#[test]
fn selection_survives_persistence_reopen_same_task_children_and_resume_without_reroll() {
    let (_root, runtime, repo, global) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok", "terra"]}),
    );
    let selected_task = task(&runtime, TaskComplexity::Medium, None);
    let selected = admit(&runtime, &parent, &selected_task, 1);
    let child = persist(&runtime, "task_auto_pipeline", selected.clone());
    drop(runtime);
    std::fs::write(
        repo.join(".orbit/config.toml"),
        "[workflow]\ndefault_crew = \"astra\"\nmedium_complexity_crews = [\"astra\"]\n",
    )
    .expect("change configuration");
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).expect("reopen");
    let stored = runtime
        .get_job_run_backend(&child)
        .expect("read")
        .expect("child")
        .input
        .expect("input");
    assert_eq!(stored, selected);
    let mut nested = json!({"task_ids": [selected_task.id]});
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut nested,
            Some(&child),
            false,
            &mut no_draw,
        )
        .expect("inherit selection");
    assert_eq!(nested["crew_selection"], stored["crew_selection"]);
    let mut resumed = stored.clone();
    runtime
        .install_auto_crew_admission("task_auto_pipeline", &mut resumed, None, true, &mut no_draw)
        .expect("resume");
    assert_eq!(resumed, stored);
    assert_eq!(
        runtime
            .resolve_crew_for_run_input(&resumed)
            .expect("dispatch crew")
            .name,
        "terra"
    );
    let independent = task(&runtime, TaskComplexity::Medium, None);
    assert_eq!(
        admit(&runtime, &parent, &independent, 0)["crew"],
        "grok",
        "old coordinator retains its pool"
    );
}

#[test]
fn unknown_blank_and_wrong_shape_pools_fail_before_admission() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    for bad in [
        json!(["missing"]),
        json!([""]),
        json!(["grok", " "]),
        json!("grok"),
        json!([1]),
    ] {
        let mut input = json!({"medium_complexity_crews": bad});
        let error = runtime
            .install_auto_crew_admission(
                "workspace_auto_pipeline",
                &mut input,
                None,
                false,
                &mut no_draw,
            )
            .expect_err("invalid pool");
        assert!(
            error.to_string().contains("medium_complexity_crews"),
            "{error}"
        );
    }
}

#[test]
fn random_sampling_rejects_biased_ticket_and_propagates_entropy_failure() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["astra", "grok", "terra"]}),
    );
    let selected_task = task(&runtime, TaskComplexity::Medium, None);
    let mut input = json!({"task_ids": [selected_task.id]});
    let mut tickets = [0, 2].into_iter();
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(&parent),
            false,
            &mut || Ok(tickets.next().expect("two tickets")),
        )
        .expect("unbiased selection");
    assert_eq!(input["crew"], "terra");
    assert_eq!(tickets.next(), None);
    let mut input = json!({"task_ids": [selected_task.id]});
    let error = runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(&parent),
            false,
            &mut || Err(OrbitError::Execution("entropy unavailable".into())),
        )
        .expect_err("entropy error");
    assert!(error.to_string().contains("entropy unavailable"));
    assert!(input.get("crew_selection").is_none());
}

#[test]
fn epic_descendants_draw_independently_and_system_jobs_keep_their_crew() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok", "terra"]}),
    );
    let root = task(&runtime, TaskComplexity::Medium, None);
    let mut epic = json!({"epic_task_id": root.id});
    runtime
        .install_auto_crew_admission(
            "epic_pipeline",
            &mut epic,
            Some(&parent),
            false,
            &mut || Ok(0),
        )
        .expect("epic admission");
    assert_eq!(epic["crew"], "grok");
    let epic_run = persist(&runtime, "epic_pipeline", epic);
    let descendant = task(&runtime, TaskComplexity::Medium, None);
    let mut child = json!({"task_ids": [descendant.id]});
    runtime
        .install_auto_crew_admission(
            "task_local_pipeline",
            &mut child,
            Some(&epic_run),
            false,
            &mut || Ok(1),
        )
        .expect("descendant admission");
    assert_eq!(child["crew"], "terra");
    assert_eq!(child["crew_selection"]["task_id"], descendant.id);
    let mut system = json!({"task_id": root.id, "crew": "astra"});
    let original = system.clone();
    runtime
        .install_auto_crew_admission(
            "task_triage_pipeline",
            &mut system,
            Some(&epic_run),
            false,
            &mut no_draw,
        )
        .expect("independent system route");
    assert_eq!(system, original);
}

#[test]
fn explicit_run_crew_wins_and_ordinary_ship_admission_has_no_pool_policy() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    );
    let parent = coordinator(&runtime, json!({}));
    let assigned = task(&runtime, TaskComplexity::Medium, Some("astra"));
    let mut input = json!({"task_ids": [assigned.id], "crew": "luna"});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut input,
            Some(&parent),
            false,
            &mut no_draw,
        )
        .expect("explicit run assignment");
    assert_eq!(input["crew"], "luna");
    assert_eq!(input["crew_selection"]["source"], "explicit");
    let ship = persist(&runtime, "workspace_ship_pipeline", json!({}));
    let mut ordinary = json!({"task_ids": [assigned.id]});
    let original = ordinary.clone();
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut ordinary,
            Some(&ship),
            false,
            &mut no_draw,
        )
        .expect("manual ship");
    assert_eq!(ordinary, original);
    assert_eq!(
        runtime
            .resolve_crew_for_run_input(&ordinary)
            .expect("manual crew")
            .name,
        "astra"
    );
}
