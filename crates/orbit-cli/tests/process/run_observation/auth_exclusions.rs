//! Doctor and run show project a persisted auth incident without probing it.
use super::*;
use orbit_types::workflow::{PipelineState, PullCrewPreflight};

#[test]
fn doctor_and_run_show_report_active_auth_exclusions_without_launching_a_probe() {
    if !isolated_run_observation(
        "run_observation::auth_exclusions::doctor_and_run_show_report_active_auth_exclusions_without_launching_a_probe",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let drain = "jrun-auth-drain";
    let leaf = "jrun-auth-leaf";
    let at = chrono::Utc::now();
    let workspace = fixture.workspace_id();
    let db = fixture.db();
    db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running',?3,?3,?3,?4)", params![drain, workspace, at.to_rfc3339(), std::process::id()]).unwrap();
    db.execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, finished_at, created_at) VALUES (?1,?2,'task_claimed_pr_pipeline',1,'failed',?3,?3,?3,?3)", params![leaf, workspace, at.to_rfc3339()]).unwrap();
    db.execute("INSERT INTO job_run_steps (workspace_id,run_id,step_index,target_type,target_id,state,error_code,error_message) VALUES (?1,?2,0,'activity','implement_one','failed','provider_unavailable','provider authentication failed (HTTP 401)')", params![workspace, leaf]).unwrap();
    db.execute_batch("CREATE TABLE local_pull_admissions (workspace_id TEXT NOT NULL, owner_machine TEXT NOT NULL, owner_workspace TEXT NOT NULL, execution_machine TEXT NOT NULL, request_id TEXT NOT NULL, claim_id TEXT, leaf_run_id TEXT, record_json TEXT NOT NULL, PRIMARY KEY(workspace_id,owner_machine,owner_workspace,execution_machine,request_id), UNIQUE(workspace_id,owner_machine,owner_workspace,claim_id), UNIQUE(workspace_id,leaf_run_id));").unwrap();
    let record = serde_json::json!({
        "destination": {"owner_machine_id":"fixture-owner","owner_workspace_id":"owner-workspace","execution_machine_id":"fixture-host","selector":"fixture-owner/owner-workspace"},
        "request": {"request_id":"auth-incident", "caller_version":"fixture", "caller_schema":1,
            "run_context":{"run_id":drain,"job_name":"workspace_pull_pipeline","machine_name":"fixture-mac"},
            "ship":{"mode":"pr","base_branch":"agent-main","landing_branch":"agent-main","completion":"owner"}},
        "receipt":null, "leaf_run_id":leaf, "phase":"settled",
        "settlement":{"Release":{"summary":"authentication failed", "comment":null, "artifacts":[],
            "provider_unavailable":{"crew":"opus","reason":"authentication failed (HTTP 401)"}}}
    });
    db.execute("INSERT INTO local_pull_admissions VALUES (?1,'fixture-owner','owner-workspace','fixture-host','auth-incident','claim-fixture',?2,?3)", params![workspace,leaf,record.to_string()]).unwrap();
    let mut state = PipelineState::new(
        drain.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.pull_crew_preflight = Some(PullCrewPreflight {
        checked_at: at,
        runnable: vec!["opus".into()],
        default_crew: Some("opus".into()),
        excluded: vec![],
    });
    runtime.write_run_state(drain, &state).unwrap();
    let expected_fields = [
        "claude",
        "fixture-mac",
        "provider_unavailable/auth",
        "claude auth login",
        "next probe",
        "credentials",
    ];
    let output = fixture
        .orbit()
        .args(["run", "show", drain, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    for field in expected_fields {
        assert!(text.contains(field), "run show misses {field}: {text}");
    }
    let shown = fixture.json(&["run", "show", drain, "--no-reconcile", "--json"]);
    let exclusion = &shown["crew_window"]["auth_exclusions"][0];
    assert_eq!(exclusion["provider"], "claude");
    assert_eq!(exclusion["host"], "fixture-mac");
    assert_eq!(exclusion["excluded_at"], serde_json::json!(at));
    assert!(exclusion["next_probe_at"].is_string());
    // Doctor can exit nonzero for unrelated missing providers in this fixture.
    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == "provider-auth:claude")
        .unwrap();
    assert_eq!(row["status"], "warning");
    for field in expected_fields {
        assert!(
            row["message"].as_str().unwrap().contains(field),
            "doctor misses {field}: {row}"
        );
    }
    assert!(
        runtime
            .read_run_state(drain)
            .unwrap()
            .unwrap()
            .pull_auth_recovery
            .is_empty(),
        "read-only diagnostics do not reserve probes or rewrite state"
    );
}
