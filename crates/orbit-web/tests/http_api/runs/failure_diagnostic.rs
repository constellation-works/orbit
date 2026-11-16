use super::*;

#[test]
fn run_detail_keeps_the_terminal_diagnostic_without_a_started_step() {
    isolated(
        "runs::failure_diagnostic::run_detail_keeps_the_terminal_diagnostic_without_a_started_step",
        || {
            let fixture = Fixture::new();
            let run = fixture.seed_run(
                "jrun-worktree-refused",
                "resident_cycle_pipeline",
                JobRunState::Failed,
            );
            let diagnostic = json!({"code":"worktree_mismatch", "requested_repo_root":null,
            "requested_workspace_path":"/work/candidate", "automatic_reconciliation":false});
            insert_test_event(
                &fixture,
                &ws_id(&fixture),
                &run.run_id,
                "worktree-refusal",
                "2026-11-15T20:52:00Z",
                None,
                json!({
                    "event_id":"worktree-refusal", "ts":"2026-11-15T20:52:00Z",
                    "body_kind":"run_finished", "outcome":"error", "error_diagnostic":diagnostic,
                }),
            );
            let server = fixture.server(false);
            let detail = run_detail(&server, &run.run_id);
            assert_eq!(
                detail["run"]["error_diagnostic"], diagnostic,
                "a refusal before a step starts must keep its diagnostic reachable in run detail"
            );
            assert!(detail["steps"].as_array().unwrap().is_empty());
        },
    );
}
