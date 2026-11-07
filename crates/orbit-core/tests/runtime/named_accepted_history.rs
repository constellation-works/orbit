//! [ORB-22676] A routine's condition reads a distinct auto-task's accepted
//! result history as `auto_tasks['<name>'].last_succeeded_at`. Through the
//! public explain (which carries the status projection), a disabled
//! workspace routine loads and evaluates a 24-hour guard: a readable history
//! with no accepted result takes the never-accepted branch, a recent receipt
//! holds and an old one satisfies the time clause. A raw `done` task never
//! advances the history. A missing definition, a definition with no
//! completion contract and a corrupt history are unknown, `has()` included,
//! never "never accepted".

use chrono::{DateTime, Duration, Utc};
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::{
    AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION, AutoTaskAcceptedHistoryFile,
    AutoTaskCompletionContract, AutoTaskCompletionReceipt, AutoTaskResultStatus, auto_task_tag,
};
use serde_json::Value;

use crate::task_params_test_support::task_create_params;

const GUARD: &str = "!has(auto_tasks['TARGET'].last_succeeded_at) || \
                     now - auto_tasks['TARGET'].last_succeeded_at > duration('24h')";

fn add_auto_task(runtime: &OrbitRuntime, name: &str, contract: bool) {
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: "Prepare a release".into(),
            schedule: AutoTaskSchedule::every_minutes(60),
            template: AutoTaskTemplate {
                title: "Prepare a release".into(),
                description: "Prepare".into(),
                acceptance_criteria: vec!["Release prepared".into()],
                task_type: TaskType::Chore,
                tags: Vec::new(),
                required_tools: Vec::new(),
                context_files: Vec::new(),
                priority: TaskPriority::Medium,
                complexity: None,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
    if !contract {
        return;
    }
    let mut definition = runtime.auto_task_show(name).unwrap().unwrap();
    definition.completion = Some(AutoTaskCompletionContract {
        contract: "release-readiness/v1".into(),
        adapter: "accept-evidence".into(),
        form: "fixture/v1".into(),
        evidence_artifact: "evidence.json".into(),
        max_age_minutes: 60,
        read_task_tags: Vec::new(),
        evidence_instructions: None,
    });
    let path =
        orbit_automation::auto_tasks::loader::definition_path(&runtime.paths().local_dir, name);
    std::fs::write(path, serde_yaml::to_string(&definition).unwrap()).unwrap();
}

/// A disabled workspace-scope routine whose condition reads `target`.
fn add_guard_routine(runtime: &OrbitRuntime, name: &str, target: &str) {
    let dir = runtime.shared_root().join("routines");
    std::fs::create_dir_all(&dir).unwrap();
    let when = GUARD.replace("TARGET", target);
    std::fs::write(
        dir.join(format!("{name}.yaml")),
        format!(
            "schemaVersion: 1\nname: {name}\ndescription: Release guard.\nenabled: false\n\
             trigger:\n  condition:\n    scope: workspace\n    when: \"{when}\"\n\
             target: job:weekly_report_pipeline\n"
        ),
    )
    .unwrap();
}

fn history_path(runtime: &OrbitRuntime) -> std::path::PathBuf {
    runtime
        .paths()
        .state_dir
        .join("auto-task-accepted-results.json")
}

fn record_accepted(runtime: &OrbitRuntime, name: &str, accepted_at: DateTime<Utc>) {
    let mut history = AutoTaskAcceptedHistoryFile {
        schema_version: AUTO_TASK_ACCEPTED_HISTORY_SCHEMA_VERSION,
        ..AutoTaskAcceptedHistoryFile::default()
    };
    history.definitions.insert(
        name.into(),
        AutoTaskCompletionReceipt {
            contract: "release-readiness/v1".into(),
            status: AutoTaskResultStatus::Passed,
            workspace_id: runtime.workspace_id().unwrap(),
            definition: name.into(),
            definition_digest: "digest".into(),
            task_id: "ORB-1".into(),
            run_id: "jrun-fixture".into(),
            revision: "0".repeat(40),
            completed_at: accepted_at,
            evidence: Vec::new(),
            adapter: "accept-evidence".into(),
            adapter_digest: "digest".into(),
            accepted_at,
        },
    );
    let path = history_path(runtime);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&history).unwrap()).unwrap();
}

/// The routine's explain as JSON: matched/missed/unknown counts, the
/// unavailable notes and the tracking (status) block.
fn explain(runtime: &OrbitRuntime, routine: &str) -> Value {
    serde_json::to_value(
        runtime
            .explain_automation(&format!("routine:{routine}"), None)
            .unwrap(),
    )
    .unwrap()
}

/// `true`, `false` or `unknown` for the single workspace subject.
fn verdict(report: &Value) -> &'static str {
    let subjects = &report["subjects"];
    match (
        subjects["matched"].as_u64(),
        subjects["missed"].as_u64(),
        subjects["unknown"].as_u64(),
    ) {
        (Some(1), Some(0), Some(0)) => "true",
        (Some(0), Some(1), Some(0)) => "false",
        (Some(0), Some(0), Some(1)) => "unknown",
        other => panic!("unexpected subject counts {other:?}: {report:#}"),
    }
}

fn readable_history_drives_the_named_24_hour_guard() {
    let runtime = OrbitRuntime::in_memory()
        .unwrap()
        .with_automation_machine_identity(Some("hm_fixture".into()));
    add_auto_task(&runtime, "release-prep", true);
    add_guard_routine(&runtime, "release-guard", "release-prep");

    // Readable, never accepted: the never-accepted branch holds.
    let report = explain(&runtime, "release-guard");
    assert_eq!(report["definition"]["enabled"], false);
    assert_eq!(report["cross_reads"]["auto_tasks"][0], "release-prep");
    assert_eq!(verdict(&report), "true", "{report:#}");
    assert!(report.get("unavailable").is_none(), "{report:#}");
    let tracked = &report["tracking"]["auto_tasks"]["release-prep"];
    assert!(tracked.is_object(), "status lists the named history");
    assert!(tracked.get("last_succeeded_at").is_none());
    assert!(tracked.get("unavailable").is_none());

    // A finished task carrying the definition's tag is not an accepted result.
    let mut params = task_create_params(
        "Release prep that merely finished",
        "A probe that found nothing to release",
        TaskStatus::Done,
    );
    params.tags = vec![auto_task_tag("release-prep")];
    runtime.stores().tasks().create_task(params).unwrap();
    assert_eq!(verdict(&explain(&runtime, "release-guard")), "true");

    // A receipt accepted an hour ago holds the guard; the status agrees.
    record_accepted(&runtime, "release-prep", Utc::now() - Duration::hours(1));
    let report = explain(&runtime, "release-guard");
    assert_eq!(verdict(&report), "false", "{report:#}");
    assert!(
        report["tracking"]["auto_tasks"]["release-prep"]["last_succeeded_at"].is_string(),
        "{report:#}"
    );

    // One accepted 30 hours ago satisfies the time clause.
    record_accepted(&runtime, "release-prep", Utc::now() - Duration::hours(30));
    assert_eq!(verdict(&explain(&runtime, "release-guard")), "true");
}

fn unavailable_named_history_fails_closed_even_under_has() {
    let runtime = OrbitRuntime::in_memory()
        .unwrap()
        .with_automation_machine_identity(Some("hm_fixture".into()));
    add_auto_task(&runtime, "release-prep", true);
    add_auto_task(&runtime, "plain-chore", false);
    add_guard_routine(&runtime, "corrupt-guard", "release-prep");
    add_guard_routine(&runtime, "contractless-guard", "plain-chore");
    add_guard_routine(&runtime, "missing-guard", "ghost");

    let path = history_path(&runtime);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();

    for (routine, name, why) in [
        ("corrupt-guard", "release-prep", "parse"),
        (
            "contractless-guard",
            "plain-chore",
            "no completion contract",
        ),
        ("missing-guard", "ghost", "no auto-task `ghost`"),
    ] {
        let report = explain(&runtime, routine);
        // `!has(...)` would read true on an empty history; it must be unknown.
        assert_eq!(verdict(&report), "unknown", "{routine}: {report:#}");
        let notes = report["unavailable"].as_array().unwrap();
        assert!(
            notes.iter().any(|note| {
                let note = note.as_str().unwrap();
                note.starts_with(&format!("auto_tasks['{name}'].last_succeeded_at:"))
                    && note.contains(why)
            }),
            "{routine}: {report:#}"
        );
        let tracked = &report["tracking"]["auto_tasks"][name];
        assert!(tracked.get("last_succeeded_at").is_none());
        assert!(
            tracked["unavailable"]
                .as_str()
                .is_some_and(|text| text.contains(why)),
            "{routine}: {report:#}"
        );
    }
}

crate::scenario_table!(
    named_accepted_history_scenarios,
    readable_history_drives_the_named_24_hour_guard,
    unavailable_named_history_fails_closed_even_under_has,
);
