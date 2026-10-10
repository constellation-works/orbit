//! A pilot's typed `required_os` finding routes an untagged task by the `os:`
//! tag apply adds, and otherwise holds a backlog task on a host of another OS
//! across the real drain, ship-selection, readiness and workflow admission
//! boundaries, until the matching `os:` tag, a re-scope, an evidenced
//! operator decision or a newer assessment without it clears the wait. A
//! `required_machine` finding holds every machine but the owner it names.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use orbit_types::task::HostOs;
use serde_json::{Value, json};

use super::Workspace;
use crate::dispatch_admission::isolated;

const NATIVE_CRITERION: &str = "A real macOS `sandbox-exec` launch of the fixture succeeds.";

fn on(workspace: &mut Workspace, os: HostOs) {
    workspace.runtime = workspace.runtime.clone().with_host_os(Some(os));
}

fn backlog_task(workspace: &Workspace, criteria: &[&str], tags: &[&str]) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Native host validation work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: criteria.iter().map(ToString::to_string).collect(),
            plan: "Inspect README.md.".into(),
            tags: tags.iter().map(ToString::to_string).collect(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

/// Apply one assessment through the routine prepare/apply actions. `required_os`
/// is the pilot's typed finding; `None` omits the field, as an older pilot
/// result does.
fn apply(workspace: &Workspace, task: &Task, required_os: Option<Value>) -> Value {
    apply_findings(
        workspace,
        task,
        required_os.map(|required_os| json!({"required_os": required_os})),
    )
}

/// [`apply`] with any typed findings, merged into the assessment.
fn apply_findings(workspace: &Workspace, task: &Task, findings: Option<Value>) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    let mut assessment = json!({
        "task_id": task.id,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Exercise drain admission.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    for (field, finding) in findings
        .iter()
        .flat_map(|findings| findings.as_object().unwrap())
    {
        assessment[field] = finding.clone();
    }
    workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        }),
    )
}

fn assess(workspace: &Workspace, task: &Task, required_os: Option<Value>) {
    let applied = apply(workspace, task, required_os);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
}

fn tags(workspace: &Workspace, task: &Task) -> Vec<String> {
    workspace.runtime.get_task(&task.id).unwrap().tags
}

fn needs_macos(criterion: usize) -> Value {
    json!([{
        "criterion": criterion, "os": "macos",
        "evidence": "The criterion requires a native macOS sandbox-exec launch.",
    }])
}

/// The reason automatic and explicit selection both give for withholding
/// `task`, with its detail, or `None` when both admit it. Readiness and the
/// drain's wave must agree.
fn exclusion(workspace: &Workspace, task: &Task) -> Option<(String, String)> {
    let mut reasons = Vec::new();
    for input in [json!({}), json!({"task_ids": [task.id]})] {
        let output = workspace.action("list_backlog_tasks", input);
        let excluded = output["excluded"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == task.id);
        assert_eq!(
            output["task_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(task.id)),
            excluded.is_none(),
            "{output}"
        );
        reasons.push(excluded.map(|entry| {
            (
                entry["reason"].as_str().unwrap().to_string(),
                entry["detail"].as_str().unwrap_or_default().to_string(),
            )
        }));
    }
    assert_eq!(reasons[0], reasons[1], "automatic and explicit selection");
    let wave = workspace.action(
        "classify_workspace_auto_tasks",
        json!({"max_active_leaf_runs": 2}),
    );
    assert_eq!(
        wave["loose_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        reasons[0].is_none(),
        "{wave}"
    );
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap_or_else(|| panic!("task missing from readiness: {readiness}"));
    assert_eq!(entry["eligible"], reasons[0].is_none(), "{readiness}");
    if let Some((reason, detail)) = &reasons[0] {
        assert_eq!(entry["reason"], reason.as_str(), "{readiness}");
        assert_eq!(entry["detail"], detail.as_str(), "{readiness}");
    }
    reasons.swap_remove(0)
}

fn assert_native_os_wait(workspace: &Workspace, task: &Task, criterion: usize) {
    let (reason, detail) = exclusion(workspace, task).expect("the task is held");
    assert_eq!(reason, "native_os_required");
    assert!(
        detail.contains(&format!(
            "criterion {criterion} needs native macos evidence"
        )) && detail.contains("`os:macos`"),
        "the wait names the criterion and the tag to add: {detail}"
    );
}

fn resolutions(workspace: &Workspace, task: &Task) -> Vec<String> {
    workspace
        .runtime
        .get_task_history(&task.id)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "native_os_requirement_resolved")
        .map(|entry| entry.by)
        .collect()
}

#[test]
fn a_native_os_finding_tags_an_untagged_task_for_that_os() {
    if !isolated("task_pilot::native_os::a_native_os_finding_tags_an_untagged_task_for_that_os") {
        return;
    }
    let mut workspace = Workspace::new();
    on(&mut workspace, HostOs::Linux);
    let task = backlog_task(
        &workspace,
        &["The README fixture is repaired.", NATIVE_CRITERION],
        &["ci", "macos"],
    );
    assert_eq!(exclusion(&workspace, &task), None);
    assess(&workspace, &task, Some(needs_macos(2)));
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit: Value =
        serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1).unwrap();
    assert_eq!(
        audit["native_os_hold"]["requirements"],
        json!([{"criterion": 2, "os": "macos"}]),
        "{audit}"
    );
    assert_eq!(
        tags(&workspace, &task),
        ["ci", "macos", "os:macos"],
        "the finding's OS is added to an untagged task"
    );

    // Tag routing now refuses a Linux host in selection and readiness.
    let (reason, detail) = exclusion(&workspace, &task).unwrap();
    assert_eq!(reason, "host_os_mismatch");
    assert!(
        detail.contains("waits for a macos host (os:macos)"),
        "{detail}"
    );

    // A macOS host can produce the evidence and starts the task.
    on(&mut workspace, HostOs::Macos);
    assert_eq!(exclusion(&workspace, &task), None);

    // Removing the tag restores the pilot's wait rather than admitting the
    // Linux host.
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                tags: Some(vec!["ci".into(), "macos".into()]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    on(&mut workspace, HostOs::Linux);
    assert_native_os_wait(&workspace, &task, 2);
    assert!(resolutions(&workspace, &task).is_empty());
}

#[test]
fn a_native_os_wait_clears_on_a_decision_a_rescope_or_a_newer_assessment() {
    if !isolated(
        "task_pilot::native_os::a_native_os_wait_clears_on_a_decision_a_rescope_or_a_newer_assessment",
    ) {
        return;
    }
    for clearing in ["approve-anyway", "clear", "rescope", "reassessed"] {
        let mut workspace = Workspace::new();
        on(&mut workspace, HostOs::Linux);
        // The operator routed the task to Linux; the pilot keeps that tag, so
        // its finding holds the Linux host instead.
        let task = backlog_task(&workspace, &[NATIVE_CRITERION], &["os:linux"]);
        let decision = format!(
            "task-pilot-admission: {clearing}\nThe operator ran the macOS launch on their own host."
        );
        // A decision made before the assessment cannot approve it.
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some(decision.clone()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assess(&workspace, &task, Some(needs_macos(1)));
        assert_eq!(tags(&workspace, &task), ["os:linux"], "{clearing}");
        assert_native_os_wait(&workspace, &task, 1);
        // Agent provenance cannot mint the decision, and a decision without
        // evidence is refused.
        workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    comment: Some(decision.clone()),
                    ..Default::default()
                },
                Some("codex".into()),
                Some("gpt-6.1-sol".into()),
            )
            .unwrap();
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    comment: Some("task-pilot-admission: clear".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap_err();
        assert_native_os_wait(&workspace, &task, 1);
        assert!(resolutions(&workspace, &task).is_empty());

        match clearing {
            "rescope" => {
                workspace
                    .runtime
                    .update_task_as_human(
                        &task.id,
                        TaskUpdateParams {
                            acceptance_criteria: Some(vec![
                                "A mocked macOS sandbox probe reports the launch.".into(),
                            ]),
                            ..Default::default()
                        },
                        "human:fixture".into(),
                    )
                    .unwrap();
            }
            "reassessed" => assess(&workspace, &task, Some(json!([]))),
            _ => {
                workspace
                    .runtime
                    .update_task_as_human(
                        &task.id,
                        TaskUpdateParams {
                            comment: Some(decision),
                            ..Default::default()
                        },
                        "human:fixture".into(),
                    )
                    .unwrap();
            }
        }
        assert_eq!(exclusion(&workspace, &task), None, "{clearing}");
        assert_eq!(
            resolutions(&workspace, &task),
            if matches!(clearing, "approve-anyway" | "clear" | "rescope") {
                vec!["human:fixture".to_string()]
            } else {
                Vec::new()
            },
            "{clearing}"
        );
        if clearing != "rescope" {
            // A newer assessment with the finding supersedes the decision.
            assess(&workspace, &task, Some(needs_macos(1)));
            assert_native_os_wait(&workspace, &task, 1);
        }
    }
}

#[test]
fn tagged_unfounded_and_other_os_tasks_keep_their_admission() {
    if !isolated("task_pilot::native_os::tagged_unfounded_and_other_os_tasks_keep_their_admission")
    {
        return;
    }
    // Each case has its own workspace: the fixture tasks share one file, and
    // the drain's wave would defer all but the first.
    let workspace_on = |os| {
        let mut workspace = Workspace::new();
        on(&mut workspace, os);
        workspace
    };
    let workspace = workspace_on(HostOs::Macos);
    let tagged = backlog_task(&workspace, &[NATIVE_CRITERION], &["os:macos"]);
    assess(&workspace, &tagged, Some(needs_macos(1)));
    assert_eq!(tags(&workspace, &tagged), ["os:macos"]);
    assert_eq!(exclusion(&workspace, &tagged), None);

    // An OS or host mentioned only as a mock, a cross-compilation target,
    // another host's expected refusal or prose is no finding, so the pilot
    // records none and routing is unchanged.
    for criterion in [
        "A mocked macOS platform probe reports the sandbox launch.",
        "`cargo build --target aarch64-apple-darwin` cross-compiles the fixture.",
        "A Linux host refuses an `os:macos` task with `host_os_mismatch`.",
    ] {
        let workspace = workspace_on(HostOs::Linux);
        let task = backlog_task(&workspace, &[criterion], &[]);
        assess(&workspace, &task, None);
        assert_eq!(exclusion(&workspace, &task), None, "{criterion}");
        let applied = apply_findings(
            &workspace,
            &task,
            Some(json!({
                "required_os": [], "required_machine": [],
                "utility_warnings": [format!("Criterion 1 mentions macOS: {criterion}")],
            })),
        );
        assert_eq!(applied["applied_count"], 1, "{applied}");
        assert!(tags(&workspace, &task).is_empty(), "{criterion}");
        assert_eq!(exclusion(&workspace, &task), None, "{criterion}");
    }

    // Requirements on several OSes name no single host: no tag is added,
    // and every host that misses one of them is held.
    let workspace = workspace_on(HostOs::Linux);
    let both = backlog_task(
        &workspace,
        &["A Linux Bubblewrap launch succeeds.", NATIVE_CRITERION],
        &[],
    );
    assess(
        &workspace,
        &both,
        Some(json!([
            {"criterion": 1, "os": "linux", "evidence": "A native Bubblewrap launch."},
            {"criterion": 2, "os": "macos", "evidence": "A native sandbox-exec launch."},
        ])),
    );
    assert!(tags(&workspace, &both).is_empty());
    assert_native_os_wait(&workspace, &both, 2);

    // A task tagged for a different OS is still refused by its tags.
    for finding in [None, Some(needs_macos(1))] {
        let workspace = workspace_on(HostOs::Linux);
        let windows = backlog_task(&workspace, &[NATIVE_CRITERION], &["os:windows"]);
        assess(&workspace, &windows, finding);
        assert_eq!(tags(&workspace, &windows), ["os:windows"]);
        let (reason, detail) = exclusion(&workspace, &windows).unwrap();
        assert_eq!(reason, "host_os_mismatch");
        assert!(
            detail.contains("waits for a windows host (os:windows)"),
            "{detail}"
        );
    }
}

#[test]
fn a_malformed_native_os_finding_is_refused_at_apply() {
    if !isolated("task_pilot::native_os::a_malformed_native_os_finding_is_refused_at_apply") {
        return;
    }
    let workspace = Workspace::new();
    let task = backlog_task(&workspace, &[NATIVE_CRITERION], &[]);
    for (required_os, expected) in [
        (needs_macos(2), "criterion must be a 1-based index"),
        (needs_macos(0), "criterion must be a 1-based index"),
        (
            json!([{"criterion": 1, "os": "mac", "evidence": "native launch"}]),
            "os must be linux, macos, or windows",
        ),
        (
            json!([{"criterion": 1, "os": "macos", "evidence": " "}]),
            "evidence must be a non-empty string",
        ),
        (json!({"criterion": 1}), "must be an array or null"),
    ] {
        let applied = apply(&workspace, &task, Some(required_os));
        assert_eq!(applied["applied_count"], 0, "{applied}");
        let outcome = &applied["task_outcomes"][0];
        assert_eq!(outcome["outcome"], "invalid", "{applied}");
        assert!(
            outcome.to_string().contains(expected),
            "{expected}: {applied}"
        );
    }
    assert_eq!(exclusion(&workspace, &task), None);
}

#[test]
fn a_machine_finding_holds_every_machine_but_the_owner() {
    if !isolated("task_pilot::native_os::a_machine_finding_holds_every_machine_but_the_owner") {
        return;
    }
    let mut workspace = Workspace::new();
    let task = backlog_task(
        &workspace,
        &["Timings against the owner's live task store reach a p95 under 1 s."],
        &[],
    );
    let needs_owner = |machine: &str| {
        Some(json!({"required_machine": [{
            "criterion": 1, "machine": machine,
            "evidence": "Only the owner's live store holds the measured workspace.",
        }]}))
    };
    // The finding must name this owner: a pilot cannot route work to a
    // machine the owner does not know, and a malformed entry is refused.
    for (finding, expected) in [
        (
            needs_owner("dk-server-9"),
            "must name this owner, fixture-machine",
        ),
        (
            Some(
                json!({"required_machine": [{"criterion": 2, "machine": "fixture-machine", "evidence": "e"}]}),
            ),
            "criterion must be a 1-based index",
        ),
        (
            Some(
                json!({"required_machine": [{"criterion": 1, "machine": "fixture-machine", "evidence": ""}]}),
            ),
            "evidence must be a non-empty string",
        ),
    ] {
        let applied = apply_findings(&workspace, &task, finding);
        assert_eq!(applied["applied_count"], 0, "{applied}");
        assert!(
            applied.to_string().contains(expected),
            "{expected}: {applied}"
        );
    }

    let applied = apply_findings(&workspace, &task, needs_owner("FIXTURE-MACHINE"));
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit: Value =
        serde_json::from_str(comments.last().unwrap().message.split_once('\n').unwrap().1).unwrap();
    assert_eq!(
        audit["native_os_hold"]["machine"],
        json!({"criteria": [1], "machine_id": "fixture-machine"}),
        "{audit}"
    );
    assert!(tags(&workspace, &task).is_empty(), "a machine has no tag");
    // The owner itself may start the task.
    assert_eq!(exclusion(&workspace, &task), None);

    // Any other machine is held, with the machine named.
    workspace.runtime = workspace
        .runtime
        .clone()
        .with_automation_machine_identity(Some("other-machine".into()));
    let (reason, detail) = exclusion(&workspace, &task).expect("another machine is held");
    assert_eq!(reason, "native_os_required");
    assert!(
        detail.starts_with("Machine requirement: criterion 1")
            && detail.contains("only machine fixture-machine")
            && detail.contains("machine other-machine cannot satisfy it"),
        "{detail}"
    );

    // An evidenced operator decision releases it.
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                comment: Some(
                    "task-pilot-admission: clear\nThe operator recorded the timings on the owner."
                        .into(),
                ),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    assert_eq!(exclusion(&workspace, &task), None);
    assert_eq!(resolutions(&workspace, &task), ["human:fixture"]);
}
