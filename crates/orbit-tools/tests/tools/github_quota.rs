//! The host's GitHub quota guard [ORB-25868]: one implementer's
//! `github.run.logs` loop emptied the account's 5,000-an-hour REST quota and
//! stopped every delivery until the reset. These cases pin the guard's
//! contract at its public surface: the agent reserve, the host-wide hold after
//! a rate-limit answer, the shared refresh, untorn records and the unknown
//! answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_tools::github_quota::{
    self, GithubCaller, GithubQuotaPolicy, GithubQuotaStore, ObservedQuota, QuotaRefusalReason,
    RESOURCE_CORE, RESOURCE_GRAPHQL, RateLimitAnswer,
};
use orbit_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use tempfile::TempDir;

const ACCOUNT: &str = "github.com-test";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn known(core_remaining: u64, reset: u64) -> RateLimitAnswer {
    let mut observed = BTreeMap::new();
    observed.insert(
        RESOURCE_CORE.to_string(),
        ObservedQuota {
            limit: 5000,
            remaining: core_remaining,
            reset_epoch_seconds: reset,
        },
    );
    observed.insert(
        RESOURCE_GRAPHQL.to_string(),
        ObservedQuota {
            limit: 5000,
            remaining: 4000,
            reset_epoch_seconds: reset,
        },
    );
    RateLimitAnswer::Known(observed)
}

/// A fake `gh` that logs every call and answers the rate-limit endpoint from
/// `rate_limit` and every other call from `answer` (`ok`, `empty` or `throttle`).
struct FakeGh {
    dir: TempDir,
}

impl FakeGh {
    fn new(rate_limit: &str, answer: &str) -> Self {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("rate_limit"), rate_limit).unwrap();
        std::fs::write(dir.path().join("answer"), answer).unwrap();
        let program = dir.path().join("gh");
        std::fs::write(
            &program,
            "#!/bin/sh\n\
             [ \"$1\" = --warm-up ] && exit 0\n\
             dir=$(dirname \"$0\")\n\
             printf '%s\\n' \"$*\" >> \"$dir/calls\"\n\
             if [ \"$*\" = 'api rate_limit' ]; then\n\
               case $(cat \"$dir/rate_limit\") in\n\
                 refuse) echo 'HTTP 401: Bad credentials (https://api.github.com/rate_limit)' >&2; exit 1 ;;\n\
                 hang) sleep 30; exit 0 ;;\n\
                 *) cat \"$dir/rate_limit\"; exit 0 ;;\n\
               esac\n\
             fi\n\
             if [ \"$(cat \"$dir/answer\")\" = empty ]; then exit 0; fi\n\
             if [ \"$(cat \"$dir/answer\")\" = throttle ]; then\n\
               echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2\n\
               exit 1\n\
             fi\n\
             echo '{}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // Another thread forking while the script was open for writing makes
        // exec fail with ETXTBSY until that child execs; wait it out.
        let mut attempts = 0;
        while std::process::Command::new(&program)
            .arg("--warm-up")
            .status()
            .is_err()
        {
            attempts += 1;
            assert!(attempts < 200, "fake gh never became executable");
            std::thread::sleep(Duration::from_millis(10));
        }
        Self { dir }
    }

    fn request(&self, args: &[&str]) -> ExecRequest {
        ExecRequest {
            program: self.dir.path().join("gh").to_string_lossy().into_owned(),
            args: args.iter().map(ToString::to_string).collect(),
            current_dir: None,
            timeout_ms: Some(10_000),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(vec![
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
                ("GH_TOKEN".to_string(), "fixture-token".to_string()),
            ]),
            debug: false,
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }
}

fn store(dir: &Path) -> GithubQuotaStore {
    GithubQuotaStore::new(dir.to_path_buf())
}

fn refused_throttle(error: &orbit_common::OrbitError) -> orbit_common::ProviderReadThrottle {
    error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
        .unwrap_or_else(|| panic!("expected a typed throttle, got {error}"))
}

/// Criterion 1 and the `github.*` execution point: at the agent reserve,
/// `github.run.view`, `github.run.list` and `github.run.logs` are refused
/// before any `gh` runs, with the resource and reset typed; a delivery read in
/// the same state runs.
#[test]
fn agent_run_reads_are_refused_at_the_agent_reserve_while_delivery_runs() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    let account = github_quota::inherited_account_key();
    // 1,000 of 5,000 left is exactly the 20% agent reserve.
    store(records.path()).refresh_if_due(&policy, &account, now, &|| known(1000, reset));

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let refusals = github_quota::with_scoped_guard(store(records.path()), policy, || {
        [
            ("github.run.view", json!({"run": "123"})),
            ("github.run.list", json!({})),
            ("github.run.logs", json!({"run": "123"})),
        ]
        .map(|(tool, input)| {
            let error = registry
                .execute(tool, &ToolContext::default(), input)
                .expect_err(tool);
            (tool, refused_throttle(&error), error.to_string())
        })
    });
    for (tool, throttle, message) in refusals {
        assert_eq!(throttle.resource.as_deref(), Some(RESOURCE_CORE), "{tool}");
        assert_eq!(throttle.reset_epoch_seconds, Some(reset), "{tool}");
        assert!(message.contains("do not retry before"), "{tool}: {message}");
    }
    let status = store(records.path()).status(&policy, &account, now);
    let core = &status.resources[0];
    assert_eq!(core.resource, RESOURCE_CORE);
    assert!(core.refused.agent >= 3, "{core:?}");
    assert_eq!(
        core.calls.agent, 0,
        "a refused read must run nothing: {core:?}"
    );
    assert!(core.agent_reads_refused && !core.background_reads_refused);

    let gh = FakeGh::new("{}", "ok");
    let delivery = gh.request(&["run", "view", "123"]);
    let delivery_account = github_quota::request_account_key(&delivery);
    store(records.path()).refresh_if_due(&policy, &delivery_account, now, &|| known(1000, reset));
    let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &delivery)
    })
    .unwrap();
    assert!(result.success);
    assert_eq!(gh.calls(), vec!["run view 123".to_string()]);
}

/// Criterion 2: one authenticated primary rate-limit answer for `core` holds
/// every other process's `core` reads on the host until the recorded reset,
/// and leaves `graphql` alone.
#[test]
fn primary_rate_limit_answer_holds_core_reads_host_wide_but_not_graphql() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("{}", "throttle");
    let request = gh.request(&["run", "view", "123"]);
    let account = github_quota::request_account_key(&request);
    let now = now();
    let reset = now + 1800;
    store(records.path()).refresh_if_due(&policy, &account, now, &|| known(4000, reset));

    let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &request)
    })
    .unwrap();
    assert!(!result.success);

    // Another process: its own handle on the same host directory.
    let other = std::thread::spawn({
        let dir = records.path().to_path_buf();
        let account = account.clone();
        move || {
            let other = store(&dir);
            let unknown = || RateLimitAnswer::Unknown("not consulted".into());
            (
                other.admit(
                    &policy,
                    &account,
                    GithubCaller::Delivery,
                    RESOURCE_CORE,
                    now + 1,
                    &unknown,
                ),
                other.admit(
                    &policy,
                    &account,
                    GithubCaller::Agent,
                    RESOURCE_GRAPHQL,
                    now + 1,
                    &unknown,
                ),
                other.admit(
                    &policy,
                    &account,
                    GithubCaller::Delivery,
                    RESOURCE_CORE,
                    reset,
                    &unknown,
                ),
            )
        }
    })
    .join()
    .unwrap();
    let held = other.0.expect_err("core must be held");
    assert_eq!(held.reason, QuotaRefusalReason::Exhausted);
    assert_eq!(held.resource, RESOURCE_CORE);
    assert_eq!(held.resume_epoch_seconds, reset);
    assert!(other.1.is_ok(), "graphql is a separate bucket");
    assert!(other.2.is_ok(), "the hold ends at the reset");
    assert_eq!(gh.calls(), vec!["run view 123".to_string()]);
}

/// Criterion 3: however many readers race, the rate-limit endpoint is read at
/// most once per interval.
#[test]
fn concurrent_readers_refresh_at_most_once_per_interval() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let start = now();
    let race = |at: u64| {
        let barrier = Arc::new(Barrier::new(12));
        let workers = (0..12)
            .map(|_| {
                let dir = records.path().to_path_buf();
                let refreshes = Arc::clone(&refreshes);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store(&dir)
                        .admit(
                            &policy,
                            ACCOUNT,
                            GithubCaller::Background,
                            RESOURCE_CORE,
                            at,
                            &|| {
                                refreshes.fetch_add(1, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(30));
                                known(4000, start + 3600)
                            },
                        )
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .for_each(|worker| worker.join().unwrap());
    };
    race(start);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    race(start + 59);
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "still inside the interval"
    );
    race(start + 60);
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
}

/// A caller that sampled its time before waiting behind a later claimant
/// cannot move the claim backward and start a second read inside the interval.
#[test]
fn out_of_order_samples_never_move_the_refresh_claim_backward() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let refreshes = AtomicUsize::new(0);
    let start = now();
    let refresh = || {
        refreshes.fetch_add(1, Ordering::SeqCst);
        known(4000, start + 3600)
    };
    let store = store(records.path());
    store.refresh_if_due(&policy, ACCOUNT, start + 1, &refresh);
    store.refresh_if_due(&policy, ACCOUNT, start, &refresh);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1, "older sample waits");
    store.refresh_if_due(&policy, ACCOUNT, start + 60, &refresh);
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "the claim stayed at start + 1"
    );
    store.refresh_if_due(&policy, ACCOUNT, start + 61, &refresh);
    assert_eq!(refreshes.load(Ordering::SeqCst), 2, "interval elapsed");
}

/// Criteria 3 and 9: concurrent writers never tear the record, and it counts
/// every call by caller class.
#[test]
fn concurrent_writers_leave_a_whole_record_counting_every_class() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    store(records.path()).refresh_if_due(&policy, ACCOUNT, now, &|| known(4000, now + 3600));
    let classes = [
        GithubCaller::Delivery,
        GithubCaller::Background,
        GithubCaller::Agent,
    ];
    let workers = (0..9)
        .map(|index| {
            let dir: PathBuf = records.path().to_path_buf();
            let caller = classes[index % 3];
            std::thread::spawn(move || {
                for _ in 0..30 {
                    store(&dir)
                        .admit(&policy, ACCOUNT, caller, RESOURCE_CORE, now, &|| {
                            RateLimitAnswer::Unknown("not due".into())
                        })
                        .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    workers
        .into_iter()
        .for_each(|worker| worker.join().unwrap());

    let text = std::fs::read_to_string(records.path().join(format!("{ACCOUNT}.json"))).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).expect("a whole record");
    let core = store(records.path())
        .status(&policy, ACCOUNT, now)
        .resources[0]
        .clone();
    assert_eq!(
        (core.calls.delivery, core.calls.background, core.calls.agent),
        (90, 90, 90)
    );
    assert_eq!(core.remaining, Some(4000 - 270));
}

/// Criterion 4: a reserve of 0 disables that class's refusal.
#[test]
fn zero_reserve_disables_the_class_refusal() {
    let records = TempDir::new().unwrap();
    let now = now();
    let off = GithubQuotaPolicy {
        agent_reserve_percent: 0,
        background_reserve_percent: 0,
        ..GithubQuotaPolicy::default()
    };
    store(records.path()).refresh_if_due(&off, ACCOUNT, now, &|| known(5, now + 600));
    let unknown = || RateLimitAnswer::Unknown("not due".into());
    let admit = |policy: &GithubQuotaPolicy, caller| {
        store(records.path()).admit(policy, ACCOUNT, caller, RESOURCE_CORE, now, &unknown)
    };
    assert!(admit(&off, GithubCaller::Agent).is_ok());
    assert!(admit(&off, GithubCaller::Background).is_ok());
    let defaults = GithubQuotaPolicy::default();
    assert_eq!(
        admit(&defaults, GithubCaller::Agent).unwrap_err().reason,
        QuotaRefusalReason::Reserve
    );
    assert!(admit(&defaults, GithubCaller::Background).is_err());
    assert!(admit(&defaults, GithubCaller::Delivery).is_ok());
}

/// Criterion 5: a refused, unauthenticated or unreadable rate-limit answer
/// leaves reads allowed and reports the quota unknown, never zero.
#[test]
fn unusable_rate_limit_answers_report_unknown_and_admit_reads() {
    let unauthenticated = json!({"resources": {
        "core": {"limit": 60, "remaining": 0, "reset": now() + 600},
        "graphql": {"limit": 0, "remaining": 0, "reset": now() + 600},
    }})
    .to_string();
    for answer in ["refuse", unauthenticated.as_str(), "not json"] {
        let records = TempDir::new().unwrap();
        let policy = GithubQuotaPolicy::default();
        let gh = FakeGh::new(answer, "ok");
        let request = gh.request(&["run", "view", "123"]);
        let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Agent, &request)
        })
        .unwrap();
        assert!(result.success, "{answer}");
        assert_eq!(
            gh.calls(),
            vec!["api rate_limit", "run view 123"],
            "{answer}"
        );
        let account = github_quota::request_account_key(&request);
        let status = store(records.path()).status(&policy, &account, now());
        assert!(status.refresh_error.is_some(), "{answer}");
        for resource in &status.resources {
            assert_eq!(resource.state, "unknown", "{answer}");
            assert_eq!(resource.remaining, None, "{answer}");
            assert!(!resource.agent_reads_refused, "{answer}");
        }
    }
}

/// Regression [ORB-25933]: an admission that lands while a rate-limit refresh
/// probe is in flight outside the lock must still be debited when the probe's
/// answer commits, so a near-reserve refresh cannot lift remaining back above
/// the reserve and admit an agent read.
#[test]
fn in_flight_refresh_preserves_overlapping_delivery_debits_for_reserve() {
    for seeded in [true, false] {
        let records = TempDir::new().unwrap();
        let policy = GithubQuotaPolicy::default();
        let start = now();
        let reset = start + 1800;
        let refresh_at = if seeded {
            store(records.path()).refresh_if_due(&policy, ACCOUNT, start, &|| known(1001, reset));
            start + policy.refresh_interval.as_secs()
        } else {
            start
        };

        let probe_started = Arc::new(Barrier::new(2));
        let probe_finish = Arc::new(Barrier::new(2));
        let refresher = std::thread::spawn({
            let dir = records.path().to_path_buf();
            let probe_started = Arc::clone(&probe_started);
            let probe_finish = Arc::clone(&probe_finish);
            move || {
                store(&dir).refresh_if_due(&policy, ACCOUNT, refresh_at, &|| {
                    probe_started.wait();
                    probe_finish.wait();
                    known(1001, reset)
                });
            }
        });

        probe_started.wait();
        let delivery = store(records.path()).admit(
            &policy,
            ACCOUNT,
            GithubCaller::Delivery,
            RESOURCE_CORE,
            refresh_at,
            &|| unreachable!("refresh claim is already held"),
        );
        probe_finish.wait();
        refresher.join().unwrap();
        delivery.expect("delivery read must be admitted while near-reserve");

        let core = store(records.path())
            .status(&policy, ACCOUNT, refresh_at)
            .resources
            .into_iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .unwrap();
        assert_eq!(core.remaining, Some(1000), "seeded={seeded}");
        assert_eq!(core.calls.delivery, 1, "seeded={seeded}");
        assert!(core.agent_reads_refused, "seeded={seeded}");

        let refusal = store(records.path())
            .admit(
                &policy,
                ACCOUNT,
                GithubCaller::Agent,
                RESOURCE_CORE,
                refresh_at,
                &|| RateLimitAnswer::Unknown("not due".into()),
            )
            .expect_err("agent read must be refused at the 20% reserve");
        assert_eq!(refusal.reason, QuotaRefusalReason::Reserve);
        assert_eq!(refusal.remaining, Some(1000));
        assert_eq!(refusal.limit, Some(5000));
        assert_eq!(refusal.reserve, 1000);
    }
}

/// Regression [ORB-25934]: a slow `api rate_limit` probe must be bounded by the
/// caller's `request.timeout_ms` instead of hard-coding 15 s, and an expired
/// budget must return timed-out without spawning the original `gh` request.
#[test]
fn slow_rate_limit_probe_bounds_to_request_timeout_and_skips_expired_request() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("hang", "ok");

    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(60);
    let started = std::time::Instant::now();
    let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &request)
    })
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "probe must respect the 60 ms request timeout instead of waiting 15 s, took {:?}",
        started.elapsed()
    );
    assert!(result.timed_out);
    assert!(!result.success);
    assert_eq!(
        gh.calls(),
        vec!["api rate_limit".to_string()],
        "an expired budget after admission must not spawn the original request"
    );

    // Zero budget must not spawn even the probe.
    let gh_zero = FakeGh::new("hang", "ok");
    let mut zero_request = gh_zero.request(&["run", "view", "456"]);
    zero_request.timeout_ms = Some(0);
    let zero_result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &zero_request)
    })
    .unwrap();
    assert!(zero_result.timed_out);
    assert!(!zero_result.success);
    assert!(gh_zero.calls().is_empty());
}

/// Regression [ORB-25964]: a probe cut short by one caller's remaining deadline
/// says nothing about the account, so it must neither claim the refresh
/// interval nor replace a known record with an unknown one. A second caller
/// still meets the agent reserve and the interval stays open for a real read.
#[test]
fn deadline_clipped_probe_leaves_known_quota_and_reserve_refusal_in_place() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("hang", "ok");
    let request = gh.request(&["run", "view", "123"]);
    let account = github_quota::request_account_key(&request);
    let now = now();
    let reset = now + 1800;
    // 1,000 of 5,000 left is exactly the 20% agent reserve, observed long
    // enough ago that the next admission finds the refresh due.
    store(records.path()).refresh_if_due(&policy, &account, now - 120, &|| known(1000, reset));

    let mut short = request.clone();
    short.timeout_ms = Some(60);
    let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &short)
    })
    .unwrap();
    assert!(result.timed_out);
    assert_eq!(gh.calls(), vec!["api rate_limit".to_string()]);

    let status = store(records.path()).status(&policy, &account, now);
    let core = &status.resources[0];
    assert_eq!(
        core.state, "known",
        "a clipped probe must keep the record: {status:?}"
    );
    assert_eq!(status.refresh_error, None, "{status:?}");
    assert!(core.agent_reads_refused, "{core:?}");

    // The interval was not claimed: the second caller reads the endpoint
    // itself (clipped again here) and still meets the reserve.
    let mut agent = request.clone();
    agent.timeout_ms = Some(200);
    let error = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::admit_gh_request(GithubCaller::Agent, &agent)
    })
    .expect_err("the agent reserve still applies after a clipped probe");
    assert_eq!(
        refused_throttle(&error).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert_eq!(
        gh.calls(),
        vec!["api rate_limit".to_string(), "api rate_limit".to_string()],
        "a clipped probe must leave the interval due for the next caller"
    );
}

/// A run-scoped log admitted with one call left above the agent reserve comes
/// back empty and spends that call, so the fallback's metadata read is refused
/// locally. Both the unscoped and the scoped read must report that refusal as
/// the typed throttle (never an empty success or a missing-selection error),
/// the refused fallback must never reach `gh`, and an unavailable fallback with
/// quota to spare stays the ordinary untyped answer.
#[test]
fn refused_log_fallback_is_a_typed_throttle_for_unscoped_and_scoped_reads() {
    let policy = GithubQuotaPolicy::default();
    let reset = now() + 1800;
    let gh = FakeGh::new("{}", "empty");
    let probe = gh.request(&["run", "view", "123"]);
    let account = github_quota::request_account_key(&probe);

    let read = |remaining: u64, input: serde_json::Value| {
        let records = TempDir::new().unwrap();
        store(records.path()).refresh_if_due(&policy, &account, now(), &|| known(remaining, reset));
        let mut requests = orbit_tools::github_cli::RunLogRequests::from_input(&input).unwrap();
        for request in [&mut requests.run_log, &mut requests.run_view] {
            request.program = probe.program.clone();
            request.environment_mode = probe.environment_mode.clone();
        }
        let _ = std::fs::remove_file(gh.dir.path().join("calls"));
        let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
            orbit_tools::github_cli::read_run_log_guarded(
                &requests,
                orbit_tools::github_cli::LogReadBounds::new(4096),
                None,
                &|request| {
                    github_quota::admit_gh_request(GithubCaller::Agent, request)?;
                    Ok(request.clone())
                },
            )
        });
        (result, gh.calls())
    };

    // 1,001 of 5,000 left: one call above the 1,000 reserve.
    let (unscoped, calls) = read(1001, json!({"run": "123", "repo": "acme/orbit"}));
    let throttle = unscoped
        .expect("an unscoped read still returns its read")
        .throttle_refusal()
        .expect("the refused fallback is a typed throttle");
    assert_eq!(
        refused_throttle(&throttle).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert_eq!(refused_throttle(&throttle).reset_epoch_seconds, Some(reset));
    assert_eq!(
        calls.len(),
        1,
        "the refused fallback must not run: {calls:?}"
    );

    let (scoped, calls) = read(
        1001,
        json!({"run": "123", "repo": "acme/orbit", "step": "Build"}),
    );
    let error = scoped
        .err()
        .expect("a scoped read has no selection to return");
    assert_eq!(
        refused_throttle(&error).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert!(error.to_string().contains("do not retry before"), "{error}");
    assert_eq!(
        calls.len(),
        1,
        "the refused fallback must not run: {calls:?}"
    );

    // With quota to spare the fallback runs and its ordinary failure is kept.
    let (scoped, calls) = read(
        4000,
        json!({"run": "123", "repo": "acme/orbit", "step": "Build"}),
    );
    let error = scoped.err().expect("the selection is unavailable");
    assert!(error.private_vcs_failure().is_none(), "{error}");
    assert!(error.to_string().contains("log step not found"), "{error}");
    assert_eq!(calls.len(), 2, "{calls:?}");
}

/// Regression [ORB-26095]: an unreadable or partially malformed existing quota
/// record must be preserved byte-for-byte across guard calls while admission
/// stays fail-open and status surfaces the read/parse failure as unknown; a
/// genuinely missing account file still initializes and counts normally.
#[test]
fn partially_malformed_record_survives_admit_and_reports_unknown_in_status() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let malformed_bytes =
        br#"{"resources":{"core":{"calls":{"delivery":42,"agent":7},"remaining":"corrupt"}}}"#;
    let record_path = records.path().join(format!("{ACCOUNT}.json"));
    std::fs::write(&record_path, malformed_bytes).unwrap();

    let refreshes = AtomicUsize::new(0);
    store(records.path())
        .admit(
            &policy,
            ACCOUNT,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| {
                refreshes.fetch_add(1, Ordering::SeqCst);
                known(4000, now + 1800)
            },
        )
        .expect("admission must stay fail-open on a malformed record");
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "refresh probe must not run over an unreadable record"
    );
    assert_eq!(
        std::fs::read(&record_path).unwrap(),
        malformed_bytes,
        "partially malformed record bytes must stay identical after admit"
    );

    let status = store(records.path()).status(&policy, ACCOUNT, now);
    let error = status
        .refresh_error
        .as_deref()
        .expect("status must surface the record read/parse failure");
    assert!(
        error.contains("unreadable") && error.contains("malformed"),
        "unexpected refresh_error: {error}"
    );
    for resource in &status.resources {
        assert_eq!(resource.state, "unknown");
        assert_eq!(resource.remaining, None);
        assert!(!resource.agent_reads_refused);
    }

    // Non-UTF-8 bytes also fail the read path before JSON parsing and stay intact.
    let binary_account = "github.com-binary";
    let binary_bytes = b"{\"calls\":42}\xff\xfe";
    let binary_path = records.path().join(format!("{binary_account}.json"));
    std::fs::write(&binary_path, binary_bytes).unwrap();
    store(records.path())
        .admit(
            &policy,
            binary_account,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| known(4000, now + 1800),
        )
        .expect("admission must stay fail-open on an unreadable record");
    assert_eq!(std::fs::read(&binary_path).unwrap(), binary_bytes);
    let binary_status = store(records.path()).status(&policy, binary_account, now);
    assert!(
        binary_status
            .refresh_error
            .as_deref()
            .is_some_and(|err| err.contains("unreadable")),
        "{binary_status:?}"
    );
    assert!(
        binary_status
            .resources
            .iter()
            .all(|resource| resource.state == "unknown")
    );

    // A genuinely missing account file still initializes and counts calls.
    let missing_account = "github.com-missing";
    let before = store(records.path()).status(&policy, missing_account, now);
    assert_eq!(before.refresh_error, None);
    store(records.path())
        .admit(
            &policy,
            missing_account,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| known(4000, now + 1800),
        )
        .unwrap();
    let after = store(records.path()).status(&policy, missing_account, now);
    assert_eq!(after.refresh_error, None);
    let core = after
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core.state, "known");
    assert_eq!(core.calls.agent, 1);
    assert_eq!(core.remaining, Some(3999));
}
