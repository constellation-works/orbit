//! [ORB-14699] A provider budget reads its spend from the host invocation
//! ledger: invocations attributed to the provider by the provider they ran on,
//! else by their agent, in the trailing window, oldest first.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]
use chrono::{Duration, Utc};
use orbit_common::{process, test_env};
use orbit_store::Store;
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;

/// Run `test` in a child process with its own home, so opening a store
/// cannot touch the caller's Orbit state. Returns whether this is the child.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_PROVIDER_LEDGER_TEST_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let result =
        process::run_bounded_capped(&mut command, std::time::Duration::from_secs(60), 256 * 1024)
            .unwrap();
    test_env::assert_child_test_passed(test, result.status, result.stdout, result.stderr);
    false
}

/// Record an invocation, then move it to `age` ago.
fn record(
    store: &Store,
    path: &Path,
    agent: &str,
    provider: Option<&str>,
    tokens: (u64, u64),
    cost: Option<f64>,
    age: Duration,
) {
    store
        .insert_invocation_trace_record(
            "ws_ledger",
            &InvocationInsertParams {
                job_run_id: "run".to_string(),
                activity_id: "implement_one".to_string(),
                agent: agent.to_string(),
                provider: provider.map(ToOwned::to_owned),
                model: None,
                task_ids: Vec::new(),
                trace: InvocationTrace {
                    usage: TokenUsage {
                        input: tokens.0,
                        output: tokens.1,
                        ..TokenUsage::default()
                    },
                    provider_cost_usd: cost,
                    ..InvocationTrace::default()
                },
            },
        )
        .unwrap();
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE invocations SET ts = ?1 WHERE id = (SELECT MAX(id) FROM invocations)",
            [(Utc::now() - age).to_rfc3339()],
        )
        .unwrap();
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// A row is the provider's by its provider column, else by its agent; an
/// alias counts; a row outside the window does not; entries come oldest
/// first with the ledger's token total and the cost, if any.
#[test]
fn spend_is_attributed_by_provider_else_agent_inside_the_window() {
    if !isolated("spend_is_attributed_by_provider_else_agent_inside_the_window") {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("orbit.db");
    let store = Store::open(&path).unwrap();
    let hour = Duration::hours(1);
    record(
        &store,
        &path,
        "grok",
        Some("grok"),
        (100, 20),
        Some(4.5),
        hour,
    );
    record(&store, &path, "xai", None, (10, 5), None, hour * 3);
    record(
        &store,
        &path,
        "grok",
        Some("grok"),
        (1, 1),
        Some(9.0),
        hour * 6,
    );
    // Antigravity runs carry the model's family as their agent.
    record(
        &store,
        &path,
        "gemini",
        Some("antigravity"),
        (7, 3),
        None,
        hour,
    );
    record(
        &store,
        &path,
        "gemini",
        Some("gemini"),
        (50, 50),
        None,
        hour,
    );
    record(
        &store,
        &path,
        "claude",
        Some("claude"),
        (1, 1),
        Some(1.0),
        hour,
    );

    let since = Utc::now() - hour * 5;
    let grok = store
        .list_provider_ledger_entries(&names(&["grok", "xai"]), since)
        .unwrap();
    assert_eq!(
        grok.iter()
            .map(|entry| (entry.tokens, entry.cost_usd))
            .collect::<Vec<_>>(),
        [(15, None), (120, Some(4.5))],
        "the 6h-old row is outside a 5h window; a legacy `xai` agent row counts"
    );
    assert!(grok[0].ts < grok[1].ts, "oldest first");

    let antigravity = store
        .list_provider_ledger_entries(&names(&["antigravity"]), since)
        .unwrap();
    assert_eq!(antigravity.len(), 1, "attributed by provider, not by agent");
    assert_eq!(antigravity[0].tokens, 10);
    let gemini = store
        .list_provider_ledger_entries(&names(&["gemini", "google"]), since)
        .unwrap();
    assert_eq!(gemini.len(), 1, "the Antigravity row is not Gemini's");
    assert!(
        store
            .list_provider_ledger_entries(&[], since)
            .unwrap()
            .is_empty()
    );
}

/// A store from before the provider column gains it on open without touching
/// what it held, and attributes the old row by its agent.
#[test]
fn an_existing_store_gains_the_provider_column_additively() {
    if !isolated("an_existing_store_gains_the_provider_column_additively") {
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("orbit.db");
    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    // Recreate schema v41, with a row the older binary wrote.
    conn.execute_batch(
        "ALTER TABLE invocations DROP COLUMN provider;
        DELETE FROM schema_meta WHERE key = 'migration.v0042';
        INSERT INTO invocations (ts, job_run_id, activity_id, agent, input_tokens, output_tokens)
        VALUES (strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now'), 'run', 'act', 'grok', 40, 2);",
    )
    .unwrap();
    drop(conn);

    for _ in 0..2 {
        let store = Store::open(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let migration: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'migration.v0042'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migration, "invocation_provider");
        let entries = store
            .list_provider_ledger_entries(&names(&["grok"]), Utc::now() - Duration::hours(1))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tokens, 42);
    }
}
