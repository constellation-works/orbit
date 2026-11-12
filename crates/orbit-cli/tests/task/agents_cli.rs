#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::{Command, Stdio};
use std::time::Duration;

use chrono::Utc;
use orbit_core::JobRunState;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::agents_fixture::{self, AgentsFixture, SeedRun};
use crate::bound_server::{Announcement, BoundServer};

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

    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    // One implemented task per call, worked by its run on `crew`/`model`.
    let implement = |run_id: &str, crew: &str, model: &str, status: TaskStatus| {
        let seed = SeedRun {
            title: format!("implemented by {run_id}"),
            description: "agent scorecard fixture",
            complexity: Some(TaskComplexity::Hard),
            tags: Vec::new(),
            model,
            run_id,
            crew,
            state: JobRunState::Success,
        };
        let id = agents.seed_with_run(&seed);
        if status != TaskStatus::InProgress {
            agents.finish_task(&id, status);
        }
        id
    };
    // Table rows: (run, crew, model, final status); the first row lands.
    let mut landed = String::new();
    for (index, (run_id, crew, model, status)) in [
        ("jrun-opus-1", "opus", "claude-opus-5-5", TaskStatus::Done),
        (
            "jrun-opus-2",
            "opus",
            "claude-opus-5-5",
            TaskStatus::InProgress,
        ),
        (
            "jrun-sonnet-1",
            "sonnet",
            "claude-sonnet-5-5",
            TaskStatus::InProgress,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = implement(run_id, crew, model, status);
        if index == 0 {
            landed = id;
        }
    }

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
    let tool = agents_fixture::tool_json(fixture, "orbit.agents.scorecard", &input);
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
    let tool_detail = agents_fixture::tool_json(fixture, "orbit.agents.show", &detail_input);
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
    let refused = agents_fixture::refused_stderr(
        fixture,
        &["agents", "scorecard", "--role", "orchestrators"],
    );
    agents_fixture::assert_contains(
        "the refusal names the role and the choices",
        &refused,
        &["orchestrators", "implementers"],
    );
    let refused = agents_fixture::refused_text(
        fixture,
        &[
            "tool",
            "run",
            "orbit.agents.scorecard",
            "--input",
            &json!({ "role": "orchestrators" }).to_string(),
        ],
    );
    agents_fixture::assert_contains(
        "the tool refusal names the role and the choices",
        &refused,
        &["orchestrators", "implementers"],
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

    let refused = agents_fixture::refused_stderr(
        fixture,
        &[
            "agents",
            "show",
            "--role",
            "implementers",
            "--crew",
            "nobody",
        ],
    );
    agents_fixture::assert_contains(
        "a crew with no row is refused by name",
        &refused,
        &["nobody"],
    );
    // [ORB-19171] The missing-row refusal keeps crew, role and window and
    // offers a usable scorecard or window-selection next step instead of
    // implying the crew is unconfigured: a configured crew with no work in
    // the window also has no row.
    agents_fixture::assert_contains(
        "ORB-19171: the missing-row refusal names a recovery step",
        &refused,
        &[
            "implementers",
            "30d",
            "scorecard",
            "orbit agents scorecard --role implementers",
            "--window",
            "no work in the window has no row",
        ],
    );
    agents_fixture::assert_excludes(
        "ORB-19171: the CLI refusal excludes other surfaces and configuration diagnoses",
        &refused,
        &[
            "unconfigured",
            "not configured",
            "unknown crew",
            "orbit_agents_",
            "orbit.agents.",
            "dashboard",
        ],
    );

    // The tool surface refuses with the same code and recovery step.
    let refused = agents_fixture::refused_tool(
        fixture,
        "orbit.agents.show",
        json!({ "role": "implementers", "crew": "nobody" }),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    let message = refused["error"].as_str().unwrap();
    agents_fixture::assert_contains(
        "ORB-19171: the tool refusal names a recovery step",
        message,
        &[
            "nobody",
            "implementers",
            "30d",
            "orbit.agents.scorecard",
            "window",
            "no work in the window has no row",
        ],
    );
    agents_fixture::assert_excludes(
        "ORB-19171: the tool refusal only names its own surface",
        message,
        &["--window", "orbit agents", "orbit_agents_", "dashboard"],
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
    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    for (index, model) in MODELS.iter().enumerate() {
        let run_id = format!("jrun-sol-{index}");
        agents.seed_ok(format!("implemented {index}"), None, "sol", model, &run_id);
    }

    // The CLI refusal exits with code 1.
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
    agents_fixture::assert_contains("the CLI names --model", &refused, &["--model"]);
    agents_fixture::assert_contains("a candidate model", &refused, &MODELS);

    // Over MCP and `orbit tool run` the selector is `row_model`, and the
    // refusal keeps its error code. The JSON error payload goes to stderr.
    let refused = agents_fixture::refused_tool(
        fixture,
        "orbit.agents.show",
        json!({ "role": "implementers", "crew": "sol", "window": "30d" }),
    );
    assert_eq!(refused["code"], "invalid_input", "{refused}");
    let message = refused["error"].as_str().unwrap();
    agents_fixture::assert_contains("the tool names row_model", message, &["row_model"]);
    agents_fixture::assert_contains("a candidate model", message, &MODELS);

    // A listed model is accepted through each surface's selector.
    let by_row_model = fixture.json(&[
        "tool",
        "run",
        "orbit.agents.show",
        "--full",
        "--input",
        &json!({ "role": "implementers", "crew": "sol", "window": "30d", "row_model": MODELS[1] })
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
    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let config_path = agents.workspace_root.join("config.toml");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    std::fs::write(
        &config_path,
        format!(
            "{existing}\n[[workflow.task_types]]\nname = \"review\"\ntags = [\"audit-work\"]\n"
        ),
    )
    .unwrap();
    // Seed one counted implementation attempt so the cell matrix is nonempty.
    agents.seed_with_run(&SeedRun {
        title: "by-type implemented".into(),
        description: "by-type scorecard fixture",
        complexity: Some(TaskComplexity::Low),
        tags: vec!["audit-work".to_string()],
        model: "model-fixture",
        run_id: "jrun-by-type-0",
        crew: "sol",
        state: JobRunState::Success,
    });

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
    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let runtime = &agents.runtime;
    for (index, (crew, effort)) in specs.iter().enumerate() {
        let run_id = format!("jrun-effort-{index}");
        let id = agents.seed_ok(
            format!("effort-row {index}"),
            Some(TaskComplexity::Low),
            crew,
            "model-fixture",
            &run_id,
        );
        if let Some(effort) = effort {
            let execution_id = agents.begin_execution(&run_id, "fixture", crew, *effort);
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

    // Table rows: (case, plain output) — each must distinguish every label.
    let by_type = plain(&["--by-type"]);
    let per_task = plain(&["--per-task"]);
    for (case, output) in [("by-type", &by_type), ("per-task", &per_task)] {
        let header: Vec<&str> = output.lines().next().unwrap().split('\t').collect();
        assert_eq!(header[0], "CREWS", "{case}: {output}");
        assert_eq!(header[1], "ROW", "{case}: {output}");
        let rows = records(output);
        for label in labels {
            assert!(
                rows.iter().any(|f| f[1] == label),
                "plain {case} distinguishes {label}:\n{output}"
            );
        }
        if case == "per-task" {
            assert_eq!(rows.len(), specs.len(), "one row per task:\n{output}");
        }
    }
    let cells = records(&by_type);
    let medium = cells.iter().find(|fields| fields[1] == labels[0]).unwrap();
    assert!(
        medium[0].contains("other") && medium[0].contains("sol"),
        "the medium by-type cell lists both contributing crews:\n{by_type}"
    );
    let tasks = records(&per_task);
    assert!(
        tasks
            .iter()
            .filter(|f| f[1] == labels[0])
            .any(|f| f[0].contains("other")),
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

    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let run_id = "jrun-launch-only";
    agents.seed_fixture_run(
        "launched but unfinished".into(),
        Some(TaskComplexity::Low),
        "sol",
        "model-fixture",
        run_id,
        JobRunState::Failed,
    );
    // The agent launched at high effort, but its invocation never
    // completed, so no invocation record follows this launch.
    agents.begin_execution(run_id, "fixture", "sol", Some(High));

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

/// An attempt whose own run provably launched no implementer (its complete
/// audit trail ran only `final_recovery`, and no implementing invocation or
/// launch exists) is not an implementer unit. An attempt whose run left no
/// audit trail, or whose trail started `implement_one`, stays one
/// [ORB-24150].
#[test]
fn agents_scorecard_drops_an_attempt_whose_run_launched_no_implementer() {
    if crate::isolated_test::run_in_child(
        "agents_cli::agents_scorecard_drops_an_attempt_whose_run_launched_no_implementer",
    ) {
        return;
    }
    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let seed = |title: &str, run_id: &str| {
        agents.seed_fixture_run(
            title.into(),
            Some(TaskComplexity::Low),
            "sol",
            "model-fixture",
            run_id,
            JobRunState::Failed,
        )
    };
    let recovery = seed("recovery only", "jrun-recovery-only");
    seed("no audit trail", "jrun-no-trail");
    seed("implemented without telemetry", "jrun-implemented");
    let at = Utc::now() - chrono::Duration::minutes(30);
    let trail = |run_id: &str, activities: &[&str]| {
        let mut events = vec![("run.started", "run_started", None)];
        events.extend(
            activities
                .iter()
                .map(|name| ("activity.started", "activity_started", Some(*name))),
        );
        events.push(("run.finished", "run_finished", None));
        for (index, (event_type, body_kind, activity)) in events.into_iter().enumerate() {
            let event_id = format!("{run_id}-{index}");
            let mut payload = json!({ "event_id": event_id, "body_kind": body_kind });
            if let Some(activity) = activity {
                payload["activity_name"] = json!(activity);
            }
            let ts = at + chrono::Duration::seconds(i64::try_from(index).unwrap());
            agents.audit(run_id, event_id, event_type, ts, payload);
        }
    };
    trail(
        "jrun-recovery-only",
        &[
            "worktree",
            "resume_candidate",
            "final_recovery",
            "pr_failure_handoff",
        ],
    );
    agents.invoke(
        "jrun-recovery-only",
        "final_recovery",
        "model-fixture",
        &recovery,
        None,
    );
    trail("jrun-implemented", &["worktree", "implement_one"]);

    let card = fixture.json(&["agents", "scorecard", "--role", "implementers", "--json"]);
    let rows = card["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one row for the implementer units: {card}");
    assert_eq!(rows[0]["label"], "model-fixture @ effort unknown", "{card}");
    assert_eq!(
        rows[0]["units"],
        json!(2),
        "the recovery-only attempt is no unit; the other two stay: {card}"
    );
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

    // Table rows: (crew, recorded dispatch effort, launch argvs for the run).
    type BackfillSpec<'a> = (&'a str, Option<ReasoningEffort>, Vec<Vec<&'a str>>);
    let specs: [BackfillSpec<'_>; 8] = [
        // 0: claude
        (
            "claude-crew",
            None,
            vec![vec![
                "claude",
                "--model",
                "model-fixture",
                "--effort",
                "high",
            ]],
        ),
        // 1: codex
        (
            "codex-crew",
            None,
            vec![vec![
                "codex",
                "exec",
                "--config",
                "model_reasoning_effort=\"xhigh\"",
            ]],
        ),
        // 2: grok
        (
            "grok-crew",
            None,
            vec![vec!["grok", "--reasoning-effort", "high"]],
        ),
        // 3: muse
        (
            "muse-crew",
            None,
            vec![vec!["muse", "exec", "--reasoning-effort", "medium"]],
        ),
        // 4: no effort flag rendered -> provider default
        (
            "default-crew",
            None,
            vec![vec!["claude", "--model", "model-fixture"]],
        ),
        // 5: no matching launch -> unknown
        ("unknown-crew", None, vec![]),
        // 6: two candidate launches disagree -> unknown
        (
            "unknown-crew",
            None,
            vec![
                vec!["claude", "--effort", "high"],
                vec!["claude", "--effort", "medium"],
            ],
        ),
        // 7: identity recorded at dispatch; its launch says otherwise
        (
            "low-crew",
            Some(ReasoningEffort::Low),
            vec![vec!["claude", "--effort", "max"]],
        ),
    ];
    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let workspace = agents.workspace.clone();
    for (index, (crew, dispatch, launches)) in specs.iter().enumerate() {
        let run_id = format!("jrun-backfill-{index}");
        let id = agents.seed_ok(
            format!("backfill {index}"),
            Some(TaskComplexity::Low),
            crew,
            "model-fixture",
            &run_id,
        );
        // The invocation is recorded now and ran for a minute, so its
        // launch was a minute ago.
        let launched = Utc::now() - chrono::Duration::seconds(60);
        let audit = |seq: usize, model: &str, argv: &[&str]| {
            agents.audit(
                &run_id,
                format!("v2evt-{run_id}-{seq}"),
                "cli.invocation.started",
                launched,
                json!({ "body_kind": "cli_invocation_started", "model": model, "argv_redacted": argv }),
            );
        };
        for (seq, argv) in launches.iter().enumerate() {
            audit(seq, "model-fixture", argv);
        }
        let invoke = |activity: &str, model: &str, execution: Option<i64>| {
            agents.invoke(&run_id, activity, model, &id, execution);
        };
        let execution =
            dispatch.map(|effort| agents.begin_execution(&run_id, "claude", crew, Some(effort)));
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
    let recorded_run = "jrun-backfill-recorded";
    let recorded_id = agents.seed_ok(
        "backfill recorded effort".into(),
        Some(TaskComplexity::Low),
        "recorded-crew",
        "model-fixture",
        recorded_run,
    );
    agents.audit(recorded_run, format!("v2evt-{recorded_run}-0"), "cli.invocation.started",
        Utc::now() - chrono::Duration::seconds(60),
        json!({ "body_kind": "cli_invocation_started", "provider": "claude", "model": "model-fixture", "reasoning_effort": "low" }));
    agents.invoke(
        recorded_run,
        "implement_one",
        "model-fixture",
        &recorded_id,
        None,
    );
    // A run whose agent launched twice, at different efforts, but whose
    // invocation never completed. No invocation exists, so the backfill
    // records each launch on its own, proven to its own step interval.
    let launch_id = agents.seed_task(
        "backfill launch only".into(),
        "dispatch effort backfill fixture",
    );
    let launch_run = "jrun-backfill-launch-only";
    let launch_now = Utc::now();
    let launch_started = launch_now - chrono::Duration::hours(2);
    agents.record_run_window(
        launch_run,
        &launch_id,
        "launch-crew",
        "model-fixture",
        JobRunState::Failed,
        (launch_started, launch_now),
    );
    let step_event = |seq: usize, event_type: &str, body: Value, at: chrono::DateTime<Utc>| {
        agents.audit(
            launch_run,
            format!("v2evt-{launch_run}-{seq}"),
            event_type,
            at,
            body,
        );
    };
    let launch_body = |effort: &str| {
        json!({
            "body_kind": "cli_invocation_started", "provider": "claude", "model": "model-fixture",
            "argv_redacted": ["claude", "--model", "model-fixture", "--effort", effort],
        })
    };
    // Table rows: (seq, event type, body, minutes before the last launch).
    let start_body = json!({ "body_kind": "step_started", "step_id": "implement_one" });
    let finish_body = json!({ "body_kind": "step_finished", "step_id": "implement_one" });
    for (seq, event_type, body, at) in [
        (
            0,
            "step.started",
            start_body.clone(),
            launch_now - chrono::Duration::hours(2),
        ),
        (
            1,
            "cli.invocation.started",
            launch_body("high"),
            launch_now - chrono::Duration::minutes(90),
        ),
        (
            2,
            "step.finished",
            finish_body.clone(),
            launch_now - chrono::Duration::minutes(80),
        ),
        (
            3,
            "step.started",
            start_body,
            launch_now - chrono::Duration::minutes(70),
        ),
        (
            4,
            "cli.invocation.started",
            launch_body("medium"),
            launch_now - chrono::Duration::minutes(40),
        ),
        (
            5,
            "step.finished",
            finish_body,
            launch_now - chrono::Duration::minutes(30),
        ),
    ] {
        step_event(seq, event_type, body, at);
    }
    // A launch Orbit already recorded at dispatch. The backfill must
    // leave that row as it is, not write a second one from the audit.
    let covered_id = agents.seed_task(
        "backfill dispatch covered".into(),
        "dispatch effort backfill fixture",
    );
    let covered_run = "jrun-backfill-dispatch-covered";
    let covered_now = Utc::now();
    agents.record_run_window(
        covered_run,
        &covered_id,
        "covered-crew",
        "model-fixture",
        JobRunState::Failed,
        (covered_now - chrono::Duration::hours(1), covered_now),
    );
    agents.audit(
        covered_run,
        format!("v2evt-{covered_run}-0"),
        "step.started",
        covered_now - chrono::Duration::minutes(1),
        json!({ "body_kind": "step_started", "step_id": "implement_one" }),
    );
    agents.audit(covered_run, format!("v2evt-{covered_run}-1"), "cli.invocation.started", Utc::now(),
        json!({ "body_kind": "cli_invocation_started", "provider": "claude", "model": "model-fixture", "argv_redacted": ["claude", "--model", "model-fixture", "--effort", "high"] }));
    agents.begin_execution(
        covered_run,
        "claude",
        "covered-crew",
        Some(ReasoningEffort::Low),
    );

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
    agents_fixture::assert_report(
        report,
        &[
            ("examined", 9),
            ("recorded", 7),
            ("no_launch_event", 1),
            ("conflicting", 1),
            ("launch_only_recorded", 2),
            ("launch_only_no_step", 10),
            ("launch_only_no_provider", 0),
            ("launch_only_unreadable", 0),
            ("launch_only_unmatched", 10),
        ],
    );

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
    agents_fixture::assert_report(
        again,
        &[
            ("examined", 2),
            ("recorded", 0),
            ("launch_only_recorded", 0),
            ("launch_only_no_step", 10),
            ("launch_only_no_provider", 0),
            ("launch_only_unreadable", 0),
            ("launch_only_unmatched", 10),
        ],
    );
    assert_eq!(rows(), after, "a second run changes nothing");

    let runtime = agents.reopen();
    let store = runtime.sqlite_store().unwrap();
    let executions = |run_id: &str| store.agent_executions(&workspace, run_id).unwrap();
    let backfilled = executions("jrun-backfill-0");
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
    let dispatched = executions("jrun-backfill-7");
    assert_eq!(dispatched.len(), 1, "{dispatched:?}");
    assert!(dispatched[0].backfill.is_none(), "{dispatched:?}");
    assert_eq!(
        dispatched[0].identity.reasoning_effort,
        Some(ReasoningEffort::Low)
    );
    let mut launch_only = executions("jrun-backfill-launch-only");
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
    let covered = executions("jrun-backfill-dispatch-covered");
    assert_eq!(covered.len(), 1, "{covered:?}");
    assert!(covered[0].backfill.is_none(), "{covered:?}");
    assert_eq!(
        covered[0].identity.reasoning_effort,
        Some(ReasoningEffort::Low),
        "{covered:?}"
    );
}

/// An unreadable launch at a dispatch row's own time stays that row's closest
/// sibling. The row keeps the effort recorded at dispatch, and a readable
/// launch of the same step, provider and model thirty seconds later is
/// recorded from its own evidence. A second migrate writes nothing new.
#[test]
fn migrate_backfill_keeps_unreadable_sibling_on_the_dispatch_row() {
    if crate::isolated_test::run_in_child(
        "agents_cli::migrate_backfill_keeps_unreadable_sibling_on_the_dispatch_row",
    ) {
        return;
    }
    use orbit_types::identity::ReasoningEffort;

    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let runtime = &agents.runtime;
    let workspace = agents.workspace.clone();
    let run_id = "jrun-backfill-unreadable-sibling";
    let task_id = agents.seed_task(
        "backfill unreadable sibling".into(),
        "dispatch effort backfill fixture",
    );
    let now = Utc::now();
    agents.record_run_window(
        run_id,
        &task_id,
        "review-crew",
        "model-fixture",
        JobRunState::Failed,
        (now, now + chrono::Duration::seconds(50)),
    );
    let store = runtime.sqlite_store().unwrap();
    agents.begin_execution(run_id, "claude", "review-crew", Some(ReasoningEffort::Low));
    let dispatch_at = store.agent_executions(&workspace, run_id).unwrap()[0].started_at;
    let audit = |event_id: &str, event_type: &str, at: chrono::DateTime<Utc>, payload: Value| {
        agents.audit(
            run_id,
            format!("v2evt-{run_id}-{event_id}"),
            event_type,
            at,
            payload,
        );
    };
    audit(
        "step-start",
        "step.started",
        dispatch_at - chrono::Duration::seconds(1),
        json!({ "body_kind": "step_started", "step_id": "implement_one" }),
    );
    audit(
        "unreadable",
        "cli.invocation.started",
        dispatch_at,
        json!({
            "body_kind": "cli_invocation_started",
            "provider": "claude",
            "model": "model-fixture",
            "reasoning_effort": "low",
            "argv_redacted": ["claude", "--effort", "[REDACTED]"],
        }),
    );
    audit(
        "later-high",
        "cli.invocation.started",
        dispatch_at + chrono::Duration::seconds(30),
        json!({
            "body_kind": "cli_invocation_started",
            "provider": "claude",
            "model": "model-fixture",
            "reasoning_effort": "high",
            "argv_redacted": ["claude", "--effort", "high"],
        }),
    );
    audit(
        "step-end",
        "step.finished",
        dispatch_at + chrono::Duration::seconds(50),
        json!({ "body_kind": "step_finished", "step_id": "implement_one" }),
    );

    let report = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    let report = &report["dispatch_effort_backfill"];
    agents_fixture::assert_report(
        report,
        &[
            ("examined", 0),
            ("recorded", 0),
            ("launch_only_recorded", 1),
            ("launch_only_unreadable", 1),
            ("launch_only_no_step", 0),
            ("launch_only_no_provider", 0),
            ("launch_only_unmatched", 1),
        ],
    );
    assert_eq!(report["recorded_by_effort"]["high"], 1, "{report}");
    assert!(
        report["recorded_by_effort"].get("low").is_none(),
        "the unreadable launch must not be recorded at an inferred effort: {report}"
    );

    let runtime = agents.reopen();
    let store = runtime.sqlite_store().unwrap();
    let rows = store.agent_executions(&workspace, run_id).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let low = rows
        .iter()
        .find(|row| row.identity.reasoning_effort == Some(ReasoningEffort::Low))
        .expect("dispatch row");
    assert!(low.backfill.is_none(), "{low:?}");
    assert_eq!(low.started_at, dispatch_at, "{low:?}");
    let high = rows
        .iter()
        .find(|row| row.identity.reasoning_effort == Some(ReasoningEffort::High))
        .expect("readable launch row");
    assert_eq!(
        high.started_at,
        dispatch_at + chrono::Duration::seconds(30),
        "{high:?}"
    );
    let high_events = high
        .backfill
        .as_ref()
        .expect("readable launch is backfilled")
        .event_ids
        .as_slice();
    assert_eq!(
        high_events,
        [format!("v2evt-{run_id}-later-high")].as_slice(),
        "{high:?}"
    );
    assert!(
        rows.iter().all(|row| {
            row.backfill.as_ref().is_none_or(|backfill| {
                !backfill
                    .event_ids
                    .iter()
                    .any(|event_id| event_id.ends_with("-unreadable"))
            })
        }),
        "an unreadable launch is never assigned an effort: {rows:?}"
    );

    let again = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    let again = &again["dispatch_effort_backfill"];
    agents_fixture::assert_report(
        again,
        &[
            ("examined", 0),
            ("recorded", 0),
            ("launch_only_recorded", 0),
            ("launch_only_unreadable", 1),
            ("launch_only_unmatched", 1),
        ],
    );
    let quiet = store.agent_executions(&workspace, run_id).unwrap();
    assert_eq!(quiet.len(), 2, "{quiet:?}");
    assert_eq!(quiet[0].id, rows[0].id, "{quiet:?}");
    assert_eq!(quiet[1].id, rows[1].id, "{quiet:?}");
    assert!(quiet[0].backfill.is_none(), "{quiet:?}");
    assert_eq!(
        quiet[0].identity.reasoning_effort,
        Some(ReasoningEffort::Low),
        "{quiet:?}"
    );
}

/// A launch whose audit intervals overlap — `implement_one` nested in
/// `implement_bundle` — was skipped as "no single step provably contained the
/// launch". Its causal links (launch → `activity.started` → `step.started`)
/// prove its step, so it is recorded. A run-level `final_recovery` launch,
/// which no step contains, is proven by its own `activity.started`. A launch
/// whose recorded effort and argv disagree is still left unrecorded, and a
/// second migrate records nothing new [ORB-24150].
#[test]
fn migrate_backfill_records_a_launch_its_causal_links_prove() {
    if crate::isolated_test::run_in_child(
        "agents_cli::migrate_backfill_records_a_launch_its_causal_links_prove",
    ) {
        return;
    }
    use orbit_types::identity::ReasoningEffort;

    let agents = AgentsFixture::new();
    let fixture = &agents.fixture;
    let workspace = agents.workspace.clone();
    let run_id = "jrun-backfill-nested-steps";
    let task_id = agents.seed_task(
        "backfill nested steps".into(),
        "dispatch effort backfill fixture",
    );
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    agents.record_run_window(
        run_id,
        &task_id,
        "nested-crew",
        "model-fixture",
        JobRunState::Failed,
        (start, now),
    );
    let event_id = |name: &str| format!("v2evt-{run_id}-{name}");
    let audit =
        |name: &str, event_type: &str, minutes: i64, parent: Option<&str>, payload: Value| {
            agents.audit_linked(
                run_id,
                event_id(name),
                event_type,
                start + chrono::Duration::minutes(minutes),
                parent.map(event_id),
                payload,
            );
        };
    let launch = |effort: &str, argv_effort: &str| {
        json!({
            "body_kind": "cli_invocation_started", "provider": "claude", "model": "model-fixture",
            "reasoning_effort": effort,
            "argv_redacted": ["claude", "--model", "model-fixture", "--effort", argv_effort],
        })
    };
    // Table rows: (name, event type, minutes after the run started, parent, body).
    for (name, event_type, minutes, parent, body) in [
        (
            "bundle",
            "step.started",
            1,
            None,
            json!({ "body_kind": "step_started", "step_id": "implement_bundle" }),
        ),
        (
            "one",
            "step.started",
            1,
            Some("bundle"),
            json!({ "body_kind": "step_started", "step_id": "implement_one" }),
        ),
        (
            "one-activity",
            "activity.started",
            1,
            Some("one"),
            json!({ "body_kind": "activity_started", "activity_name": "implement_one" }),
        ),
        (
            "nested-launch",
            "cli.invocation.started",
            2,
            Some("one-activity"),
            launch("high", "high"),
        ),
        (
            "disagreeing-launch",
            "cli.invocation.started",
            3,
            Some("one-activity"),
            launch("high", "low"),
        ),
        (
            "one-end",
            "step.finished",
            10,
            Some("bundle"),
            json!({ "body_kind": "step_finished", "step_id": "implement_one" }),
        ),
        (
            "bundle-end",
            "step.finished",
            10,
            None,
            json!({ "body_kind": "step_finished", "step_id": "implement_bundle" }),
        ),
        (
            "recovery",
            "activity.started",
            20,
            None,
            json!({ "body_kind": "activity_started", "activity_name": "final_recovery" }),
        ),
        (
            "recovery-launch",
            "cli.invocation.started",
            21,
            Some("recovery"),
            launch("medium", "medium"),
        ),
    ] {
        audit(name, event_type, minutes, parent, body);
    }

    let report = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    let report = &report["dispatch_effort_backfill"];
    agents_fixture::assert_report(
        report,
        &[
            ("launch_only_recorded", 2),
            ("launch_only_no_step", 0),
            ("launch_only_no_provider", 0),
            ("launch_only_unreadable", 1),
            ("launch_only_unmatched", 1),
        ],
    );

    let runtime = agents.reopen();
    let store = runtime.sqlite_store().unwrap();
    let mut rows: Vec<_> = store
        .agent_executions(&workspace, run_id)
        .unwrap()
        .into_iter()
        .map(|row| {
            let events = row
                .backfill
                .map(|backfill| backfill.event_ids)
                .unwrap_or_default();
            (row.activity_id, row.identity.reasoning_effort, events)
        })
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        rows,
        vec![
            (
                "final_recovery".to_string(),
                Some(ReasoningEffort::Medium),
                vec![event_id("recovery-launch")],
            ),
            (
                "implement_one".to_string(),
                Some(ReasoningEffort::High),
                vec![event_id("nested-launch")],
            ),
        ],
        "the nested launch counts under its own step, the run-level launch under its activity, \
         and the disagreeing launch is never recorded"
    );

    let again = fixture.json(&["migrate", "--backfill-dispatch-effort", "--json"]);
    agents_fixture::assert_report(
        &again["dispatch_effort_backfill"],
        &[
            ("launch_only_recorded", 0),
            ("launch_only_no_step", 0),
            ("launch_only_unreadable", 1),
        ],
    );
    assert_eq!(store.agent_executions(&workspace, run_id).unwrap().len(), 2);
}
