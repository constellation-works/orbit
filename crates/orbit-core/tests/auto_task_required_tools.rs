//! Contract checks for tool requirements in bundled auto-task definitions.

use std::path::Path;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{AutoTaskDefinition, OrbitRuntime};
use serde_json::Value;

const BUNDLED_AUTO_TASKS: &[(&str, &str)] = &[
    (
        "backlog-hygiene",
        include_str!("../assets/auto_tasks/backlog-hygiene.yaml"),
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

const TASK_FILING_TEMPLATES: &[&str] = &[
    "code-review",
    "delivery-code-review",
    "full-code-review",
    "run-failure-patterns",
    "security-review",
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

#[test]
fn bundled_task_filing_templates_declare_task_add() {
    let definitions = BUNDLED_AUTO_TASKS
        .iter()
        .map(|(name, source)| (*name, parse_template(name, source)))
        .collect::<Vec<_>>();

    for name in TASK_FILING_TEMPLATES {
        let definition = definitions
            .iter()
            .find_map(|(candidate, definition)| (candidate == name).then_some(definition))
            .unwrap_or_else(|| panic!("missing bundled task-filing template {name}"));
        assert!(
            has_required_tool(definition, "orbit.task.add"),
            "bundled template {name} files tasks but does not declare orbit.task.add"
        );
    }
}

fn task_add_payloads(description: &str) -> Vec<Value> {
    let mut remaining = description;
    let mut payloads = Vec::new();
    while let Some((_, input)) = remaining.split_once("orbit.task.add --input '") {
        let (json, tail) = input
            .split_once('\'')
            .unwrap_or_else(|| panic!("task-add input in template is not closed"));
        let payload = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("task-add input in template is invalid JSON: {error}"));
        payloads.push(payload);
        remaining = tail;
    }
    payloads
}

#[test]
fn full_code_review_area_chore_declares_tools_used_for_findings() {
    let source = BUNDLED_AUTO_TASKS
        .iter()
        .find_map(|(name, source)| (*name == "full-code-review").then_some(*source))
        .unwrap_or_else(|| panic!("full-code-review is a bundled auto-task"));
    let coordinator = parse_template("full-code-review", source);
    assert!(has_required_tool(&coordinator, "orbit.task.add"));
    assert!(has_required_tool(&coordinator, "orbit.task.list"));

    // This is the preparation check: the generated chore must advertise every
    // Orbit tool its finding workflow needs, so task-pilot has no utility gap.
    let area_chore = task_add_payloads(&coordinator.template.description)
        .into_iter()
        .find(|payload| {
            payload["type"] == "chore"
                && payload["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|tag| tag == "full-code-review"))
        })
        .unwrap_or_else(|| panic!("full-code-review files an area chore"));
    let required_tools = area_chore["required_tools"]
        .as_array()
        .unwrap_or_else(|| panic!("area chore declares its required tools"));
    for tool in ["orbit.search", "orbit.task.add"] {
        assert!(
            required_tools.iter().any(|required| required == tool),
            "area chore omits {tool}; task-pilot would warn that findings cannot be filed"
        );
    }
}

#[test]
fn run_failure_fix_tasks_wait_for_human_approval() {
    let source = BUNDLED_AUTO_TASKS
        .iter()
        .find_map(|(name, source)| (*name == "run-failure-patterns").then_some(*source))
        .unwrap_or_else(|| panic!("run-failure-patterns is a bundled auto-task"));
    let definition = parse_template("run-failure-patterns", source);
    let payloads = task_add_payloads(&definition.template.description);
    let [fix_task] = payloads.as_slice() else {
        panic!("run-failure-patterns files one kind of fix task: {payloads:?}");
    };
    let tags = fix_task["tags"]
        .as_array()
        .unwrap_or_else(|| panic!("the fix task carries tags: {fix_task}"));
    for tag in ["run-failure-patterns", "no-auto-approve"] {
        assert!(
            tags.iter().any(|candidate| candidate == tag),
            "fix task omits `{tag}`: an unattended scan's tasks must stay proposed for human \
             approval, never promoted by an --approve-proposed drain"
        );
    }
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

fn normalize_markdown_text(text: &str) -> String {
    text.lines()
        .map(|line| line.trim().trim_start_matches('>').trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Finding tasks filed without scope sit held in `proposed` because
/// `orbit task lint` warns they declare no usable context_files (ORB-14935).
/// Every shipped auto-task template whose instructions call `orbit.task.add`
/// for findings must mention `context_files` in that command.
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

            let normalized = normalize_markdown_text(desc);
            let rule = "Derive canonical `file:` selectors from every evidenced source path, removing the line suffix, and add the matching regression-test location. Use existing paths where possible; for a test file that must be created, set `allow_missing_context: true` and identify that intended path. Never file a finding without context selectors.";
            if !normalized.contains(rule) {
                missing_context_files.push(format!(
                    "{name}: finding-filing template omits canonical file: selector derivation rule"
                ));
            }
        }
    }

    for expected in [
        "code-review",
        "delivery-code-review",
        "full-code-review",
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
