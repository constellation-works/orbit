//! Deterministic admission tests over the real task and run stores.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

use super::exec::test_runtime_with_workspace_config;

/// A task record written straight to the store, so `crew` is exactly what the
/// case asks for. [ORB-12717] moved automatic assignment to `add_task`, so a
/// crew-less record is now only reachable this way — which is precisely the
/// legacy shape dispatch-time pool routing still has to serve.
fn task(runtime: &OrbitRuntime, complexity: TaskComplexity, crew: Option<&str>) -> Task {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: "Crew pool admission fixture".to_string(),
            description: "Exercise automatic crew selection".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "Inspect admission evidence".to_string(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: Some(complexity),
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: crew.map(ToOwned::to_owned),
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("task")
}

fn added_task(runtime: &OrbitRuntime, complexity: TaskComplexity, crew: Option<&str>) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "Crew assignment fixture".into(),
            description: "Exercise creation-time crew assignment".into(),
            plan: "Inspect creation evidence".into(),
            complexity,
            crew: crew.map(ToOwned::to_owned),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task")
}

fn crew_assigned_notes(runtime: &OrbitRuntime, task_id: &str) -> Vec<String> {
    runtime
        .get_task_history(task_id)
        .expect("history")
        .into_iter()
        .filter(|entry| entry.event == "crew_assigned")
        .filter_map(|entry| entry.note)
        .collect()
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
xhard_complexity_crews = ["fable"]
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
        json!([{"name": "grok", "weight": 1}, {"name": "terra", "weight": 1}])
    );
    assert!(left.get("allowed_crews").is_none());
    assert_eq!(
        runtime.get_task(&first.id).expect("unchanged task").crew,
        None
    );
    for (complexity, crew) in [
        (TaskComplexity::Low, "luna"),
        (TaskComplexity::Hard, "sol"),
        (TaskComplexity::XHard, "fable"),
    ] {
        let selected = admit(&runtime, &parent, &task(&runtime, complexity, None), 0);
        assert_eq!(selected["crew"], crew);
        assert_eq!(
            selected["crew_selection"]["source"],
            format!("workflow.{complexity}_complexity_crews")
        );
        assert_eq!(
            selected["crew_selection"]["complexity"],
            json!(complexity.as_str())
        );
    }
}

/// [ORB-12605] The reserved top tier behaves like every other pool: a run
/// override replaces the configured pool, and an empty effective pool falls
/// back to the default crew chain rather than stranding the task.
#[test]
fn xhard_pool_overrides_config_and_an_empty_pool_falls_back_to_the_default_chain() {
    let (_root, runtime, _, _) =
        test_runtime_with_workspace_config("[workflow]\nxhard_complexity_crews = [\"sol\"]\n");
    let overridden = coordinator(&runtime, json!({"xhard_complexity_crews": ["fable"]}));
    let selected = admit(
        &runtime,
        &overridden,
        &task(&runtime, TaskComplexity::XHard, None),
        0,
    );
    assert_eq!(selected["crew"], "fable");
    assert_eq!(
        selected["crew_selection"]["source"],
        "run_input.xhard_complexity_crews"
    );
    assert_eq!(selected["crew_selection"]["complexity"], "xhard");

    let (_root, unset, _, _) = test_runtime_with_workspace_config("");
    let empty = coordinator(&unset, json!({}));
    let fallback = admit(
        &unset,
        &empty,
        &task(&unset, TaskComplexity::XHard, None),
        0,
    );
    assert_eq!(fallback["crew"], "opus");
    assert_eq!(fallback["crew_selection"]["source"], "default");
}

/// [ORB-12719] The file `orbit init` seeds scaffolds all four pools empty.
/// An empty configured pool is "no pool": a crew-less task of any complexity
/// is routed to `default_crew`, at creation and at admission alike.
#[test]
fn the_seeded_empty_pools_route_every_complexity_to_the_default_crew() {
    let seeded = tempfile::tempdir().expect("seed dir");
    let seeded_path = seeded.path().join("config.toml");
    orbit_config::seed_default_config(
        &seeded_path,
        Some(&orbit_config::ConfigSeed::from_families([
            "claude", "codex",
        ])),
    )
    .expect("seed the init shape");
    let config = std::fs::read_to_string(&seeded_path).expect("seeded config");
    assert!(config.contains("low_complexity_crews = []"), "{config}");
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(&config);

    let mut policy = json!({});
    runtime
        .install_auto_crew_admission(
            "workspace_auto_pipeline",
            &mut policy,
            None,
            false,
            &mut no_draw,
        )
        .expect("capture coordinator policy");
    let parent = persist(&runtime, "workspace_auto_pipeline", policy.clone());
    for complexity in COMPLEXITIES_UNDER_TEST {
        let captured = &policy["auto_crew_pools"][complexity.as_str()];
        assert_eq!(
            captured["source"],
            format!("workflow.{complexity}_complexity_crews")
        );
        assert_eq!(captured["crews"], json!([]));

        let created = added_task(&runtime, complexity, None);
        assert_eq!(created.crew.as_deref(), Some("opus"), "{complexity}");
        assert_eq!(
            crew_assigned_notes(&runtime, &created.id),
            vec!["assigned crew `opus` from default".to_string()],
            "{complexity}"
        );

        let admitted = admit(&runtime, &parent, &task(&runtime, complexity, None), 0);
        assert_eq!(admitted["crew"], "opus", "{complexity}");
        assert_eq!(
            admitted["crew_selection"]["source"], "default",
            "{complexity}"
        );
    }
}

const COMPLEXITIES_UNDER_TEST: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];

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
    assert_eq!(candidates[0].crew.name, "opus");
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
fn children_draw_independently_and_system_jobs_keep_their_crew() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok", "terra"]}),
    );
    let root = task(&runtime, TaskComplexity::Medium, None);
    let mut leaf = json!({"task_ids": [root.id]});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut leaf,
            Some(&parent),
            false,
            &mut || Ok(0),
        )
        .expect("leaf admission");
    assert_eq!(leaf["crew"], "grok");
    let leaf_run = persist(&runtime, "task_auto_pipeline", leaf);
    let descendant = task(&runtime, TaskComplexity::Medium, None);
    let mut child = json!({"task_ids": [descendant.id]});
    runtime
        .install_auto_crew_admission(
            "task_local_pipeline",
            &mut child,
            Some(&leaf_run),
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
            "task_pilot_pipeline",
            &mut system,
            Some(&leaf_run),
            false,
            &mut no_draw,
        )
        .expect("independent system route");
    assert_eq!(system, original);
}

/// [ORB-12606] Pool policy routes any crew-less task, whichever pipeline
/// admits it: a top-level `task_auto_pipeline` — what `orbit run ship` and
/// `orbit.workflow.ship` submit — captures the effective pools itself and
/// draws. An explicit run-input crew and `task.crew` still win outright.
#[test]
fn explicit_run_crew_wins_and_ordinary_ship_admission_draws_from_the_matching_pool() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    );
    let assigned = task(&runtime, TaskComplexity::Medium, Some("astra"));
    let unassigned = task(&runtime, TaskComplexity::Medium, None);

    let mut explicit = json!({"task_ids": [unassigned.id], "crew": "luna"});
    runtime
        .install_auto_crew_admission(
            "task_auto_pipeline",
            &mut explicit,
            None,
            false,
            &mut no_draw,
        )
        .expect("explicit run assignment");
    assert_eq!(explicit["crew"], "luna");
    assert_eq!(explicit["crew_selection"]["source"], "explicit");

    let mut manual = json!({"task_ids": [assigned.id]});
    runtime
        .install_auto_crew_admission("task_auto_pipeline", &mut manual, None, false, &mut no_draw)
        .expect("manual assignment");
    assert_eq!(manual["crew"], "astra");
    assert_eq!(manual["crew_selection"]["source"], "task.crew");

    let mut drawn = json!({"task_ids": [unassigned.id]});
    runtime
        .install_auto_crew_admission("task_auto_pipeline", &mut drawn, None, false, &mut || Ok(1))
        .expect("ordinary ship draw");
    assert_eq!(drawn["crew"], "terra");
    assert_eq!(drawn["crew_selection"]["task_id"], unassigned.id);
    assert_eq!(
        drawn["crew_selection"]["source"],
        "workflow.medium_complexity_crews"
    );
    assert_eq!(
        runtime
            .resolve_crew_for_run_input(&drawn)
            .expect("dispatch crew")
            .name,
        "terra"
    );
    assert_eq!(
        runtime
            .get_task(&unassigned.id)
            .expect("unchanged task")
            .crew,
        None,
        "a draw never rewrites the task"
    );
}

/// [ORB-12606] A `workspace_ship_pipeline` run carries the same frozen policy
/// a drain coordinator does: each leaf draws for its own task, a same-task
/// child and a resume keep the admitted selection, and `allowed_crews`
/// filters the pool and diagnoses a disjoint one exactly as `run auto` does.
#[test]
fn ship_coordinator_policy_filters_pools_and_holds_each_task_selection() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    );
    let ship = ship_coordinator(&runtime, json!({"mode": "pr", "base_branch": "main"}));
    let first = task(&runtime, TaskComplexity::Medium, None);
    let second = task(&runtime, TaskComplexity::Medium, None);

    let mut leaf = json!({"task_ids": [first.id]});
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut leaf,
            Some(&ship),
            false,
            &mut || Ok(0),
        )
        .expect("ship leaf admission");
    assert_eq!(leaf["crew"], "grok");
    assert_eq!(
        leaf["crew_selection"]["source"],
        "workflow.medium_complexity_crews"
    );
    let leaf_run = persist(&runtime, "task_gate_pipeline", leaf.clone());

    let mut same_task = json!({"task_ids": [first.id]});
    runtime
        .install_auto_crew_admission(
            "task_pr_pipeline",
            &mut same_task,
            Some(&leaf_run),
            false,
            &mut no_draw,
        )
        .expect("same-task child keeps the selection");
    assert_eq!(same_task["crew_selection"], leaf["crew_selection"]);

    let mut resumed = leaf.clone();
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut resumed,
            Some(&ship),
            true,
            &mut no_draw,
        )
        .expect("resume");
    assert_eq!(resumed, leaf);

    let mut sibling = json!({"task_ids": [second.id]});
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut sibling,
            Some(&ship),
            false,
            &mut || Ok(1),
        )
        .expect("sibling draws independently");
    assert_eq!(sibling["crew"], "terra");
    assert_eq!(sibling["crew_selection"]["task_id"], second.id);

    let restricted = ship_coordinator(&runtime, json!({"allowed_crews": ["terra"]}));
    let mut permitted = json!({"task_ids": [second.id]});
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut permitted,
            Some(&restricted),
            false,
            &mut no_draw,
        )
        .expect("sole permitted member needs no draw");
    assert_eq!(permitted["crew"], "terra");
    assert_eq!(permitted["allowed_crews"], json!(["terra"]));

    let disjoint = ship_coordinator(&runtime, json!({"allowed_crews": ["astra"]}));
    let mut rejected = json!({"task_ids": [second.id]});
    let error = runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut rejected,
            Some(&disjoint),
            false,
            &mut no_draw,
        )
        .expect_err("disjoint pool");
    assert!(error.to_string().contains("no member permitted"), "{error}");
}

fn ship_coordinator(runtime: &OrbitRuntime, mut input: Value) -> String {
    runtime
        .install_auto_crew_admission(
            "workspace_ship_pipeline",
            &mut input,
            None,
            false,
            &mut no_draw,
        )
        .expect("capture ship policy");
    assert_eq!(
        input["auto_crew_pools"]["medium"]["source"], "workflow.medium_complexity_crews",
        "an ordinary ship captures the configured pools"
    );
    persist(runtime, "workspace_ship_pipeline", input)
}

/// [ORB-12118] `no-diff-expected` work is admitted without an assessed
/// complexity, so its crew must come from the task's own configuration or the
/// workspace default — never from a complexity-derived pool, and never by
/// rewriting the stored non-answer to obtain one. [ORB-12717] The same holds
/// for the creation-time assignment: an unassessed task falls to the default.
#[test]
fn exempt_no_diff_expected_tasks_route_on_their_crew_or_the_workspace_default() {
    let (_root, runtime, _, _) =
        test_runtime_with_workspace_config("[workflow]\nmedium_complexity_crews = [\"grok\"]\n");
    let parent = coordinator(&runtime, json!({}));
    let configured = no_diff_expected_task(&runtime, Some("astra"));
    let inherited = no_diff_expected_task(&runtime, None);
    assert_eq!(
        inherited.crew.as_deref(),
        Some("opus"),
        "creation must not reach into a complexity pool for unassessed work"
    );

    let configured_input = admit(&runtime, &parent, &configured, 0);
    assert_eq!(configured_input["crew"], "astra");
    assert_eq!(configured_input["crew_selection"]["source"], "task.crew");
    assert_eq!(
        configured_input["crew_selection"]["complexity"],
        "unassessed"
    );

    let inherited_input = admit(&runtime, &parent, &inherited, 0);
    assert_eq!(inherited_input["crew"], "opus");
    assert_eq!(inherited_input["crew_selection"]["source"], "task.crew");

    for task in [&configured, &inherited] {
        assert_eq!(
            runtime.get_task(&task.id).expect("task").complexity,
            Some(TaskComplexity::Unassessed),
            "admission must not fabricate an assessed complexity"
        );
    }
}

fn no_diff_expected_task(runtime: &OrbitRuntime, crew: Option<&str>) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: "Operational no-diff fixture".into(),
            description: "Exercise crew routing without an assessment".into(),
            plan: "Inspect admission evidence".into(),
            tags: vec!["no-diff-expected".to_string()],
            complexity: TaskComplexity::Unassessed,
            crew: crew.map(ToOwned::to_owned),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task")
}

/// [ORB-12604] A weighted pool hands out one ticket per unit of weight. The
/// draw is swept across every ticket in `[0, total_weight)` — offset past the
/// rejection threshold, which the uniform case already covers — so each crew
/// must win exactly its share, in cumulative name order.
#[test]
fn weighted_pools_select_each_crew_for_exactly_its_share_of_the_tickets() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok:70\", \"opus:10\", \"sol:20\"]\n",
    );
    let parent = coordinator(&runtime, json!({}));
    let drawn = task(&runtime, TaskComplexity::Medium, None);
    let mut wins: BTreeMap<String, u32> = BTreeMap::new();
    for ticket in 0..100 {
        // Offsetting by the bound keeps the ticket above the rejection
        // threshold while `ticket % 100` still sweeps the whole range.
        let input = admit(&runtime, &parent, &drawn, ticket + 100);
        let crew = input["crew"].as_str().expect("crew").to_string();
        let expected = match ticket {
            0..70 => "grok",
            70..80 => "opus",
            _ => "sol",
        };
        assert_eq!(crew, expected, "ticket {ticket}");
        *wins.entry(crew).or_default() += 1;
    }
    assert_eq!(
        wins,
        BTreeMap::from([
            ("grok".to_string(), 70),
            ("opus".to_string(), 10),
            ("sol".to_string(), 20),
        ])
    );
    let evidence = admit(&runtime, &parent, &drawn, 100);
    assert_eq!(
        evidence["crew_selection"]["eligible_pool"],
        json!([
            {"name": "grok", "weight": 70},
            {"name": "opus", "weight": 10},
            {"name": "sol", "weight": 20},
        ])
    );
    assert_eq!(
        evidence["crew_selection"]["source"],
        "workflow.medium_complexity_crews"
    );
}

/// [ORB-12604] An allowlist renormalises the odds over the members it permits,
/// and a crew parked at weight 0 holds no ticket: it can neither win a draw
/// nor stand in for a pool the allowlist has otherwise emptied.
#[test]
fn allowlists_renormalise_over_permitted_members_and_never_draw_a_parked_crew() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let parent = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok:70", "opus:10", "sol:20"]}),
    );
    let drawn = task(&runtime, TaskComplexity::Medium, None);
    for (ticket, expected) in [(0, "opus"), (9, "opus"), (10, "sol"), (29, "sol")] {
        let mut input = json!({"task_ids": [drawn.id], "allowed_crews": ["opus", "sol"]});
        runtime
            .install_auto_crew_admission(
                "task_auto_pipeline",
                &mut input,
                Some(&parent),
                false,
                // 30 permitted tickets, offset past the rejection threshold.
                &mut || Ok(ticket + 30),
            )
            .expect("renormalised draw");
        assert_eq!(input["crew"], expected, "ticket {ticket} of 30");
        assert_eq!(
            input["crew_selection"]["eligible_pool"],
            json!([{"name": "opus", "weight": 10}, {"name": "sol", "weight": 20}]),
            "the recorded odds are the permitted ones"
        );
    }
    let parked = coordinator(
        &runtime,
        json!({"medium_complexity_crews": ["grok:0", "terra:50"]}),
    );
    // terra alone is permitted and carries every ticket: no draw is needed.
    assert_eq!(
        admit_allowed(&runtime, &parked, &drawn, &["terra"]).expect("sole permitted member")["crew"],
        "terra"
    );
    let error = admit_allowed(&runtime, &parked, &drawn, &["grok"])
        .expect_err("a parked crew is not a permitted member");
    assert!(error.to_string().contains("no member permitted"), "{error}");
}

fn admit_allowed(
    runtime: &OrbitRuntime,
    parent: &str,
    task: &Task,
    allowed: &[&str],
) -> Result<Value, OrbitError> {
    let mut input = json!({"task_ids": [task.id], "allowed_crews": allowed});
    runtime.install_auto_crew_admission(
        "task_auto_pipeline",
        &mut input,
        Some(parent),
        false,
        &mut no_draw,
    )?;
    Ok(input)
}

/// [ORB-12604] `auto_crew_pools` persisted before weights existed stores plain
/// crew names. Such a run must resume, and its same-task children must inherit,
/// without a reroll; a sibling task admitted from it draws uniformly, exactly
/// as it did when the pool was written.
#[test]
fn a_legacy_named_pool_resumes_inherits_and_draws_uniformly() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config("");
    let inherited = task(&runtime, TaskComplexity::Medium, None);
    let sibling = task(&runtime, TaskComplexity::Medium, None);
    // Hand-written in the shape a pre-ORB-12604 coordinator persisted.
    let legacy = json!({
        "task_ids": [inherited.id],
        "crew": "terra",
        "auto_crew_pools": {
            "low": {"crews": [], "source": "workflow.low_complexity_crews"},
            "medium": {
                "crews": ["grok", "terra"],
                "source": "run_input.medium_complexity_crews",
            },
            "hard": {"crews": [], "source": "workflow.hard_complexity_crews"},
        },
        "crew_selection": {
            "task_id": inherited.id,
            "crew": "terra",
            "source": "run_input.medium_complexity_crews",
            "complexity": "medium",
            "eligible_pool": ["grok", "terra"],
        },
    });
    let parent = persist(&runtime, "task_auto_pipeline", legacy.clone());

    let mut resumed = legacy.clone();
    runtime
        .install_auto_crew_admission("task_auto_pipeline", &mut resumed, None, true, &mut no_draw)
        .expect("resume a legacy run");
    assert_eq!(resumed, legacy);

    let mut child = json!({"task_ids": [inherited.id]});
    runtime
        .install_auto_crew_admission(
            "task_gate_pipeline",
            &mut child,
            Some(&parent),
            false,
            &mut no_draw,
        )
        .expect("inherit the legacy selection");
    assert_eq!(child["crew"], "terra");
    assert_eq!(child["crew_selection"], legacy["crew_selection"]);

    for (ticket, expected) in [(0, "grok"), (1, "terra")] {
        let drawn = admit(&runtime, &parent, &sibling, ticket);
        assert_eq!(drawn["crew"], expected);
        assert_eq!(
            drawn["crew_selection"]["eligible_pool"],
            json!([{"name": "grok", "weight": 1}, {"name": "terra", "weight": 1}]),
            "a legacy name carries the single ticket it always had"
        );
    }
}

/// [ORB-12717] Creating a task without a crew draws one from the pool for its
/// complexity and records the draw's provenance.
#[test]
fn task_creation_draws_its_crew_from_the_complexity_pool() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        r#"
[workflow]
low_complexity_crews = ["luna"]
medium_complexity_crews = ["grok"]
hard_complexity_crews = ["sol"]
"#,
    );
    for (complexity, crew) in [
        (TaskComplexity::Low, "luna"),
        (TaskComplexity::Medium, "grok"),
        (TaskComplexity::Hard, "sol"),
    ] {
        let created = added_task(&runtime, complexity, None);
        assert_eq!(created.crew.as_deref(), Some(crew), "{complexity}");
        assert_eq!(
            runtime
                .get_task(&created.id)
                .expect("reread")
                .crew
                .as_deref(),
            Some(crew),
            "{complexity}: the crew is persisted by the create itself"
        );
        assert_eq!(
            crew_assigned_notes(&runtime, &created.id),
            vec![format!("assigned crew `{crew}` from pool:{complexity}")],
        );
    }
}

/// [ORB-12717] A complexity no pool covers falls back to `default_crew`, and an
/// explicit crew is stored as given with source `explicit`.
#[test]
fn task_creation_falls_back_to_the_default_crew_and_keeps_an_explicit_one() {
    let (_root, runtime, _, _) =
        test_runtime_with_workspace_config("[workflow]\nmedium_complexity_crews = [\"grok\"]\n");

    let fallback = added_task(&runtime, TaskComplexity::Unassessed, None);
    assert_eq!(fallback.crew.as_deref(), Some("opus"));
    assert_eq!(
        crew_assigned_notes(&runtime, &fallback.id),
        vec!["assigned crew `opus` from default".to_string()],
    );

    let explicit = added_task(&runtime, TaskComplexity::Medium, Some("astra"));
    assert_eq!(
        explicit.crew.as_deref(),
        Some("astra"),
        "an explicit crew wins over the pool for this complexity"
    );
    assert_eq!(
        crew_assigned_notes(&runtime, &explicit.id),
        vec!["assigned crew `astra` from explicit".to_string()],
    );
}

/// [ORB-12717] An auto-task template that names no crew mints a task whose
/// crew comes from the same creation-time pool draw as any other task.
#[test]
fn an_auto_task_template_without_a_crew_mints_a_pool_drawn_crew() {
    use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};

    use crate::application::auto_tasks::crud::AutoTaskAddParams;

    let (_root, runtime, _, _) =
        test_runtime_with_workspace_config("[workflow]\nhard_complexity_crews = [\"sol\"]\n");
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: "sweep".to_string(),
            description: "Recurring sweep".to_string(),
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
            template: AutoTaskTemplate {
                title: "Sweep the workspace".to_string(),
                description: "Recurring chore body.".to_string(),
                acceptance_criteria: vec!["Sweep is observable.".to_string()],
                task_type: orbit_types::task::TaskType::Chore,
                tags: Vec::new(),
                required_tools: Vec::new(),
                priority: TaskPriority::Medium,
                complexity: Some(TaskComplexity::Hard),
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .expect("add the definition");

    let minted = runtime.auto_task_mint("sweep").expect("mint");

    assert_eq!(minted.crew.as_deref(), Some("sol"));
    assert_eq!(
        crew_assigned_notes(&runtime, &minted.id),
        vec!["assigned crew `sol` from pool:hard".to_string()],
    );
}

/// [ORB-12717] Clearing the crew re-draws for the task's current complexity;
/// editing the complexity alone leaves the crew alone.
#[test]
fn clearing_the_crew_redraws_and_a_complexity_edit_does_not() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        r#"
[workflow]
medium_complexity_crews = ["grok"]
hard_complexity_crews = ["sol"]
"#,
    );
    let created = added_task(&runtime, TaskComplexity::Medium, Some("astra"));

    let reclassified = runtime
        .update_task(
            &created.id,
            crate::application::task::TaskUpdateParams {
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
        )
        .expect("reclassify");
    assert_eq!(
        reclassified.crew.as_deref(),
        Some("astra"),
        "a complexity edit alone never re-routes the task"
    );

    let redrawn = runtime
        .update_task(
            &created.id,
            crate::application::task::TaskUpdateParams {
                crew: Some(Some(String::new())),
                ..Default::default()
            },
        )
        .expect("clear the crew");
    assert_eq!(
        redrawn.crew.as_deref(),
        Some("sol"),
        "clearing draws again for the complexity the task now carries"
    );
    assert_eq!(
        crew_assigned_notes(&runtime, &created.id),
        vec![
            "assigned crew `astra` from explicit".to_string(),
            "assigned crew `sol` from pool:hard".to_string(),
        ],
    );
}

/// [ORB-12717] The `backlog -> in-progress` transition is crew-preserving on
/// every surface: the system workflow admission a drain and `run ship` use, and
/// an operator's `task update --status`. This replaces the ORB-12678 tests that
/// asserted the same transition stamped a crew.
#[test]
fn the_in_progress_transition_never_changes_the_crew() {
    let (_root, runtime, _, _) = test_runtime_with_workspace_config(
        "[workflow]\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    );
    let parent = coordinator(&runtime, json!({}));

    // A legacy crew-less record: dispatch routes it through the pool, and the
    // task still carries no crew afterwards.
    let legacy = task(&runtime, TaskComplexity::Medium, None);
    let input = admit(&runtime, &parent, &legacy, 0);
    assert_eq!(input["crew"], "grok");
    let run_id = persist(&runtime, "task_auto_pipeline", input);
    runtime
        .record_run_crew_from_input(
            &run_id,
            &json!({"crew": "grok", "task_ids": [legacy.id.clone()]}),
        )
        .expect("record resolved crew");
    let started = runtime
        .admit_task_for_workflow_as_system(&legacy.id, "worktree_setup")
        .expect("admit dispatched task");
    assert_eq!(started.status, TaskStatus::InProgress);
    assert_eq!(started.crew, None);

    // A task created with a crew keeps exactly that string, whichever surface
    // moves it to in-progress.
    for mover in ["workflow", "update"] {
        let assigned = added_task(&runtime, TaskComplexity::Medium, None);
        let before = assigned.crew.clone();
        assert!(before.is_some(), "creation assigns a crew");
        let started = match mover {
            "workflow" => runtime
                .admit_task_for_workflow_as_system(&assigned.id, "worktree_setup")
                .expect("workflow admission"),
            _ => runtime
                .update_task(
                    &assigned.id,
                    crate::application::task::TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                )
                .expect("operator start"),
        };
        assert_eq!(started.status, TaskStatus::InProgress, "{mover}");
        assert_eq!(started.crew, before, "{mover}");
        assert_eq!(
            crew_assigned_notes(&runtime, &assigned.id).len(),
            1,
            "{mover}: a transition writes no further crew provenance"
        );
    }

    assert!(
        runtime
            .get_task_history(&legacy.id)
            .expect("history")
            .iter()
            .all(|entry| entry.event != "crew_stamped"),
        "no surface writes a crew_stamped entry any more"
    );
}
