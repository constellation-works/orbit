//! Public-route regression guard for measured files and observed outcomes.

use std::path::Path;

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::{
    DiffClassCounts, LandedDiffSource, LandedDiffStats, LandedDiffStoreBackend,
    TaskArtifactUpdateParams, V2AuditEventInsertParams,
};
use orbit_types::task::{ArtifactWriter, EXPLORE_BRIEF_ARTIFACT, TaskArtifact, TaskStatus};
use serde_json::{Value, json};

use super::git_test_support;
use super::support::{Fixture, isolated, json_ok};

const ENDPOINT: &str = "/api/diagnostics/brief_accuracy?workspace=ws_http_fixture";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = git_test_support::command()
        .current_dir(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn task(fixture: &Fixture, status: TaskStatus, targets: Option<&[&str]>) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Brief report fixture".into(),
            description: "Compare predicted files with a recorded delivery.".into(),
            status: Some(status),
            tags: vec!["explore-brief".into()],
            ..Default::default()
        })
        .unwrap()
        .id;
    if let Some(targets) = targets {
        // Seed the producer artifact at the store boundary. Pilot apply is
        // covered elsewhere; this fixture exercises the real HTTP endpoint.
        let document = json!({
            "schema": "explore_brief.v1", "task_id": id,
            "brief": {
                "change": targets.iter().map(|target| json!({"target": target, "what": "fixture"})).collect::<Vec<_>>(),
                "call_path": [{"target": "file:src/b.txt", "role": "read only"}],
            },
        });
        fixture
            .runtime
            .stores()
            .task_artifacts()
            .upsert_task_artifacts(
                &id,
                TaskArtifactUpdateParams {
                    actor: "system".into(),
                    writer: Some(ArtifactWriter::System),
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        EXPLORE_BRIEF_ARTIFACT,
                        document.to_string(),
                    )],
                    ..Default::default()
                },
            )
            .unwrap();
    }
    id
}

fn landing(fixture: &Fixture, task_id: &str, base: &str, head: &str) {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .record_landed_diff_stats(&LandedDiffStats {
            workspace_id: "ws_http_fixture".into(),
            task_id: task_id.into(),
            base_commit: base.into(),
            landed_commit: head.into(),
            landed_at: Utc::now() - Duration::minutes(1),
            recorded_at: Utc::now(),
            job_run_id: None,
            source: LandedDiffSource::Landing,
            crew: None,
            model: None,
            complexity: None,
            task_type: None,
            code: DiffClassCounts::default(),
            tests: DiffClassCounts::default(),
            other: DiffClassCounts::default(),
            reviewable: None,
        })
        .unwrap();
}

fn dispatch(fixture: &Fixture, id: &str, task: &str, outcome: &str, workspace: &str) {
    // Dispatch can predate completion by weeks and must still count.
    let at = Utc::now() - Duration::days(5);
    fixture
        .runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: workspace.into(),
            event_id: id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "explore_brief_applied".into(),
            ts: at,
            run_id: "brief-fixture".into(),
            agent_identity: "codex".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({
                "schemaVersion": 1, "event_type": "activity.explore_brief_applied",
                "event_id": id, "ts": at, "run_id": "brief-fixture", "agent_identity": "codex",
                "body_kind": "explore_brief_applied", "step_id": "implement",
                "activity_name": "agent_implement", "task_id": task,
                "brief": {"outcome": outcome, "delivered": false},
            })
            .to_string(),
        })
        .unwrap();
}

#[test]
fn brief_accuracy_route_measures_delivered_files_and_actual_dispatches() {
    isolated(
        "brief_accuracy::brief_accuracy_route_measures_delivered_files_and_actual_dispatches",
        || {
            let fixture = Fixture::new();
            let repo = fixture.work.parent().unwrap();
            git(repo, &["init", "-q", "-b", "main"]);
            git(repo, &["config", "commit.gpgsign", "false"]);
            std::fs::create_dir_all(repo.join("src")).unwrap();
            std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
            for path in ["src/a.txt", "src/deleted.txt", "src/old.txt"] {
                std::fs::write(repo.join(path), path).unwrap();
            }
            git(repo, &["add", "--", ".gitignore", "src"]);
            git(repo, &["commit", "-qm", "base"]);
            let base = git(repo, &["rev-parse", "HEAD"]);
            std::fs::write(repo.join("src/a.txt"), "changed").unwrap();
            std::fs::write(repo.join("src/b.txt"), "new").unwrap();
            git(repo, &["rm", "src/deleted.txt"]);
            git(repo, &["mv", "src/old.txt", "src/new.txt"]);
            git(repo, &["add", "src"]);
            git(repo, &["commit", "-qm", "first change"]);
            let intermediate = git(repo, &["rev-parse", "HEAD"]);
            std::fs::write(repo.join("src/c.txt"), "second commit").unwrap();
            git(repo, &["add", "src"]);
            git(repo, &["commit", "-qm", "last change"]);
            let head = git(repo, &["rev-parse", "HEAD"]);

            let scored = task(
                &fixture,
                TaskStatus::Done,
                Some(&[
                    "file:src/a.txt",
                    "symbol:src/a.txt#greet:function",
                    "file:src/deleted.txt",
                    "file:src/old.txt",
                    "file:src/missing.txt",
                    "dir:src",
                ]),
            );
            landing(&fixture, &scored, &base, &intermediate);
            landing(&fixture, &scored, &base, &head);
            let no_diff = task(&fixture, TaskStatus::Done, Some(&["file:src/a.txt"]));
            let missing = task(&fixture, TaskStatus::Done, Some(&["file:src/a.txt"]));
            landing(&fixture, &missing, &base, &"f".repeat(40));
            let partial = task(&fixture, TaskStatus::Done, Some(&["file:src/a.txt"]));
            landing(&fixture, &partial, &base, &head);
            landing(&fixture, &partial, &base, &"e".repeat(40));
            task(&fixture, TaskStatus::Done, Some(&["module:invalid"]));
            let absent = task(&fixture, TaskStatus::Done, None);
            task(&fixture, TaskStatus::Done, Some(&["dir:src"]));
            let empty = task(&fixture, TaskStatus::Done, Some(&["file:src/a.txt"]));
            landing(&fixture, &empty, &base, &base);
            let active = task(&fixture, TaskStatus::InProgress, Some(&["file:src/a.txt"]));
            landing(&fixture, &active, &base, &head);
            for (id, task, outcome) in [
                ("fresh", &scored, "fresh"),
                ("retry", &scored, "drifted"),
                ("dropped", &no_diff, "dropped"),
                ("absent", &absent, "absent"),
                ("active", &active, "fresh"),
            ] {
                dispatch(&fixture, id, task, outcome, "ws_http_fixture");
            }
            dispatch(&fixture, "foreign", &scored, "fresh", "foreign");

            let server = fixture.server(false);
            let response = server.get(&format!("{ENDPOINT}&since=24h&tags=explore-brief"));
            assert_eq!(
                response.status().as_u16(),
                200,
                "the diagnostics route must be registered"
            );
            let report = json_ok(response);
            assert_eq!(report["done_tasks"], 8);
            assert_eq!(
                report["accuracy"],
                json!({
                    "scored_tasks": 2, "predicted_files": 5, "delivered_files": 6,
                    "matched_files": 3, "recall": 0.5, "precision": 0.6,
                    "excluded_no_brief": 1, "excluded_invalid_brief": 1,
                    "excluded_no_file_targets": 1, "excluded_no_diff": 1,
                    "excluded_unavailable_diff": 2, "directory_targets": 2,
                }),
                "routes.rs no_test risk guard: expose base-to-head file accuracy, not just the last commit"
            );
            assert_eq!(
                report["outcomes"],
                json!({"fresh":1,"drifted":1,"dropped":1,"absent":1})
            );
            assert_eq!(report["dispatches"], 4);
            assert_eq!(report["tasks_without_outcome"], 5);
            assert_eq!(report["truncated"], false);

            for query in ["tags=unmatched", "month=2001-01"] {
                let report = json_ok(server.get(&format!("{ENDPOINT}&{query}")));
                assert_eq!(report["done_tasks"], 0);
                assert_eq!(report["accuracy"]["recall"], Value::Null);
                assert_eq!(report["accuracy"]["precision"], Value::Null);
                assert_eq!(report["dispatches"], 0);
            }
            for query in [
                "since=bogus",
                "since=24h&month=2026-10",
                "tags=a,b,c,d,e,f,g,h,i",
            ] {
                let response = server.get(&format!("{ENDPOINT}&{query}"));
                assert_eq!(response.status().as_u16(), 400, "{query}");
            }
        },
    );
}
