#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskOrigin, TaskStatus};
use serde_json::{Value, json};

use crate::bound_server::{Announcement, BoundServer};
use crate::isolated_cli_fixture::Fixture;

/// The fields that differ between two reads of the same scan: when it ran,
/// the window start derived from it, and how long the caller keeps it.
fn scan_independent(mut payload: Value) -> Value {
    let object = payload.as_object_mut().unwrap();
    for key in ["computed_at", "since", "cache_ttl_seconds"] {
        object.remove(key);
    }
    payload
}

/// `orbit agents scorecard|show` and the `orbit.agents.*` tools answer the
/// dashboard's `/api/agents/{role}/*` payload from one scan per call; an
/// unknown role is refused; `orbit orchestrator` and `orbit.orchestrator.*`
/// answer the orchestrators payload with a note naming the replacement.
#[test]
fn agents_cli_and_tools_serve_the_api_payload_and_orchestrator_aliases_name_them() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_cli_and_tools_serve_the_api_payload_and_orchestrator_aliases_name_them",
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
            .with_actor(ActorIdentity::human("agents-fixture"));
        let add = |title: &str, orchestrator: &str, origin: Option<TaskOrigin>, status| {
            let id = runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "agent scorecard fixture".into(),
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
    let input = json!({ "role": "orchestrators", "window": "30d", "min_support": 2 });

    // The command and the tool scan once per call and keep nothing.
    let scorecard = fixture.json(
        &[
            &["agents", "scorecard", "--role", "orchestrators"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    assert_eq!(scorecard["role"], "orchestrators", "{scorecard}");
    assert_eq!(scorecard["cache_ttl_seconds"], 0, "{scorecard}");
    let rows = scorecard["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{scorecard}");
    let opus = rows.iter().find(|row| row["crew"] == "opus").unwrap();
    assert_eq!(opus["units"], 2, "{opus}");
    assert_eq!(opus["below_support"], false, "{opus}");
    let sonnet = rows.iter().find(|row| row["crew"] == "sonnet").unwrap();
    assert_eq!(sonnet["below_support"], true, "one owned task is below 2");
    let tool = fixture.json(&[
        "tool",
        "run",
        "orbit.agents.scorecard",
        "--full",
        "--input",
        &input.to_string(),
    ]);
    assert_eq!(tool["cache_ttl_seconds"], 0, "{tool}");
    assert_eq!(
        scan_independent(scorecard.clone()),
        scan_independent(tool),
        "the command prints the tool's payload"
    );

    let detail_input = json!({
        "role": "orchestrators", "crew": "sonnet", "window": "30d", "min_support": 2,
    });
    let detail = fixture.json(
        &[
            &[
                "agents",
                "show",
                "--role",
                "orchestrators",
                "--crew",
                "sonnet",
            ][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    assert_eq!(detail["row"]["crew"], "sonnet", "{detail}");
    assert_eq!(detail["cache_ttl_seconds"], 0, "{detail}");
    let tool_detail = fixture.json(&[
        "tool",
        "run",
        "orbit.agents.show",
        "--full",
        "--input",
        &detail_input.to_string(),
    ]);
    assert_eq!(
        scan_independent(detail.clone()),
        scan_independent(tool_detail),
        "show prints the tool's payload"
    );

    // The dashboard's routes answer the same payloads from its cached scan.
    {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&fixture.repo)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("XDG_CONFIG_HOME", fixture.home.join("xdg"))
            .arg("--root")
            .arg(&fixture.root)
            .args(["web", "serve", "--port", "0", "--no-open"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut server =
            BoundServer::spawn(&mut command, Announcement::Dashboard).expect("spawn dashboard");
        let addr = server
            .wait_for_address(Duration::from_secs(60))
            .expect("dashboard bound");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        let get = |path: &str| -> Value {
            let response = client
                .get(format!("http://{addr}{path}"))
                .send()
                .expect("GET");
            assert!(response.status().is_success(), "GET {path}: {response:?}");
            response.json().expect("JSON body")
        };
        let api = get("/api/agents/orchestrators/scorecard?window=30d&min_support=2");
        assert_eq!(
            scan_independent(scorecard.clone()),
            scan_independent(api),
            "the command prints the API's scorecard"
        );
        let api_detail =
            get("/api/agents/orchestrators/detail?window=30d&min_support=2&crew=sonnet");
        assert_eq!(
            scan_independent(detail.clone()),
            scan_independent(api_detail),
            "show prints the API's detail"
        );
    }

    let dropped_term = detail["terms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|term| term["tasks"].as_array().unwrap())
        .any(|task| task["task_id"] == dropped.as_str());
    let landed_term = fixture.json(
        &[
            &[
                "agents",
                "show",
                "--role",
                "orchestrators",
                "--crew",
                "opus",
            ][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    )["terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|term| term["key"] == "weighted_landed")
        .unwrap()
        .clone();
    assert!(!dropped_term, "a dropped task earns no term: {detail}");
    assert_eq!(landed_term["count"], 4, "hard weighs 4: {landed_term}");
    assert_eq!(landed_term["tasks"][0]["task_id"], landed.as_str());

    // An unknown role is refused by name on both surfaces.
    let refused = fixture
        .command(&["agents", "scorecard", "--role", "operators"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("operators") && refused.contains("orchestrators"),
        "the refusal names the role and the choices: {refused}"
    );
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.agents.scorecard",
            "--input",
            &json!({ "role": "operators" }).to_string(),
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let refused = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        refused.contains("operators") && refused.contains("orchestrators"),
        "the tool refusal names the role and the choices: {refused}"
    );

    // The deprecated aliases answer the orchestrators payload and name the
    // replacement: in the payload, and on stderr for the command.
    let alias = fixture
        .command(&[&["orchestrator", "scorecard"][..], &selection, &["--json"]].concat())
        .assert()
        .success()
        .get_output()
        .clone();
    let warning = String::from_utf8_lossy(&alias.stderr);
    assert!(
        warning.contains("deprecated") && warning.contains("orbit agents scorecard"),
        "the alias names `orbit agents`: {warning}"
    );
    let mut alias: Value = serde_json::from_slice(&alias.stdout).unwrap();
    let note = alias
        .as_object_mut()
        .unwrap()
        .remove("deprecation")
        .unwrap();
    assert!(
        note.as_str().unwrap().contains("orbit.agents.scorecard"),
        "{note}"
    );
    assert_eq!(
        scan_independent(alias),
        scan_independent(scorecard),
        "the alias answers the orchestrators scorecard"
    );
    let mut alias_tool = fixture.json(&[
        "tool",
        "run",
        "orbit.orchestrator.show",
        "--full",
        "--input",
        &json!({ "orchestrator": "sonnet", "window": "30d", "min_support": 2 }).to_string(),
    ]);
    let note = alias_tool
        .as_object_mut()
        .unwrap()
        .remove("deprecation")
        .unwrap();
    assert!(
        note.as_str().unwrap().contains("orbit.agents.show"),
        "{note}"
    );
    assert_eq!(
        scan_independent(alias_tool),
        scan_independent(detail),
        "the show alias answers the orchestrators detail"
    );

    // The human views list each row and the tasks behind a count.
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
    let table = human(&["agents", "scorecard", "--role", "orchestrators"]);
    assert!(
        table.contains("opus") && table.contains("sonnet"),
        "one row per orchestrator:\n{table}"
    );
    let shown = human(&[
        "agents",
        "show",
        "--role",
        "orchestrators",
        "--crew",
        "opus",
    ]);
    assert!(
        shown
            .lines()
            .any(|line| line.trim_start().starts_with(landed.as_str())),
        "the landed task is listed under its count:\n{shown}"
    );

    let refused = fixture
        .command(&[
            "agents",
            "show",
            "--role",
            "orchestrators",
            "--crew",
            "nobody",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("nobody"),
        "a crew with no row is refused by name: {refused}"
    );
}
