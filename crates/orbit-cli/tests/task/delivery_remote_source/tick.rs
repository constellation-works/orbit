//! Record real subprocesses at the clock boundary, in an isolated fixture.

use super::*;
use orbit_types::workflow::automation::{
    CoverageBatch, CoverageClass, DeliveryAssociation, SourceRevision,
};
use std::collections::BTreeSet;

const SECOND: &str = "tick-other-consumer";

fn consumers(fixture: &Fixture) -> orbit_core::OrbitRuntime {
    let runtime = baselined_remote_consumer(fixture);
    fixture.json(&[
        "auto-task",
        "add",
        "--name",
        SECOND,
        "--deliveries-landed",
        &trigger(60).to_string(),
        "--title",
        "Observe the same source",
        "--json",
    ]);
    fixture.json(&["auto-task", "toggle", SECOND, "on", "--json"]);
    evaluate_auto_task(
        &runtime,
        &runtime.auto_task_show(SECOND).unwrap().unwrap(),
        false,
        Utc::now(),
    )
    .unwrap();
    runtime
}

fn land(fixture: &Fixture, count: usize) -> Vec<String> {
    let publisher = publisher(fixture);
    let mut commits = vec![];
    for i in 0..count {
        fs::write(publisher.join("fixture.txt"), format!("landing {i}\n")).unwrap();
        git_at(
            &publisher,
            &fixture.home,
            &["commit", "-am", "Fixture landing"],
        );
        commits.push(git_at(&publisher, &fixture.home, &["rev-parse", "HEAD"]));
    }
    git_at(&publisher, &fixture.home, &["push", "origin", BRANCH]);
    commits
}

fn recorded_commands(fixture: &Fixture) -> Vec<String> {
    fs::read_to_string(fixture._temp.path().join("commands.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn recording_runner(fixture: &Fixture) -> String {
    use std::os::unix::fs::PermissionsExt;
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let bin = fixture._temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let quote = |path: &Path| format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"));
    let log = quote(&fixture._temp.path().join("commands.log"));
    for (program, script) in [
        (
            "git",
            format!(
                "#!/bin/sh\nif [ \"$1\" = fetch ]; then printf 'fetch\\n' >> {log}; fi\nexec {} \"$@\"\n",
                quote(&real_git)
            ),
        ),
        (
            "gh",
            format!("#!/bin/sh\nprintf '%s\\n' \"$2\" >> {log}\nprintf '[]\\n'\n"),
        ),
    ] {
        let path = bin.join(program);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    gh_path(fixture)
}

fn tick(fixture: &Fixture, path: &str) -> Value {
    let output = fixture
        .command(&["clock", "tick", "--format", "json"])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "clock tick: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["lock_busy"], false);
    for name in [CONSUMER, SECOND] {
        let row = report["reports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_ne!(row["action"], "error", "{row}");
    }
    report
}

#[cfg(unix)]
#[test]
fn clock_tick_shares_fetch_and_each_lookup_across_consumers() {
    const TEST: &str =
        "delivery_remote_source::tick::clock_tick_shares_fetch_and_each_lookup_across_consumers";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = consumers(&fixture);
    land(&fixture, 24);
    let path = recording_runner(&fixture);
    tick(&fixture, &path);
    let commands = recorded_commands(&fixture);
    assert_eq!(
        commands.iter().filter(|cmd| *cmd == "fetch").count(),
        1,
        "consumers share one fetched head per tick"
    );
    let lookups: BTreeSet<_> = commands
        .iter()
        .filter(|cmd| *cmd != "fetch")
        .cloned()
        .collect();
    assert!(
        !lookups.is_empty(),
        "the fixture must exercise provider observation"
    );
    assert_eq!(
        lookups.len(),
        commands.len() - 1,
        "each commit is queried once across both consumers"
    );
    let store = runtime.automation_store().unwrap();
    for name in [CONSUMER, SECOND] {
        let key =
            orbit_core::application::automation::consumer_key(&runtime, "auto-task", name).unwrap();
        let state = store.automation_state(&key).unwrap().unwrap();
        assert_eq!(state.pending_commits.len(), 24);
        assert_eq!(
            state.associations.len(),
            lookups.len(),
            "both consumers persist the shared response"
        );
    }
    // A new tick gets a fresh fetch and resolves another slice of debt; the
    // already recorded results are never sent to the provider again.
    fs::write(fixture._temp.path().join("commands.log"), "").unwrap();
    tick(&fixture, &path);
    let commands = recorded_commands(&fixture);
    assert_eq!(commands.iter().filter(|cmd| *cmd == "fetch").count(), 1);
    let next: BTreeSet<_> = commands
        .iter()
        .filter(|cmd| *cmd != "fetch")
        .cloned()
        .collect();
    assert!(!next.is_empty());
    assert_eq!(next.len(), commands.len() - 1);
    assert!(
        lookups.is_disjoint(&next),
        "recorded results survive the cache's tick lifetime"
    );
}

#[cfg(unix)]
#[test]
fn clock_tick_leaves_two_parked_consumers_with_24_obligations_untouched() {
    const TEST: &str = "delivery_remote_source::tick::clock_tick_leaves_two_parked_consumers_with_24_obligations_untouched";
    if !in_isolated_child(TEST) {
        return;
    }
    let fixture = Fixture::new();
    let runtime = consumers(&fixture);
    let commits = land(&fixture, 24);
    // Read the remote object from the publisher without a consumer fetch.
    let publisher = fixture._temp.path().join("publisher");
    let after = SourceRevision {
        commit: commits.last().unwrap().clone(),
        tree: git_at(&publisher, &fixture.home, &["rev-parse", "HEAD^{tree}"]),
    };
    let store = runtime.automation_store().unwrap();
    let mut originals = vec![];
    for (name, parked) in [
        (CONSUMER, BatchState::Exhausted),
        (SECOND, BatchState::Failed),
    ] {
        let key =
            orbit_core::application::automation::consumer_key(&runtime, "auto-task", name).unwrap();
        let mut state = store.automation_state(&key).unwrap().unwrap();
        state.observed = after.clone();
        state.pending_commits = commits.clone();
        state.unresolved = commits
            .iter()
            .map(|sha| (sha.clone(), "landing_span_pending".into()))
            .collect();
        let association = DeliveryAssociation {
            key: "pr:owner/repository:fixture-delivery:42".into(),
            anchor: after.commit.clone(),
            reference: "https://github.com/owner/repository/pull/42".into(),
            landed_at: Utc::now(),
        };
        state.associations = commits
            .iter()
            .map(|sha| (sha.clone(), Some(association.clone())))
            .collect();
        state.active = Some(BatchAttempt {
            batch: CoverageBatch {
                schema_version: 1,
                id: format!("parked-{name}"),
                consumer: key.clone(),
                epoch: state.epoch.clone(),
                repository: state.repository.clone(),
                branch: BRANCH.into(),
                coverage: CoverageClass::LandedCodeReviewV1,
                from_exclusive: state.baseline.clone(),
                through_inclusive: after.clone(),
                commits: commits.clone(),
                deliveries: vec![],
                exclusions: vec![],
                created_at: Utc::now(),
                max_attempts: 1,
                retry_until: Utc::now(),
            },
            input_digest: "parked-input".into(),
            attempt: 1,
            action_key: format!("parked-{name}"),
            action_id: Some("stopped-action".into()),
            state: parked,
            reason: Some("operator recovery required".into()),
            retry_after: None,
            reissue: None,
        });
        let raw = serde_json::to_string(&state).unwrap();
        runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE automation_consumers SET state_json=?1 WHERE consumer=?2",
                        [raw.as_str(), key.as_str()],
                    )
                    .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                Ok(())
            })
            .unwrap();
        originals.push(state);
    }
    let path = recording_runner(&fixture);
    for dry_run in [false, true] {
        let report = if dry_run {
            let output = fixture
                .command(&["clock", "tick", "--dry-run", "--format", "json"])
                .env("PATH", &path)
                .output()
                .unwrap();
            assert!(output.status.success());
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        } else {
            tick(&fixture, &path)
        };
        for name in [CONSUMER, SECOND] {
            let row = report["reports"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .unwrap();
            assert_eq!(row["reason"], "needs_attention", "{row}");
        }
        assert!(
            recorded_commands(&fixture).is_empty(),
            "parked consumers perform zero fetches and zero provider lookups"
        );
        for state in &originals {
            assert_eq!(
                store.automation_state(&state.consumer).unwrap().as_ref(),
                Some(state),
                "parked debt and coverage remain unchanged"
            );
        }
    }
}
