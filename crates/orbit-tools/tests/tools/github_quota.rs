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
        // The directory is written into the script, not derived with a
        // `dirname` fork, so the call is logged as the probe's first act.
        let script = format!(
            "#!/bin/sh\n\
             [ \"$1\" = --warm-up ] && exit 0\n\
             dir='{}'\n\
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
             if [ \"$(cat \"$dir/answer\")\" = slow-throttle ]; then\n\
               sleep 1\n\
               printf 'throttled\\n' >> \"$dir/throttled\"\n\
               echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2\n\
               exit 1\n\
             fi\n\
             if [ \"$(cat \"$dir/answer\")\" = empty-then-slow-throttle ]; then\n\
               case \"$*\" in *--log*) exit 0 ;; esac\n\
               sleep 1\n\
               printf 'throttled\\n' >> \"$dir/throttled\"\n\
               echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2\n\
               exit 1\n\
             fi\n\
             if [ \"$(cat \"$dir/answer\")\" = empty-then-throttle ]; then\n\
               case \"$*\" in *--log*) exit 0 ;; esac\n\
               echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2\n\
               exit 1\n\
             fi\n\
             echo '{{}}'\n",
            dir.path().display()
        );
        std::fs::write(&program, script).unwrap();
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

fn run_with_simulated_clipped_probe<T>(
    expected_timeout_ms: u64,
    body: impl FnOnce() -> T,
) -> (usize, T) {
    let probes = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let observed = std::rc::Rc::clone(&probes);
    let output = github_quota::with_simulated_clipped_probe(
        move |probe| {
            assert_eq!(probe.args, ["api", "rate_limit"]);
            assert_eq!(probe.timeout_ms, Some(expected_timeout_ms));
            observed.set(observed.get() + 1);
        },
        body,
    );
    (probes.get(), output)
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
    let (probes, result) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &request)
        })
    });
    let result = result.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "probe must respect the 60 ms request timeout instead of waiting 15 s, took {:?}",
        started.elapsed()
    );
    assert_eq!(probes, 1, "the clipped rate-limit probe must be attempted");
    assert!(result.timed_out);
    assert!(!result.success);
    assert!(
        gh.calls().is_empty(),
        "an expired budget after admission must not spawn the original request: {:?}",
        gh.calls()
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

/// Regression [ORB-26104]: a read whose budget a slow probe spent before the
/// child could start is not charged. A known record keeps its remaining count
/// and protected-call counts, while a request that does run still counts once.
#[test]
fn request_timed_out_before_spawn_is_not_charged_to_a_known_record() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("hang", "ok");
    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(60);
    let account = github_quota::request_account_key(&request);
    let seeded_at = now() - 10_000;
    store(records.path()).refresh_if_due(&policy, &account, seeded_at, &|| {
        known(4000, seeded_at + 20_000)
    });
    let core = |status: github_quota::GithubQuotaStatus| status.resources[0].clone();
    let before = core(store(records.path()).status(&policy, &account, now()));
    assert_eq!(before.remaining, Some(4000));

    let (probes, result) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &request)
        })
    });
    let result = result.unwrap();
    assert!(result.timed_out, "{result:?}");
    assert_eq!(probes, 1);
    assert!(
        gh.calls().is_empty(),
        "the expired request must not spawn gh: {:?}",
        gh.calls()
    );
    let after = core(store(records.path()).status(&policy, &account, now()));
    assert_eq!(
        after.remaining,
        Some(4000),
        "a request that never ran is free"
    );
    assert_eq!(
        (
            after.calls.delivery,
            after.calls.background,
            after.calls.agent
        ),
        (0, 0, 0)
    );

    // A request with budget to run counts once.
    let ran = FakeGh::new("{}", "ok");
    let mut runnable = ran.request(&["run", "view", "123"]);
    runnable.timeout_ms = Some(5_000);
    let runnable_account = github_quota::request_account_key(&runnable);
    store(records.path()).refresh_if_due(&policy, &runnable_account, now(), &|| {
        known(4000, now() + 1800)
    });
    github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &runnable)
    })
    .unwrap();
    let counted = core(store(records.path()).status(&policy, &runnable_account, now()));
    assert_eq!(counted.calls.delivery, 1);
    assert_eq!(counted.remaining, Some(3999));
}

/// Regression [ORB-26133]: a healthy known record whose read's deadline passes
/// while the record's refusal is being evaluated returns the pre-spawn timeout,
/// spawns nothing and leaves the call counts and remaining quota untouched. The
/// same healthy read with budget left charges exactly once.
#[test]
fn deadline_crossed_during_refusal_evaluation_is_not_charged_to_a_healthy_record() {
    let policy = GithubQuotaPolicy::default();
    let core = |records: &Path, account: &str| {
        store(records)
            .status(&policy, account, now())
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .expect("the core resource is reported")
            .clone()
    };

    // A healthy read with budget left charges once.
    let ran_records = TempDir::new().unwrap();
    let ran_gh = FakeGh::new("{}", "ok");
    let mut ran = ran_gh.request(&["run", "view", "123"]);
    ran.timeout_ms = Some(60_000);
    let ran_account = github_quota::request_account_key(&ran);
    store(ran_records.path())
        .refresh_if_due(&policy, &ran_account, now(), &|| known(4000, now() + 1800));
    github_quota::with_scoped_guard(store(ran_records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &ran)
    })
    .unwrap();
    let counted = core(ran_records.path(), &ran_account);
    assert_eq!(counted.calls.delivery, 1);
    assert_eq!(counted.remaining, Some(3999));

    // Armed last: the skewed clock stays on this thread afterwards.
    let expired_records = TempDir::new().unwrap();
    let expired_gh = FakeGh::new("{}", "ok");
    let mut expired = expired_gh.request(&["run", "view", "123"]);
    expired.timeout_ms = Some(60_000);
    let account = github_quota::request_account_key(&expired);
    store(expired_records.path())
        .refresh_if_due(&policy, &account, now(), &|| known(4000, now() + 1800));
    github_quota::advance_deadline_clock_at_next_refusal(Duration::from_secs(120));
    let result = github_quota::with_scoped_guard(store(expired_records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &expired)
    })
    .unwrap();
    assert!(result.timed_out && !result.success, "{result:?}");
    assert!(expired_gh.calls().is_empty(), "{:?}", expired_gh.calls());
    let after = core(expired_records.path(), &account);
    assert_eq!(
        after.remaining,
        Some(4000),
        "a request that never ran is free"
    );
    assert_eq!(after.calls.delivery, 0);
}

/// Regression [ORB-26122]: `admit_gh_request` (delivery completion reads and
/// auto-task source reads) admitted after its budget ran out must not charge a
/// known record. Both callers then return their typed budget error without
/// spawning `gh`, so a charge here drains the quota for a request that never
/// ran. A read admitted with budget left still counts once.
#[test]
fn admission_past_the_budget_is_not_charged_to_a_known_record() {
    let policy = GithubQuotaPolicy::default();

    let records = TempDir::new().unwrap();
    let gh = FakeGh::new("hang", "ok");
    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(60);
    let account = github_quota::request_account_key(&request);
    let seeded_at = now() - 10_000;
    store(records.path()).refresh_if_due(&policy, &account, seeded_at, &|| {
        known(4000, seeded_at + 20_000)
    });
    let core = |status: github_quota::GithubQuotaStatus| status.resources[0].clone();

    // The unresponsive probe spends the whole 60 ms budget, so admission
    // returns with none left. Only the simulated probe ran; nothing is charged.
    let (probes, admitted) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::admit_gh_request(GithubCaller::Delivery, &request)
        })
    });
    admitted.unwrap();
    assert_eq!(probes, 1);
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
    let after = core(store(records.path()).status(&policy, &account, now()));
    assert_eq!(after.remaining, Some(4000), "{after:?}");
    assert_eq!(
        (
            after.calls.delivery,
            after.calls.background,
            after.calls.agent
        ),
        (0, 0, 0),
        "{after:?}"
    );

    // A read admitted with budget left counts once.
    let ran = FakeGh::new("{}", "ok");
    let mut runnable = ran.request(&["run", "view", "123"]);
    runnable.timeout_ms = Some(5_000);
    let records = TempDir::new().unwrap();
    let runnable_account = github_quota::request_account_key(&runnable);
    store(records.path()).refresh_if_due(&policy, &runnable_account, now(), &|| {
        known(4000, now() + 1800)
    });
    github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::admit_gh_request(GithubCaller::Delivery, &runnable)
    })
    .unwrap();
    let counted = core(store(records.path()).status(&policy, &runnable_account, now()));
    assert_eq!(counted.calls.delivery, 1);
    assert_eq!(counted.remaining, Some(3999));
}

/// Regression [ORB-26103]: a caller that spawns the child itself through
/// `admit_gh_request_within_budget` gets only the budget left after admission,
/// and no request to spawn once admission spent all of it. A record lock held
/// 300 ms of a 5 s budget leaves under 4.8 s; an unresponsive probe under a
/// 60 ms budget leaves nothing, and the unguarded zero budget never starts.
#[test]
fn admission_returns_only_the_budget_left_and_never_a_request_past_it() {
    let policy = GithubQuotaPolicy::default();

    let records = TempDir::new().unwrap();
    let gh = FakeGh::new("{}", "ok");
    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(5_000);
    let account = github_quota::request_account_key(&request);
    let record_path = records.path().join(format!("{account}.json"));
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
            &record_path,
            "contending quota holder",
            orbit_common::fs::io::FileLockOptions::default(),
        )
        .unwrap();
        held_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the holder never took the account lock");
    let admitted = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &request)
    })
    .expect("a contended lock that frees inside the budget admits the request");
    holder.join().unwrap();
    let left = admitted.timeout_ms.expect("a finite budget stays finite");
    assert!(
        (1..=4_800).contains(&left),
        "the 300 ms of lock contention must come off the 5 s budget, left {left} ms"
    );

    let records = TempDir::new().unwrap();
    let gh = FakeGh::new("hang", "ok");
    let mut slow = gh.request(&["run", "view", "123"]);
    slow.timeout_ms = Some(60);
    let (probes, admitted) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &slow)
        })
    });
    let error = admitted.expect_err("a budget spent by the probe leaves no request to start");
    assert!(
        matches!(
            error,
            orbit_common::OrbitError::ProcessTimeout { timeout_ms: 60, .. }
        ),
        "{error:?}"
    );
    assert_eq!(probes, 1);
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

    let mut zero = gh.request(&["run", "view", "456"]);
    zero.timeout_ms = Some(0);
    let _ = std::fs::remove_file(gh.dir.path().join("calls"));
    let error = github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &zero)
        .expect_err("a zero budget never starts");
    assert!(
        matches!(error, orbit_common::OrbitError::ProcessTimeout { .. }),
        "{error:?}"
    );
    assert!(gh.calls().is_empty());
}

/// Regression [ORB-26096]: the quota record's lock waits belong to the
/// request's deadline. A second process holding the account lock must not keep
/// a 40 ms request in a fresh 30-second acquisition: the request returns its
/// timeout while the holder still owns the lock, and neither the rate-limit
/// probe nor the protected call reaches `gh`.
#[test]
fn contended_record_lock_is_bounded_by_the_request_deadline() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("{}", "ok");
    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(40);
    let account = github_quota::request_account_key(&request);
    let record_path = records.path().join(format!("{account}.json"));

    // The holder reports the lock taken, then keeps it until released.
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
            &record_path,
            "contending quota holder",
            orbit_common::fs::io::FileLockOptions::default(),
        )
        .unwrap();
        held_tx.send(()).unwrap();
        // Bounded so a failed assertion cannot strand the holder.
        let _ = release_rx.recv_timeout(Duration::from_secs(60));
    });
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the holder never took the account lock");

    let started = std::time::Instant::now();
    let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &request)
    });
    let elapsed = started.elapsed();
    release_tx.send(()).unwrap();
    holder.join().unwrap();

    let result = result.unwrap();
    assert!(
        elapsed < Duration::from_secs(5),
        "lock waits must stop at the 40 ms request deadline, took {elapsed:?}"
    );
    assert!(result.timed_out, "{result:?}");
    assert!(!result.success);
    assert!(
        gh.calls().is_empty(),
        "neither the probe nor the protected call may run: {:?}",
        gh.calls()
    );
}

/// Regression [ORB-26105]: a finite request whose admission spends its budget
/// reports the timeout, not the typed throttle, when the quota record is
/// readable and holds a live hold. A peer holds the record lock past the
/// request's deadline, so admission reads the held record without the lock. A
/// refusal decided with budget left keeps its typed throttle, and the
/// child-spawning admission with the same spent budget returns its timeout.
#[test]
fn expired_admission_over_a_readable_hold_returns_the_timeout() {
    let policy = GithubQuotaPolicy::default();
    let records = TempDir::new().unwrap();
    let gh = FakeGh::new("{}", "ok");
    let now = now();
    let reset = now + 1800;
    let account = github_quota::request_account_key(&gh.request(&["run", "view", "123"]));
    // A fresh core answer keeps the refresh interval claimed, so no probe is
    // due; the primary hold then leaves a readable record with a live hold.
    store(records.path()).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
    store(records.path()).record_throttle(
        &policy,
        &account,
        RESOURCE_CORE,
        &orbit_common::ProviderReadThrottle {
            retry_after_seconds: None,
            reset_epoch_seconds: Some(reset),
            resource: Some(RESOURCE_CORE.to_string()),
            principal: None,
            request_id: None,
        },
        now,
    );

    // Budget left: the refusal stays the typed throttle, and nothing spawns.
    let mut within = gh.request(&["run", "view", "123"]);
    within.timeout_ms = Some(5_000);
    let refused = github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &within)
    })
    .expect_err("a live core hold refuses a read admitted within its budget");
    assert_eq!(
        refused_throttle(&refused).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

    // Budget spent waiting on a peer-held record lock: the record is still
    // readable and holds the core read, yet both child-spawning entry points
    // return their timeout, and neither reaches `gh`.
    let record_path = records.path().join(format!("{account}.json"));
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
            &record_path,
            "contending quota holder",
            orbit_common::fs::io::FileLockOptions::default(),
        )
        .unwrap();
        held_tx.send(()).unwrap();
        // Bounded so a failed assertion cannot strand the holder.
        let _ = release_rx.recv_timeout(Duration::from_secs(60));
    });
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the holder never took the account lock");

    let mut expired = gh.request(&["run", "view", "456"]);
    expired.timeout_ms = Some(40);
    let mut expired_spawn = gh.request(&["run", "view", "789"]);
    expired_spawn.timeout_ms = Some(40);
    let (run, admitted) = github_quota::with_scoped_guard(store(records.path()), policy, || {
        (
            github_quota::run_gh_request(GithubCaller::Delivery, &expired),
            github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &expired_spawn),
        )
    });
    release_tx.send(()).unwrap();
    holder.join().unwrap();

    let run = run.expect("an expired budget returns the timeout outcome, not the hold's throttle");
    assert!(run.timed_out, "{run:?}");
    assert!(!run.success, "{run:?}");
    assert!(
        matches!(
            admitted,
            Err(orbit_common::OrbitError::ProcessTimeout { timeout_ms: 40, .. })
        ),
        "an expired budget returns the process timeout, not the hold's throttle: {admitted:?}"
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

/// Regression [ORB-26129]: a finite read whose budget runs out while it probes
/// a writable record with a live core hold returns its timeout, spawns no
/// protected call and leaves the core refusal count unchanged. The same hold
/// refusing a read with budget left still returns the typed throttle and
/// counts that refusal once.
#[test]
fn expired_refusal_over_a_writable_hold_leaves_the_refusal_count() {
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    let hold = orbit_common::ProviderReadThrottle {
        retry_after_seconds: None,
        reset_epoch_seconds: Some(reset),
        resource: Some(RESOURCE_CORE.to_string()),
        principal: None,
        request_id: None,
    };
    let refused_by_delivery = |records: &Path, account: &str| {
        store(records)
            .status(&policy, account, now)
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .expect("the core resource is reported")
            .refused
            .delivery
    };

    // Budget left: the live hold refuses the read with the typed throttle,
    // counts that refusal once for its caller class, and nothing spawns.
    let within_records = TempDir::new().unwrap();
    let within_gh = FakeGh::new("{}", "ok");
    let mut within = within_gh.request(&["run", "view", "123"]);
    within.timeout_ms = Some(5_000);
    let within_account = github_quota::request_account_key(&within);
    store(within_records.path())
        .refresh_if_due(&policy, &within_account, now, &|| known(4000, reset));
    store(within_records.path()).record_throttle(
        &policy,
        &within_account,
        RESOURCE_CORE,
        &hold,
        now,
    );
    let refused = github_quota::with_scoped_guard(store(within_records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &within)
    })
    .expect_err("a live core hold refuses a read admitted within its budget");
    assert_eq!(
        refused_throttle(&refused).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert!(within_gh.calls().is_empty(), "{:?}", within_gh.calls());
    assert_eq!(
        refused_by_delivery(within_records.path(), &within_account),
        1
    );

    // Budget spent probing a due refresh: the clipped probe keeps the hold,
    // the writable record refuses the read, and the caller gets its timeout
    // with the refusal left uncounted.
    let expired_records = TempDir::new().unwrap();
    let expired_gh = FakeGh::new("hang", "ok");
    let mut expired = expired_gh.request(&["run", "view", "456"]);
    expired.timeout_ms = Some(60);
    let expired_account = github_quota::request_account_key(&expired);
    // Last claimed ten minutes ago, so the refresh is due and its probe runs
    // inside the 60 ms budget.
    store(expired_records.path())
        .refresh_if_due(&policy, &expired_account, now - 600, &|| known(4000, reset));
    store(expired_records.path()).record_throttle(
        &policy,
        &expired_account,
        RESOURCE_CORE,
        &hold,
        now,
    );
    let (probes, run) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(expired_records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &expired)
        })
    });
    let run = run.expect("an expired budget returns the timeout outcome, not the hold's throttle");
    assert!(run.timed_out && !run.success, "{run:?}");
    assert_eq!(probes, 1);
    assert!(expired_gh.calls().is_empty(), "{:?}", expired_gh.calls());
    assert_eq!(
        refused_by_delivery(expired_records.path(), &expired_account),
        0,
        "a refusal the caller receives as a timeout must not be counted"
    );
}

/// Regression [ORB-26134]: a finite read whose budget runs out after the quota
/// decision a writable live core hold produced, and before the wrapper delivers
/// it, returns its timeout, spawns no protected call and leaves the core
/// refusal count unchanged. The same hold surfaced as a typed throttle within
/// the budget counts the refusal exactly once.
#[test]
fn refusal_decided_before_the_budget_ran_out_is_not_counted_when_it_becomes_a_timeout() {
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    let hold = orbit_common::ProviderReadThrottle {
        retry_after_seconds: None,
        reset_epoch_seconds: Some(reset),
        resource: Some(RESOURCE_CORE.to_string()),
        principal: None,
        request_id: None,
    };
    let seeded = |records: &Path, gh: &FakeGh| {
        let mut request = gh.request(&["run", "view", "123"]);
        request.timeout_ms = Some(60_000);
        let account = github_quota::request_account_key(&request);
        store(records).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
        store(records).record_throttle(&policy, &account, RESOURCE_CORE, &hold, now);
        (request, account)
    };
    let refused_by_delivery = |records: &Path, account: &str| {
        store(records)
            .status(&policy, account, now)
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .expect("the core resource is reported")
            .refused
            .delivery
    };

    // Budget left: the typed throttle is returned and counted once.
    let within_records = TempDir::new().unwrap();
    let within_gh = FakeGh::new("{}", "ok");
    let (within, within_account) = seeded(within_records.path(), &within_gh);
    let refused = github_quota::with_scoped_guard(store(within_records.path()), policy, || {
        github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &within)
    })
    .expect_err("a live core hold refuses a read admitted within its budget");
    assert_eq!(
        refused_throttle(&refused).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert_eq!(
        refused_by_delivery(within_records.path(), &within_account),
        1
    );

    // The deadline crosses after the hold's refusal is decided: both
    // child-spawning entry points return their timeout and count nothing.
    // Armed last: the skewed clock stays on this thread afterwards.
    let run_records = TempDir::new().unwrap();
    let run_gh = FakeGh::new("{}", "ok");
    let (run_request, run_account) = seeded(run_records.path(), &run_gh);
    github_quota::advance_deadline_clock_after_next_refusal_decision(Duration::from_secs(120));
    let run = github_quota::with_scoped_guard(store(run_records.path()), policy, || {
        github_quota::run_gh_request(GithubCaller::Delivery, &run_request)
    })
    .expect("a budget spent after the decision returns the timeout, not the hold's throttle");
    assert!(run.timed_out && !run.success, "{run:?}");
    assert!(run_gh.calls().is_empty(), "{:?}", run_gh.calls());
    assert_eq!(refused_by_delivery(run_records.path(), &run_account), 0);

    let spawn_records = TempDir::new().unwrap();
    let spawn_gh = FakeGh::new("{}", "ok");
    let (spawn_request, spawn_account) = seeded(spawn_records.path(), &spawn_gh);
    github_quota::advance_deadline_clock_after_next_refusal_decision(Duration::from_secs(120));
    let admitted = github_quota::with_scoped_guard(store(spawn_records.path()), policy, || {
        github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, &spawn_request)
    });
    assert!(
        matches!(
            admitted,
            Err(orbit_common::OrbitError::ProcessTimeout { .. })
        ),
        "{admitted:?}"
    );
    assert!(spawn_gh.calls().is_empty(), "{:?}", spawn_gh.calls());
    assert_eq!(refused_by_delivery(spawn_records.path(), &spawn_account), 0);
}

/// Regression [ORB-26140]: `admit_gh_request` returns the outcome the host
/// record agrees with. A refusal decided with budget left is counted and
/// returned as the typed throttle; one decided once the budget is spent is
/// not counted and comes back as the typed timeout, never an uncounted throttle.
#[test]
fn admit_gh_request_outcome_agrees_with_whether_the_refusal_was_counted() {
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    let hold = orbit_common::ProviderReadThrottle {
        retry_after_seconds: None,
        reset_epoch_seconds: Some(reset),
        resource: Some(RESOURCE_CORE.to_string()),
        principal: None,
        request_id: None,
    };
    let seeded = |records: &Path, gh: &FakeGh| {
        let mut request = gh.request(&["run", "view", "123"]);
        request.timeout_ms = Some(60_000);
        let account = github_quota::request_account_key(&request);
        store(records).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
        store(records).record_throttle(&policy, &account, RESOURCE_CORE, &hold, now);
        (request, account)
    };
    let refused_by_background = |records: &Path, account: &str| {
        store(records)
            .status(&policy, account, now)
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .expect("the core resource is reported")
            .refused
            .background
    };

    let within_records = TempDir::new().unwrap();
    let within_gh = FakeGh::new("{}", "ok");
    let (within, within_account) = seeded(within_records.path(), &within_gh);
    let refused = github_quota::with_scoped_guard(store(within_records.path()), policy, || {
        github_quota::admit_gh_request(GithubCaller::Background, &within)
    })
    .expect_err("a live core hold refuses a read admitted within its budget");
    assert_eq!(
        refused_throttle(&refused).resource.as_deref(),
        Some(RESOURCE_CORE)
    );
    assert_eq!(
        refused_by_background(within_records.path(), &within_account),
        1
    );

    // Armed last: the skewed clock stays on this thread afterwards.
    let spent_records = TempDir::new().unwrap();
    let spent_gh = FakeGh::new("{}", "ok");
    let (spent, spent_account) = seeded(spent_records.path(), &spent_gh);
    github_quota::advance_deadline_clock_after_next_refusal_decision(Duration::from_secs(120));
    let admitted = github_quota::with_scoped_guard(store(spent_records.path()), policy, || {
        github_quota::admit_gh_request(GithubCaller::Background, &spent)
    });
    assert!(
        matches!(
            admitted,
            Err(orbit_common::OrbitError::ProcessTimeout { .. })
        ),
        "{admitted:?}"
    );
    assert_eq!(
        refused_by_background(spent_records.path(), &spent_account),
        0
    );
}

/// Regression [ORB-26101]: the accounting a caller runs after its own child
/// returns belongs to the original request deadline, not a fresh budget. A
/// primary throttle reported with 50 ms of a 5 s budget left must give up on a
/// peer-held record lock at that instant, not wait another 5 s; with the
/// record available the same near-expiry answer still records the hold.
#[test]
fn post_child_throttle_accounting_waits_only_for_the_remaining_budget() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("{}", "ok");
    let mut request = gh.request(&["run", "view", "123"]);
    request.timeout_ms = Some(5_050);
    let account = github_quota::request_account_key(&request);
    let record_path = records.path().join(format!("{account}.json"));
    let now = now();
    let reset = now + 1800;
    store(records.path()).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
    let throttled = orbit_exec::ExecutionResult {
        success: false,
        timed_out: false,
        stdout: String::new(),
        stderr: "gh: API rate limit exceeded for user ID 42. (HTTP 403)".to_string(),
        exit_code: Some(1),
        duration_ms: 5_000,
        output: None,
    };
    let held_deadline = || {
        // The request started 5 s ago: 50 ms of its budget remain.
        let started = std::time::Instant::now()
            .checked_sub(Duration::from_secs(5))
            .expect("monotonic clock far enough from its origin");
        github_quota::gh_request_deadline(&request, started)
    };

    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
            &record_path,
            "contending quota holder",
            orbit_common::fs::io::FileLockOptions::default(),
        )
        .unwrap();
        held_tx.send(()).unwrap();
        // Bounded so a failed assertion cannot strand the holder.
        let _ = release_rx.recv_timeout(Duration::from_secs(60));
    });
    held_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the holder never took the account lock");

    let deadline = held_deadline();
    let started = std::time::Instant::now();
    github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::observe_gh_result_by_deadline(&request, &throttled, deadline);
    });
    let elapsed = started.elapsed();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(2),
        "the record wait must stop at the original deadline, took {elapsed:?}"
    );

    // An available record takes the hold even with the budget nearly spent.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    github_quota::with_scoped_guard(store(records.path()), policy, || {
        github_quota::observe_gh_result_by_deadline(&request, &throttled, Some(deadline));
    });
    let held = store(records.path())
        .admit(
            &policy,
            &account,
            GithubCaller::Delivery,
            RESOURCE_CORE,
            now + 1,
            &|| RateLimitAnswer::Unknown("not consulted".into()),
        )
        .expect_err("the throttle must hold core reads");
    assert_eq!(held.reason, QuotaRefusalReason::Exhausted);
    assert_eq!(held.resume_epoch_seconds, reset);
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
    let (probes, result) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &short)
        })
    });
    let result = result.unwrap();
    assert!(result.timed_out);
    assert_eq!(probes, 1, "first clipped probe must be attempted");
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

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
    let (probes, admitted) = run_with_simulated_clipped_probe(200, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::admit_gh_request(GithubCaller::Agent, &agent)
        })
    });
    let error = admitted.expect_err("the agent reserve still applies after a clipped probe");
    // The clipped probe spent the whole budget, so the reserve's refusal is
    // decided once the request's budget is gone: the typed timeout, uncounted.
    assert!(
        matches!(error, orbit_common::OrbitError::ProcessTimeout { .. }),
        "{error:?}"
    );
    assert_eq!(
        probes, 1,
        "a clipped probe must leave the interval due for the next caller"
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

/// Regression [ORB-26220]: a clipped probe inside `with_simulated_clipped_probe`
/// must not leave this thread's admission clock advanced once it returns. A
/// 60 ms read that follows on the same thread still runs its `gh api
/// rate_limit` probe instead of timing out before spawn.
#[test]
fn clipped_probe_does_not_leak_its_deadline_into_a_later_short_read() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let gh = FakeGh::new("hang", "ok");
    let mut long = gh.request(&["run", "view", "123"]);
    long.timeout_ms = Some(200);
    let account = github_quota::request_account_key(&long);
    let now = now();
    // Observed long enough ago that every read below finds the refresh due.
    store(records.path()).refresh_if_due(&policy, &account, now - 120, &|| known(4000, now + 1800));

    let (probes, result) = run_with_simulated_clipped_probe(200, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &long)
        })
    });
    let result = result.unwrap();
    assert!(result.timed_out, "{result:?}");
    assert_eq!(probes, 1);

    let mut short = gh.request(&["run", "view", "123"]);
    short.timeout_ms = Some(60);
    let (probes, result) = run_with_simulated_clipped_probe(60, || {
        github_quota::with_scoped_guard(store(records.path()), policy, || {
            github_quota::run_gh_request(GithubCaller::Delivery, &short)
        })
    });
    let result = result.unwrap();
    assert!(result.timed_out, "{result:?}");
    assert_eq!(
        probes, 1,
        "a 60 ms read after a clipped probe must still run its rate-limit probe"
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
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

fn run_contended_clipped_probe(dir: &Path, policy: GithubQuotaPolicy, account: &str, now: u64) {
    let record_path = dir.join(format!("{account}.json"));
    let (probe_started_tx, probe_started_rx) = std::sync::mpsc::channel();
    let (lock_held_tx, lock_held_rx) = std::sync::mpsc::channel();
    let dir_buf = dir.to_path_buf();
    let account_owned = account.to_string();
    let started = std::time::Instant::now();
    let refresher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_millis(80);
        store(&dir_buf).bounded_by(Some(deadline)).refresh_if_due(
            &policy,
            &account_owned,
            now,
            &|| {
                probe_started_tx.send(()).unwrap();
                lock_held_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("holder never acquired lock");
                std::thread::sleep(Duration::from_millis(100));
                RateLimitAnswer::Clipped("rate-limit read cut short".into())
            },
        );
    });

    probe_started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("probe never started");
    let lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
        &record_path,
        "contending finalization lock",
        orbit_common::fs::io::FileLockOptions::default(),
    )
    .unwrap();
    lock_held_tx.send(()).unwrap();

    // The clipped refresher must finish within its deadline while the lock is
    // still held.
    refresher.join().unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "clipped refresh must stay deadline-bounded under finalization contention, took {elapsed:?}"
    );
    drop(lock);
}

fn assert_contended_clipped_recovery(
    dir: &Path,
    policy: GithubQuotaPolicy,
    account: &str,
    now: u64,
    reset: u64,
) {
    store(dir).refresh_if_due(&policy, account, now - 120, &|| known(1000, reset));
    run_contended_clipped_probe(dir, policy, account, now);

    let before = store(dir).status(&policy, account, now);
    let core_before = before
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core_before.state, "known");
    assert_eq!(core_before.remaining, Some(1000));
    assert_eq!(before.refresh_error, None);
    assert!(core_before.agent_reads_refused);

    // Immediately after the lock is released (at the same `now`), the next
    // caller can refresh without waiting out the interval.
    let refreshes = AtomicUsize::new(0);
    store(dir)
        .admit(
            &policy,
            account,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| {
                refreshes.fetch_add(1, Ordering::SeqCst);
                known(3500, reset)
            },
        )
        .expect("fresh observation above reserve must admit agent read");
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "subsequent caller must refresh immediately after contended clipped finalization"
    );
    let after = store(dir).status(&policy, account, now);
    let core_after = after
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core_after.remaining, Some(3499));
}

/// Regression [ORB-26102]: when a caller's probe is clipped while a peer holds
/// the record lock at finalization, the caller must stay bounded by its
/// deadline, preserve the prior valid observation, and still let the next
/// caller refresh immediately once the lock is released.
#[test]
fn clipped_probe_with_contended_finalization_lock_releases_claim_and_preserves_record() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    assert_contended_clipped_recovery(records.path(), policy, ACCOUNT, now, reset);
}

/// Regression [ORB-26115]: a malformed candidate at the active generation's
/// clipped-release path is diagnostic evidence, not proof of ownership. A
/// healthy account still admits and counts its read while the marker bytes are
/// preserved for diagnosis.
#[test]
fn malformed_clipped_release_is_preserved_while_healthy_admission_proceeds() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    store(records.path()).refresh_if_due(&policy, ACCOUNT, now - 120, &|| known(3500, reset));
    run_contended_clipped_probe(records.path(), policy, ACCOUNT, now);

    let marker = records.path().join(format!(".{ACCOUNT}.clipped.2.json"));
    let malformed_bytes = b"{\"generation\":";
    std::fs::write(&marker, malformed_bytes).unwrap();

    store(records.path())
        .admit(
            &policy,
            ACCOUNT,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| unreachable!("active clipped refresh must not probe again"),
        )
        .expect("healthy account should admit the read despite malformed marker");

    assert_eq!(
        std::fs::read(&marker).unwrap(),
        malformed_bytes,
        "malformed release marker bytes must remain available for diagnosis"
    );
    let status = store(records.path()).status(&policy, ACCOUNT, now);
    let core = status
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core.remaining, Some(3499));
    assert_eq!(core.calls.agent, 1);
}

/// Regression [ORB-26135]: a same-generation clipped release marker that omits
/// `previous_claim` is ambiguous, not an explicit `null`. It must not release
/// the fresh active claim: the bytes stay on disk for diagnosis, the healthy
/// account still admits the read, and no same-instant refresh probes.
#[test]
fn clipped_release_missing_previous_claim_keeps_fresh_claim_and_marker() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    store(records.path()).refresh_if_due(&policy, ACCOUNT, now - 120, &|| known(3500, reset));
    run_contended_clipped_probe(records.path(), policy, ACCOUNT, now);

    let marker = records.path().join(format!(".{ACCOUNT}.clipped.2.json"));
    let ambiguous_bytes = b"{\"generation\":2}";
    std::fs::write(&marker, ambiguous_bytes).unwrap();

    let probes = AtomicUsize::new(0);
    store(records.path())
        .admit(
            &policy,
            ACCOUNT,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| {
                probes.fetch_add(1, Ordering::SeqCst);
                known(3500, reset)
            },
        )
        .expect("healthy account should admit the read despite an ambiguous marker");

    assert_eq!(
        probes.load(Ordering::SeqCst),
        0,
        "a fresh active claim must keep its refresh interval"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        ambiguous_bytes,
        "a marker without previous_claim must stay on disk for diagnosis"
    );
    let status = store(records.path()).status(&policy, ACCOUNT, now);
    let core = status
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core.remaining, Some(3499));
    assert_eq!(core.calls.agent, 1);
}

/// A positional clipped-release marker has no named-field provenance. It is
/// malformed diagnostic evidence and must not release a fresh refresh claim.
#[test]
fn positional_clipped_release_keeps_fresh_claim_and_marker() {
    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;
    store(records.path()).refresh_if_due(&policy, ACCOUNT, now - 120, &|| known(3500, reset));
    run_contended_clipped_probe(records.path(), policy, ACCOUNT, now);

    let marker = records.path().join(format!(".{ACCOUNT}.clipped.2.json"));
    let positional_bytes = b"[2,null]";
    std::fs::write(&marker, positional_bytes).unwrap();

    let probes = AtomicUsize::new(0);
    store(records.path())
        .admit(
            &policy,
            ACCOUNT,
            GithubCaller::Agent,
            RESOURCE_CORE,
            now,
            &|| {
                probes.fetch_add(1, Ordering::SeqCst);
                known(3500, reset)
            },
        )
        .expect("healthy quota observation should still admit the read");

    assert_eq!(
        probes.load(Ordering::SeqCst),
        0,
        "a positional marker must not release the fresh refresh claim"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        positional_bytes,
        "positional marker bytes must remain available for diagnosis"
    );
    let status = store(records.path()).status(&policy, ACCOUNT, now);
    let core = status
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core.remaining, Some(3499));
    assert_eq!(core.calls.agent, 1);
}

/// Regression [ORB-26137]: ambiguous clipped-release bytes at the active
/// generation (malformed, or a payload naming another generation) are
/// diagnostic evidence. The due claim that advances the generation, and the
/// ordinary update after it, must leave those bytes on disk while accounting
/// and refresh proceed normally.
#[test]
fn ambiguous_clipped_release_survives_claim_advance_and_later_updates() {
    let ambiguous: [&[u8]; 2] = [
        b"{\"generation\":",
        br#"{"generation":7,"previous_claim":null}"#,
    ];
    for ambiguous_bytes in ambiguous {
        let records = TempDir::new().unwrap();
        let policy = GithubQuotaPolicy::default();
        let now = now();
        let reset = now + 1800;
        store(records.path()).refresh_if_due(&policy, ACCOUNT, now - 120, &|| known(3500, reset));
        run_contended_clipped_probe(records.path(), policy, ACCOUNT, now);

        let marker = records.path().join(format!(".{ACCOUNT}.clipped.2.json"));
        std::fs::write(&marker, ambiguous_bytes).unwrap();

        // One refresh interval later the claim is due and advances to generation 3.
        let later = now + policy.refresh_interval.as_secs();
        let probes = AtomicUsize::new(0);
        store(records.path()).refresh_if_due(&policy, ACCOUNT, later, &|| {
            probes.fetch_add(1, Ordering::SeqCst);
            known(3400, reset)
        });
        assert_eq!(
            probes.load(Ordering::SeqCst),
            1,
            "the due claim must refresh once"
        );
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            ambiguous_bytes,
            "ambiguous release bytes must survive the claim advancing to the next generation"
        );

        store(records.path())
            .admit(
                &policy,
                ACCOUNT,
                GithubCaller::Agent,
                RESOURCE_CORE,
                later,
                &|| unreachable!("the generation-3 claim is fresh"),
            )
            .expect("fresh observation above reserve must admit agent read");
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            ambiguous_bytes,
            "ambiguous release bytes must survive a later ordinary update"
        );
        let status = store(records.path()).status(&policy, ACCOUNT, later);
        let core = status
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .unwrap();
        assert_eq!(core.remaining, Some(3399));
        assert_eq!(core.calls.agent, 1);
    }
}

/// Regression [ORB-26137]: a clipped-release marker at the previous generation
/// is removed only once its payload proves it is a well-formed release for that
/// generation. An ordinary update after the record advanced must leave
/// ambiguous bytes at that path in place.
#[test]
fn ambiguous_previous_generation_marker_survives_ordinary_update() {
    let ambiguous: [&[u8]; 2] = [
        b"{\"generation\":",
        br#"{"generation":7,"previous_claim":null}"#,
    ];
    for ambiguous_bytes in ambiguous {
        let records = TempDir::new().unwrap();
        let policy = GithubQuotaPolicy::default();
        let now = now();
        let reset = now + 1800;
        // Three completed refreshes, one interval apart, leave the record at generation 3.
        for at in [now - 120, now - 60, now] {
            store(records.path()).refresh_if_due(&policy, ACCOUNT, at, &|| known(3500, reset));
        }

        let marker = records.path().join(format!(".{ACCOUNT}.clipped.2.json"));
        std::fs::write(&marker, ambiguous_bytes).unwrap();

        store(records.path())
            .admit(
                &policy,
                ACCOUNT,
                GithubCaller::Agent,
                RESOURCE_CORE,
                now,
                &|| unreachable!("the generation-3 observation is fresh"),
            )
            .expect("fresh observation above reserve must admit agent read");
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            ambiguous_bytes,
            "ambiguous previous-generation release bytes must survive an ordinary update"
        );
    }
}

/// Regression [ORB-26114]: a writable/searchable quota directory that denies
/// listing (`0o300`) must still reconcile a staged clipped release by name,
/// refresh once at the same logical instant, admit an agent once the new
/// observation clears its reserve, and leave newer claims and debits intact.
#[cfg(unix)]
#[test]
fn clipped_probe_recovers_without_directory_listing_and_ignores_unrelated_entries() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePermissions<'a>(&'a Path);
    impl Drop for RestorePermissions<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    let records = TempDir::new().unwrap();
    let policy = GithubQuotaPolicy::default();
    let now = now();
    let reset = now + 1800;

    for index in 0..8 {
        std::fs::write(records.path().join(format!("unrelated-{index}.json")), "{}").unwrap();
    }

    let _restore = RestorePermissions(records.path());
    std::fs::set_permissions(records.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    assert!(
        std::fs::read_dir(records.path()).is_err(),
        "test fixture requires a directory that denies listing"
    );

    assert_contended_clipped_recovery(records.path(), policy, ACCOUNT, now, reset);

    // Generation 2's marker must have been unlinked after reconciliation.
    assert!(
        !records
            .path()
            .join(format!(".{ACCOUNT}.clipped.2.json"))
            .exists(),
        "reconciled clipped marker should be removed"
    );

    // A stale generation-3 marker written while generation 4 is in flight must
    // leave generation 4's interval and overlapping debits intact.
    let t1 = now + 60;
    let started = Arc::new(Barrier::new(2));
    let finish = Arc::new(Barrier::new(2));
    let dir_worker = records.path().to_path_buf();
    let started_worker = Arc::clone(&started);
    let finish_worker = Arc::clone(&finish);
    let refresher = std::thread::spawn(move || {
        store(&dir_worker).refresh_if_due(&policy, ACCOUNT, t1, &|| {
            started_worker.wait();
            finish_worker.wait();
            known(3500, reset + 1800)
        });
    });
    started.wait();

    let stale_marker = records.path().join(format!(".{ACCOUNT}.clipped.3.json"));
    std::fs::write(&stale_marker, r#"{"generation":3,"previous_claim":null}"#).unwrap();

    // Delivery admission during gen 4 records an in-flight debit and must not
    // be clobbered by the stale generation-3 marker, and the update unlinks
    // that superseded marker without listing the directory.
    store(records.path())
        .admit(
            &policy,
            ACCOUNT,
            GithubCaller::Delivery,
            RESOURCE_CORE,
            t1,
            &|| unreachable!("newer generation refresh is already in flight"),
        )
        .unwrap();
    assert!(
        !stale_marker.exists(),
        "next quota record update must remove superseded clipped marker without listing"
    );
    finish.wait();
    refresher.join().unwrap();

    // A stale generation-3 marker staged after generation 4 already completed
    // must also be removed on the next quota record update.
    std::fs::write(&stale_marker, r#"{"generation":3,"previous_claim":null}"#).unwrap();

    let refreshes = AtomicUsize::new(0);
    store(records.path()).refresh_if_due(&policy, ACCOUNT, t1, &|| {
        refreshes.fetch_add(1, Ordering::SeqCst);
        known(999, reset + 1800)
    });
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert!(
        !stale_marker.exists(),
        "next quota record update after completion must remove stale clipped marker without listing"
    );

    let status = store(records.path()).status(&policy, ACCOUNT, t1);
    let core = status
        .resources
        .iter()
        .find(|resource| resource.resource == RESOURCE_CORE)
        .unwrap();
    assert_eq!(core.remaining, Some(3499));
}

/// Regression [ORB-26102]: a delayed clipped-claim release from an older
/// refresh generation must never roll back or clear an active or completed
/// newer refresh generation.
#[test]
fn stale_clipped_release_does_not_clobber_newer_refresh_generation() {
    for complete_newer_before_release in [true, false] {
        let records = TempDir::new().unwrap();
        let policy = GithubQuotaPolicy::default();
        let start = now();
        let t1 = start + 60;
        let t2 = t1 + 60;
        let reset = t2 + 1800;
        let record_path = records.path().join(format!("{ACCOUNT}.json"));
        store(records.path()).refresh_if_due(&policy, ACCOUNT, start, &|| known(1000, reset));

        let (a_started_tx, a_started_rx) = std::sync::mpsc::channel();
        let (a_proceed_tx, a_proceed_rx) = std::sync::mpsc::channel();
        let dir_a = records.path().to_path_buf();
        let caller_a = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(80);
            store(&dir_a)
                .bounded_by(Some(deadline))
                .refresh_if_due(&policy, ACCOUNT, t1, &|| {
                    a_started_tx.send(()).unwrap();
                    a_proceed_rx
                        .recv_timeout(Duration::from_secs(10))
                        .expect("caller A never signaled to finish");
                    std::thread::sleep(Duration::from_millis(100));
                    RateLimitAnswer::Clipped("stale clipped probe".into())
                });
        });

        a_started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("caller A never claimed generation");

        let mut caller_b = None;
        let b_finish = Arc::new(Barrier::new(2));
        if complete_newer_before_release {
            store(records.path()).refresh_if_due(&policy, ACCOUNT, t2, &|| known(3500, reset));
        } else {
            let b_started = Arc::new(Barrier::new(2));
            let dir_b = records.path().to_path_buf();
            let b_started_worker = Arc::clone(&b_started);
            let b_finish_worker = Arc::clone(&b_finish);
            caller_b = Some(std::thread::spawn(move || {
                store(&dir_b).refresh_if_due(&policy, ACCOUNT, t2, &|| {
                    b_started_worker.wait();
                    b_finish_worker.wait();
                    known(3500, reset)
                });
            }));
            b_started.wait();
        }

        // Hold the record lock so caller A must stage its clipped release
        // instead of acquiring the completion lock.
        let lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
            &record_path,
            "contending lock for stale clipped release",
            orbit_common::fs::io::FileLockOptions::default(),
        )
        .unwrap();
        a_proceed_tx.send(()).unwrap();
        caller_a.join().unwrap();
        drop(lock);

        if let Some(worker) = caller_b {
            // While generation 3 is still in flight, reconciling the stale
            // generation 2 release must leave `refresh_in_flight` and
            // `in_flight_debits` intact.
            store(records.path())
                .admit(
                    &policy,
                    ACCOUNT,
                    GithubCaller::Delivery,
                    RESOURCE_CORE,
                    t2,
                    &|| unreachable!("newer generation refresh is already in flight"),
                )
                .unwrap();
            b_finish.wait();
            worker.join().unwrap();
        }

        let refreshes = AtomicUsize::new(0);
        store(records.path()).refresh_if_due(&policy, ACCOUNT, t2, &|| {
            refreshes.fetch_add(1, Ordering::SeqCst);
            known(999, reset)
        });
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            0,
            "stale clipped release must not reopen newer generation's interval (completed_before={complete_newer_before_release})"
        );

        let status = store(records.path()).status(&policy, ACCOUNT, t2);
        let core = status
            .resources
            .iter()
            .find(|resource| resource.resource == RESOURCE_CORE)
            .unwrap();
        let expected_remaining = if complete_newer_before_release {
            3500
        } else {
            3499
        };
        assert_eq!(
            core.remaining,
            Some(expected_remaining),
            "completed_before={complete_newer_before_release}"
        );
        assert!(
            !records
                .path()
                .join(format!(".{ACCOUNT}.clipped.2.json"))
                .exists(),
            "stale clipped release marker must not remain on disk (completed_before={complete_newer_before_release})"
        );
    }
}

/// Regression [ORB-26108]: `stream_log_source`, `read_api_json` and
/// `fallback_jobs` must bound their post-child primary-throttle recording by
/// the deadline left at child exit rather than minting a fresh timeout budget.
/// With a peer holding the quota record lock, a child finishing near its
/// 1.6 s budget gives up at that deadline instead of waiting another 1.6 s;
/// with the lock available, each reader still records the host-wide hold.
#[test]
fn run_log_readers_bound_post_child_throttle_recording_by_remaining_deadline() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Reader {
        Stream,
        Metadata,
        Fallback,
    }

    let build_requests = |reader: Reader, probe: &ExecRequest, cache_dir: &Path| {
        let input = match reader {
            Reader::Metadata => json!({"run": "123", "job": "9", "repo": "acme/orbit"}),
            Reader::Stream | Reader::Fallback => json!({"run": "123", "repo": "acme/orbit"}),
        };
        let mut requests = orbit_tools::github_cli::RunLogRequests::from_input(&input).unwrap();
        if reader == Reader::Metadata {
            requests = requests.with_test_log_cache(cache_dir);
        }
        for request in [&mut requests.run_log, &mut requests.run_view] {
            request.program = probe.program.clone();
            request.environment_mode = probe.environment_mode.clone();
            request.timeout_ms = Some(1_600);
        }
        requests
    };

    let policy = GithubQuotaPolicy::default();

    for reader in [Reader::Stream, Reader::Metadata, Reader::Fallback] {
        let records = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();
        let answer = match reader {
            Reader::Fallback => "empty-then-slow-throttle",
            Reader::Stream | Reader::Metadata => "slow-throttle",
        };
        let gh = FakeGh::new("{}", answer);
        let probe = gh.request(&["run", "view", "123"]);
        let account = github_quota::request_account_key(&probe);
        let record_path = records.path().join(format!("{account}.json"));
        let now = now();
        let reset = now + 1800;
        store(records.path()).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
        let requests = build_requests(reader, &probe, cache_dir.path());

        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (lock_held_tx, lock_held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            admitted_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("target child was never admitted");
            let _lock = orbit_common::fs::io::hold_exclusive_file_lock_options(
                &record_path,
                "contending quota holder",
                orbit_common::fs::io::FileLockOptions::default(),
            )
            .unwrap();
            lock_held_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(60));
        });

        let target_call = usize::from(reader == Reader::Fallback);
        let call_index = AtomicUsize::new(0);
        let target_started = std::sync::Mutex::new(None);
        let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
            orbit_tools::github_cli::read_run_log_guarded(
                &requests,
                orbit_tools::github_cli::LogReadBounds::new(4096),
                None,
                &|request| {
                    let index = call_index.fetch_add(1, Ordering::SeqCst);
                    if reader == Reader::Metadata && index > target_call {
                        return Err(orbit_common::OrbitError::Execution(
                            "stop after metadata read".to_string(),
                        ));
                    }
                    let admitted = github_quota::admit_gh_request_within_budget(
                        GithubCaller::Delivery,
                        request,
                    )?;
                    if index == target_call {
                        admitted_tx.send(()).unwrap();
                        lock_held_rx
                            .recv_timeout(Duration::from_secs(10))
                            .expect("holder never acquired record lock");
                        *target_started.lock().unwrap() = Some(std::time::Instant::now());
                    }
                    Ok(admitted)
                },
            )
        });
        let elapsed = target_started
            .lock()
            .unwrap()
            .expect("target child never ran")
            .elapsed();
        release_tx.send(()).unwrap();
        holder.join().unwrap();

        assert!(
            gh.dir.path().join("throttled").exists(),
            "{reader:?}: child must finish sleeping and emit primary throttle rather than timing out"
        );
        assert!(
            elapsed < Duration::from_millis(2_200),
            "{reader:?}: post-child lock wait must stop at remaining deadline (~1.6 s), took {elapsed:?}"
        );
        match reader {
            Reader::Stream => {
                let error = result.err().expect("throttled stream must fail");
                assert!(refused_throttle(&error).principal.is_some(), "{error}");
            }
            Reader::Metadata => {
                let error = result.err().expect("metadata stop marker must fail");
                assert!(
                    error.to_string().contains("stop after metadata read"),
                    "{error}"
                );
            }
            Reader::Fallback => {
                let read = result.expect("unscoped fallback returns RunLogRead");
                let throttle = read.throttle_refusal().expect("typed fallback throttle");
                assert!(
                    refused_throttle(&throttle).principal.is_some(),
                    "{throttle}"
                );
            }
        }
    }

    // With the lock available, every reader records the primary throttle hold.
    for reader in [Reader::Stream, Reader::Metadata, Reader::Fallback] {
        let records = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();
        let answer = match reader {
            Reader::Fallback => "empty-then-throttle",
            Reader::Stream | Reader::Metadata => "throttle",
        };
        let gh = FakeGh::new("{}", answer);
        let probe = gh.request(&["run", "view", "123"]);
        let account = github_quota::request_account_key(&probe);
        let now = now();
        let reset = now + 1800;
        store(records.path()).refresh_if_due(&policy, &account, now, &|| known(4000, reset));
        let requests = build_requests(reader, &probe, cache_dir.path());

        let result = github_quota::with_scoped_guard(store(records.path()), policy, || {
            orbit_tools::github_cli::read_run_log_guarded(
                &requests,
                orbit_tools::github_cli::LogReadBounds::new(4096),
                None,
                &|request| {
                    github_quota::admit_gh_request_within_budget(GithubCaller::Delivery, request)
                },
            )
        });
        match reader {
            Reader::Stream => {
                let error = result.err().expect("throttled stream must fail");
                assert!(refused_throttle(&error).principal.is_some(), "{error}");
            }
            Reader::Metadata => {
                let error = result.err().expect("throttled metadata must hold read");
                assert_eq!(
                    refused_throttle(&error).resource.as_deref(),
                    Some(RESOURCE_CORE),
                    "{error}"
                );
            }
            Reader::Fallback => {
                let read = result.expect("unscoped fallback returns RunLogRead");
                assert!(read.throttle_refusal().is_some(), "{reader:?}");
            }
        }
        if reader == Reader::Metadata {
            assert_eq!(
                gh.calls(),
                vec!["api --method GET repos/acme/orbit/actions/jobs/9".to_string()],
                "recorded metadata throttle must hold the fallback stream locally"
            );
        }

        let held = store(records.path())
            .admit(
                &policy,
                &account,
                GithubCaller::Delivery,
                RESOURCE_CORE,
                now + 1,
                &|| RateLimitAnswer::Unknown("not consulted".into()),
            )
            .expect_err("recorded primary throttle must hold core reads");
        assert_eq!(held.reason, QuotaRefusalReason::Exhausted, "{reader:?}");
        assert_eq!(held.resume_epoch_seconds, reset, "{reader:?}");
    }
}
