//! A drain started while another runs waits as a pending successor. Readiness
//! names what its window will do once it starts — ceiling, completion, crews,
//! window and the pass-listed names its start did not hold — and which Claude
//! crews in it start without a worker credential. Cancelling it leaves the
//! live drain alone.

use super::*;

const LIVE: &str = "jrun-queued-live";
const QUEUED_CLAUDE: &str = "jrun-queued-claude";
const QUEUED_CODEX: &str = "jrun-queued-codex";
const CREDENTIALS: [&str; 2] = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

/// Lists both Claude credentials in `execution.env.pass` and adds a Claude
/// crew beside the fixture's Codex `sol`.
fn configure_claude_crew_and_pass_list(fixture: &Fixture) {
    use toml_edit::{Array, DocumentMut, value};
    let path = fixture.home.join(".orbit/config.toml");
    let mut config: DocumentMut = fs::read_to_string(&path).unwrap().parse().unwrap();
    config["crews"]["opus"]["enabled"] = value(true);
    config["crews"]["opus"]["provider"] = value("claude");
    config["crews"]["opus"]["model"] = value("claude-opus-5-5");
    config["crews"]["opus"]["backend"] = value("cli");
    let mut pass = Array::new();
    for name in ["HOME", "PATH"].into_iter().chain(CREDENTIALS) {
        pass.push(name);
    }
    config["execution"]["env"]["pass"] = value(pass);
    fs::write(&path, config.to_string()).unwrap();
}

fn insert_drain(
    fixture: &Fixture,
    runtime: &OrbitRuntime,
    run_id: &str,
    state: &str,
    input: &Value,
    at: chrono::DateTime<chrono::Utc>,
    shape: impl FnOnce(&mut orbit_types::workflow::PipelineState),
) {
    let at_text = at.to_rfc3339();
    let pid = (state == "running").then(std::process::id);
    let started = (state == "running").then(|| at_text.clone());
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,?3,?4,?5,?6,?5,?7)",
        params![run_id, fixture.workspace_id(), state, input.to_string(), at_text, started, pid],
    ).unwrap();
    let mut run_state = orbit_types::workflow::PipelineState::new(
        run_id.into(),
        "workspace_auto_pipeline".into(),
        input.clone(),
    );
    shape(&mut run_state);
    runtime.write_run_state(run_id, &run_state).unwrap();
}

#[test]
fn readiness_lists_queued_drains_with_their_window_and_credential_gap() {
    if !isolated_run_observation(
        "run_observation::queued_drains::readiness_lists_queued_drains_with_their_window_and_credential_gap",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    configure_claude_crew_and_pass_list(&fixture);
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let now = chrono::Utc::now();
    let deadline =
        (now + chrono::Duration::hours(3)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    insert_drain(
        &fixture,
        &runtime,
        LIVE,
        "running",
        &serde_json::json!({"for_seconds": 28800, "allowed_crews": ["opus"]}),
        now - chrono::Duration::hours(5),
        |state| {
            state.record_pipeline_output("open_window", serde_json::json!({"deadline": deadline}))
        },
    );
    let unset: Vec<String> = CREDENTIALS.iter().map(ToString::to_string).collect();
    // The 2026-10-21 shape: a second window whose start held neither Claude
    // credential while a Claude crew is in its window.
    insert_drain(
        &fixture,
        &runtime,
        QUEUED_CLAUDE,
        "pending",
        &serde_json::json!({"for_seconds": 28800, "max_active_leaf_runs": "3", "completion": "done", "allowed_crews": ["opus"]}),
        now - chrono::Duration::minutes(2),
        |state| state.env_pass_unset = unset.clone(),
    );
    // The same gap with only a Codex crew in the window is not flagged.
    insert_drain(
        &fixture,
        &runtime,
        QUEUED_CODEX,
        "pending",
        &serde_json::json!({"for_seconds": 0, "allowed_crews": ["sol"]}),
        now - chrono::Duration::minutes(1),
        |state| state.env_pass_unset = unset.clone(),
    );

    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let capacity = &readiness["capacity"];
    assert_eq!(capacity["drain_run_id"], LIVE, "{capacity:#}");
    let queued = |id: &str| {
        capacity["queued_drains"]
            .as_array()
            .unwrap()
            .iter()
            .find(|drain| drain["run_id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} missing from queued_drains: {capacity:#}"))
    };
    let order: Vec<&str> = capacity["queued_drains"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|drain| drain["run_id"].as_str())
        .collect();
    assert_eq!(
        order,
        [QUEUED_CLAUDE, QUEUED_CODEX],
        "queued drains are listed in the order they start: {capacity:#}"
    );
    let claude = queued(QUEUED_CLAUDE);
    assert_eq!(claude["completion"], "done", "{claude}");
    assert_eq!(claude["max_active_leaf_runs"], 3, "{claude}");
    assert_eq!(
        claude["allowed_crews"],
        serde_json::json!(["opus"]),
        "{claude}"
    );
    assert_eq!(claude["for_seconds"], 28800, "{claude}");
    assert_eq!(
        claude["env_pass_unset"],
        serde_json::json!(CREDENTIALS),
        "names only: {claude}"
    );
    assert_eq!(
        claude["uncredentialed_crews"],
        serde_json::json!(["opus"]),
        "a Claude crew with every listed credential unset is flagged: {claude}"
    );
    let codex = queued(QUEUED_CODEX);
    assert_eq!(codex["completion"], "review", "{codex}");
    assert_eq!(codex["max_active_leaf_runs"], 5, "{codex}");
    assert_eq!(codex["for_seconds"], 0, "{codex}");
    assert_eq!(
        codex["uncredentialed_crews"],
        serde_json::json!([]),
        "a window with no Claude crew needs no Claude credential: {codex}"
    );

    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(&format!(
            "Queued drain: {QUEUED_CLAUDE} (completion: done; ceiling: 3; crews: opus; window: 8h 0m 0s); starts when {LIVE} ends (window closes {deadline})."
        )),
        "{text}"
    );
    assert!(
        text.contains("  Unset env: CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY."),
        "{text}"
    );
    assert!(
        text.contains("  Starts without a Claude worker credential for crews: opus."),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "Queued drain: {QUEUED_CODEX} (completion: review; ceiling: 5; crews: sol; window: single pass)"
        )),
        "{text}"
    );
    assert_eq!(
        text.matches("Starts without a Claude worker credential")
            .count(),
        1,
        "only the Claude window is flagged: {text}"
    );

    // The dashboard's queued Cancel uses this per-run cancel: it ends that
    // window alone, and the live drain keeps running.
    let cancelled = fixture.json(&["run", "cancel", QUEUED_CLAUDE, "--confirm", "--json"]);
    assert_eq!(cancelled["outcome"], "cancelled", "{cancelled}");
    assert_eq!(fixture.run_state(QUEUED_CLAUDE), "cancelled");
    assert_eq!(fixture.run_state(LIVE), "running");
    let after = fixture.json(&["run", "readiness", "--json"]);
    let remaining: Vec<&str> = after["capacity"]["queued_drains"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|drain| drain["run_id"].as_str())
        .collect();
    assert_eq!(remaining, [QUEUED_CODEX], "{after:#}");
    assert_eq!(after["capacity"]["drain_run_id"], LIVE, "{after:#}");
}
