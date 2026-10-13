//! [ORB-15791] A pull drain whose claimed leaf failed on model capacity shows
//! the crew backed off until a set time, and a before-landing reviewer held
//! by that backoff as a before-landing hold that names when it lifts.
use super::*;
use chrono::{Duration, SubsecRound, Utc};
use orbit_types::workflow::{DrainAdmissionPass, PipelineState, PullCrewPreflight};

#[test]
fn run_show_names_a_capacity_backoff_and_the_reviewer_hold_it_lifts() {
    if !isolated_run_observation(
        "run_observation::capacity_backoff::run_show_names_a_capacity_backoff_and_the_reviewer_hold_it_lifts",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let drain = "jrun-capacity-drain";
    let leaf = "jrun-capacity-leaf";
    let at = Utc::now().trunc_subsecs(0);
    let until = (at + Duration::minutes(10)).to_rfc3339();
    let workspace = fixture.workspace_id();
    let db = fixture.db();
    db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running',?3,?3,?3,?4)", params![drain, workspace, at.to_rfc3339(), std::process::id()]).unwrap();
    db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, finished_at, created_at) VALUES (?1,?2,'task_claimed_pr_pipeline',1,'failed',?3,?3,?3,?3)", params![leaf, workspace, at.to_rfc3339()]).unwrap();
    db.execute("INSERT INTO job_run_steps (workspace_id,run_id,step_index,target_type,target_id,state,error_message) VALUES (?1,?2,0,'activity','implement_one','failed','[provider_capacity] codex provider reported the selected model at capacity: Selected model is at capacity')", params![workspace, leaf]).unwrap();
    let record = serde_json::json!({
        "destination": {"owner_machine_id":"fixture-owner","owner_workspace_id":"owner-workspace","execution_machine_id":"fixture-host","selector":"fixture-owner/owner-workspace"},
        "request": {"request_id":"capacity", "caller_version":"fixture", "caller_schema":1,
            "run_context":{"run_id":drain,"job_name":"workspace_pull_pipeline","machine_name":"fixture-mac"},
            "ship":{"mode":"pr","base_branch":"agent-main","landing_branch":"agent-main","completion":"owner"}},
        "receipt":null, "leaf_run_id":leaf, "phase":"settled",
        "settlement":{"Release":{"summary":"model at capacity", "comment":null, "artifacts":[],
            "provider_unavailable":{"crew":"sol","reason":"Selected model is at capacity"}}}
    });
    db.execute("INSERT INTO local_pull_admissions VALUES (?1,'fixture-owner','owner-workspace','fixture-host','capacity','claim-fixture',?2,?3)", params![workspace,leaf,record.to_string()]).unwrap();
    let mut state = PipelineState::new(
        drain.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.pull_crew_preflight = Some(PullCrewPreflight {
        checked_at: at,
        runnable: vec!["sol".into()],
        default_crew: Some("sol".into()),
        excluded: vec![],
    });
    runtime.write_run_state(drain, &state).unwrap();
    // The hold the drain's pass records for an owner whose before-landing
    // review runs as `sol`, read off the window this store derives.
    let reason = runtime
        .pull_drain_crew_window(drain)
        .unwrap()
        .expect("a pull drain has a crew window")
        .reviewer_unrunnable_reason("sol")
        .expect("the backed-off reviewer cannot run");
    let refusal =
        format!("before_pr_reviewer_unavailable: the owner's before-landing review {reason}");
    state.drain_last_pass = Some(
        serde_json::from_value::<DrainAdmissionPass>(serde_json::json!({
            "recorded_at": at,
            "queued": 0,
            "admission_refusal": refusal,
        }))
        .unwrap(),
    );
    runtime.write_run_state(drain, &state).unwrap();

    let shown = fixture.json(&["run", "show", drain, "--no-reconcile", "--json"]);
    let exclusion = &shown["crew_window"]["excluded"][0];
    assert_eq!(exclusion["crew"], "sol", "{shown:#}");
    assert_eq!(exclusion["source"], "provider_unavailable", "{shown:#}");
    assert_eq!(
        exclusion["until"]
            .as_str()
            .map(|text| text.replace('Z', "+00:00")),
        Some(until.clone()),
        "{shown:#}"
    );
    let held = shown["pipeline_state"]["drain_last_pass"]["admission_refusal"]
        .as_str()
        .unwrap_or_default();
    assert!(
        held.starts_with(
            "before_pr_reviewer_unavailable: the owner's before-landing review crew `sol` is \
             backed off on the executor until "
        ) && held.contains(&until),
        "{shown:#}"
    );

    let output = fixture
        .orbit()
        .args(["run", "show", drain, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .any(|line| line.starts_with(&format!("Requests held: {held} (last pass "))),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "excluded sol (provider_unavailable until {until})"
        )),
        "{text}"
    );
}
