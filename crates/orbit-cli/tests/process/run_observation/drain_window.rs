//! Oversized windows fail at submission, while CLI single-pass windows remain valid.

use super::*;

#[test]
fn oversized_windows_create_no_run_and_single_pass_still_submits() {
    if !isolated_run_observation(
        "run_observation::drain_window::oversized_windows_create_no_run_and_single_pass_still_submits",
    ) {
        return;
    }
    let fixture = Fixture::terminal();
    // Keep accepted submissions queued so the fixture launches no worker.
    let now = chrono::Utc::now().to_rfc3339();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,created_at,pid) VALUES ('jrun-window-incumbent',?1,'workspace_auto_pipeline',1,'running',?2,?2,?2,?3)",
        params![fixture.workspace_id(), now, std::process::id()],
    ).unwrap();
    let before = fixture.snapshot();
    for pull in [false, true] {
        for duration in ["2d", "86401s"] {
            let mut args = vec!["run", "auto", "--for", duration, "--json"];
            if pull {
                args.extend(["--pull", "hm_owner/ws_fixture"]);
            }
            let output = fixture.orbit().args(&args).output().unwrap();
            assert!(
                !output.status.success(),
                "{args:?} submitted a run: {output:?}"
            );
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert!(
                error["error"]
                    .as_str()
                    .unwrap()
                    .contains("for_seconds must be a whole number from 1 to 86400 (24 hours)"),
                "{args:?}: {error}"
            );
            assert_eq!(fixture.snapshot(), before, "refused input creates no run");
        }
    }
    for (duration, seconds) in [(None, 0), (Some("0s"), 0), (Some("24h"), 86_400)] {
        let mut args = vec!["run", "auto", "--json"];
        if let Some(duration) = duration {
            args.extend(["--for", duration]);
        }
        let receipt = fixture.json(&args);
        assert_eq!(receipt["state"], "queued", "{receipt}");
        assert_eq!(receipt["job_id"], "workspace_auto_pipeline");
        assert!(receipt.get("submitted_at").is_none());
        assert!(receipt.get("review").is_none());
        let input: String = fixture
            .db()
            .query_row(
                "SELECT input_json FROM job_runs WHERE run_id = ?1",
                [receipt["run_id"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        let input: Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input["for_seconds"], seconds);
    }
}
