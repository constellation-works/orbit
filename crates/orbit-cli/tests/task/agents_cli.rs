#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, JobRun, JobRunState, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::bound_server::{Announcement, BoundServer};
use crate::isolated_cli_fixture::Fixture;

/// The fields that differ between two reads of the same scan: when it ran,
/// the window start derived from it, the reported window end, and how long
/// the caller keeps it.
fn scan_independent(mut payload: Value) -> Value {
    let object = payload.as_object_mut().unwrap();
    for key in ["computed_at", "since", "until", "cache_ttl_seconds"] {
        object.remove(key);
    }
    payload
}

/// `orbit agents scorecard|show` and the `orbit.agents.*` tools answer the
/// dashboard's `/api/agents/{role}/*` payload from one scan per call; an
/// unknown role, including the removed orchestrators role, is refused.
#[test]
fn agents_cli_and_tools_serve_the_api_payload() {
    if crate::isolated_test::run_in_child("agents_cli::agents_cli_and_tools_serve_the_api_payload")
    {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let landed = {
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
        // One implemented task per call: the agent's move into in-progress
        // starts the attempt, and the implementation run on `crew` and
        // `model` that began before it and finished during it worked it.
        let implement = |run_id: &str, crew: &str, model: &str, status| {
            let id = runtime
                .add_task(TaskAddParams {
                    title: format!("implemented by {run_id}"),
                    description: "agent scorecard fixture".into(),
                    plan: "implement the change".into(),
                    complexity: TaskComplexity::Hard,
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            runtime
                .update_task_with_identity(
                    &id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    Some("claude".into()),
                    Some(model.into()),
                )
                .unwrap();
            let finished = Utc::now();
            let started = finished - chrono::Duration::hours(1);
            runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &workspace,
                    &JobRun {
                        build: None,
                        run_id: run_id.into(),
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
                        resolved_crew: Some(crew.into()),
                        crew_model: Some(model.into()),
                        steps: Vec::new(),
                        executed_on: None,
                    },
                    None,
                )
                .unwrap();
            if status != TaskStatus::InProgress {
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
        let landed = implement("jrun-opus-1", "opus", "claude-opus-5-5", TaskStatus::Done);
        implement(
            "jrun-opus-2",
            "opus",
            "claude-opus-5-5",
            TaskStatus::InProgress,
        );
        implement(
            "jrun-sonnet-1",
            "sonnet",
            "claude-sonnet-5-5",
            TaskStatus::InProgress,
        );
        landed
    };

    let selection = ["--window", "30d", "--min-support", "2"];
    let input = json!({ "role": "implementers", "window": "30d", "min_support": 2 });

    // The command and the tool scan once per call and keep nothing.
    let scorecard = fixture.json(
        &[
            &["agents", "scorecard", "--role", "implementers"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    assert_eq!(scorecard["role"], "implementers", "{scorecard}");
    assert_eq!(scorecard["cache_ttl_seconds"], 0, "{scorecard}");
    let rows = scorecard["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{scorecard}");
    // A plain record includes effort among seven fixed columns, plus one per term.
    let width = 6 + scorecard["terms"].as_array().unwrap().len();
    let opus = rows.iter().find(|row| row["crew"] == "opus").unwrap();
    assert_eq!(opus["units"], 2, "{opus}");
    assert_eq!(opus["below_support"], false, "{opus}");
    assert!(
        opus["interval"].is_object(),
        "ranked row needs an interval: {opus}"
    );
    let sonnet = rows.iter().find(|row| row["crew"] == "sonnet").unwrap();
    assert_eq!(sonnet["below_support"], true, "one owned task is below 2");
    assert!(
        sonnet["interval"].is_null(),
        "below-support row has no interval: {sonnet}"
    );
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

    let last1000 = fixture.json(&[
        "agents",
        "scorecard",
        "--role",
        "implementers",
        "--window",
        "last1000",
        "--min-support",
        "2",
        "--json",
    ]);
    assert_eq!(last1000["requested_window"], "last1000", "{last1000}");
    assert_eq!(last1000["window_attempts"], 1000, "{last1000}");

    let detail_input = json!({
        "role": "implementers", "crew": "sonnet", "window": "30d", "min_support": 2,
    });
    let detail = fixture.json(
        &[
            &[
                "agents",
                "show",
                "--role",
                "implementers",
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
        let api = get("/api/agents/implementers/scorecard?window=30d&min_support=2");
        assert_eq!(
            scan_independent(scorecard.clone()),
            scan_independent(api),
            "the command prints the API's scorecard"
        );
        let api_detail =
            get("/api/agents/implementers/detail?window=30d&min_support=2&crew=sonnet");
        assert_eq!(
            scan_independent(detail.clone()),
            scan_independent(api_detail),
            "show prints the API's detail"
        );
    }

    // An unknown role is refused by name on both surfaces.
    let refused = fixture
        .command(&["agents", "scorecard", "--role", "orchestrators"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("orchestrators") && refused.contains("implementers"),
        "the refusal names the role and the choices: {refused}"
    );
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.agents.scorecard",
            "--input",
            &json!({ "role": "orchestrators" }).to_string(),
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
        refused.contains("orchestrators") && refused.contains("implementers"),
        "the tool refusal names the role and the choices: {refused}"
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
    let table = human(&["agents", "scorecard", "--role", "implementers"]);
    assert!(
        table.contains("opus") && table.contains("sonnet"),
        "one row per implementer crew and model:\n{table}"
    );
    let shown = human(&["agents", "show", "--role", "implementers", "--crew", "opus"]);
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
                    &["agents", "scorecard", "--role", "implementers"][..],
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
    assert_ne!(
        records[0][5], "-",
        "ranked rows show their interval:\n{piped_stdout}"
    );
    assert_eq!(
        records[1][5], "-",
        "below-support rows show no interval:\n{piped_stdout}"
    );
    let status = String::from_utf8_lossy(&piped.stderr);
    assert!(!status.is_empty(), "the summary stays on stderr:\n{status}");
    let with_header = plain(&["--header"]);
    let with_header = String::from_utf8(with_header.stdout).unwrap();
    let header: Vec<&str> = with_header.lines().next().unwrap().split('\t').collect();
    assert_eq!(header.len(), width, "{with_header}");
    assert_eq!(header[0], "RANK", "{with_header}");
    assert_eq!(header[1], "ROW", "{with_header}");
    assert!(
        records
            .iter()
            .all(|fields| fields[1].ends_with("@ effort unknown")),
        "historical rows must not guess an effort: {piped_stdout}"
    );
    assert_eq!(
        with_header.lines().count(),
        records.len() + 1,
        "--header adds exactly one line before the rows:\n{with_header}"
    );

    // An empty scorecard leaves stdout empty; its status is on stderr.
    let empty = fixture
        .command(&[&["agents", "scorecard", "--role", "pilots"][..], &selection].concat())
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
            "implementers",
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
    // [ORB-19171] The missing-row refusal keeps crew, role and window and
    // offers a usable scorecard or window-selection next step instead of
    // implying the crew is unconfigured: a configured crew with no work in
    // the window also has no row.
    for part in [
        "implementers",
        "30d",
        "scorecard",
        "orbit agents scorecard --role implementers",
        "--window",
        "no work in the window has no row",
    ] {
        assert!(
            refused.contains(part),
            "ORB-19171: the missing-row refusal names a recovery step ({part}): {refused}"
        );
    }
    for part in [
        "unconfigured",
        "not configured",
        "unknown crew",
        "orbit_agents_",
        "orbit.agents.",
        "dashboard",
    ] {
        assert!(
            !refused.contains(part),
            "ORB-19171: the CLI refusal excludes other surfaces and configuration diagnoses ({part}): {refused}"
        );
    }

    // The tool surface refuses with the same code and recovery step.
    let refused = fixture
        .command(&[
            "tool",
            "run",
            "orbit.agents.show",
            "--input",
            &json!({ "role": "implementers", "crew": "nobody" }).to_string(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused: Value = serde_json::from_slice(&refused).unwrap();
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    let message = refused["error"].as_str().unwrap();
    for part in [
        "nobody",
        "implementers",
        "30d",
        "orbit.agents.scorecard",
        "window",
        "no work in the window has no row",
    ] {
        assert!(
            message.contains(part),
            "ORB-19171: the tool refusal names a recovery step ({part}): {message}"
        );
    }
    for part in ["--window", "orbit agents", "orbit_agents_", "dashboard"] {
        assert!(
            !message.contains(part),
            "ORB-19171: the tool refusal only names its own surface ({part}): {message}"
        );
    }
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
                        build: None,
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
            "{existing}\n[[workflow.task_types]]\nname = \"review\"\ntags = [\"audit-work\"]\n"
        ),
    )
    .unwrap();

    // Seed one counted implementation attempt so the cell matrix is
    // nonempty: a tagged task moved into in-progress by its crew/model,
    // worked by a finished implementation run in the window.
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("agents-fixture"));
        let workspace = runtime.workspace_id().unwrap();
        let id = runtime
            .add_task(TaskAddParams {
                title: "by-type implemented".into(),
                description: "by-type scorecard fixture".into(),
                plan: "implement the change".into(),
                tags: vec!["audit-work".to_string()],
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some("model-fixture".into()),
            )
            .unwrap();
        let finished = Utc::now();
        let started = finished - chrono::Duration::hours(1);
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(
                &workspace,
                &JobRun {
                    build: None,
                    run_id: "jrun-by-type-0".into(),
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
                    crew_model: Some("model-fixture".into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
    }

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
    let cells = card["cells"].as_array().unwrap();
    assert!(
        !cells.is_empty(),
        "the seeded attempt must produce cells: {card}"
    );
    assert!(
        cells
            .iter()
            .any(|cell| cell["type"] == "review" && cell["n"].as_u64().is_some_and(|n| n >= 1)),
        "the seeded audit-work attempt lands in a review cell: {card}"
    );
    for cell in cells {
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
            "reviewers",
            "--by-type",
            "--json",
        ])
        .assert()
        .failure();
}

/// Plain `--by-type` and `--per-task` output keeps the (model, recorded
/// effort) row identity and contributing crews that `--json` carries: one
/// crew and model at medium, high, provider-default and unknown effort must
/// not collapse into indistinguishable rows.
#[test]
fn agents_scorecard_plain_by_type_and_per_task_distinguish_effort_rows() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_scorecard_plain_by_type_and_per_task_distinguish_effort_rows",
    ) {
        return;
    }
    use orbit_types::identity::ReasoningEffort::{High, Medium};

    // (crew, recorded effort); `None` effort with an identity is the
    // provider default, and the last row records no invocation at all.
    let specs: [(&str, Option<Option<orbit_types::identity::ReasoningEffort>>); 5] = [
        ("sol", Some(Some(Medium))),
        ("other", Some(Some(Medium))),
        ("sol", Some(Some(High))),
        ("sol", Some(None)),
        ("sol", None),
    ];
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("agents-fixture"));
        let workspace = runtime.workspace_id().unwrap();
        for (index, (crew, effort)) in specs.iter().enumerate() {
            let id = runtime
                .add_task(TaskAddParams {
                    title: format!("effort-row {index}"),
                    description: "plain effort-row fixture".into(),
                    plan: "implement the change".into(),
                    complexity: TaskComplexity::Low,
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            runtime
                .update_task_with_identity(
                    &id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    Some("claude".into()),
                    Some("model-fixture".into()),
                )
                .unwrap();
            let run_id = format!("jrun-effort-{index}");
            let finished = Utc::now();
            let started = finished - chrono::Duration::hours(1);
            runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(
                    &workspace,
                    &JobRun {
                        build: None,
                        run_id: run_id.clone(),
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
                        resolved_crew: Some((*crew).into()),
                        crew_model: Some("model-fixture".into()),
                        steps: Vec::new(),
                        executed_on: None,
                    },
                    None,
                )
                .unwrap();
            if let Some(effort) = effort {
                let execution_id = runtime
                    .sqlite_store()
                    .unwrap()
                    .begin_agent_execution(
                        &workspace,
                        &run_id,
                        "implement_one",
                        "fixture",
                        Some("model-fixture"),
                        &orbit_types::telemetry::AgentExecutionIdentity {
                            crew: Some((*crew).into()),
                            reasoning_effort: *effort,
                        },
                    )
                    .unwrap();
                runtime
                    .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
                        agent_execution_id: Some(execution_id),
                        job_run_id: run_id,
                        account: None,
                        activity_id: "implement_one".into(),
                        agent: "fixture".into(),
                        provider: None,
                        model: Some("model-fixture".into()),
                        task_ids: vec![id],
                        trace: orbit_types::telemetry::InvocationTrace {
                            duration_ms: 60_000,
                            ..Default::default()
                        },
                    })
                    .unwrap();
            }
        }
    }

    let labels = [
        "model-fixture @ medium",
        "model-fixture @ high",
        "model-fixture @ provider default",
        "model-fixture @ effort unknown",
    ];
    // The JSON payload already distinguishes the rows; the plain tables
    // must carry the same identity instead of hiding it.
    let card = fixture.json(&[
        "agents",
        "scorecard",
        "--role",
        "implementers",
        "--by-type",
        "--json",
    ]);
    let json_labels: Vec<&str> = card["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["n"].as_u64().is_some_and(|n| n >= 1))
        .map(|cell| cell["label"].as_str().unwrap())
        .collect();
    for label in labels {
        assert!(json_labels.contains(&label), "JSON cell {label}: {card}");
    }

    let plain = |extra: &[&str]| {
        let out = fixture
            .command(
                &[
                    &["agents", "scorecard", "--role", "implementers"][..],
                    extra,
                    &["--header"],
                ]
                .concat(),
            )
            .env("NO_COLOR", "1")
            .assert()
            .success()
            .get_output()
            .clone();
        String::from_utf8(out.stdout).unwrap()
    };
    fn records(output: &str) -> Vec<Vec<&str>> {
        output
            .lines()
            .skip(1)
            .map(|line| line.split('\t').collect())
            .collect()
    }

    let by_type = plain(&["--by-type"]);
    let header: Vec<&str> = by_type.lines().next().unwrap().split('\t').collect();
    assert_eq!(header[0], "CREWS", "{by_type}");
    assert_eq!(header[1], "ROW", "{by_type}");
    let cells = records(&by_type);
    for label in labels {
        assert!(
            cells.iter().any(|fields| fields[1] == label),
            "plain by-type distinguishes {label}:\n{by_type}"
        );
    }
    let medium = cells.iter().find(|fields| fields[1] == labels[0]).unwrap();
    assert!(
        medium[0].contains("other") && medium[0].contains("sol"),
        "the medium by-type cell lists both contributing crews:\n{by_type}"
    );

    let per_task = plain(&["--per-task"]);
    let header: Vec<&str> = per_task.lines().next().unwrap().split('\t').collect();
    assert_eq!(header[0], "CREWS", "{per_task}");
    assert_eq!(header[1], "ROW", "{per_task}");
    let tasks = records(&per_task);
    assert_eq!(tasks.len(), specs.len(), "one row per task:\n{per_task}");
    for label in labels {
        assert!(
            tasks.iter().any(|fields| fields[1] == label),
            "plain per-task retains {label}:\n{per_task}"
        );
    }
    assert!(
        tasks
            .iter()
            .filter(|fields| fields[1] == labels[0])
            .any(|fields| fields[0].contains("other")),
        "a medium per-task row lists its contributing crew:\n{per_task}"
    );
}

#[test]
fn agents_scorecard_and_show_use_launch_effort_when_invocation_never_completed() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_scorecard_and_show_use_launch_effort_when_invocation_never_completed",
    ) {
        return;
    }
    use orbit_types::identity::ReasoningEffort::High;

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("agents-fixture"));
        let workspace = runtime.workspace_id().unwrap();
        let id = runtime
            .add_task(TaskAddParams {
                title: "launched but unfinished".into(),
                description: "plain launch-effort fixture".into(),
                plan: "implement the change".into(),
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some("model-fixture".into()),
            )
            .unwrap();
        let run_id = "jrun-launch-only";
        let finished = Utc::now();
        let started = finished - chrono::Duration::hours(1);
        runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(
                &workspace,
                &JobRun {
                    build: None,
                    run_id: run_id.into(),
                    job_id: "task_pr_pipeline".into(),
                    attempt: 1,
                    state: JobRunState::Failed,
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
                    crew_model: Some("model-fixture".into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
        // The agent launched at high effort, but its invocation never
        // completed, so no invocation record follows this launch.
        runtime
            .sqlite_store()
            .unwrap()
            .begin_agent_execution(
                &workspace,
                run_id,
                "implement_one",
                "fixture",
                Some("model-fixture"),
                &orbit_types::telemetry::AgentExecutionIdentity {
                    crew: Some("sol".into()),
                    reasoning_effort: Some(High),
                },
            )
            .unwrap();
    }

    let card = fixture.json(&["agents", "scorecard", "--role", "implementers", "--json"]);
    let rows = card["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one launch-attributed row: {card}");
    assert_eq!(rows[0]["label"], "model-fixture @ high", "{card}");
    assert_eq!(rows[0]["units"], json!(1), "{card}");

    let detail = fixture.json(&[
        "agents",
        "show",
        "--role",
        "implementers",
        "--crew",
        "sol",
        "--json",
    ]);
    assert_eq!(detail["row"]["label"], "model-fixture @ high", "{detail}");
    assert_eq!(detail["row"]["reasoning_effort"], json!("high"), "{detail}");
}

/// `orbit migrate --backfill-dispatch-effort` attributes an invocation that
/// predates dispatch identities to the effort its audited launch argv
/// rendered, in each provider's flag form, and leaves it unknown without a
/// matching launch or with launches that disagree. Identities recorded at
/// dispatch are untouched, a backfilled one is marked as such, and a second
/// run changes nothing [ORB-23823]. Two launches of one run, step and model
/// at different times are each recorded, and a launch a dispatch-time record
/// already covers is not recorded again [ORB-23945].
#[test]
fn migrate_backfill_dispatch_effort_attributes_pre_identity_invocations() {
    if crate::isolated_test::run_in_child(
        "agents_cli::migrate_backfill_dispatch_effort_attributes_pre_identity_invocations",
    ) {
        return;
    }
    use orbit_types::identity::ReasoningEffort;

    // (crew, recorded dispatch effort, launch argvs audited for the run).
    struct Spec {
        crew: &'static str,
        dispatch: Option<ReasoningEffort>,
        launches: Vec<Vec<&'static str>>,
    }
    let specs = [
        // 0: claude
        Spec {
            crew: "claude-crew",
            dispatch: None,
            launches: vec![vec![
                "claude",
                "--model",
                "model-fixture",
                "--effort",
                "high",
            ]],
        },
        // 1: codex
        Spec {
            crew: "codex-crew",
            dispatch: None,
            launches: vec![vec![
                "codex",
                "exec",
                "--config",
                "model_reasoning_effort=\"xhigh\"",
            ]],
        },
        // 2: grok
        Spec {
            crew: "grok-crew",
            dispatch: None,
            launches: vec![vec!["grok", "--reasoning-effort", "high"]],
        },
        // 3: muse
        Spec {
            crew: "muse-crew",
            dispatch: None,
            launches: vec![vec!["muse", "exec", "--reasoning-effort", "medium"]],
        },
        // 4: no effort flag rendered → provider default
        Spec {
            crew: "default-crew",
            dispatch: None,
            launches: vec![vec!["claude", "--model", "model-fixture"]],
        },
        // 5: no matching launch → unknown
        Spec {
            crew: "unknown-crew",
            dispatch: None,
            launches: vec![],
        },
        // 6: two candidate launches disagree → unknown
        Spec {
            crew: "unknown-crew",
            dispatch: None,
            launches: vec![
                vec!["claude", "--effort", "high"],
                vec!["claude", "--effort", "medium"],
            ],
        },
        // 7: identity recorded at dispatch; its launch says otherwise
        Spec {
            crew: "low-crew",
            dispatch: Some(ReasoningEffort::Low),
            launches: vec![vec!["claude", "--effort", "max"]],
        },
    ];
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let workspace;
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("agents-fixture"));
        workspace = runtime.workspace_id().unwrap();
        let store = runtime.sqlite_store().unwrap();
        for (index, spec) in specs.iter().enumerate() {
            let id = runtime
                .add_task(TaskAddParams {
                    title: format!("backfill {index}"),
                    description: "dispatch effort backfill fixture".into(),
                    plan: "implement the change".into(),
                    complexity: TaskComplexity::Low,
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            runtime
                .update_task_with_identity(
                    &id,
                    TaskUpdateParams {
                        status: Some(TaskStatus::InProgress),
                        ..Default::default()
                    },
                    Some("claude".into()),
                    Some("model-fixture".into()),
                )
                .unwrap();
            let run_id = format!("jrun-backfill-{index}");
            let finished = Utc::now();
            let started = finished - chrono::Duration::hours(1);
            store
                .upsert_job_run_for_workspace(
                    &workspace,
                    &JobRun {
                        build: None,
                        run_id: run_id.clone(),
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
                        resolved_crew: Some(spec.crew.into()),
                        crew_model: Some("model-fixture".into()),
                        steps: Vec::new(),
                        executed_on: None,
                    },
                    None,
                )
                .unwrap();
            // The invocation is recorded now and ran for a minute, so its
            // launch was a minute ago.
            let launched = Utc::now() - chrono::Duration::seconds(60);
            let audit = |seq: usize, model: &str, argv: &[&str]| {
                store
                    .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                        workspace_id: workspace.clone(),
                        event_id: format!("v2evt-{run_id}-{seq}"),
                        source: "fixture".into(),
                        schema_version: 1,
                        event_type: "cli.invocation.started".into(),
                        ts: launched,
                        run_id: run_id.clone(),
                        agent_identity: "system".into(),
                        parent_event_id: None,
                        workspace_path: None,
                        payload_json: json!({
                            "body_kind": "cli_invocation_started",
                            "model": model,
                            "argv_redacted": argv,
                        })
                        .to_string(),
                    })
                    .unwrap();
            };
            for (seq, argv) in spec.launches.iter().enumerate() {
                audit(seq, "model-fixture", argv);
            }
            let invoke = |activity: &str, model: &str, execution: Option<i64>| {
                runtime
                    .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
                        agent_execution_id: execution,
                        job_run_id: run_id.clone(),
                        account: None,
                        activity_id: activity.into(),
                        agent: "claude".into(),
                        provider: None,
                        model: Some(model.into()),
                        task_ids: vec![id.clone()],
                        trace: orbit_types::telemetry::InvocationTrace {
                            duration_ms: 60_000,
                            ..Default::default()
                        },
                    })
                    .unwrap();
            };
            let execution = spec.dispatch.map(|effort| {
                store
                    .begin_agent_execution(
                        &workspace,
                        &run_id,
                        "implement_one",
                        "claude",
                        Some("model-fixture"),
                        &orbit_types::telemetry::AgentExecutionIdentity {
                            crew: Some(spec.crew.into()),
                            reasoning_effort: Some(effort),
                        },
                    )
                    .unwrap()
            });
            invoke("implement_one", "model-fixture", execution);
            if index == 0 {
                // A pilot in the same run is not the run's executor: its
                // effort is backfilled, its crew is not guessed.
                audit(9, "model-pilot", &["claude", "--effort", "low"]);
                invoke("pilot", "model-pilot", None);
            }
        }
        // A launch event that records the effort directly, with no argv:
        // the recorded effort alone is evidence for its invocation.
        let recorded_id = runtime
            .add_task(TaskAddParams {
                title: "backfill recorded effort".into(),
                description: "dispatch effort backfill fixture".into(),
                plan: "implement the change".into(),
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &recorded_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some("model-fixture".into()),
            )
            .unwrap();
        let recorded_run = "jrun-backfill-recorded";
        let finished = Utc::now();
        let started = finished - chrono::Duration::hours(1);
        store
            .upsert_job_run_for_workspace(
                &workspace,
                &JobRun {
                    build: None,
                    run_id: recorded_run.into(),
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
                    input: Some(json!({ "task_ids": [recorded_id] })),
                    retry_source_run_id: None,
                    knowledge_metrics: None,
                    resolved_crew: Some("recorded-crew".into()),
                    crew_model: Some("model-fixture".into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
        store
            .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                workspace_id: workspace.clone(),
                event_id: format!("v2evt-{recorded_run}-0"),
                source: "fixture".into(),
                schema_version: 1,
                event_type: "cli.invocation.started".into(),
                ts: Utc::now() - chrono::Duration::seconds(60),
                run_id: recorded_run.into(),
                agent_identity: "system".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: json!({
                    "body_kind": "cli_invocation_started",
                    "provider": "claude",
                    "model": "model-fixture",
                    "reasoning_effort": "low",
                })
                .to_string(),
            })
            .unwrap();
        runtime
            .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
                agent_execution_id: None,
                job_run_id: recorded_run.into(),
                account: None,
                activity_id: "implement_one".into(),
                agent: "claude".into(),
                provider: None,
                model: Some("model-fixture".into()),
                task_ids: vec![recorded_id],
                trace: orbit_types::telemetry::InvocationTrace {
                    duration_ms: 60_000,
                    ..Default::default()
                },
            })
            .unwrap();
        // A run whose agent launched twice, at different efforts, but whose
        // invocation never completed. No invocation exists, so the backfill
        // records each launch on its own, proven to its own step interval.
        let launch_id = runtime
            .add_task(TaskAddParams {
                title: "backfill launch only".into(),
                description: "dispatch effort backfill fixture".into(),
                plan: "implement the change".into(),
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &launch_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some("model-fixture".into()),
            )
            .unwrap();
        let launch_run = "jrun-backfill-launch-only";
        let launch_now = Utc::now();
        let launch_started = launch_now - chrono::Duration::hours(2);
        store
            .upsert_job_run_for_workspace(
                &workspace,
                &JobRun {
                    build: None,
                    run_id: launch_run.into(),
                    job_id: "task_pr_pipeline".into(),
                    attempt: 1,
                    state: JobRunState::Failed,
                    scheduled_at: launch_started,
                    created_at: launch_started,
                    started_at: Some(launch_started),
                    finished_at: Some(launch_now),
                    duration_ms: None,
                    pid: None,
                    pid_start_time: None,
                    input: Some(json!({ "task_ids": [launch_id] })),
                    retry_source_run_id: None,
                    knowledge_metrics: None,
                    resolved_crew: Some("launch-crew".into()),
                    crew_model: Some("model-fixture".into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
        let step_event =
            |seq: usize, event_type: &str, body: serde_json::Value, at: chrono::DateTime<Utc>| {
                store
                    .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                        workspace_id: workspace.clone(),
                        event_id: format!("v2evt-{launch_run}-{seq}"),
                        source: "fixture".into(),
                        schema_version: 1,
                        event_type: event_type.into(),
                        ts: at,
                        run_id: launch_run.into(),
                        agent_identity: "system".into(),
                        parent_event_id: None,
                        workspace_path: None,
                        payload_json: body.to_string(),
                    })
                    .unwrap();
            };
        let launch_body = |effort: &str| {
            json!({
                "body_kind": "cli_invocation_started",
                "provider": "claude",
                "model": "model-fixture",
                "argv_redacted": ["claude", "--model", "model-fixture", "--effort", effort],
            })
        };
        step_event(
            0,
            "step.started",
            json!({ "body_kind": "step_started", "step_id": "implement_one" }),
            launch_now - chrono::Duration::hours(2),
        );
        step_event(
            1,
            "cli.invocation.started",
            launch_body("high"),
            launch_now - chrono::Duration::minutes(90),
        );
        step_event(
            2,
            "step.finished",
            json!({ "body_kind": "step_finished", "step_id": "implement_one" }),
            launch_now - chrono::Duration::minutes(80),
        );
        step_event(
            3,
            "step.started",
            json!({ "body_kind": "step_started", "step_id": "implement_one" }),
            launch_now - chrono::Duration::minutes(70),
        );
        step_event(
            4,
            "cli.invocation.started",
            launch_body("medium"),
            launch_now - chrono::Duration::minutes(40),
        );
        step_event(
            5,
            "step.finished",
            json!({ "body_kind": "step_finished", "step_id": "implement_one" }),
            launch_now - chrono::Duration::minutes(30),
        );
        // A launch Orbit already recorded at dispatch. The backfill must
        // leave that row as it is, not write a second one from the audit.
        let covered_id = runtime
            .add_task(TaskAddParams {
                title: "backfill dispatch covered".into(),
                description: "dispatch effort backfill fixture".into(),
                plan: "implement the change".into(),
                complexity: TaskComplexity::Low,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        runtime
            .update_task_with_identity(
                &covered_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some("model-fixture".into()),
            )
            .unwrap();
        let covered_run = "jrun-backfill-dispatch-covered";
        let covered_now = Utc::now();
        let covered_started = covered_now - chrono::Duration::hours(1);
        store
            .upsert_job_run_for_workspace(
                &workspace,
                &JobRun {
                    build: None,
                    run_id: covered_run.into(),
                    job_id: "task_pr_pipeline".into(),
                    attempt: 1,
                    state: JobRunState::Failed,
                    scheduled_at: covered_started,
                    created_at: covered_started,
                    started_at: Some(covered_started),
                    finished_at: Some(covered_now),
                    duration_ms: None,
                    pid: None,
                    pid_start_time: None,
                    input: Some(json!({ "task_ids": [covered_id] })),
                    retry_source_run_id: None,
                    knowledge_metrics: None,
                    resolved_crew: Some("covered-crew".into()),
                    crew_model: Some("model-fixture".into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
        store
            .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                workspace_id: workspace.clone(),
                event_id: format!("v2evt-{covered_run}-0"),
                source: "fixture".into(),
                schema_version: 1,
                event_type: "step.started".into(),
                ts: covered_now - chrono::Duration::minutes(1),
                run_id: covered_run.into(),
                agent_identity: "system".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: json!({
                    "body_kind": "step_started",
                    "step_id": "implement_one",
                })
                .to_string(),
            })
            .unwrap();
        let covered_launch = Utc::now();
        store
            .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                workspace_id: workspace.clone(),
                event_id: format!("v2evt-{covered_run}-1"),
                source: "fixture".into(),
                schema_version: 1,
                event_type: "cli.invocation.started".into(),
                ts: covered_launch,
                run_id: covered_run.into(),
                agent_identity: "system".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: json!({
                    "body_kind": "cli_invocation_started",
                    "provider": "claude",
                    "model": "model-fixture",
                    "argv_redacted": ["claude", "--model", "model-fixture", "--effort", "high"],
                })
                .to_string(),
            })
            .unwrap();
        store
            .begin_agent_execution(
                &workspace,
                covered_run,
                "implement_one",
                "claude",
                Some("model-fixture"),
                &orbit_types::telemetry::AgentExecutionIdentity {
                    crew: Some("covered-crew".into()),
                    reasoning_effort: Some(ReasoningEffort::Low),
                },
            )
            .unwrap();
    }

    let rows = || {
        let card = fixture.json(&[
            "agents",
            "scorecard",
            "--role",
            "implementers",
            "--min-support",
            "1",
            "--json",
        ]);
        let mut rows: Vec<(String, u64)> = card["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["label"].as_str().unwrap().to_string(),
                    row["units"].as_u64().unwrap(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    let before = rows();
    assert_eq!(
        before,
        vec![
            ("model-fixture @ effort unknown".to_string(), 9),
            ("model-fixture @ low".to_string(), 2),
        ],
        "only the dispatch-time identities are known before the backfill"
    );

    let report = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    let report = &report["dispatch_effort_backfill"];
    assert_eq!(report["examined"], 9, "{report}");
    assert_eq!(report["recorded"], 7, "{report}");
    assert_eq!(report["no_launch_event"], 1, "{report}");
    assert_eq!(report["conflicting"], 1, "{report}");
    assert_eq!(report["launch_only_recorded"], 2, "{report}");
    assert_eq!(report["launch_only_no_step"], 10, "{report}");
    assert_eq!(report["launch_only_no_provider"], 0, "{report}");
    assert_eq!(report["launch_only_unreadable"], 0, "{report}");
    assert_eq!(report["launch_only_unmatched"], 10, "{report}");

    let after = rows();
    assert_eq!(
        after,
        vec![
            ("model-fixture @ effort unknown".to_string(), 2),
            ("model-fixture @ high".to_string(), 2),
            ("model-fixture @ low".to_string(), 3),
            ("model-fixture @ medium".to_string(), 2),
            ("model-fixture @ provider default".to_string(), 1),
            ("model-fixture @ xhigh".to_string(), 1),
        ],
        "each flag form is attributed; both launch-only launches are recorded \
         and the unit follows the later one; the dispatch-time identities keep \
         their own effort; no launch and disagreeing invocation launches stay \
         unknown"
    );

    let again = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    let again = &again["dispatch_effort_backfill"];
    assert_eq!(again["examined"], 2, "{again}");
    assert_eq!(again["recorded"], 0, "{again}");
    assert_eq!(again["launch_only_recorded"], 0, "{again}");
    assert_eq!(again["launch_only_no_step"], 10, "{again}");
    assert_eq!(again["launch_only_no_provider"], 0, "{again}");
    assert_eq!(again["launch_only_unreadable"], 0, "{again}");
    assert_eq!(again["launch_only_unmatched"], 10, "{again}");
    assert_eq!(rows(), after, "a second run changes nothing");

    let _authority = orbit_common::test_env::unset(
        orbit_common::test_env::INHERITED_AUTHORITY_ENV
            .iter()
            .copied(),
    );
    let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root).unwrap();
    let store = runtime.sqlite_store().unwrap();
    let backfilled = store
        .agent_executions(&workspace, "jrun-backfill-0")
        .unwrap();
    assert_eq!(backfilled.len(), 2, "{backfilled:?}");
    let executor = backfilled
        .iter()
        .find(|record| record.activity_id == "implement_one")
        .unwrap();
    let source = executor
        .backfill
        .as_ref()
        .map(|backfill| backfill.source.as_str());
    assert_eq!(source, Some("cli.invocation.started"), "{executor:?}");
    assert_eq!(executor.identity.crew.as_deref(), Some("claude-crew"));
    assert_eq!(
        executor.identity.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    let pilot = backfilled
        .iter()
        .find(|record| record.activity_id == "pilot")
        .unwrap();
    assert_eq!(pilot.identity.crew, None, "{pilot:?}");
    assert_eq!(pilot.identity.reasoning_effort, Some(ReasoningEffort::Low));
    let dispatched = store
        .agent_executions(&workspace, "jrun-backfill-7")
        .unwrap();
    assert_eq!(dispatched.len(), 1, "{dispatched:?}");
    assert!(dispatched[0].backfill.is_none(), "{dispatched:?}");
    assert_eq!(
        dispatched[0].identity.reasoning_effort,
        Some(ReasoningEffort::Low)
    );
    let mut launch_only = store
        .agent_executions(&workspace, "jrun-backfill-launch-only")
        .unwrap();
    launch_only.sort_by_key(|record| record.started_at);
    assert_eq!(launch_only.len(), 2, "{launch_only:?}");
    assert!(launch_only[0].started_at < launch_only[1].started_at);
    assert_eq!(
        launch_only[0].identity.reasoning_effort,
        Some(ReasoningEffort::High),
        "{launch_only:?}"
    );
    assert_eq!(
        launch_only[1].identity.reasoning_effort,
        Some(ReasoningEffort::Medium),
        "{launch_only:?}"
    );
    let event_ids: Vec<&str> = launch_only
        .iter()
        .map(|record| {
            let backfill = record
                .backfill
                .as_ref()
                .expect("launch record is backfilled");
            assert_eq!(backfill.event_ids.len(), 1, "{record:?}");
            backfill.event_ids[0].as_str()
        })
        .collect();
    assert_ne!(event_ids[0], event_ids[1], "{launch_only:?}");
    let covered = store
        .agent_executions(&workspace, "jrun-backfill-dispatch-covered")
        .unwrap();
    assert_eq!(covered.len(), 1, "{covered:?}");
    assert!(covered[0].backfill.is_none(), "{covered:?}");
    assert_eq!(
        covered[0].identity.reasoning_effort,
        Some(ReasoningEffort::Low),
        "{covered:?}"
    );
}
