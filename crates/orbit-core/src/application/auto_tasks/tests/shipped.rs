//! Embedded default auto-task tests [ORB-10549]. Defaults must parse through
//! the same schema as workspace definitions and remain inert until explicitly
//! enabled or manually minted.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_tools::ToolRegistry;
use orbit_types::workflow::{AutoTaskSchedule, DedupePolicy};

use crate::application::auto_tasks::{
    BASE_BRANCH_PLACEHOLDER, DEFAULT_AUTO_TASK_FILES, render_default_auto_task,
};

/// Every embedded default parses, uses its filename identity, and remains
/// disabled. An enabled default would turn workspace initialization into an
/// implicit scheduler opt-in, so make that regression deterministic here.
#[test]
fn shipped_defaults_all_parse_and_are_disabled() {
    assert!(
        !DEFAULT_AUTO_TASK_FILES.is_empty(),
        "expected at least one shipped auto-task definition"
    );
    let names: Vec<&str> = DEFAULT_AUTO_TASK_FILES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    for required in [
        "backlog-hygiene",
        "code-review",
        "doc-duties",
        "friction-curation",
        "qa-sweep",
        "run-failure-patterns",
        "security-review",
    ] {
        assert!(
            names.contains(&required),
            "missing shipped default {required}"
        );
    }
    for (stem, yaml) in DEFAULT_AUTO_TASK_FILES {
        let yaml = render_default_auto_task(yaml, "main");
        let definition =
            parse_auto_task_yaml(&yaml).unwrap_or_else(|error| panic!("parse {stem}: {error}"));
        assert_eq!(
            definition.name, *stem,
            "name must match file stem for {stem}"
        );
        assert!(
            !definition.enabled,
            "default auto-task {stem} must ship disabled"
        );
        assert!(
            definition.template.complexity.is_some(),
            "[ORB-12463] default auto-task {stem} must declare an explicit complexity"
        );
    }
}

/// The hygiene task reports candidates for a human to act on. Granting a
/// status-changing task tool here would undermine its report-only contract.
#[test]
fn backlog_hygiene_default_is_weekly_inert_and_read_only() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "backlog-hygiene")
        .expect("backlog-hygiene default");
    let definition = parse_auto_task_yaml(yaml).expect("parse backlog-hygiene");

    assert!(!definition.enabled);
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    let AutoTaskSchedule::Cron { cron } = &definition.schedule else {
        panic!("backlog-hygiene must use cron");
    };
    assert_eq!(cron.split_whitespace().count(), 5);
    assert_eq!(cron.split_whitespace().nth(4), Some("1"));
    assert!(definition.template.complexity.is_some());
    for required_tag in ["backlog-hygiene", "no-diff-expected"] {
        assert!(
            definition
                .template
                .tags
                .iter()
                .any(|tag| tag == required_tag)
        );
    }
    assert!(
        definition
            .template
            .required_tools
            .iter()
            .all(|tool| matches!(
                tool.as_str(),
                "orbit.task.list" | "orbit.task.show" | "orbit.search"
            )),
        "report-only task must not request status-mutating tools"
    );
}

/// The delivery defaults ship to every workspace, so they carry the base
/// branch placeholder rather than this repository's `agent-main`; seeding
/// renders it to the registered base branch and nothing else changes.
#[test]
fn shipped_delivery_defaults_render_the_workspace_base_branch() {
    for name in ["delivery-qa", "delivery-code-review"] {
        let (_, yaml) = DEFAULT_AUTO_TASK_FILES
            .iter()
            .find(|(stem, _)| *stem == name)
            .unwrap_or_else(|| panic!("missing shipped default {name}"));
        assert!(
            yaml.contains(&format!("\n    branch: {BASE_BRANCH_PLACEHOLDER}\n")),
            "{name} must carry the base branch placeholder"
        );
        assert!(
            !yaml.contains("agent-main"),
            "{name} must not hardcode this repository's integration branch"
        );

        let rendered = render_default_auto_task(yaml, "main");
        assert!(!rendered.contains(BASE_BRANCH_PLACEHOLDER));
        assert_eq!(
            rendered.matches("\n    branch: main\n").count(),
            1,
            "{name} renders exactly the branch line"
        );
        let definition = parse_auto_task_yaml(&rendered)
            .unwrap_or_else(|error| panic!("parse rendered {name}: {error}"));
        let AutoTaskSchedule::Deliveries { deliveries_landed } = definition.schedule else {
            panic!("{name} is a delivery definition");
        };
        assert_eq!(deliveries_landed.branch, "main");
        assert!(!definition.enabled);
    }

    // The periodic sweeps carry the placeholder too: their `skip_if_unchanged`
    // precondition compares the workspace's own integration branch [ORB-12698].
    for name in ["code-review", "qa-sweep"] {
        let (_, yaml) = DEFAULT_AUTO_TASK_FILES
            .iter()
            .find(|(stem, _)| *stem == name)
            .unwrap_or_else(|| panic!("missing shipped default {name}"));
        assert!(
            yaml.contains(&format!("\n  ref: {BASE_BRANCH_PLACEHOLDER}\n")),
            "{name} must carry the base branch placeholder"
        );
        let rendered = render_default_auto_task(yaml, "main");
        let definition = parse_auto_task_yaml(&rendered)
            .unwrap_or_else(|error| panic!("parse rendered {name}: {error}"));
        let precondition = definition
            .skip_if_unchanged
            .unwrap_or_else(|| panic!("{name} must ship the mint-time precondition"));
        assert_eq!(precondition.reference, "main");
        assert!(
            precondition.cursor.tags.iter().any(|tag| tag == name),
            "{name} must select its own completed sweeps"
        );
        assert!(
            precondition
                .cursor
                .tags
                .iter()
                .any(|tag| tag == "no-diff-expected"),
            "{name} must select completed sweeps, not their findings"
        );
    }

    let (_, without_placeholder) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(stem, _)| *stem == "friction-curation")
        .expect("friction-curation default");
    assert!(
        matches!(
            render_default_auto_task(without_placeholder, "main"),
            std::borrow::Cow::Borrowed(_)
        ),
        "defaults without the placeholder are returned as shipped"
    );
}

/// Every definition in `dir` parses, uses its filename identity, and declares
/// a complexity. Workspace-authored definitions may intentionally differ from
/// the inert defaults, so this checks structure only.
fn assert_auto_task_directory_all_parse(dir: &Path) -> usize {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    let mut count = 0usize;
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        let yaml = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let (definition, has_complexity) = match parse_auto_task_yaml(&yaml) {
            Ok(def) => {
                let has_comp = def.template.complexity.is_some();
                (def, has_comp)
            }
            Err(err)
                if err
                    .to_string()
                    .contains("template.complexity: unknown variant") =>
            {
                // [ORB-12711] An operator may configure a custom or experimental complexity
                // variant (such as `easy`). Normalize the complexity token to validate all
                // other structural invariants through parse_auto_task_yaml while confirming
                // that complexity was explicitly declared.
                let normalized: String = yaml
                    .lines()
                    .map(|line| {
                        let trimmed = line.trim_start();
                        if trimmed.starts_with("complexity:") {
                            let indent = &line[..line.len() - trimmed.len()];
                            format!("{indent}complexity: low")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let def = parse_auto_task_yaml(&normalized)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
                (def, true)
            }
            Err(err) => panic!("parse {}: {err}", path.display()),
        };
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("file stem");
        assert_eq!(
            definition.name, stem,
            "name must match file stem for {stem}"
        );
        assert!(
            has_complexity,
            "[ORB-12463] auto-task {stem} must declare an explicit complexity"
        );
        count += 1;
    }
    count
}

/// [ORB-12711] Adding an auto-task or editing complexity, crew, or schedule
/// must leave tests green without updating Rust match tables.
#[test]
fn repository_definitions_parser_is_directory_agnostic_and_tolerates_unseen_definition() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let unseen_yaml = r#"
schemaVersion: 1
name: unseen-custom-task
description: Operator-authored task with custom crew, schedule, and complexity.
enabled: false
schedule:
  cron: "15 4 * * 2"
dedupe: skip_if_open
template:
  title: Unseen custom task
  description: Custom task description.
  status: backlog
  task_type: chore
  priority: low
  complexity: low
  crew: custom-crew
"#;
    std::fs::write(temp.path().join("unseen-custom-task.yaml"), unseen_yaml)
        .expect("write unseen definition");

    let count = assert_auto_task_directory_all_parse(temp.path());
    assert_eq!(count, 1);
}

/// Friction curation is the portable default. It keeps the curation safeguards
/// while remaining disabled until an operator opts in.
#[test]
fn friction_curation_default_is_portable_and_inert() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "friction-curation")
        .expect("friction-curation default");
    let definition = parse_auto_task_yaml(yaml).expect("parse friction-curation");

    assert_eq!(definition.name, "friction-curation");
    assert!(!definition.enabled, "definition must ship disabled");
    assert!(
        matches!(definition.schedule, AutoTaskSchedule::Cron { .. }),
        "friction curation runs on a cron cadence"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    // [ORB-10877] `system` is a portable lane seeded for every detected family,
    // rather than a family-specific crew such as Luna or Sonnet.
    assert_eq!(
        definition.template.crew.as_deref(),
        Some("system"),
        "[ORB-10877] friction curation must use the portable system crew"
    );
    assert!(
        yaml.contains("\n  crew: system"),
        "[ORB-10877] default must name the portable system crew"
    );
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/"),
        "[ORB-10877] default must not contain a machine-specific path"
    );

    let body = definition.template.description.to_lowercase();
    assert!(
        body.contains("orbit tool run orbit.friction.list"),
        "[ORB-12248] instruct agent-reachable tool"
    );
    assert!(
        body.contains("orbit tool run orbit.friction.update"),
        "[ORB-12248] instruct agent-reachable tool"
    );
    assert!(
        body.contains(
            r#"orbit tool run orbit.friction.update --input '{"id":"<id>","status":"resolved"}'"#
        ),
        "[ORB-12248] resolving a friction must go through the agent-reachable `update` tool, not the hidden `resolve` tool"
    );
    assert!(
        !body.contains("orbit tool run orbit.friction.resolve"),
        "[ORB-12248] orbit.friction.resolve is hidden from the agent tool surface and must not be instructed here"
    );
    assert!(
        !body.contains("orbit friction list"),
        "[ORB-12248] must use `orbit tool run` syntax"
    );
    assert!(
        !body.contains("orbit friction update"),
        "[ORB-12248] must use `orbit tool run` syntax"
    );
}

#[test]
fn qa_sweep_default_preserves_hands_on_validation_contract() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "qa-sweep")
        .expect("qa-sweep default");
    let definition = parse_auto_task_yaml(yaml).expect("parse qa-sweep");

    assert_eq!(definition.name, "qa-sweep");
    assert!(!definition.enabled);
    assert!(
        matches!(definition.schedule, AutoTaskSchedule::Cron { .. }),
        "qa-sweep must use a cron schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(
        definition.template.status,
        orbit_types::task::TaskStatus::Backlog
    );
    assert_eq!(
        definition.template.task_type,
        orbit_types::task::TaskType::Chore
    );
    assert_eq!(
        definition.template.priority,
        orbit_types::task::TaskPriority::Medium
    );
    assert!(definition.template.tags.iter().any(|tag| tag == "qa-sweep"));
    assert!(
        definition
            .template
            .tags
            .iter()
            .any(|tag| tag == "no-diff-expected")
    );
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/"),
        "[ORB-10550] default must not contain a machine-specific path"
    );
    let yaml_lower = yaml.to_lowercase();
    for orbit_specific in [
        "orbit init",
        "workspace init",
        "--root",
        "~/.orbit",
        "orbit mcp",
        "orbit tool run",
        "filed as an orbit task",
        "filed as orbit tasks",
        "tag it `qa-sweep`",
    ] {
        assert!(
            !yaml_lower.contains(orbit_specific),
            "[ORB-10550] qa-sweep instructions must stay product-agnostic; found '{orbit_specific}'"
        );
    }
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| {
                let criterion = criterion.to_lowercase();
                criterion.contains("configured task or issue surface")
                    && criterion.contains("evidence")
                    && criterion.contains("reproduction")
            }),
        "[ORB-10550] qa-sweep acceptance criteria must require durable reporting on the workspace issue surface"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| {
                let criterion = criterion.to_lowercase();
                criterion.contains("failing test")
                    && criterion.contains("validation command")
                    && criterion.contains("validation impact")
                    && criterion.contains("production impact")
                    && !criterion.contains("orbit task")
            }),
        "[ORB-10550] qa-sweep acceptance criteria must require filing breaking tests"
    );
}

/// Code review sweep carries its window cursor in execution summaries rather
/// than in scheduler state, so the template must keep saying so, and it must
/// stay generic across workspaces.
#[test]
fn code_review_default_is_portable_cursor_driven_and_inert() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "code-review")
        .expect("code-review default");
    let definition = parse_auto_task_yaml(yaml).expect("parse code-review");

    assert_eq!(definition.name, "code-review");
    assert!(!definition.enabled, "definition must ship disabled");
    assert!(
        matches!(definition.schedule, AutoTaskSchedule::Cron { .. }),
        "code-review must use a documented cron schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(
        definition.template.status,
        orbit_types::task::TaskStatus::Backlog
    );
    for required_tag in ["code-review", "no-diff-expected"] {
        assert!(
            definition
                .template
                .tags
                .iter()
                .any(|tag| tag == required_tag),
            "missing required tag {required_tag}"
        );
    }
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/"),
        "[ORB-11095] default must not contain a machine-specific path"
    );
    // The template ships to every workspace, so it must not name this
    // repository's branches or files.
    for repo_specific in ["agent-main", "ORB-", "CLAUDE.md", "make ci"] {
        assert!(
            !yaml.contains(repo_specific),
            "[ORB-11095] template must stay workspace-generic; found '{repo_specific}'"
        );
    }

    assert!(
        !definition.template.description.contains("orbit task add"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    assert!(
        !definition.template.description.contains("orbit task list"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    assert!(
        !definition.template.description.contains("orbit task show"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    for sweep_query in [
        r#""tag":["code-review","no-diff-expected"],"limit":1"#,
        r#""tag":["code-review-sweep","no-diff-expected"],"limit":1"#,
    ] {
        assert!(
            definition.template.description.contains(sweep_query),
            "[ORB-11095] code-review must retain deterministic sweep query {sweep_query}"
        );
    }
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| {
                let criterion = criterion.to_lowercase();
                criterion.contains("reviewed range")
                    && criterion.contains("last-reviewed commit")
                    && criterion.contains("execution summary")
            }),
        "[ORB-11095] code-review must require recording the window cursor"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| {
                let criterion = criterion.to_lowercase();
                criterion.contains("verified against live code")
                    && criterion.contains("non-duplicate")
                    && criterion.contains("file:line")
            }),
        "[ORB-11095] code-review must require verified, evidenced, non-duplicate findings"
    );
}

#[test]
fn code_review_cursor_fixture_ignores_newer_finding_and_current_sweep() {
    struct ReviewTask<'a> {
        id: &'a str,
        created_order: u8,
        completed_order: Option<u8>,
        task_type: &'a str,
        tags: &'a [&'a str],
        cursor: Option<&'a str>,
    }

    let tasks = [
        ReviewTask {
            id: "legacy-sweep",
            created_order: 1,
            completed_order: Some(1),
            task_type: "chore",
            tags: &["code-review-sweep", "no-diff-expected"],
            cursor: Some("legacy-sweep-cursor"),
        },
        ReviewTask {
            id: "newer-finding",
            created_order: 2,
            completed_order: Some(2),
            task_type: "bug",
            tags: &["code-review"],
            cursor: None,
        },
        ReviewTask {
            id: "current-sweep",
            created_order: 3,
            completed_order: None,
            task_type: "chore",
            tags: &["code-review", "no-diff-expected"],
            cursor: None,
        },
    ];

    let selected = tasks
        .iter()
        .filter(|task| {
            let current_sweep =
                task.tags.contains(&"code-review") && task.tags.contains(&"no-diff-expected");
            let legacy_sweep =
                task.tags.contains(&"code-review-sweep") && task.tags.contains(&"no-diff-expected");
            task.completed_order.is_some()
                && task.task_type == "chore"
                && (current_sweep || legacy_sweep)
        })
        .max_by(|left, right| {
            left.created_order
                .cmp(&right.created_order)
                .then_with(|| right.id.cmp(left.id))
        })
        .and_then(|task| task.cursor);

    assert_eq!(selected, Some("legacy-sweep-cursor"));
}

#[test]
fn security_review_default_is_portable_weekly_and_inert() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "security-review")
        .expect("security-review default");
    let definition = parse_auto_task_yaml(yaml).expect("parse security-review");

    assert_eq!(definition.name, "security-review");
    assert!(!definition.enabled, "definition must ship disabled");
    assert!(
        matches!(definition.schedule, AutoTaskSchedule::Cron { .. }),
        "security-review must use a documented weekly schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(
        definition.template.status,
        orbit_types::task::TaskStatus::Backlog
    );
    assert!(
        definition
            .template
            .tags
            .iter()
            .any(|tag| tag == "security-review"),
        "minted tasks must carry the security-review tag"
    );
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/"),
        "[ORB-10950] default must not contain a machine-specific path"
    );

    assert!(
        !definition.template.description.contains("orbit task add"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    assert!(
        !definition.template.description.contains("orbit task list"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    assert!(
        !definition.template.description.contains("orbit task show"),
        "[ORB-12248] must use orbit tool run syntax"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| {
                let criterion = criterion.to_lowercase();
                criterion.contains("durable")
                    && criterion.contains("evidence")
                    && criterion.contains("severity")
                    && criterion.contains("impact")
                    && criterion.contains("narrative-only")
            }),
        "[ORB-10950] security-review acceptance criteria must require durable filed findings"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.to_lowercase().contains("no findings")
                && criterion.to_lowercase().contains("no-op")),
        "[ORB-10950] security-review acceptance criteria must treat a clean review as success"
    );
}

/// The run-failure scan's window starts at the prior instance's cursor, so the
/// prior-instance query must select this definition's own minted tags and the
/// cursor artifact it reads must be the one it attaches.
#[test]
fn run_failure_patterns_default_is_cursor_driven_portable_and_inert() {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "run-failure-patterns")
        .expect("run-failure-patterns default");
    let definition = parse_auto_task_yaml(yaml).expect("parse run-failure-patterns");

    assert!(!definition.enabled, "definition must ship disabled");
    assert!(matches!(definition.schedule, AutoTaskSchedule::Cron { .. }));
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    for required_tag in ["run-failure-patterns", "no-diff-expected"] {
        assert!(
            definition
                .template
                .tags
                .iter()
                .any(|tag| tag == required_tag),
            "missing required tag {required_tag}"
        );
    }
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/") && !yaml.contains("agent-main"),
        "default must stay workspace-generic"
    );

    let body = &definition.template.description;
    assert!(
        body.contains(r#""tag":["run-failure-patterns","no-diff-expected"],"limit":1"#),
        "the prior-instance query must select this definition's own completed scans"
    );
    assert!(
        body.contains(r#""id":"<id>","path":"run-failure-cursor.json""#),
        "the scan must read the prior instance's cursor artifact"
    );
    assert!(
        body.contains(
            r#""source_path":".orbit/tmp/run-failure-cursor.json","path":"run-failure-cursor.json""#
        ),
        "the scan must attach the cursor artifact the next scan reads"
    );

    // A run read without `--no-reconcile` finalizes an orphaned run and
    // releases its reservations, breaking the scan's no-mutation criterion.
    let mut run_reads = 0;
    for verb in ["history", "show", "logs", "events"] {
        for (start, _) in body.match_indices(&format!("`orbit run {verb}")) {
            let command = body[start + 1..].split('`').next().unwrap_or_default();
            assert!(
                command.contains("--no-reconcile"),
                "[ORB-12941] run read must not reconcile stale runs: {command}"
            );
            run_reads += 1;
        }
    }
    assert!(run_reads >= 4, "the scan must name its run read commands");
}

/// Every `orbit tool run <name>` mentioned in a shipped auto-task template
/// must name a tool the default registry actually exposes to an agent
/// caller. A template that instructs a hidden or retired tool sends an agent
/// to a call it cannot make [ORB-12248].
#[test]
fn shipped_auto_task_tool_run_mentions_resolve_to_registered_tools() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let active: BTreeSet<String> = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();

    for (stem, yaml) in DEFAULT_AUTO_TASK_FILES {
        for name in tool_run_mentions(yaml) {
            assert!(
                active.contains(&name),
                "{stem} instructs `orbit tool run {name}`, which is not a registered default tool"
            );
        }
    }
}

/// Every tool name immediately following an `orbit tool run ` mention in
/// `text`, in order of appearance.
fn tool_run_mentions(text: &str) -> Vec<String> {
    const MARKER: &str = "orbit tool run ";
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find(MARKER) {
        let after = &rest[index + MARKER.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
            .unwrap_or(after.len());
        names.push(after[..end].to_string());
        rest = &after[end..];
    }
    names
}

#[cfg(test)]
mod tool_run_mentions_tests {
    use super::tool_run_mentions;

    #[test]
    fn extracts_every_dotted_tool_name_in_order() {
        let text = "run `orbit tool run orbit.friction.update --input '{}'` then \
                     `orbit tool run orbit.task.show --input '{}'`.";

        assert_eq!(
            tool_run_mentions(text),
            vec!["orbit.friction.update", "orbit.task.show"]
        );
    }

    #[test]
    fn finds_nothing_when_absent() {
        assert!(tool_run_mentions("no tool invocations here").is_empty());
    }
}
