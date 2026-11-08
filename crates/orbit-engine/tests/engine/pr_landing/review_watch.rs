//! [ORB-23003] An owner-local delivery that ends in review (a dashboard
//! ship, `orbit run ship`) has no handoff, so the landing never watched its
//! pull request: a red one sat in review with nothing on the task. The
//! review watch reads its checks and records the landing's classification.
use super::*;
use orbit_types::workflow::handoff::LANDING_CHECKS_OBSERVED_EVENT;

#[test]
fn a_review_delivery_with_failed_checks_records_their_classification() {
    isolated(
        "review_watch::a_review_delivery_with_failed_checks_records_their_classification",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["failure"]);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the delivery's pull request");
            host.set_status(TASK_ID, TaskStatus::Review);
            plant_run(
                &fx,
                &fx.candidate,
                10,
                "failure",
                "error: assertion failed: answer == 42\n",
            );
            plant_run(&fx, &fx.base_sha, 20, "success", "");
            let watch = json!({
                "task_ids": [TASK_ID],
                "pr_number": PR_NUMBER,
                "workspace_path": fx.repo,
                "base": BASE,
                "base_sync": "local",
                "max_wait_seconds": 30,
                "poll_interval_seconds": 5,
            });
            let watched = action(&host, "pr_checks_watch", &watch)
                .expect("the review watch is a shipped action");
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed: Vec<Value> = host
                .history(TASK_ID)
                .iter()
                .filter(|entry| entry.event == LANDING_CHECKS_OBSERVED_EVENT)
                .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
                .collect();
            assert_eq!(observed.len(), 1, "one landing check event: {observed:?}");
            let note = &observed[0];
            assert_eq!(note["state"], "failed", "{note}");
            assert_eq!(note["pr_number"], PR_NUMBER, "{note}");
            assert_eq!(
                note["classification"], "candidate",
                "the landing's classification: green on the fork point, red on the head: {note}"
            );
            assert_eq!(note["failed_checks"], json!(["test"]), "{note}");
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::Review,
                "the watch never moves the task"
            );

            // A repeated watch of the same head records nothing new.
            action(&host, "pr_checks_watch", &watch).expect("watch again");
            let again = host
                .history(TASK_ID)
                .iter()
                .filter(|entry| entry.event == LANDING_CHECKS_OBSERVED_EVENT)
                .count();
            assert_eq!(again, 1, "an identical observation is recorded once");
        },
    );
}

/// One push run on `sha` whose `test` job concluded `conclusion`, served to
/// the substitute `gh` as the commit's jobs and failed-job log.
fn plant_run(fx: &Fixture, sha: &str, run_id: u64, conclusion: &str, log: &str) {
    let dir = fx.forge.join("commit-ci").join(sha);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run_id"), run_id.to_string()).unwrap();
    let runs = json!({
        "total_count": 1,
        "workflow_runs": [{
            "id": run_id, "name": "CI", "workflow_id": 1, "event": "push",
            "head_sha": sha, "head_branch": BASE
        }]
    });
    fs::write(dir.join("runs.json"), serde_json::to_vec(&runs).unwrap()).unwrap();
    let jobs = json!({
        "total_count": 1,
        "jobs": [{
            "id": run_id + 1, "name": "test", "status": "completed", "conclusion": conclusion,
            "steps": [{"name": "compile", "conclusion": conclusion}]
        }]
    });
    fs::write(dir.join("jobs.json"), serde_json::to_vec(&jobs).unwrap()).unwrap();
    fs::write(dir.join("log"), log).unwrap();
}
