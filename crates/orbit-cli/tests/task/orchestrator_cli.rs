#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskOrigin, TaskStatus};
use serde_json::json;

use crate::isolated_cli_fixture::Fixture;

/// `orbit orchestrator scorecard` prints the tool's rows, one per
/// orchestrator, and `orbit orchestrator show` follows a count to its tasks.
#[test]
fn orchestrator_cli_scores_each_orchestrator_and_lists_the_tasks_behind_a_count() {
    if crate::isolated_test::run_in_child(
        "orchestrator_cli::orchestrator_cli_scores_each_orchestrator_and_lists_the_tasks_behind_a_count",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let (landed, dropped) = {
        // A managed run exports worker authority that would bind the fixture
        // runtime to the live store.
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("orchestrator-fixture"));
        let add = |title: &str, orchestrator: &str, origin: Option<TaskOrigin>, status| {
            let id = runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "orchestrator scorecard fixture".into(),
                    orchestrator: Some(orchestrator.into()),
                    origin,
                    complexity: TaskComplexity::Hard,
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            // An orchestrator is attributed in the backlog; settle the task
            // after, as the human override does.
            if status != TaskStatus::Backlog {
                runtime
                    .force_update_task_with_identity(
                        &id,
                        TaskUpdateParams {
                            status: Some(status),
                            ..Default::default()
                        },
                        None,
                        None,
                    )
                    .unwrap();
            }
            id
        };
        let landed = add(
            "landed",
            "opus",
            Some(TaskOrigin::OrchestratorInitiative),
            TaskStatus::Done,
        );
        add("open", "opus", None, TaskStatus::Backlog);
        let dropped = add(
            "dropped",
            "sonnet",
            Some(TaskOrigin::OrchestratorInitiative),
            TaskStatus::Rejected,
        );
        (landed, dropped)
    };

    let selection = ["--window", "30d", "--min-support", "2"];
    let mut scorecard =
        fixture.json(&[&["orchestrator", "scorecard"][..], &selection, &["--json"]].concat());
    let rows = scorecard["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{scorecard}");
    let opus = &rows[0];
    assert_eq!(opus["orchestrator"], "opus");
    assert_eq!(opus["below_support"], false);
    assert_eq!(opus["counts"]["owned"], 2);
    assert_eq!(opus["counts"]["landed"], 1);
    assert_eq!(opus["counts"]["weighted_landed"], 4, "hard weighs 4");
    assert_eq!(opus["counts"]["landed_initiative"], 1);
    assert_eq!(opus["counts"]["unknown_origin"], 1);
    assert_eq!(opus["per_100_tasks"]["landed"], 50.0);
    let sonnet = &rows[1];
    assert_eq!(sonnet["orchestrator"], "sonnet");
    assert_eq!(sonnet["below_support"], true, "one owned task is below 2");
    assert_eq!(sonnet["counts"]["dropped_initiative"], 1);
    assert_eq!(sonnet["counts"]["net_initiative"], -1);

    let input = json!({ "window": "30d", "min_support": 2 }).to_string();
    let mut tool = fixture.json(&[
        "tool",
        "run",
        "orbit.orchestrator.scorecard",
        "--full",
        "--input",
        &input,
    ]);
    for payload in [&mut scorecard, &mut tool] {
        let object = payload.as_object_mut().unwrap();
        object.remove("computed_at");
        object.remove("since");
    }
    assert_eq!(scorecard, tool, "the command prints the tool's payload");

    let detail = fixture.json(
        &[
            &["orchestrator", "show", "sonnet"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    let dropped_component = detail["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["key"] == "dropped_initiative")
        .unwrap();
    assert_eq!(dropped_component["tasks"][0]["task_id"], dropped.as_str());

    // The human views list each orchestrator and the tasks behind a count.
    let human = |args: &[&str]| {
        let out = fixture
            .command(&[args, &selection].concat())
            .env("NO_COLOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    };
    let table = human(&["orchestrator", "scorecard"]);
    assert!(
        table.lines().any(|line| line.starts_with("opus")) && table.contains("sonnet"),
        "one row per orchestrator:\n{table}"
    );
    let shown = human(&["orchestrator", "show", "opus"]);
    assert!(
        shown
            .lines()
            .any(|line| line.trim_start().starts_with(landed.as_str())),
        "the landed task is listed under its count:\n{shown}"
    );

    let refused = fixture
        .command(&["orchestrator", "show", "nobody"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("nobody"),
        "an orchestrator no task names is refused by name: {refused}"
    );
}
