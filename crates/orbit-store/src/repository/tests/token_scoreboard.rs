use orbit_types::telemetry::{InvocationTrace, TokenUsage};

use crate::Store;
use crate::contracts::{InvocationInsertParams, InvocationStoreBackend};
use crate::repository::token_scoreboard::write_token_scoreboard;

const TOKEN_SCOREBOARD_KEYS: [&str; 6] = [
    "generated_at",
    "activities",
    "agents",
    "top_tasks",
    "tools",
    "known_limitations",
];

fn insert_trace(store: &Store, job_run_id: &str, agent: &str, input: u64) {
    store
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: job_run_id.to_string(),
            activity_id: "implement".to_string(),
            agent: agent.to_string(),
            model: Some("gpt-test".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input,
                    ..TokenUsage::default()
                },
                ..InvocationTrace::default()
            },
        })
        .expect("insert invocation");
}

fn tokens_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("tokens.json")
}

fn read_tokens(dir: &std::path::Path) -> String {
    std::fs::read_to_string(tokens_path(dir)).expect("read tokens.json")
}

fn assert_tokens_schema(raw: &str) {
    let value: serde_json::Value = serde_json::from_str(raw).expect("parse tokens.json");
    let object = value.as_object().expect("tokens.json object");
    assert_eq!(object.len(), TOKEN_SCOREBOARD_KEYS.len(), "{raw}");
    for key in TOKEN_SCOREBOARD_KEYS {
        assert!(object.contains_key(key), "missing {key} in {raw}");
    }
}

#[test]
fn empty_store_watermark_is_zero() {
    let store = Store::open_in_memory().expect("open store");
    assert_eq!(
        InvocationStoreBackend::invocation_scoreboard_watermark(&store).expect("watermark"),
        Some(0)
    );
}

#[test]
fn second_refresh_without_a_new_invocation_does_not_rewrite_tokens_json() {
    let store = Store::open_in_memory().expect("open store");
    insert_trace(&store, "jrun-one", "codex", 10);
    let dir = tempfile::tempdir().expect("scoreboard dir");

    write_token_scoreboard(dir.path(), &store).expect("first write");
    let first = read_tokens(dir.path());
    assert_tokens_schema(&first);
    assert!(first.contains("codex"), "{first}");

    write_token_scoreboard(dir.path(), &store).expect("second write");
    let second = read_tokens(dir.path());
    assert_eq!(
        first, second,
        "tokens.json must be left untouched when no invocation landed"
    );
}

#[test]
fn refresh_after_a_new_invocation_rewrites_metrics_without_changing_schema() {
    let store = Store::open_in_memory().expect("open store");
    insert_trace(&store, "jrun-one", "codex", 10);
    let dir = tempfile::tempdir().expect("scoreboard dir");

    write_token_scoreboard(dir.path(), &store).expect("first write");
    let first = read_tokens(dir.path());
    assert_tokens_schema(&first);

    insert_trace(&store, "jrun-two", "claude", 20);
    write_token_scoreboard(dir.path(), &store).expect("second write");
    let second = read_tokens(dir.path());
    assert_ne!(
        first, second,
        "a new invocation must refresh the scoreboard"
    );
    assert_tokens_schema(&second);
    assert!(second.contains("claude"), "{second}");
    assert!(second.contains("codex"), "{second}");
}

fn insert_task_trace(store: &Store, task_ids: &[&str], input: u64, output: u64) {
    store
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-tasks".to_string(),
            activity_id: "implement".to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-test".to_string()),
            task_ids: task_ids.iter().map(ToString::to_string).collect(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input,
                    output,
                    ..TokenUsage::default()
                },
                ..InvocationTrace::default()
            },
        })
        .expect("insert invocation");
}

#[test]
fn top_task_metrics_sum_per_task_heaviest_first_within_the_limit() {
    let store = Store::open_in_memory().expect("open store");
    insert_task_trace(&store, &["light"], 5, 1);
    insert_task_trace(&store, &["heavy", "light"], 100, 20);
    insert_task_trace(&store, &["heavy"], 30, 0);
    insert_task_trace(&store, &["dropped"], 1, 0);

    let top = store
        .list_top_task_invocation_metrics(2)
        .expect("top tasks");

    let ids: Vec<&str> = top.iter().map(|row| row.task_id.as_str()).collect();
    assert_eq!(ids, ["heavy", "light"]);
    assert_eq!(top[0].invocation_count, 2);
    assert_eq!(top[0].total_input_tokens, 130);
    assert_eq!(top[0].total_output_tokens, 20);
    assert_eq!(top[0].total_tokens, 150);
    assert_eq!(top[1].total_tokens, 126);

    let one = store
        .get_task_invocation_metrics("light")
        .expect("one task");
    assert_eq!(one.invocation_count, 2);
    let missing = store
        .get_task_invocation_metrics("absent")
        .expect("absent task");
    assert_eq!(missing.invocation_count, 0);
}
