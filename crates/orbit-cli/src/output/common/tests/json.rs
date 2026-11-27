use std::process::Command;

use chrono::Utc;
use clap::Parser;
use orbit_common::test_env;
use orbit_core::OrbitError;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskArtifact;
use orbit_types::workflow::{RESIDENT_ACTION_EVENT, ResidentActionPhase, ResidentActionRow};
use serde_json::{Value, json};

use super::super::json::error_payload;
use crate::command::tool::{ToolCommand, ToolRunArgs, ToolSubcommand};
use crate::command::{Cli, Commands};

fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_ISOLATED_OUTPUT_JSON";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }

    let scratch = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    std::fs::create_dir_all(&scratch).expect("isolated test scratch");
    let root = tempfile::tempdir_in(scratch).expect("isolated test root");
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).expect("private child home");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD, test_name)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(&home);
    let output = test_env::run_child_test(&mut command, test_name, root.path());
    test_env::assert_child_test_passed(test_name, output.status, &output.stdout, &output.stderr);
    true
}

fn parse_tool_run(args: &[&str]) -> ToolRunArgs {
    let cli =
        Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{args:?} should parse: {err}"));
    match cli.command {
        Commands::Tool(ToolCommand {
            command: ToolSubcommand::Run(run_args),
        }) => run_args,
        _ => panic!("expected `orbit tool run` command"),
    }
}

#[test]
fn failed_terminal_resident_ledger_append_keeps_started_action_key() {
    if run_isolated_test(
        "output::common::tests::json::failed_terminal_resident_ledger_append_keeps_started_action_key",
    ) {
        return;
    }

    let temp = tempfile::tempdir_in(test_env::bulk_write_temp_dir()).expect("fixture temp");
    let global = temp.path().join("global");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(repo.join(".orbit")).expect("repo .orbit");
    std::fs::write(
        repo.join(".orbit/config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n",
    )
    .expect("fixture config");

    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let status = command
            .args([
                "-c",
                "user.name=Orbit Test",
                "-c",
                "user.email=orbit-test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git status");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);

    let runtime = orbit_core::test_support::runtime_from_roots(&global, &repo.join(".orbit"))
        .expect("runtime");
    let watched = runtime
        .add_task(TaskAddParams {
            title: "Watched work".into(),
            ..Default::default()
        })
        .expect("watched task")
        .id
        .to_string();
    let mandate = runtime
        .add_task(TaskAddParams {
            title: "Resident assignment".into(),
            tags: vec!["resident".into()],
            ..Default::default()
        })
        .expect("mandate task")
        .id
        .to_string();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Watch work",
        "exit_criteria": ["Done"],
        "workspaces": [runtime.workspace_id().expect("workspace id")],
        "watch": {"tasks": [watched], "tags": []},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120},
    });
    runtime
        .update_task_as_human(
            &mandate,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "resident/mandate.json",
                    body.to_string(),
                )],
                ..Default::default()
            },
            "human:test".into(),
        )
        .expect("attach mandate");

    let jobs = runtime.stores().jobs();
    jobs.insert_automation_job_run(
        "resident_cycle_pipeline",
        json!({"task_ids": [mandate]}),
        "resident-close-failure",
    )
    .expect("insert cycle job run");
    let runs = jobs
        .list_job_runs("resident_cycle_pipeline")
        .expect("list job runs");
    let run = runs[0].run_id.clone();
    let brief = runtime
        .begin_resident_cycle(&run, Utc::now())
        .expect("begin cycle");
    assert_eq!(brief["cycle_count"], 1, "{brief}");

    let _env = test_env::scoped([
        ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
        ("ORBIT_RUN_ID", Some(run.as_str())),
    ]);
    // The runtime reads the managed-run marker from the Env it is built with.
    let runtime = orbit_core::test_support::with_runtime_env(runtime, orbit_common::Env::capture());
    let core =
        orbit_cmd::registry_runtime::RegisteredRuntimeFactory::core_for_command(runtime.clone());

    let input = json!({
        "id": watched,
        "comment": "Handler succeeded before ledger close failed",
    })
    .to_string();
    let args = parse_tool_run(&[
        "orbit",
        "tool",
        "run",
        "orbit.task.update",
        "--input",
        &input,
    ]);
    let error = orbit_core::test_support::with_resident_action_close_failure(
        || OrbitError::Store("injected terminal ledger append failure".into()),
        || args.execute_with_env(&core, &orbit_common::Env::capture()),
    )
    .expect_err("failed terminal ledger append must remain an error");
    let payload: Value = error_payload(&error);
    assert_eq!(payload["code"], "store_error", "{payload}");
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("injected terminal ledger append failure")),
        "{payload}"
    );

    let comments = runtime
        .get_task_comments(&watched)
        .expect("watched comments");
    assert!(
        comments
            .iter()
            .any(|comment| comment.message == "Handler succeeded before ledger close failed"),
        "expected task.update handler to succeed before ledger close failed: {comments:?}"
    );

    let rows: Vec<ResidentActionRow> = runtime
        .get_task_history(&mandate)
        .expect("mandate history")
        .iter()
        .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
        .filter_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
        .collect();
    assert_eq!(rows.len(), 1, "only the Started row should exist: {rows:?}");
    assert_eq!(rows[0].phase, ResidentActionPhase::Started, "{rows:?}");
    assert_eq!(payload["action_key"], rows[0].key, "{payload}");

    // Pre-admission refusals without a ledger entry continue to omit action_key.
    let refused_args =
        parse_tool_run(&["orbit", "tool", "run", "orbit.task.update", "--input", "{}"]);
    let refusal = refused_args
        .execute_with_env(&core, &orbit_common::Env::capture())
        .expect_err("empty update input must be refused");
    let denied = error_payload(&refusal);
    assert!(denied.get("action_key").is_none(), "{denied}");
    assert_eq!(
        runtime
            .get_task_history(&mandate)
            .expect("mandate history after refusal")
            .iter()
            .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
            .count(),
        rows.len()
    );
}
