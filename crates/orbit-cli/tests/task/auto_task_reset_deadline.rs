//! Operator reset remains usable after fetch spends the source deadline.

use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

use chrono::Utc;
use orbit_core::application::{
    auto_tasks::AutoTaskDeleteParams,
    automation::{consumer_key, evaluate_auto_task, reset_auto_task},
};
use orbit_types::workflow::automation::recovery::ResetRequest;

use crate::{
    auto_task_lifecycle_cli::{baselined_delivery_consumer, git, publish_origin_if_configured},
    isolated_cli_fixture::Fixture,
};

/// Intercept only fetch; every local ref command still runs the real Git.
/// The supervised leader announces its PID before hanging. Advance time only
/// while that leader lives, repeatedly so readiness before the supervisor's
/// first clock sample also times out. Reaping freezes time again, allowing a
/// fresh Source to work while every pre-fetch Source stays expired.
fn with_fetch_timeout<T>(fixture: &Fixture, fetches: usize, operation: impl FnOnce() -> T) -> T {
    let bin = fixture._temp.path().join("timeout-bin");
    fs::create_dir_all(&bin).unwrap();
    let ready = bin.join("fetch-pids");
    fs::write(&ready, "").unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let quote =
        |path: &std::path::Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let script = bin.join("git");
    fs::write(&script, format!(
        "#!/bin/sh\nif [ \"$1\" = fetch ]; then echo $$ >> {}; exec sleep 600; fi\nexec {} \"$@\"\n",
        quote(&ready), quote(&real_git),
    )).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let path = path.to_str().unwrap();
    let _env = orbit_common::test_env::scoped([
        ("PATH", Some(path)),
        (orbit_core::test_support::SOURCE_ELAPSED_ENV, None),
    ]);
    let clock_ready = ready.clone();
    let ticks = Cell::new(0u32);
    let origin = Instant::now();
    let result = orbit_core::test_support::with_delivery_source_clock(
        move || {
            let pid = fs::read_to_string(&clock_ready)
                .unwrap()
                .lines()
                .last()
                .and_then(|pid| pid.parse::<libc::pid_t>().ok());
            // SAFETY: signal zero only probes the fixture's supervised leader.
            if pid.is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0) {
                ticks.set(ticks.get() + 1);
            }
            origin + Duration::from_secs(31) * ticks.get()
        },
        operation,
    );
    let pids = fs::read_to_string(ready).unwrap();
    assert_eq!(
        pids.lines().count(),
        fetches,
        "the public operation must reach each fetch"
    );
    for pid in pids.lines().map(|pid| pid.parse::<libc::pid_t>().unwrap()) {
        // SAFETY: signal zero only probes the fixture's supervised leader.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "fetch leader must be reaped"
        );
    }
    result
}

#[test]
fn reset_and_delete_recover_after_fetch_exhausts_source_deadline() {
    if crate::isolated_test::run_in_child(
        "auto_task_reset_deadline::reset_and_delete_recover_after_fetch_exhausts_source_deadline",
    ) {
        return;
    }
    let fixture = Fixture::new();
    git(
        &fixture,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/fixture/origin.git",
        ],
    );
    let trigger = serde_json::json!({"branch":"fixture-delivery","threshold":1,"max_wait_minutes":60,"coverage":"landed_code_review_v1","max_items":20,"retries":0});
    let (runtime, definition) = baselined_delivery_consumer(&fixture, &trigger);
    let consumer = consumer_key(&runtime, "auto-task", &definition.name).unwrap();
    let store = runtime.automation_store().unwrap();
    let original = store.automation_state(&consumer).unwrap().unwrap();
    let history = store.automation_recoveries(&consumer, 10).unwrap();
    let pin = format!(
        "refs/orbit/automation/{}/deadline-pin",
        orbit_common::security::release::sha256_hex(consumer.as_bytes())
    );
    git(&fixture, &["update-ref", &pin, "HEAD"]);

    // Origin and the executor HEAD advance, but the watched local ref stays
    // behind. A successful reset must prefer origin; a timeout must use the
    // watched local branch rather than HEAD or the stale tracking ref.
    fs::write(fixture.repo.join("fixture.txt"), "remote landing\n").unwrap();
    git(&fixture, &["commit", "-am", "Remote landing"]);
    publish_origin_if_configured(&fixture);
    let remote = git(&fixture, &["rev-parse", "HEAD"]);
    git(&fixture, &["checkout", "-b", "fixture-executor"]);
    git(
        &fixture,
        &[
            "update-ref",
            "refs/heads/fixture-delivery",
            &original.observed.commit,
        ],
    );
    let preview =
        reset_auto_task(&runtime, &definition, &ResetRequest::default(), Utc::now()).unwrap();
    assert_eq!(preview.baseline.commit, remote);
    assert_ne!(preview.baseline, original.observed);
    git(
        &fixture,
        &["update-ref", "-d", "refs/heads/fixture-delivery"],
    );
    let preview = reset_auto_task(&runtime, &definition, &ResetRequest::default(), Utc::now())
        .expect("a missing local fallback must not prevent remote-first success");
    assert_eq!(preview.baseline.commit, remote);
    git(
        &fixture,
        &[
            "update-ref",
            "refs/heads/fixture-delivery",
            &original.observed.commit,
        ],
    );

    let preview = with_fetch_timeout(&fixture, 1, || {
        reset_auto_task(&runtime, &definition, &ResetRequest::default(), Utc::now())
    })
    .expect("preview must recover from the spent fetch deadline");
    assert!(!preview.applied);
    assert!(preview.refusals.is_empty());
    assert_eq!(preview.baseline, original.observed);
    assert_eq!(
        store.automation_state(&consumer).unwrap(),
        Some(original.clone())
    );
    assert_eq!(store.automation_recoveries(&consumer, 10).unwrap(), history);
    assert_eq!(
        git(&fixture, &["rev-parse", &pin]),
        original.observed.commit
    );

    let request = ResetRequest {
        reason: "Reset after fetch deadline".into(),
        force: false,
    };
    let reset = with_fetch_timeout(&fixture, 1, || {
        reset_auto_task(&runtime, &definition, &request, Utc::now())
    })
    .expect("reset must inspect and release pins after the spent fetch deadline");
    assert!(reset.applied);
    assert_eq!(reset.baseline, original.observed);
    assert!(store.automation_state(&consumer).unwrap().is_none());
    assert!(git(&fixture, &["for-each-ref", &pin]).is_empty());
    let recoveries = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(recoveries.len(), history.len() + 1);
    assert_eq!(recoveries[0].reason, request.reason);
    let audit = recoveries[0].reset.as_ref().unwrap();
    assert_eq!(audit.previous_generation, original.generation);
    assert_eq!(audit.baseline, original.observed);
    assert_eq!(audit.released_refs, vec![pin.clone()]);

    // Rebaseline with the real origin, then force deletion through the public
    // API. Delete previews and applies reset, so both fetches must time out.
    evaluate_auto_task(&runtime, &definition, false, Utc::now()).unwrap();
    git(&fixture, &["update-ref", &pin, "HEAD"]);
    let removed = with_fetch_timeout(&fixture, 2, || {
        runtime.auto_task_delete(AutoTaskDeleteParams {
            name: definition.name.clone(),
            reason: Some("Delete after fetch deadline".into()),
            force: true,
        })
    })
    .expect("delete must retain a usable post-reset ref-cleanup budget");
    let teardown = removed.consumer.unwrap();
    assert!(teardown.reset);
    assert_eq!(teardown.released_refs, vec![pin.clone()]);
    assert!(store.automation_state(&consumer).unwrap().is_none());
    assert!(runtime.auto_task_show(&definition.name).unwrap().is_none());
    assert!(git(&fixture, &["for-each-ref", &pin]).is_empty());
    let recoveries = store.automation_recoveries(&consumer, 10).unwrap();
    assert_eq!(recoveries.len(), history.len() + 2);
    assert_eq!(recoveries[0].reason, "Delete after fetch deadline");
    let audit = recoveries[0].reset.as_ref().unwrap();
    assert_eq!(audit.baseline, original.observed);
    assert_eq!(audit.released_refs, vec![pin]);
}
