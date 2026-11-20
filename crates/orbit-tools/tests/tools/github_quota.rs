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
/// `rate_limit` and every other call from `answer` (`ok` or `throttle`).
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
                 *) cat \"$dir/rate_limit\"; exit 0 ;;\n\
               esac\n\
             fi\n\
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
