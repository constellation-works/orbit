//! Embedded default auto-task tests [ORB-10549]. Defaults must parse through
//! the same schema as workspace definitions and remain inert until explicitly
//! enabled or manually minted.

use std::path::PathBuf;

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::workflow::{AutoTaskSchedule, DedupePolicy};

use crate::application::auto_tasks::DEFAULT_AUTO_TASK_FILES;

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
        "code-review",
        "friction-curation",
        "qa-sweep",
        "security-review",
    ] {
        assert!(
            names.contains(&required),
            "missing shipped default {required}"
        );
    }
    for (stem, yaml) in DEFAULT_AUTO_TASK_FILES {
        let definition =
            parse_auto_task_yaml(yaml).unwrap_or_else(|error| panic!("parse {stem}: {error}"));
        assert_eq!(
            definition.name, *stem,
            "name must match file stem for {stem}"
        );
        assert!(
            !definition.enabled,
            "default auto-task {stem} must ship disabled"
        );
    }
}

/// Every repository-local definition remains covered in addition to the
/// embedded defaults. These files are workspace-authored and may intentionally
/// differ from the inert defaults.
#[test]
fn repository_definitions_all_parse() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".orbit/auto_tasks");
    let entries =
        std::fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    let mut count = 0usize;
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        let yaml = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let definition = parse_auto_task_yaml(&yaml)
            .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .expect("file stem");
        assert_eq!(
            definition.name, stem,
            "name must match file stem for {stem}"
        );
        count += 1;
    }
    assert!(
        count > 0,
        "expected at least one repository-local auto-task"
    );
}

#[test]
fn repository_qa_full_sweep_is_manual_opus_release_signoff() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".orbit/auto_tasks/qa-full-sweep.yaml");
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let definition = parse_auto_task_yaml(&yaml).expect("parse qa-full-sweep");

    assert_eq!(definition.name, "qa-full-sweep");
    assert!(!definition.enabled, "periodic scheduling must be opt-in");
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(definition.template.crew.as_deref(), Some("opus"));
    assert_eq!(
        definition.template.status,
        orbit_types::task::TaskStatus::Backlog
    );
    for required in [
        "scripts/qa-full-sweep-inventory.json",
        "orbit.task.artifact.put",
        "INCOMPLETE",
        "NOT_RUN",
        "Linux evidence never verifies macOS",
        "Do not implement repairs",
        "publish npm",
        "deploy the website",
    ] {
        assert!(
            yaml.contains(required),
            "missing sweep safeguard: {required}"
        );
    }
}

#[test]
fn model_price_audit_is_weekly_report_only_and_routes_to_terra() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".orbit/auto_tasks/model-price-audit.yaml");
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let definition = parse_auto_task_yaml(&yaml).expect("parse model-price-audit");

    assert_eq!(definition.name, "model-price-audit");
    assert!(!definition.enabled, "definition must ship disabled");
    assert_eq!(
        definition.schedule,
        AutoTaskSchedule::Cron {
            cron: "0 6 * * 1".to_string()
        }
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(definition.template.crew.as_deref(), Some("terra"));
    assert_eq!(
        definition.template.status,
        orbit_types::task::TaskStatus::Backlog
    );
    for required_tag in ["model-price-audit", "pricing", "no-diff-expected"] {
        assert!(
            definition
                .template
                .tags
                .iter()
                .any(|tag| tag == required_tag),
            "missing required tag {required_tag}"
        );
    }

    let body = definition.template.description.to_lowercase();
    for required in [
        "invocationrecord",
        "authoritative",
        "source url",
        "retrieval timestamp",
        "at most one",
        "historical rows",
        "non-overlapping",
        "short-context",
        "fast/service-tier",
        "long-context",
        "dry-run",
        "human review",
        "orchestration-session cost",
    ] {
        assert!(
            body.contains(required),
            "template should retain '{required}'"
        );
    }
    assert!(body.contains("must not edit model_prices.yaml"));
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
    assert_eq!(definition.template.crew.as_deref(), Some("system"));
    assert!(
        yaml.contains("\n  crew: system"),
        "default must name the portable system crew"
    );
    assert!(
        !yaml.contains("/home/") && !yaml.contains("/Users/"),
        "default must not contain a machine-specific path"
    );

    let body = definition.template.description.to_lowercase();
    for required in [
        "rejected tasks",
        "terminal rejection",
        "administrative rejection",
        "exactly one",
        "fail open",
        "repeat pass",
    ] {
        assert!(
            body.contains(required),
            "template should retain '{required}'"
        );
    }

    assert!(body.contains("orbit tool run orbit.friction.list"));
    assert!(body.contains("orbit tool run orbit.friction.update"));
    assert!(body.contains("orbit tool run orbit.friction.resolve"));
    assert!(!body.contains("orbit friction list"));
    assert!(!body.contains("orbit friction update"));
    assert!(!body.contains("orbit friction resolve"));
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
    assert_eq!(
        definition.schedule,
        AutoTaskSchedule::Cron {
            cron: "50 * * * *".to_string()
        },
        "qa-sweep must keep its documented hourly schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    // [ORB-10877] Same portable system-lane rule as friction-curation above.
    assert_eq!(definition.template.crew.as_deref(), Some("system"));
    assert!(
        yaml.contains("\n  crew: system"),
        "default must name the portable system crew"
    );
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
    assert!(!yaml.contains("/home/") && !yaml.contains("/Users/"));
    let body = definition.template.description.to_lowercase();
    for required in [
        "validate them hands-on",
        "exercise the affected",
        "documented setup",
        "writable temporary",
        "configured task or issue surface",
        "skip duplicates",
        "failing test",
        "standard validation command",
        "must be filed as a durable issue",
        "environment-specific",
        "test-harness",
        "portability",
        "narrative-only",
        "validation impact",
        "production impact",
        "failing command",
        "exact error",
        "environment evidence",
        "scope assessment",
    ] {
        assert!(
            body.contains(required),
            "template should retain '{required}'"
        );
    }
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
            "qa-sweep instructions must stay product-agnostic; found '{orbit_specific}'"
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
        "qa-sweep acceptance criteria must require durable reporting on the workspace issue surface"
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
        "qa-sweep acceptance criteria must require filing breaking tests"
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
    let repository_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".orbit/auto_tasks/code-review.yaml");
    let repository_yaml = std::fs::read_to_string(&repository_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", repository_path.display()));
    let repository_definition =
        parse_auto_task_yaml(&repository_yaml).expect("parse repository code-review");

    assert_eq!(definition.name, "code-review");
    assert!(!definition.enabled, "definition must ship disabled");
    assert_eq!(
        definition.schedule,
        AutoTaskSchedule::Cron {
            cron: "40 */6 * * *".to_string()
        },
        "code-review must use a documented six-hourly schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(definition.template.crew.as_deref(), Some("system"));
    assert!(
        yaml.contains("\n  crew: system"),
        "default must name the portable system crew"
    );
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
        "default must not contain a machine-specific path"
    );
    assert_eq!(
        repository_definition.template.description, definition.template.description,
        "repository and embedded code-review instructions must stay synchronized"
    );
    assert_eq!(
        repository_definition.template.acceptance_criteria, definition.template.acceptance_criteria,
        "repository and embedded code-review criteria must stay synchronized"
    );
    // The template ships to every workspace, so it must not name this
    // repository's branches or files.
    for repo_specific in ["agent-main", "ORB-", "CLAUDE.md", "make ci"] {
        assert!(
            !yaml.contains(repo_specific),
            "template must stay workspace-generic; found '{repo_specific}'"
        );
    }

    let body = definition.template.description.to_lowercase();
    for required in [
        "last-reviewed commit",
        "execution summary",
        "seeds the cursor",
        "verify every finding",
        "skip duplicates",
        "file:line",
        "no-op",
        "orbit tool run orbit.task.add",
        "orbit tool run orbit.task.list",
        "orbit tool run orbit.task.show",
        "orbit tool run orbit.search",
        "code-review-sweep",
        "both tag filters use and semantics",
        "lexicographically smaller task id",
        "never use a `code-review`-only finding",
        "only that case seeds the cursor",
    ] {
        assert!(
            body.contains(required),
            "template should retain '{required}'"
        );
    }
    assert!(!body.contains("orbit task add"));
    assert!(!body.contains("orbit task list"));
    assert!(!body.contains("orbit task show"));
    for sweep_query in [
        r#""tag":["code-review","no-diff-expected"],"limit":1"#,
        r#""tag":["code-review-sweep","no-diff-expected"],"limit":1"#,
    ] {
        assert!(
            definition.template.description.contains(sweep_query),
            "code-review must retain deterministic sweep query {sweep_query}"
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
        "code-review must require recording the window cursor"
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
        "code-review must require verified, evidenced, non-duplicate findings"
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
    assert_eq!(
        definition.schedule,
        AutoTaskSchedule::Cron {
            cron: "0 8 * * 1".to_string()
        },
        "security-review must use a documented weekly schedule"
    );
    assert!(matches!(definition.dedupe, DedupePolicy::SkipIfOpen));
    assert_eq!(definition.template.crew.as_deref(), Some("system"));
    assert!(
        yaml.contains("\n  crew: system"),
        "default must name the portable system crew"
    );
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
        "default must not contain a machine-specific path"
    );

    let body = definition.template.description.to_lowercase();
    for required in [
        "application code",
        "dependencies",
        "secret handling",
        "configuration",
        "evidence",
        "skip duplicates",
        "severity",
        "impact",
        "narrative-only",
        "no findings",
        "no-op",
        "orbit tool run orbit.task.add",
        "orbit tool run orbit.search",
        "orbit tool run orbit.task.show",
        "orbit tool run orbit.task.list",
    ] {
        assert!(
            body.contains(required),
            "template should retain '{required}'"
        );
    }
    assert!(!body.contains("orbit task add"));
    assert!(!body.contains("orbit task list"));
    assert!(!body.contains("orbit task show"));
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
        "security-review acceptance criteria must require durable filed findings"
    );
    assert!(
        definition
            .template
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.to_lowercase().contains("no findings")
                && criterion.to_lowercase().contains("no-op")),
        "security-review acceptance criteria must treat a clean review as success"
    );
}
