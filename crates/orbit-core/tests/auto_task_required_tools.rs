//! Contract checks for tool requirements in bundled auto-task definitions.
#![allow(clippy::unwrap_used)]

#[path = "support/git.rs"]
mod git_test_support;

orbit_common::isolate_test_process!();

use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::application::auto_tasks::state::{cursor_state_path, load_cursor_state};
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{AutoTaskDefinition, OrbitRuntime};
use orbit_types::task::{TaskArtifact, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, SWEEP_CURSOR_ARTIFACT};
use serde_json::Value;

const BUNDLED_AUTO_TASKS: &[(&str, &str)] = &[
    (
        "architecture-audit",
        include_str!("../assets/auto_tasks/architecture-audit.yaml"),
    ),
    (
        "backlog-hygiene",
        include_str!("../assets/auto_tasks/backlog-hygiene.yaml"),
    ),
    (
        "ci-budget-watch",
        include_str!("../assets/auto_tasks/ci-budget-watch.yaml"),
    ),
    (
        "code-health-sweep",
        include_str!("../assets/auto_tasks/code-health-sweep.yaml"),
    ),
    (
        "code-review",
        include_str!("../assets/auto_tasks/code-review.yaml"),
    ),
    (
        "delivery-code-review",
        include_str!("../assets/auto_tasks/delivery-code-review.yaml"),
    ),
    (
        "doc-duties",
        include_str!("../assets/auto_tasks/doc-duties.yaml"),
    ),
    (
        "friction-curation",
        include_str!("../assets/auto_tasks/friction-curation.yaml"),
    ),
    (
        "full-code-review",
        include_str!("../assets/auto_tasks/full-code-review.yaml"),
    ),
    (
        "path-risk-review",
        include_str!("../assets/auto_tasks/path-risk-review.yaml"),
    ),
    (
        "qa-full-sweep",
        include_str!("../assets/auto_tasks/qa-full-sweep.yaml"),
    ),
    (
        "qa-sweep",
        include_str!("../assets/auto_tasks/qa-sweep.yaml"),
    ),
    (
        "run-failure-patterns",
        include_str!("../assets/auto_tasks/run-failure-patterns.yaml"),
    ),
    (
        "security-review",
        include_str!("../assets/auto_tasks/security-review.yaml"),
    ),
];

fn parse_template(name: &str, source: &str) -> AutoTaskDefinition {
    let definition: AutoTaskDefinition =
        serde_yaml::from_str(source).unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_eq!(definition.name, name);
    definition
}

fn has_required_tool(definition: &AutoTaskDefinition, tool: &str) -> bool {
    definition
        .template
        .required_tools
        .iter()
        .any(|required| required == tool)
}

fn task_add_payloads(description: &str) -> Vec<Value> {
    tool_payloads(description, "orbit.task.add")
}

/// Every `--input` payload the template passes to `tool`, parsed as JSON.
fn tool_payloads(description: &str, tool: &str) -> Vec<Value> {
    let marker = format!("{tool} --input '");
    let mut remaining = description;
    let mut payloads = Vec::new();
    while let Some((_, input)) = remaining.split_once(marker.as_str()) {
        let (json, tail) = input
            .split_once('\'')
            .unwrap_or_else(|| panic!("{tool} input in template is not closed"));
        let payload = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("{tool} input in template is invalid JSON: {error}"));
        payloads.push(payload);
        remaining = tail;
    }
    payloads
}

fn tags_of(payload: &Value) -> Vec<&str> {
    payload["tags"]
        .as_array()
        .unwrap_or_else(|| panic!("filed task carries tags: {payload}"))
        .iter()
        .map(|tag| {
            tag.as_str()
                .unwrap_or_else(|| panic!("tag is a string: {tag}"))
        })
        .collect()
}

fn declares_tools(payload: &Value, tools: &[&str]) -> bool {
    payload["required_tools"]
        .as_array()
        .is_some_and(|required| {
            tools
                .iter()
                .all(|tool| required.iter().any(|candidate| candidate == tool))
        })
}

fn spawned_from_coordinator(payload: &Value) -> bool {
    payload["relations"].as_array().is_some_and(|relations| {
        relations
            .iter()
            .any(|relation| relation["type"] == "spawned_from")
    })
}

/// Parse the coordinator's tool contracts, then exercise roll-up dedupe and
/// cursor selection through the public runtime and scheduler.
#[test]
fn qa_full_sweep_tool_contracts_and_symbolic_cursor_mints_next_sweep() {
    if !isolated("qa_full_sweep_tool_contracts_and_symbolic_cursor_mints_next_sweep") {
        return;
    }
    let source = BUNDLED_AUTO_TASKS
        .iter()
        .find_map(|(name, source)| (*name == "qa-full-sweep").then_some(*source))
        .unwrap_or_else(|| panic!("qa-full-sweep is a bundled auto-task"));
    let definition = parse_template("qa-full-sweep", source);
    for tool in [
        "orbit.task.add",
        "orbit.task.list",
        "orbit.task.update",
        "orbit.task.artifact.put",
    ] {
        assert!(
            has_required_tool(&definition, tool),
            "the coordinator calls {tool} but does not declare it"
        );
    }
    let description = &definition.template.description;
    let attachments = tool_payloads(description, "orbit.task.artifact.put");
    assert!(
        !attachments.is_empty(),
        "the coordinator provides an artifact payload"
    );
    for payload in attachments {
        for field in ["id", "source_path", "path"] {
            assert!(
                payload[field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "artifact payload requires {field}: {payload}"
            );
        }
    }
    let filed = task_add_payloads(description);
    let areas = filed
        .iter()
        .filter(|payload| {
            payload["type"] == "chore" && !tags_of(payload).contains(&"qa-full-sweep-rollup")
        })
        .collect::<Vec<_>>();
    let [area] = areas.as_slice() else {
        panic!("the coordinator files one kind of area chore: {areas:?}");
    };
    assert!(spawned_from_coordinator(area), "{area}");
    assert!(
        declares_tools(
            area,
            &[
                "orbit.search",
                "orbit.task.add",
                "orbit.task.show",
                "orbit.task.artifact.get",
                "orbit.task.artifact.put",
            ]
        ),
        "area chores declare the tools their progress and findings use: {area}"
    );
    let rollup = filed
        .iter()
        .find(|payload| tags_of(payload).contains(&"qa-full-sweep-rollup"))
        .unwrap_or_else(|| panic!("the coordinator files a roll-up"));
    assert!(spawned_from_coordinator(rollup), "{rollup}");
    assert!(
        declares_tools(
            rollup,
            &[
                "orbit.task.show",
                "orbit.task.artifact.get",
                "orbit.task.artifact.put"
            ]
        ),
        "the roll-up declares the tools it reads areas and records its decision with: {rollup}"
    );
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(workspace.join("auto_tasks")).unwrap();
    git(&repo, &["init", "-b", "sweep-fixture"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "seed",
        ],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    assert_eq!(
        runtime.paths().repo_root.canonicalize().unwrap(),
        repo.canonicalize().unwrap()
    );

    // Only the fixture's enablement, due interval and branch change. The
    // bundled cursor selector and task payload tags are used unchanged.
    let mut definition = definition;
    definition.enabled = true;
    definition.schedule = AutoTaskSchedule::Interval { every_minutes: 1 };
    definition.skip_if_unchanged.as_mut().unwrap().reference = "sweep-fixture".into();
    std::fs::write(
        workspace.join("auto_tasks/qa-full-sweep.yaml"),
        serde_yaml::to_string(&definition).unwrap(),
    )
    .unwrap();
    let now = Utc::now();
    let baseline = run_auto_task_scheduler_at(&runtime, now, SchedulerOptions::default()).unwrap();
    assert!(baseline.errors.is_empty(), "{baseline:?}");
    assert_eq!(baseline.reports.len(), 1);
    assert_eq!(baseline.reports[0].action, "baselined");
    let due = now + Duration::minutes(2);
    assert_eq!(
        scheduler_report(&runtime, due).action,
        "would_fire",
        "without a completed cursor the sweep must remain eligible"
    );

    let task = runtime
        .add_task(TaskAddParams {
            title: "Full QA decision".into(),
            task_type: Some(TaskType::Chore),
            tags: tags_of(rollup).into_iter().map(str::to_string).collect(),
            ..Default::default()
        })
        .unwrap_or_else(|error| panic!("file the roll-up: {error}"));
    assert_eq!(
        runtime
            .open_auto_task_instance(&definition)
            .unwrap_or_else(|error| panic!("read open instances: {error}")),
        Some(task.id.to_string()),
        "an open roll-up is an open sweep, so skip_if_open mints no second set"
    );
    assert_eq!(
        scheduler_report(&runtime, due).blocking_task_id,
        Some(task.id.clone())
    );
    runtime.update_task_with_identity(&task.id, TaskUpdateParams {
        upsert_artifacts: vec![TaskArtifact::from_text(SWEEP_CURSOR_ARTIFACT,
            serde_json::json!({"schema_version": 1, "ref": "sweep-fixture", "cursor": head}).to_string())],
        ..Default::default()
    }, None, None).unwrap();
    complete(&runtime, &task.id);

    // Later completed coordinator and area chores have no cursor. If either
    // shadows the roll-up, the probe becomes unknown and would mint again.
    let coordinator = runtime.auto_task_mint(&definition.name).unwrap();
    complete(&runtime, &coordinator.id);
    let area = runtime
        .add_task(TaskAddParams {
            title: "Synthetic QA area".into(),
            task_type: Some(TaskType::Chore),
            tags: tags_of(area).into_iter().map(str::to_string).collect(),
            ..Default::default()
        })
        .unwrap();
    complete(&runtime, &area.id);
    assert!(
        area.created_at > task.created_at,
        "the shadowing control must be newer"
    );
    let outcome = run_auto_task_scheduler_at(&runtime, due, SchedulerOptions::default()).unwrap();
    assert!(outcome.errors.is_empty(), "{outcome:?}");
    assert_eq!(outcome.reports.len(), 1);
    assert_eq!(outcome.reports[0].action, "skipped", "{outcome:?}");
    let state = load_cursor_state(&cursor_state_path(&runtime.paths().state_dir)).unwrap();
    let skip = state.definitions[&definition.name]
        .last_skip
        .as_ref()
        .unwrap();
    assert_eq!(skip.reason, "unchanged_since_last_sweep");
    assert_eq!(skip.cursor_task_id.as_deref(), Some(task.id.as_str()));

    git(
        &repo,
        &[
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "new landing",
        ],
    );
    assert_eq!(
        scheduler_report(&runtime, due + Duration::minutes(1)).action,
        "would_fire",
        "a commit beyond the completed roll-up cursor permits the next sweep"
    );

    // A symbolic cursor can resolve to the current tip just like the valid
    // SHA above. It must be treated as inconclusive so the next tick mints.
    let symbolic_cursor = runtime
        .add_task(TaskAddParams {
            title: "Sweep with a symbolic cursor".into(),
            task_type: Some(TaskType::Chore),
            tags: tags_of(rollup).into_iter().map(str::to_string).collect(),
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task_with_identity(
            &symbolic_cursor.id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    SWEEP_CURSOR_ARTIFACT,
                    serde_json::json!({
                        "schema_version": 1,
                        "ref": "sweep-fixture",
                        "cursor": "sweep-fixture"
                    })
                    .to_string(),
                )],
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    complete(&runtime, &symbolic_cursor.id);

    let next_tick = run_auto_task_scheduler_at(
        &runtime,
        due + Duration::minutes(1),
        SchedulerOptions::default(),
    )
    .unwrap();
    assert!(next_tick.errors.is_empty(), "{next_tick:?}");
    assert_eq!(next_tick.reports.len(), 1, "{next_tick:?}");
    assert_eq!(next_tick.reports[0].action, "fired", "{next_tick:?}");
    assert!(
        runtime
            .open_auto_task_instance(&definition)
            .unwrap()
            .is_some(),
        "an inconclusive symbolic cursor leaves the next sweep eligible to mint"
    );
}

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_QA_CONTRACT_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test,
        output.status,
        output.stdout,
        output.stderr,
    );
    false
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    let output = command.current_dir(repo).args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn complete(runtime: &OrbitRuntime, id: &str) {
    runtime
        .force_update_task_with_identity(
            id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
}

fn scheduler_report(
    runtime: &OrbitRuntime,
    now: chrono::DateTime<Utc>,
) -> orbit_core::application::auto_tasks::AutoTaskFireReport {
    let mut outcome =
        run_auto_task_scheduler_at(runtime, now, SchedulerOptions { dry_run: true }).unwrap();
    assert!(outcome.errors.is_empty(), "{outcome:?}");
    assert_eq!(outcome.reports.len(), 1, "{outcome:?}");
    outcome.reports.remove(0)
}

/// Lint the criteria as a task minted from them would carry them, and return
/// the `ac_specificity` findings. Going through `lint_task` keeps the check on
/// the public surface the operator sees.
fn ac_specificity_messages(criteria: &[String]) -> Vec<String> {
    let runtime = OrbitRuntime::in_memory()
        .unwrap_or_else(|error| panic!("build in-memory runtime: {error}"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Lint a shipped auto-task template".into(),
            acceptance_criteria: criteria.to_vec(),
            ..Default::default()
        })
        .unwrap_or_else(|error| panic!("mint a task from the template criteria: {error}"));
    runtime
        .lint_task(task.id.as_str())
        .unwrap_or_else(|error| panic!("lint the minted task: {error}"))
        .findings
        .into_iter()
        .filter(|finding| finding.check == "ac_specificity")
        .map(|finding| finding.message)
        .collect()
}

/// A shipped template whose criterion the lint flags makes every task minted
/// from it look broken to the operator approving it (ORB-14893). Iterating the
/// directory keeps a template added later under the same guard.
#[test]
fn shipped_template_criteria_pass_the_ac_specificity_lint() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/auto_tasks");
    let mut templates = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read auto-task directory entry: {error}"))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect::<Vec<_>>();
    templates.sort();
    assert!(
        !templates.is_empty(),
        "no shipped auto-task templates under {}",
        dir.display()
    );

    let mut flagged = Vec::new();
    for path in &templates {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("template file name is not UTF-8: {}", path.display()));
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let definition = parse_template(name, &source);
        flagged.extend(
            ac_specificity_messages(&definition.template.acceptance_criteria)
                .into_iter()
                .map(|message| format!("{name}: {message}")),
        );
    }
    assert!(
        flagged.is_empty(),
        "shipped auto-task templates have acceptance criteria the lint flags:\n{}",
        flagged.join("\n")
    );
}

/// Finding tasks filed without scope sit held in `proposed` because
/// `orbit task lint` warns they declare no usable context_files (ORB-14935).
/// Every finding-filing `orbit.task.add` payload in a shipped auto-task
/// template must carry a non-empty `context_files` array.
#[test]
fn shipped_templates_filing_findings_declare_context_files_in_task_add() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/auto_tasks");
    let mut templates = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read auto-task directory entry: {error}"))
                .path()
        })
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect::<Vec<_>>();
    templates.sort();
    assert!(
        !templates.is_empty(),
        "no shipped auto-task templates under {}",
        dir.display()
    );

    let mut checked_finding_templates = Vec::new();
    let mut missing_context_files = Vec::new();

    for path in &templates {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_else(|| panic!("template file name is not UTF-8: {}", path.display()));
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let definition = parse_template(name, &source);
        let desc = &definition.template.description;

        let calls_task_add_for_findings =
            desc.contains("orbit.task.add") && desc.to_lowercase().contains("finding");

        if calls_task_add_for_findings {
            checked_finding_templates.push(name.to_string());
            let payloads = task_add_payloads(desc);
            let finding_payloads = payloads
                .into_iter()
                .filter(|payload| payload["type"] == "bug")
                .collect::<Vec<_>>();

            if finding_payloads.is_empty() {
                missing_context_files.push(format!(
                    "{name}: instructions call orbit.task.add for findings but no valid task-add payload was found"
                ));
            } else {
                for payload in &finding_payloads {
                    let has_context_files = payload
                        .get("context_files")
                        .and_then(|v| v.as_array())
                        .is_some_and(|arr| !arr.is_empty());
                    if !has_context_files {
                        missing_context_files.push(format!(
                            "{name}: finding task-add payload omits context_files: {payload}"
                        ));
                    }
                }
            }
        }
    }

    for expected in [
        "code-review",
        "delivery-code-review",
        "full-code-review",
        "qa-full-sweep",
        "qa-sweep",
        "security-review",
    ] {
        assert!(
            checked_finding_templates.iter().any(|t| t == expected),
            "expected template {expected} to be checked for context_files in finding filing command"
        );
    }

    assert!(
        missing_context_files.is_empty(),
        "shipped auto-task templates call orbit.task.add for findings without context_files:\n{}",
        missing_context_files.join("\n")
    );
}

/// The escalation triage lane that replaced the blocked-task-triage
/// auto-task requeues only through `orbit.task.block_triage`, whose shared
/// bound and refusals it cannot bypass: its activity withholds every agent
/// tool that would edit, requeue or resume a task another way, and lets
/// it file the friction an Orbit-defect block needs.
#[test]
fn escalation_triage_holds_no_operator_only_tool() {
    let activity: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../assets/activities/triage_escalation.yaml")).unwrap();
    let disallowed: Vec<&str> = activity["spec"]["tool_disallow_list"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|tool| tool.as_str().unwrap())
        .collect();
    for forbidden in [
        "orbit.task.update",
        "orbit.task.requeue",
        "orbit.workflow.run.resume",
    ] {
        assert!(
            disallowed.contains(&forbidden),
            "the triager writes other tasks only through orbit.task.block_triage, not {forbidden}"
        );
    }
    assert!(
        !disallowed.contains(&"orbit.task.block_triage")
            && !disallowed.contains(&"orbit.friction.add"),
        "the triager records block dispositions and Orbit-defect frictions: {disallowed:?}"
    );
    let routine: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../assets/routines/blocked_task_triage.yaml")).unwrap();
    assert_eq!(
        routine["enabled"], false,
        "[ORB-10549] the replacing routine ships inert"
    );
}
