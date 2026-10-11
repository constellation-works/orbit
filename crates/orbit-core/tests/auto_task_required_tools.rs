//! Contract checks for tool requirements in bundled auto-task definitions.

orbit_common::isolate_test_process!();

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

const TASK_FILING_TEMPLATES: &[&str] = &[
    "code-review",
    "delivery-code-review",
    "full-code-review",
    "qa-full-sweep",
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

/// The minted coordinator fans out: it attaches its inventory, files area
/// chores the pools route, and files a roll-up whose provenance tag makes the
/// scheduler's open-instance dedupe hold the next sweep until the decision is
/// made, so no second set of areas is filed while one is open.
#[test]
fn qa_full_sweep_files_routable_area_chores_and_a_rollup_that_holds_the_next_mint() {
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
    assert!(
        tool_payloads(description, "orbit.task.artifact.put")
            .iter()
            .any(|payload| payload["path"] == "qa-full-sweep-inventory.json"),
        "the coordinator attaches its inventory artifact"
    );

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
    assert_eq!(tags_of(area), ["qa-full-sweep", "no-diff-expected"]);
    assert!(
        area.get("crew").is_none(),
        "area chores name no crew, so the pools route them: {area}"
    );
    for complexity in area["complexity"]
        .as_str()
        .unwrap_or_else(|| panic!("area chore declares a complexity: {area}"))
        .trim_start_matches('<')
        .trim_end_matches('>')
        .split('|')
    {
        assert!(
            ["low", "medium", "hard"].contains(&complexity),
            "area chores are `hard` at most: {area}"
        );
    }
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
    assert!(
        filed
            .iter()
            .any(|payload| payload["type"] == "bug" && tags_of(payload).contains(&"qa-full-sweep")),
        "area testers file findings tagged qa-full-sweep"
    );

    let rollup = filed
        .iter()
        .find(|payload| tags_of(payload).contains(&"qa-full-sweep-rollup"))
        .unwrap_or_else(|| panic!("the coordinator files a roll-up"));
    assert!(spawned_from_coordinator(rollup), "{rollup}");
    assert!(rollup.get("crew").is_none(), "{rollup}");
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
    let cursor = definition
        .skip_if_unchanged
        .as_ref()
        .unwrap_or_else(|| panic!("qa-full-sweep skips a tick when nothing landed"));
    for tag in &cursor.cursor.tags {
        assert!(
            tags_of(rollup).contains(&tag.as_str()),
            "the roll-up records the cursor, so it must carry cursor tag {tag}"
        );
    }
    // The cursor task is the newest done chore carrying every cursor tag. A
    // later sweep's coordinator or area chore must not qualify, or it would
    // shadow the last decided sweep's cursor.
    let mut coordinator_tags = definition.template.tags.clone();
    coordinator_tags.push(orbit_types::workflow::auto_task_tag(&definition.name));
    for (task, tags) in [
        ("coordinator", coordinator_tags),
        (
            "area chore",
            tags_of(area).into_iter().map(str::to_string).collect(),
        ),
    ] {
        assert!(
            !cursor.cursor.tags.iter().all(|tag| tags.contains(tag)),
            "the {task} carries every cursor tag and could shadow the roll-up's cursor: {tags:?}"
        );
    }

    let runtime = OrbitRuntime::in_memory()
        .unwrap_or_else(|error| panic!("build in-memory runtime: {error}"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Full QA decision".into(),
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
