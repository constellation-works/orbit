//! Contract checks for tool requirements in bundled auto-task definitions.

use orbit_core::AutoTaskDefinition;
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
