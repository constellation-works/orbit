//! A pilot's typed `required_os` finding routes an untagged task by the `os:`
//! tag apply adds, and otherwise holds a backlog task on a host of another OS
//! across the real drain, ship-selection, readiness and workflow admission
//! boundaries, until the matching `os:` tag, a re-scope, an evidenced
//! operator decision or a newer assessment without it clears the wait. A
//! `required_machine` finding holds every machine but the owner it names.

use std::collections::BTreeSet;

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use orbit_types::task::HostOs;
use orbit_types::workflow::{ExecutorDef, ExecutorType};
use serde_json::{Value, json};

use super::{Workspace, runtime_at};

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

fn a_native_os_finding_tags_an_untagged_task_for_that_os() {
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
        serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
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

fn a_native_os_wait_clears_on_a_decision_a_rescope_or_a_newer_assessment() {
    for clearing in ["approve-anyway", "clear", "rescope", "reassessed"] {
        let mut workspace = Workspace::new();
        on(&mut workspace, HostOs::Linux);
        // The operator routed the task to Linux; the pilot keeps that tag, so
        // its finding holds the Linux host instead.
        let task = backlog_task(&workspace, &[NATIVE_CRITERION], &["os:linux"]);
        let decision = format!(
            "task-pilot-admission: {clearing}\nThe operator ran the macOS launch on their own host."
        );
        let required_os = json!([
            {"criterion": 1, "os": "macos", "evidence": "A native sandbox-exec launch."},
            {"criterion": 1, "os": "linux", "evidence": "A native Bubblewrap launch."},
        ]);
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
        assess(&workspace, &task, Some(required_os.clone()));
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
            // A new pass with the same requirement set preserves a trusted
            // decision even if the pilot emits it in a different order.
            let mut reordered = required_os.as_array().unwrap().clone();
            reordered.reverse();
            reordered.push(reordered[0].clone());
            assess(&workspace, &task, Some(json!(reordered)));
            if clearing == "reassessed" {
                assert_native_os_wait(&workspace, &task, 1);
            } else {
                assert_eq!(exclusion(&workspace, &task), None, "{clearing}");
                assert_eq!(resolutions(&workspace, &task), ["human:fixture"]);
            }
        }
    }
}

fn a_native_os_decision_is_superseded_by_changed_findings() {
    for decision in ["approve-anyway", "clear"] {
        for change in ["criteria", "os", "criterion", "machine"] {
            let mut workspace = Workspace::new();
            on(&mut workspace, HostOs::Linux);
            let task = backlog_task(
                &workspace,
                &[NATIVE_CRITERION, "A native Windows launch succeeds."],
                &["os:linux"],
            );
            assess(&workspace, &task, Some(needs_macos(1)));
            assert_native_os_wait(&workspace, &task, 1);
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(format!(
                            "task-pilot-admission: {decision}\nThe operator recorded the native launch."
                        )),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assert_eq!(exclusion(&workspace, &task), None);

            match change {
                "criteria" => {
                    workspace
                        .runtime
                        .update_task_as_human(
                            &task.id,
                            TaskUpdateParams {
                                acceptance_criteria: Some(vec![
                                    "A real macOS sandbox-exec launch of the revised fixture succeeds."
                                        .into(),
                                    "A native Windows launch succeeds.".into(),
                                ]),
                                ..Default::default()
                            },
                            "human:fixture".into(),
                        )
                        .unwrap();
                    assess(&workspace, &task, Some(needs_macos(1)));
                    assert_native_os_wait(&workspace, &task, 1);
                }
                "os" => {
                    assess(
                        &workspace,
                        &task,
                        Some(json!([{
                            "criterion": 1, "os": "windows", "evidence": "A native Windows launch.",
                        }])),
                    );
                    let (reason, detail) =
                        exclusion(&workspace, &task).expect("changed OS is held");
                    assert_eq!(reason, "native_os_required");
                    assert!(detail.contains("native windows evidence"), "{detail}");
                }
                "criterion" => {
                    assess(&workspace, &task, Some(needs_macos(2)));
                    assert_native_os_wait(&workspace, &task, 2);
                }
                "machine" => {
                    let applied = apply_findings(
                        &workspace,
                        &task,
                        Some(json!({
                            "required_os": needs_macos(1),
                            "required_machine": [{
                                "criterion": 1, "machine": "fixture-machine",
                                "evidence": "The owner's live store is also needed.",
                            }],
                        })),
                    );
                    assert_eq!(applied["applied_count"], 1, "{applied}");
                    workspace.runtime = workspace
                        .runtime
                        .clone()
                        .with_automation_machine_identity(Some("other-machine".into()));
                    let (reason, detail) =
                        exclusion(&workspace, &task).expect("changed machine is held");
                    assert_eq!(reason, "native_os_required");
                    assert!(detail.starts_with("Machine requirement:"), "{detail}");
                }
                _ => unreachable!(),
            }
            assert_eq!(resolutions(&workspace, &task), ["human:fixture"]);
        }
    }
}

fn tagged_unfounded_and_other_os_tasks_keep_their_admission() {
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

fn a_malformed_native_os_finding_is_refused_at_apply() {
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

fn a_machine_finding_holds_every_machine_but_the_owner() {
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
        serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
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

/// A workspace whose `muse-wonder` crew is backed by the macOS-only `muse`
/// executor, while `fixture` is unrestricted. The medium pool holds only
/// `muse-wonder`, so a low-to-medium rerate redraws onto it.
fn macos_only_workspace() -> Workspace {
    let mut workspace = Workspace::new();
    std::fs::write(
        workspace.repo.join(".orbit/config.toml"),
        r#"
[workflow]
default_crew = "fixture"
low_complexity_crews = ["fixture"]
medium_complexity_crews = ["muse-wonder"]
[crews.fixture]
provider = "codex"
model = "fixture-model"
[crews.muse-wonder]
provider = "muse"
model = "muse-spark-1.3"
"#,
    )
    .unwrap();
    workspace.runtime = runtime_at(
        &workspace.runtime.global_root(),
        &workspace.repo.join(".orbit"),
    );
    workspace
        .runtime
        .upsert_executor_def(&ExecutorDef {
            name: "muse".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some("muse".to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            auth_probe: None,
            env: Default::default(),
            sandbox: None,
            allow_fallback: false,
            host_os: Some(BTreeSet::from([HostOs::Macos])),
            created_at: None,
            updated_at: None,
        })
        .unwrap();
    workspace
}

fn muse_task(workspace: &Workspace, tags: &[&str]) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Native host validation work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec![
                "A native Linux Bubblewrap launch succeeds.".into(),
                "The README fixture is repaired.".into(),
            ],
            plan: "Inspect README.md.".into(),
            tags: tags.iter().map(ToString::to_string).collect(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            crew: Some("muse-wonder".into()),
            ..Default::default()
        })
        .unwrap()
}

fn needs_linux(criterion: usize) -> Value {
    json!([{
        "criterion": criterion, "os": "linux",
        "evidence": "The criterion requires a native Linux Bubblewrap launch.",
    }])
}

const OS_CREW_CONFLICT: &str = "crew `muse-wonder` (executor `muse`, host_os: macos) \
    does not intersect the task's tags `os:linux`; no host can run it";

fn assert_unapplied_conflict(workspace: &Workspace, task: &Task, before: &Task, applied: &Value) {
    assert_eq!(applied["applied_count"], 0, "{applied}");
    let outcome = &applied["task_outcomes"][0];
    assert_eq!(outcome["outcome"], "invalid", "{applied}");
    assert!(
        outcome["error"]
            .as_str()
            .unwrap()
            .contains(OS_CREW_CONFLICT),
        "{applied}"
    );
    assert_eq!(
        applied["tasks"],
        json!([]),
        "no assessment is recorded: {applied}"
    );
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap(),
        *before,
        "tags, crew, complexity and status are unchanged"
    );
    assert!(
        workspace
            .runtime
            .get_task_history(&task.id)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "task_pilot_applied"),
        "the contradictory write is not marked applied"
    );
}

fn a_pilot_os_tag_conflicting_with_the_explicit_crew_is_refused_unapplied() {
    let mut workspace = macos_only_workspace();
    on(&mut workspace, HostOs::Macos);
    let task = muse_task(&workspace, &[]);
    assert_eq!(task.crew.as_deref(), Some("muse-wonder"));
    let before = workspace.runtime.get_task(&task.id).unwrap();
    let history_len = workspace.runtime.get_task_history(&task.id).unwrap().len();
    let comments_len = workspace.runtime.get_task_comments(&task.id).unwrap().len();

    let applied = apply(&workspace, &task, Some(needs_linux(1)));

    assert_unapplied_conflict(&workspace, &task, &before, &applied);
    assert_eq!(
        workspace.runtime.get_task_history(&task.id).unwrap().len(),
        history_len
    );
    assert_eq!(
        workspace.runtime.get_task_comments(&task.id).unwrap().len(),
        comments_len,
        "no pilot audit or approval comment is written"
    );
}

fn a_pilot_complexity_redraw_into_a_conflicting_crew_is_refused_unapplied() {
    let mut workspace = macos_only_workspace();
    on(&mut workspace, HostOs::Macos);
    let task = workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Native host validation work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec!["A native Linux Bubblewrap launch succeeds.".into()],
            plan: "Inspect README.md.".into(),
            tags: vec!["os:linux".into()],
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        task.crew.as_deref(),
        Some("fixture"),
        "the low pool draw assigns the unrestricted crew"
    );
    let before = workspace.runtime.get_task(&task.id).unwrap();

    // A medium assessment redraws from the medium pool, whose only member
    // is the macOS-only crew, against the task's existing `os:linux` tag.
    let applied = apply_findings(
        &workspace,
        &task,
        Some(json!({"recommended_complexity": "medium"})),
    );

    assert_unapplied_conflict(&workspace, &task, &before, &applied);
    let kept = workspace.runtime.get_task(&task.id).unwrap();
    assert_eq!(kept.crew.as_deref(), Some("fixture"));
    assert_eq!(kept.complexity, Some(TaskComplexity::Low));
}

fn compatible_and_unrestricted_pilot_assignments_still_apply() {
    let mut workspace = macos_only_workspace();
    on(&mut workspace, HostOs::Macos);

    // The macOS-only crew intersects the tag the finding adds.
    let muse = muse_task(&workspace, &[]);
    assess(&workspace, &muse, Some(needs_macos(1)));
    let kept = workspace.runtime.get_task(&muse.id).unwrap();
    assert_eq!(kept.tags, ["os:macos"]);
    assert_eq!(kept.crew.as_deref(), Some("muse-wonder"));

    // An unrestricted crew takes any native-OS tag.
    let fixture = backlog_task(
        &workspace,
        &["A native Linux Bubblewrap launch succeeds."],
        &[],
    );
    assess(&workspace, &fixture, Some(needs_linux(1)));
    let kept = workspace.runtime.get_task(&fixture.id).unwrap();
    assert_eq!(kept.tags, ["os:linux"]);
    assert_eq!(kept.crew.as_deref(), Some("fixture"));
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_native_os_finding_tags_an_untagged_task_for_that_os_scenarios,
    a_native_os_finding_tags_an_untagged_task_for_that_os,
    a_native_os_wait_clears_on_a_decision_a_rescope_or_a_newer_assessment,
    a_native_os_decision_is_superseded_by_changed_findings
);

crate::scenario_table!(
    tagged_unfounded_and_other_os_tasks_keep_their_admission_scenarios,
    tagged_unfounded_and_other_os_tasks_keep_their_admission,
    a_malformed_native_os_finding_is_refused_at_apply,
    a_machine_finding_holds_every_machine_but_the_owner
);

crate::scenario_table!(
    pilot_os_crew_intersection_scenarios,
    a_pilot_os_tag_conflicting_with_the_explicit_crew_is_refused_unapplied,
    a_pilot_complexity_redraw_into_a_conflicting_crew_is_refused_unapplied,
    compatible_and_unrestricted_pilot_assignments_still_apply
);
