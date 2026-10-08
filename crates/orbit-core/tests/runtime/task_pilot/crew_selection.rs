//! Crew provenance survives rerating and admission through the public runtime.
//! Pool names are fixture policy; the guarded incident is creation-time pool
//! draws and default fallbacks being mistaken for operator pins after a
//! complexity assessment or pool configuration change.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use orbit_store::contracts::TaskDocumentUpdateParams;
use serde_json::{Value, json};

use super::{Workspace, runtime_at};
use crate::dispatch_admission::isolated;

fn workspace() -> Workspace {
    workspace_with_low_pool(true)
}

fn workspace_with_low_pool(populated: bool) -> Workspace {
    let mut workspace = Workspace::new();
    std::fs::write(
        workspace.repo.join(".orbit/config.toml"),
        r#"
[workflow]
default_crew = "low_lane"
system_crew = "low_lane"
low_complexity_crews = ["low_lane"]
hard_complexity_crews = ["hard_lane"]
[crews.low_lane]
provider = "codex"
model = "fixture-low"
[crews.hard_lane]
provider = "claude"
model = "fixture-hard"
"#,
    )
    .unwrap();
    if !populated {
        let config = workspace.repo.join(".orbit/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            config,
            text.replace(
                "low_complexity_crews = [\"low_lane\"]",
                "low_complexity_crews = []",
            ),
        )
        .unwrap();
    }
    workspace.runtime = runtime_at(
        &workspace.runtime.global_root(),
        &workspace.repo.join(".orbit"),
    );
    workspace
}

fn task(workspace: &Workspace, explicit: bool) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Crew rerating fixture".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec!["The fixture repair is observable.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            crew: explicit.then(|| "low_lane".into()),
            ..Default::default()
        })
        .unwrap()
}

fn assess(workspace: &Workspace, task: &Task, complexity: TaskComplexity) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [{
                "task_id": task.id,
                "context_files_before": task.context_files,
                "context_files_after": task.context_files,
                "disposition": "selectors", "recommended_crew": if complexity == TaskComplexity::Hard { "hard_lane" } else { "low_lane" },
                "recommended_complexity": complexity.as_str(), "confidence": "high",
                "assessment_rationale": "Assess the fixture tier.",
                "validation_approach": "Observe crew assignment and run admission.",
                "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                "duplicate_of": null, "already_landed": null,
            }]}],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    prepared
}

fn redraws(workspace: &Workspace, task: &Task) -> usize {
    workspace
        .runtime
        .get_task_history(&task.id)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == "crew_redrawn")
        .count()
}

#[test]
fn pilot_redraws_automatic_assignments_and_preserves_add_and_update_pins() {
    if !isolated(
        "task_pilot::crew_selection::pilot_redraws_automatic_assignments_and_preserves_add_and_update_pins",
    ) {
        return;
    }
    for (populated, pin) in [
        (true, "pool"),
        (false, "default"),
        (false, "add"),
        (false, "update"),
    ] {
        let workspace = workspace_with_low_pool(populated);
        let mut task = task(&workspace, pin == "add");
        if pin == "update" {
            // Pinning the same name must replace the pool provenance too.
            task = workspace
                .runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        crew: Some(Some("low_lane".into())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
        }
        assert_eq!(task.crew.as_deref(), Some("low_lane"));
        assert_eq!(
            task.crew_source.as_deref(),
            Some(if pin == "pool" {
                "pool:low"
            } else if pin == "default" {
                "default"
            } else {
                "explicit"
            })
        );
        // A same-tier pilot assessment preserves even a default fallback.
        assess(&workspace, &task, TaskComplexity::Low);
        assert_eq!(redraws(&workspace, &task), 0);
        let prepared = assess(&workspace, &task, TaskComplexity::Hard);
        let rerated = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(rerated.complexity, Some(TaskComplexity::Hard));
        if pin == "pool" || pin == "default" {
            assert_eq!(rerated.crew.as_deref(), Some("hard_lane"));
            assert_eq!(rerated.crew_source.as_deref(), Some("pool:hard"));
            let history = workspace.runtime.get_task_history(&task.id).unwrap();
            let note = history
                .iter()
                .find(|entry| entry.event == "crew_redrawn")
                .unwrap()
                .note
                .as_deref()
                .unwrap();
            assert!(
                note.contains(if populated { "pool:low" } else { "default" })
                    && note.contains("pool:hard"),
                "redraw evidence names both sources: {note}"
            );
            assert_eq!(redraws(&workspace, &task), 1);
        } else {
            assert_eq!(rerated.crew.as_deref(), Some("low_lane"));
            assert_eq!(rerated.crew_source.as_deref(), Some("explicit"));
            assert_eq!(redraws(&workspace, &task), 0);
        }
        // Restart preserves the provenance; another same-tier assessment
        // does not draw again.
        let restarted = runtime_at(
            &workspace.runtime.global_root(),
            &workspace.repo.join(".orbit"),
        );
        assert_eq!(
            restarted.get_task(&task.id).unwrap().crew_source,
            rerated.crew_source
        );
        assess(&workspace, &rerated, TaskComplexity::Hard);
        assert_eq!(
            redraws(&workspace, &task),
            usize::from(pin == "pool" || pin == "default")
        );
        assert_eq!(prepared["task_ids"], json!([task.id]));
    }
}

#[test]
fn complexity_update_redraws_automatic_assignments_and_clear_draws_current_tier() {
    if !isolated(
        "task_pilot::crew_selection::complexity_update_redraws_automatic_assignments_and_clear_draws_current_tier",
    ) {
        return;
    }
    for (populated, explicit) in [(true, false), (false, false), (false, true)] {
        let workspace = workspace_with_low_pool(populated);
        let task = task(&workspace, explicit);
        let updated = workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    complexity: Some(TaskComplexity::Hard),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            updated.crew.as_deref(),
            Some(if explicit { "low_lane" } else { "hard_lane" })
        );
        assert_eq!(redraws(&workspace, &task), usize::from(!explicit));
        let cleared = workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    crew: Some(None),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(cleared.crew.as_deref(), Some("hard_lane"));
        assert_eq!(cleared.crew_source.as_deref(), Some("pool:hard"));
    }
}

#[test]
fn admission_recovers_automatic_history_and_reports_current_tier_selection() {
    if !isolated(
        "task_pilot::crew_selection::admission_recovers_automatic_history_and_reports_current_tier_selection",
    ) {
        return;
    }
    for (pin, legacy) in [
        ("pool", false),
        ("pool", true),
        ("default", false),
        ("default", true),
        ("add", true),
        ("update", true),
    ] {
        let explicit = pin == "add" || pin == "update";
        let mut workspace = if pin == "default" {
            workspace_with_low_pool(false)
        } else {
            workspace()
        };
        let mut task = task(&workspace, pin == "add");
        if pin == "update" {
            task = workspace
                .runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        crew: Some(Some("low_lane".into())),
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
        }
        if pin == "default" {
            assert_eq!(task.crew_source.as_deref(), Some("default"));
            // The tier is unchanged, but its formerly empty pool now has a
            // crew. Admission must treat the fallback as automatic too.
            let config = workspace.repo.join(".orbit/config.toml");
            let text = std::fs::read_to_string(&config).unwrap();
            std::fs::write(
                config,
                text.replace(
                    "low_complexity_crews = []",
                    "low_complexity_crews = [\"hard_lane\"]",
                ),
            )
            .unwrap();
            workspace.runtime = runtime_at(
                &workspace.runtime.global_root(),
                &workspace.repo.join(".orbit"),
            );
        }
        let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
            &orbit_store::maintenance::task_registry::task_registry_path(
                &workspace.runtime.global_root(),
            ),
        )
        .unwrap();
        let backends = orbit_store::compose::workspace_coordinated_backends(
            registry,
            workspace.runtime.workspace_id().unwrap(),
            workspace.runtime.sqlite_store().unwrap(),
        )
        .unwrap()
        .task;
        // Seed stale pool tiers and legacy provenance at the persistence
        // boundary. Default cases keep their creation tier and history.
        backends
            .document
            .update_task_document(
                &task.id,
                TaskDocumentUpdateParams {
                    actor: "fixture".into(),
                    complexity: (pin != "default").then_some(TaskComplexity::Hard),
                    crew_source: legacy.then_some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        let jobs = workspace.runtime.global_root().join("resources/jobs");
        std::fs::create_dir_all(&jobs).unwrap();
        std::fs::write(jobs.join("task_local_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n").unwrap();
        // No agent executes: the fixture only exercises real submission and
        // the run-show read/projection boundary.
        orbit_core::test_support::install_substitute_pipeline_worker([
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "--list".to_string(),
        ]);
        let submitted = workspace
            .runtime
            .submit_pipeline_run(
                "task_local_pipeline",
                json!({"task_id": task.id}),
                None,
                Some("fixture"),
            )
            .unwrap();
        let run = workspace
            .runtime
            .show_job_run_observed(&submitted.run_id)
            .unwrap();
        let shown = orbit_core::application::job::job_run_to_json(&run, None);
        let selection = &run.input.as_ref().unwrap()["crew_selection"];
        assert_eq!(shown["requested_crew"], selection["crew"], "{shown}");
        assert_eq!(
            selection["crew"],
            if explicit { "low_lane" } else { "hard_lane" },
            "{shown}"
        );
        if explicit {
            assert_eq!(selection["source"], "task.crew", "{shown}");
        } else {
            assert_ne!(selection["source"], "task.crew", "{shown}");
            assert_eq!(
                selection["complexity"],
                if pin == "default" { "low" } else { "hard" },
                "{shown}"
            );
            assert_eq!(
                selection["source"],
                if pin == "default" {
                    "workflow.low_complexity_crews"
                } else {
                    "workflow.hard_complexity_crews"
                },
                "{shown}"
            );
            assert_eq!(
                selection["eligible_pool"][0]["name"], "hard_lane",
                "{shown}"
            );
        }
        let persisted = workspace.runtime.get_task(&task.id).unwrap();
        assert_eq!(persisted.crew.as_deref(), Some("low_lane"));
        assert_eq!(
            persisted.crew_source,
            if legacy { None } else { task.crew_source },
            "admission never rewrites the task"
        );
    }
}
