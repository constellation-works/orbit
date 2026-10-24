#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, JobRun, JobRunState, OrbitRuntime};
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
    // A plain record has the five fixed columns and one per term.
    let width = 5 + scorecard["terms"].as_array().unwrap().len();
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
        .find(|term| term["key"] == "landed")
        .unwrap()
        .clone();
    assert!(!dropped_term, "a dropped task earns no term: {detail}");
    assert_eq!(
        landed_term["count"], 1,
        "a landing counts once, whatever its complexity: {landed_term}"
    );
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

    // Plain stdout is the record stream: one tab-separated row per crew and
    // model, each with a field per column, and the selection and units
    // summary on stderr. `--header` puts the column header before the rows.
    let plain = |extra: &[&str]| {
        fixture
            .command(
                &[
                    &["agents", "scorecard", "--role", "orchestrators"][..],
                    &selection,
                    extra,
                ]
                .concat(),
            )
            .env("NO_COLOR", "1")
            .assert()
            .success()
            .get_output()
            .clone()
    };
    let piped = plain(&[]);
    let piped_stdout = String::from_utf8(piped.stdout).unwrap();
    let records: Vec<Vec<&str>> = piped_stdout
        .lines()
        .map(|line| line.split('\t').collect())
        .collect();
    assert_eq!(records.len(), 2, "one record per row:\n{piped_stdout}");
    assert!(
        records.iter().all(|fields| fields.len() == width),
        "every record has one field per column:\n{piped_stdout}"
    );
    let status = String::from_utf8_lossy(&piped.stderr);
    assert!(
        status.contains("tasks scanned") && status.contains("Units:"),
        "the summary stays on stderr:\n{status}"
    );
    let with_header = plain(&["--header"]);
    let with_header = String::from_utf8(with_header.stdout).unwrap();
    let header: Vec<&str> = with_header.lines().next().unwrap().split('\t').collect();
    assert_eq!(header.len(), width, "{with_header}");
    assert_eq!(header[0], "RANK", "{with_header}");
    assert_eq!(
        with_header.lines().count(),
        records.len() + 1,
        "--header adds exactly one line before the rows:\n{with_header}"
    );

    // An empty scorecard leaves stdout empty; its status is on stderr.
    let empty = fixture
        .command(
            &[
                &["agents", "scorecard", "--role", "implementers"][..],
                &selection,
            ]
            .concat(),
        )
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        empty.stdout.is_empty(),
        "an empty scorecard prints no stdout: {}",
        String::from_utf8_lossy(&empty.stdout)
    );
    assert!(
        !empty.stderr.is_empty(),
        "an empty scorecard still explains itself on stderr"
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

/// A crew that ran two models in the window has two implementers rows, so
/// `show` without a model is refused. The refusal names the selector each
/// surface takes and lists the candidate models; a listed model is accepted
/// through the selector its surface names.
#[test]
fn agents_show_ambiguous_crew_names_each_selector_and_its_candidate_models() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_show_ambiguous_crew_names_each_selector_and_its_candidate_models",
    ) {
        return;
    }

    const MODELS: [&str; 2] = ["claude-haiku-5-5", "claude-sonnet-5-5"];
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
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
        let workspace = runtime.workspace_id().unwrap();
        for (index, model) in MODELS.iter().enumerate() {
            let id = runtime
                .add_task(TaskAddParams {
                    title: format!("implemented {index}"),
                    description: "agent detail fixture".into(),
                    plan: "implement the change".into(),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            // An agent's move into in-progress starts the attempt the run worked.
            runtime
                .update_task_with_identity(
                    &id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    Some("claude".into()),
                    Some((*model).into()),
                )
                .unwrap();
            // The implementation run that worked the task, on the crew and
            // model its implementers row is keyed by. It began before the
            // attempt and finished during it, so no run owned the status move.
            let finished = Utc::now();
            let started = finished - chrono::Duration::hours(1);
            runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &workspace,
                    &JobRun {
                        run_id: format!("jrun-sol-{index}"),
                        job_id: "task_pr_pipeline".into(),
                        attempt: 1,
                        state: JobRunState::Success,
                        scheduled_at: started,
                        created_at: started,
                        started_at: Some(started),
                        finished_at: Some(finished),
                        duration_ms: None,
                        pid: None,
                        pid_start_time: None,
                        input: Some(json!({ "task_ids": [id] })),
                        retry_source_run_id: None,
                        knowledge_metrics: None,
                        resolved_crew: Some("sol".into()),
                        crew_model: Some((*model).into()),
                        steps: Vec::new(),
                        executed_on: None,
                    },
                    None,
                )
                .unwrap();
        }
    }

    // The CLI's selector is `--model`.
    let refused = fixture
        .command(&[
            "agents",
            "show",
            "--role",
            "implementers",
            "--crew",
            "sol",
            "--window",
            "30d",
        ])
        .assert()
        .code(1)
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("--model"),
        "the CLI names --model: {refused}"
    );
    for model in MODELS {
        assert!(refused.contains(model), "a candidate model: {refused}");
    }

    // Over MCP and `orbit tool run` the selector is `row_model`, and the
    // refusal keeps its error code. The JSON error payload goes to stderr.
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.agents.show",
            "--input",
            &json!({ "role": "implementers", "crew": "sol", "window": "30d" }).to_string(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused: Value = serde_json::from_slice(&refused).unwrap();
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    let message = refused["error"].as_str().unwrap();
    assert!(
        message.contains("row_model"),
        "the tool names row_model: {message}"
    );
    for model in MODELS {
        assert!(message.contains(model), "a candidate model: {message}");
    }

    // A listed model is accepted through each surface's selector.
    let by_row_model = fixture.json(&[
        "tool",
        "run",
        "orbit.agents.show",
        "--full",
        "--input",
        &json!({
            "role": "implementers", "crew": "sol", "window": "30d",
            "row_model": MODELS[1],
        })
        .to_string(),
    ]);
    assert_eq!(by_row_model["row"]["model"], MODELS[1], "{by_row_model}");
    let by_model = fixture.json(&[
        "agents",
        "show",
        "--role",
        "implementers",
        "--crew",
        "sol",
        "--window",
        "30d",
        "--model",
        MODELS[0],
        "--json",
    ]);
    assert_eq!(by_model["row"]["model"], MODELS[0], "{by_model}");
}

/// A crew whose tasks record two orchestrator models, one filed without a
/// recorded model (`model unknown`), keeps the deprecated `orbit orchestrator
/// show` and `orbit.orchestrator.show` aliases answering. Each model's row is
/// selected by `--model` (CLI) or `row_model` (tool), as on `orbit agents
/// show`, and the main Agents scorecard keeps one row per model.
#[test]
fn orchestrator_show_aliases_select_a_row_among_legacy_and_recorded_models() {
    if crate::isolated_test::run_in_child(
        "agents_cli::orchestrator_show_aliases_select_a_row_among_legacy_and_recorded_models",
    ) {
        return;
    }

    const CREW: &str = "opus";
    const RECORDED: &str = "claude-opus-5-5";
    const LEGACY: &str = "model unknown";
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
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
        // Filed without an orchestrator and given one afterward, which records
        // no model: the stored shape of a task filed before the field existed.
        let legacy = runtime
            .add_task(TaskAddParams {
                title: "legacy".into(),
                description: "orchestrator alias fixture".into(),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &legacy,
                TaskUpdateParams {
                    orchestrator: Some(Some(CREW.into())),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        runtime
            .add_task(TaskAddParams {
                title: "recorded".into(),
                description: "orchestrator alias fixture".into(),
                orchestrator: Some(CREW.into()),
                orchestrator_model: Some(RECORDED.into()),
                ..Default::default()
            })
            .unwrap();
    }

    // Main Agents keeps one row per recorded model.
    let scorecard = fixture.json(&["agents", "scorecard", "--role", "orchestrators", "--json"]);
    let models: Vec<&str> = scorecard["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["crew"] == CREW)
        .map(|row| row["model"].as_str().unwrap())
        .collect();
    assert_eq!(models.len(), 2, "one row per model: {scorecard}");
    assert!(
        models.contains(&LEGACY) && models.contains(&RECORDED),
        "{scorecard}"
    );

    // Without a selector the tool alias refuses rather than pick a row.
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.orchestrator.show",
            "--input",
            &json!({ "orchestrator": CREW }).to_string(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused: Value = serde_json::from_slice(&refused).unwrap();
    assert_eq!(refused["code"], "invalid_input", "{refused}");

    for model in [LEGACY, RECORDED] {
        // The CLI alias takes `--model`, as `orbit agents show` does.
        let mut alias = fixture.json(&["orchestrator", "show", CREW, "--model", model, "--json"]);
        alias
            .as_object_mut()
            .unwrap()
            .remove("deprecation")
            .unwrap();
        let detail = fixture.json(&[
            "agents",
            "show",
            "--role",
            "orchestrators",
            "--crew",
            CREW,
            "--model",
            model,
            "--json",
        ]);
        assert_eq!(alias["row"]["model"], model, "{alias}");
        assert_eq!(
            scan_independent(alias),
            scan_independent(detail),
            "the CLI alias answers the {model} row as main Agents does"
        );

        // The tool takes `row_model`, as `orbit.agents.show` does.
        let mut alias_tool = fixture.json(&[
            "tool",
            "run",
            "orbit.orchestrator.show",
            "--full",
            "--input",
            &json!({ "orchestrator": CREW, "row_model": model }).to_string(),
        ]);
        alias_tool
            .as_object_mut()
            .unwrap()
            .remove("deprecation")
            .unwrap();
        let agents_tool = fixture.json(&[
            "tool",
            "run",
            "orbit.agents.show",
            "--full",
            "--input",
            &json!({ "role": "orchestrators", "crew": CREW, "row_model": model }).to_string(),
        ]);
        assert_eq!(alias_tool["row"]["model"], model, "{alias_tool}");
        assert_eq!(
            scan_independent(alias_tool),
            scan_independent(agents_tool),
            "the tool alias answers the {model} row as orbit.agents.show does"
        );
    }
}

/// `--by-type` returns the cell matrix and the configured type list, is
/// refused for other roles, and leaves the plain ranking's shape alone.
#[test]
fn agents_scorecard_by_type_returns_cells_and_the_type_list() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_scorecard_by_type_returns_cells_and_the_type_list",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let config_path = workspace_root.join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    std::fs::write(
        &config_path,
        format!(
            "{existing}\n[[workflow.crew_performance_types]]\nname = \"review\"\ntags = [\"no-diff-expected\"]\n"
        ),
    )
    .unwrap();

    let card = fixture.json(&[
        "agents",
        "scorecard",
        "--role",
        "implementers",
        "--by-type",
        "--json",
    ]);
    assert_eq!(card["by_type"], true, "{card}");
    assert_eq!(card["types"][0]["name"], "review", "{card}");
    assert!(card["cells"].is_array(), "{card}");
    for cell in card["cells"].as_array().unwrap() {
        for field in [
            "crew",
            "model",
            "tier",
            "type",
            "n",
            "raw",
            "crew_mean",
            "score",
        ] {
            assert!(cell.get(field).is_some(), "cell misses {field}: {cell}");
        }
    }

    // The plain ranking keeps its rows shape and carries no cells.
    let plain = fixture.json(&["agents", "scorecard", "--role", "implementers", "--json"]);
    assert!(plain["rows"].is_array(), "{plain}");
    assert!(plain.get("cells").is_none(), "{plain}");

    // Other roles refuse the switch.
    fixture
        .command(&[
            "agents",
            "scorecard",
            "--role",
            "orchestrators",
            "--by-type",
            "--json",
        ])
        .assert()
        .failure();
}

/// `--by-type --shadow` returns the share report, including an empty window,
/// and is refused without `--by-type` or for another role. A call without
/// the switch keeps the ranking.
#[test]
fn agents_scorecard_shadow_reports_shares_and_refuses_a_partial_request() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_scorecard_shadow_reports_shares_and_refuses_a_partial_request",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let card = fixture.json(&[
        "agents",
        "scorecard",
        "--role",
        "implementers",
        "--by-type",
        "--shadow",
        "--window",
        "7d",
        "--json",
    ]);
    assert_eq!(card["shadow"], true, "{card}");
    assert_eq!(card["by_type"], true, "{card}");
    assert_eq!(card["window"], "7d", "{card}");
    assert_eq!(card["requested_window"], "7d", "{card}");
    assert!(card["pools"].as_array().unwrap().is_empty(), "{card}");
    assert_eq!(card["static_draws"]["draws"], 0, "{card}");
    assert!(
        card["static_draws"]["pools"].as_array().unwrap().is_empty(),
        "{card}"
    );
    assert!(card.get("cells").is_none(), "{card}");
    assert!(card.get("rows").is_none(), "{card}");

    let plain = fixture.json(&["agents", "scorecard", "--role", "implementers", "--json"]);
    assert!(plain.get("shadow").is_none(), "{plain}");
    assert!(plain["rows"].is_array(), "{plain}");

    let by_type = fixture.json(&[
        "agents",
        "scorecard",
        "--role",
        "implementers",
        "--by-type",
        "--json",
    ]);
    assert!(by_type.get("shadow").is_none(), "{by_type}");
    assert!(by_type["cells"].is_array(), "{by_type}");

    let shown = fixture
        .command(&[
            "agents",
            "scorecard",
            "--role",
            "implementers",
            "--by-type",
            "--shadow",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&shown.stdout),
        String::from_utf8_lossy(&shown.stderr),
    );
    assert!(
        text.contains("Would-be"),
        "the report says how to read would-be shares: {text}"
    );
    assert!(
        text.to_lowercase().contains("static"),
        "the report names the static bucket: {text}"
    );
    assert!(text.contains("no recorded draw in the window"), "{text}");

    fixture
        .command(&[
            "agents",
            "scorecard",
            "--role",
            "implementers",
            "--shadow",
            "--json",
        ])
        .assert()
        .failure();
    fixture
        .command(&[
            "agents",
            "scorecard",
            "--role",
            "orchestrators",
            "--by-type",
            "--shadow",
            "--json",
        ])
        .assert()
        .failure();
}
