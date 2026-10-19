#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use serde_json::json;

use crate::isolated_cli_fixture::Fixture;

/// `orbit codebase analytics` prints the tool's rankings, and `orbit codebase
/// show` follows a ranked path's rate to the tasks behind it.
#[test]
fn codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let (regressed, regression) = {
        // A managed run exports worker authority that would bind the fixture
        // runtime to the live store.
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let add = |title: &str, status: TaskStatus, relations: Vec<TaskRelation>| {
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "codebase analytics fixture".into(),
                    context_files: vec!["file:src/lib.rs".into()],
                    relations,
                    status: Some(status),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string()
        };
        let regressed = add("regressed", TaskStatus::Done, vec![]);
        add("clean", TaskStatus::Done, vec![]);
        let regression = add(
            "regression",
            TaskStatus::Backlog,
            vec![TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: regressed.parse().unwrap(),
                regression: None,
            }],
        );
        (regressed, regression)
    };

    let selection = ["--window", "30d", "--min-support", "1"];
    let mut analytics =
        fixture.json(&[&["codebase", "analytics"][..], &selection, &["--json"]].concat());
    assert_eq!(
        analytics["regressions"]["rows"][0]["path"], "src/lib.rs",
        "{analytics}"
    );
    assert_eq!(analytics["regressions"]["rows"][0]["numerator"], 1);
    assert_eq!(analytics["regressions"]["rows"][0]["denominator"], 2);
    let input = json!({ "window": "30d", "min_support": 1 }).to_string();
    let mut tool = fixture.json(&[
        "tool",
        "run",
        "orbit.codebase.analytics",
        "--full",
        "--input",
        &input,
    ]);
    for payload in [&mut analytics, &mut tool] {
        let object = payload.as_object_mut().unwrap();
        object.remove("computed_at");
        object.remove("since");
    }
    assert_eq!(analytics, tool, "the command prints the tool's payload");

    let detail = fixture.json(
        &[
            &["codebase", "show", "src/lib.rs"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    let regressions = &detail["rankings"]["regressions"];
    assert_eq!(regressions["rank"], 1);
    assert_eq!(regressions["numerator"], 1);
    assert_eq!(regressions["tasks"][0]["task_id"], regressed.as_str());
    assert_eq!(
        regressions["tasks"][0]["regression_tasks"],
        json!([regression])
    );
    assert_eq!(regressions["tasks"].as_array().map(Vec::len), Some(1));

    // The human view lists the regressed task with the task that regressed it.
    let human = fixture
        .command(&[&["codebase", "show", "src/lib.rs"][..], &selection].concat())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.lines().any(|line| {
            line.trim_start().starts_with(regressed.as_str()) && line.contains(regression.as_str())
        }),
        "the regressed task and its regression are listed:\n{human}"
    );

    let refused = fixture
        .command(&["codebase", "show", "src/absent.rs"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("src/absent.rs"),
        "a path nothing touched is refused by name: {refused}"
    );
}
