//! Explicit repilot commands submit one pending run through the operator CLI.

use crate::isolated_cli_fixture::Fixture;

#[test]
fn repilot_and_update_flag_submit_once_and_refuse_pending_duplicates() {
    if crate::workflow_gate::wait_if_requested() {
        return;
    }
    if crate::isolated_test::run_in_child(
        "repilot_cli::repilot_and_update_flag_submit_once_and_refuse_pending_duplicates",
    ) {
        return;
    }
    let fixture = Fixture::new();
    crate::fixture_crew::configure_sol(&fixture.root);
    let gate = crate::workflow_gate::WorkflowGate::new();
    let jobs = fixture.root.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let job = serde_json::json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pilot_pipeline"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [gate.step("hold")]}});
    std::fs::write(jobs.join("task_pilot_pipeline.yaml"), job.to_string()).unwrap();
    for update in [false, true] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            "Explicit repilot fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Inspect the requirements",
            "--json",
        ]);
        let id = task["id"].as_str().unwrap();
        let args = if update {
            vec![
                "task",
                "update",
                id,
                "--description",
                "Edited requirements",
                "--repilot",
                "--json",
            ]
        } else {
            vec!["task", "repilot", id, "--json"]
        };
        fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success();
        gate.wait_ready();
        let duplicate = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(
            String::from_utf8_lossy(&duplicate.stderr).contains("already held by pilot run"),
            "{duplicate:?}"
        );
        let shown = fixture.json(&["task", "show", id, "--json"]);
        assert_eq!(shown["status"], "proposed");
        if update {
            assert_eq!(shown["description"], "Edited requirements");
        }
        gate.release();
    }
    let runs = fixture.json(&[
        "run",
        "history",
        "-j",
        "task_pilot_pipeline",
        "--no-reconcile",
        "--json",
    ]);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 2);
}
