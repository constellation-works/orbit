//! A shell may wait on attention only through a grant a capability-holding
//! session minted, and only for the workspaces that session read.

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::attention::AttentionWaitStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn session(capabilities: &[McpCapability]) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: capabilities.iter().copied().collect(),
        ..Default::default()
    }
}

fn attention(
    runtime: &OrbitRuntime,
    input: Value,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session,
        )
        .map(|outcome| outcome.value)
}

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    (root, runtime)
}

fn is_denied(result: Result<impl std::fmt::Debug, OrbitError>) -> bool {
    matches!(result, Err(OrbitError::CapabilityDenied(_)))
}

fn shell_without_a_session_grant_is_refused() {
    let (_root, runtime) = runtime();
    let since = "atc1.unused";
    let input = || json!({"since": since});
    assert!(is_denied(runtime.attention_wait("", input(), "1s", None)));
    assert!(is_denied(runtime.attention_wait(
        "oatw_00",
        input(),
        "1s",
        None
    )));
    assert!(is_denied(runtime.attention_wait(
        &format!("oatw_{}", "0".repeat(64)),
        input(),
        "1s",
        None
    )));
    // A caller that asserts no capability cannot mint one for itself.
    assert!(
        attention(
            &runtime,
            json!({"wait_grant": true}),
            ToolSessionContext::default()
        )
        .is_err()
    );
}

fn minted_grant_waits_for_a_delta_and_stays_inside_its_scope() {
    let (_root, runtime) = runtime();
    let minted = attention(
        &runtime,
        json!({"wait_grant": true}),
        session(&[McpCapability::Agent]),
    )
    .unwrap();
    let token = minted["wait_grant"]["token"].as_str().unwrap().to_owned();
    assert!(
        minted["filters"].get("wait_grant").is_none(),
        "the grant is not echoed as a filter"
    );
    let cursor = minted["cursor"]["next"].as_str().unwrap().to_owned();

    // Nothing changed: the wait times out, and says what it could not read.
    let outcome = runtime
        .attention_wait(&token, json!({"since": cursor}), "1s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::TimedOut);
    let response = outcome.response.unwrap();
    assert!(
        response["degraded"]
            .as_array()
            .is_some_and(|degraded| !degraded.is_empty()),
        "an agent-capability read still reports the kinds it cannot read: {response}"
    );
    let cursor = response["cursor"]["next"].as_str().unwrap().to_owned();

    // A new open friction record is a delta.
    use orbit_store::contracts::FrictionAddParams;
    orbit_core::runtime::friction::store_for(&runtime)
        .unwrap()
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Wait delta".into()),
            body: "Appears after the cursor.".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    let outcome = runtime
        .attention_wait(&token, json!({"since": cursor}), "30s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::Changed);
    assert_eq!(
        outcome.response.unwrap()["items"][0]["kind"],
        "friction_untriaged"
    );

    // The grant covers what was read, and nothing wider.
    for widening in [
        json!({"since": "x", "all": true}),
        json!({"since": "x", "host": "elsewhere"}),
        json!({"since": "x", "workspace": "not-granted"}),
        json!({"since": "x", "wait_grant": true}),
    ] {
        assert!(
            is_denied(runtime.attention_wait(&token, widening.clone(), "1s", None)),
            "{widening} must be refused"
        );
    }
}

fn grant_dir_of(runtime: &OrbitRuntime) -> std::path::PathBuf {
    runtime.global_root().join("attention-wait-grants")
}

fn grant_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_str().is_some_and(|name| {
                    name.len() == 69 && name.ends_with(".json") && !name.starts_with('.')
                })
            })
        })
        .collect();
    files.sort();
    files
}

/// A mint's prune must never remove another mint's in-flight staging file:
/// the atomic store stages the record in a sibling temp name in this same
/// directory, and an unlink between staging and hard_link fails that mint
/// (ORB-25420).
fn prune_leaves_another_mints_staging_file_alone() {
    let (_root, runtime) = runtime();
    let dir = grant_dir_of(&runtime);
    std::fs::create_dir_all(&dir).unwrap();
    // Another mint's staging file: the atomic store's temp name for a grant
    // target, with only part of the record written so far.
    let staging = dir.join(format!(
        ".{}.json.{}.{}.tmp",
        "a".repeat(64),
        1_700_000_000_000_000_000u128,
        0
    ));
    std::fs::write(&staging, "{\"workspace_ids\": [\"un").unwrap();
    // An unparseable grant-named file: a newer binary's record shape, never
    // this binary's expired grant.
    let foreign = dir.join(format!("{}.json", "b".repeat(64)));
    std::fs::write(&foreign, "not a grant record").unwrap();

    for _ in 0..2 {
        let minted = attention(
            &runtime,
            json!({"wait_grant": true}),
            session(&[McpCapability::Agent]),
        )
        .unwrap();
        assert!(
            minted["wait_grant"]["token"].as_str().is_some(),
            "a mint that prunes over another mint's staging file still mints"
        );
    }

    assert_eq!(
        std::fs::read_to_string(&staging).unwrap(),
        "{\"workspace_ids\": [\"un",
        "a prune during another mint's staging write must never remove that file"
    );
    assert!(
        foreign.exists(),
        "an unparseable grant-named file must survive a mint's prune"
    );
}

/// Expired grant records are still pruned on mint, and the expired token is
/// still refused with capability_denied on redeem.
fn expired_grants_are_still_pruned_and_refused() {
    let (_root, runtime) = runtime();
    let minted = attention(
        &runtime,
        json!({"wait_grant": true}),
        session(&[McpCapability::Agent]),
    )
    .unwrap();
    let token = minted["wait_grant"]["token"].as_str().unwrap().to_owned();
    let dir = grant_dir_of(&runtime);
    assert_eq!(grant_files(&dir).len(), 1);
    let grant_file = grant_files(&dir).pop().unwrap();

    // Age the grant past its expiry.
    let mut record: Value =
        serde_json::from_str(&std::fs::read_to_string(&grant_file).unwrap()).unwrap();
    record["expires_at"] = Value::String(
        (chrono::Utc::now() - chrono::Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    std::fs::write(&grant_file, serde_json::to_string(&record).unwrap()).unwrap();

    // The next mint prunes the expired record and still mints.
    let minted = attention(
        &runtime,
        json!({"wait_grant": true}),
        session(&[McpCapability::Agent]),
    )
    .unwrap();
    let fresh_token = minted["wait_grant"]["token"].as_str().unwrap().to_owned();
    let cursor = minted["cursor"]["next"].as_str().unwrap().to_owned();
    assert!(
        !grant_file.exists(),
        "an expired grant record is pruned on mint"
    );
    assert!(
        is_denied(runtime.attention_wait(&token, json!({"since": "atc1.unused"}), "1s", None)),
        "an expired token is refused with capability_denied"
    );

    // The fresh grant still redeems: it times out, it is not denied.
    let outcome = runtime
        .attention_wait(&fresh_token, json!({"since": cursor}), "1s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::TimedOut);
}

/// Concurrent mints race each mint's prune against the others' staging
/// writes; every mint must still succeed with a distinct grant.
fn concurrent_mints_all_succeed() {
    let (_root, runtime) = runtime();
    let barrier = std::sync::Barrier::new(4);
    let tokens = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    attention(
                        &runtime,
                        json!({"wait_grant": true}),
                        session(&[McpCapability::Agent]),
                    )
                    .unwrap()["wait_grant"]["token"]
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut sorted = tokens.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        tokens.len(),
        "concurrent mints must mint distinct grants: {tokens:?}"
    );
    assert_eq!(
        grant_files(&grant_dir_of(&runtime)).len(),
        tokens.len(),
        "every concurrent mint leaves its grant record behind"
    );
    // One of the grants redeems, so the concurrent mints are usable.
    let cursor = attention(
        &runtime,
        json!({"wait_grant": true}),
        session(&[McpCapability::Agent]),
    )
    .unwrap()["cursor"]["next"]
        .as_str()
        .unwrap()
        .to_owned();
    let outcome = runtime
        .attention_wait(&tokens[0], json!({"since": cursor}), "1s", None)
        .unwrap();
    assert_eq!(outcome.status, AttentionWaitStatus::TimedOut);
}

crate::scenario_table!(
    attention_wait,
    shell_without_a_session_grant_is_refused,
    minted_grant_waits_for_a_delta_and_stays_inside_its_scope,
    prune_leaves_another_mints_staging_file_alone,
    expired_grants_are_still_pruned_and_refused,
    concurrent_mints_all_succeed,
);
