//! The host's GitHub quota guard [ORB-25868]: one implementer's
//! `github.run.logs` loop emptied the account's 5,000-an-hour REST quota and
//! stopped every delivery until the reset. These cases pin the guard's
//! contract at its public surface: the agent reserve, the host-wide hold after
//! a rate-limit answer, the shared refresh, untorn records and the unknown
//! answer.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orbit_common::fs::io::{FileLockOptions, hold_exclusive_file_lock_options};
use orbit_common::{OrbitError, ProviderReadThrottle};
use orbit_exec::{EnvironmentMode, ExecRequest, ExecutionResult, StdinMode};
use orbit_tools::github_cli::{LogReadBounds, RunLogRequests, read_run_log_guarded};
use orbit_tools::github_quota::{
    self, GithubCaller,
    GithubCaller::{Agent, Background, Delivery},
    GithubQuotaPolicy, GithubQuotaStatus, GithubQuotaStore, GithubResourceStatus, ObservedQuota,
    QuotaRefusal, QuotaRefusalReason, RESOURCE_CORE, RESOURCE_GRAPHQL, RateLimitAnswer,
};
use orbit_tools::{ToolContext, ToolRegistry};
use serde_json::json;
use tempfile::TempDir;

const ACCOUNT: &str = "github.com-test";
const AMBIGUOUS_RELEASE_PAYLOADS: [&[u8]; 2] = [
    b"{\"generation\":",
    br#"{"generation":7,"previous_claim":null}"#,
];

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
             case \"$(cat \"$dir/answer\")\" in\n\
               empty) exit 0 ;;\n\
               throttle) echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2; exit 1 ;;\n\
               slow-throttle) sleep 1; printf 'throttled\\n' >> \"$dir/throttled\"; echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2; exit 1 ;;\n\
               empty-then-slow-throttle) case \"$*\" in *--log*) exit 0 ;; esac; sleep 1; printf 'throttled\\n' >> \"$dir/throttled\"; echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2; exit 1 ;;\n\
               empty-then-throttle) case \"$*\" in *--log*) exit 0 ;; esac; echo 'gh: API rate limit exceeded for user ID 42. (HTTP 403)' >&2; exit 1 ;;\n\
               empty-then-ignore-term) case \"$*\" in *--log*) exit 0 ;; esac; trap '' TERM; sleep 30; exit 0 ;;\n\
               log-then-ignore-term) case \"$*\" in *--log*) echo 'log line'; exit 0 ;; esac; trap '' TERM; sleep 30; exit 0 ;;\n\
             esac\n\
             echo '{{}}'\n",
            dir.path().display()
        );
        std::fs::write(&program, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        // Another thread forking while open for writing can hit ETXTBSY; wait it out.
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

    fn view_req(&self, timeout_ms: Option<u64>) -> (ExecRequest, String) {
        let mut req = self.request(&["run", "view", "123"]);
        if let Some(ms) = timeout_ms {
            req.timeout_ms = Some(ms);
        }
        let account = github_quota::request_account_key(&orbit_common::Env::capture(), &req);
        (req, account)
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }
}

/// Isolated temporary quota store and policy for one scenario.
struct QuotaFixture {
    records: TempDir,
    policy: GithubQuotaPolicy,
}

impl QuotaFixture {
    fn new() -> Self {
        Self {
            records: TempDir::new().unwrap(),
            policy: GithubQuotaPolicy::default(),
        }
    }
    fn path(&self) -> &Path {
        self.records.path()
    }
    fn store(&self) -> GithubQuotaStore {
        store(self.path())
    }
    fn record_path(&self, acct: &str) -> PathBuf {
        self.path().join(format!("{acct}.json"))
    }
    fn marker(&self, acct: &str, generation: u64) -> PathBuf {
        self.path()
            .join(format!(".{acct}.clipped.{generation}.json"))
    }
    fn seed(&self, acct: &str, at: u64, remaining: u64, reset: u64) {
        self.store()
            .refresh_if_due(&self.policy, acct, at, &|| known(remaining, reset));
    }
    fn record_hold(&self, acct: &str, at: u64, reset: u64) {
        let throttle = ProviderReadThrottle {
            retry_after_seconds: None,
            reset_epoch_seconds: Some(reset),
            resource: Some(RESOURCE_CORE.to_string()),
            principal: None,
            request_id: None,
        };
        self.store()
            .record_throttle(&self.policy, acct, RESOURCE_CORE, &throttle, at);
    }
    fn status(&self, acct: &str, at: u64) -> GithubQuotaStatus {
        self.store().status(&self.policy, acct, at)
    }
    fn core(&self, acct: &str, at: u64) -> GithubResourceStatus {
        self.status(acct, at)
            .resources
            .into_iter()
            .find(|r| r.resource == RESOURCE_CORE)
            .expect("core")
    }
    fn admit(
        &self,
        acct: &str,
        c: GithubCaller,
        at: u64,
        probe: &dyn Fn() -> RateLimitAnswer,
    ) -> Result<(), QuotaRefusal> {
        self.store()
            .admit(&self.policy, acct, c, RESOURCE_CORE, at, probe)
    }
    fn scoped<T>(&self, body: impl FnOnce() -> T) -> T {
        github_quota::with_scoped_guard(self.store(), self.policy, body)
    }
    fn run_gh(&self, c: GithubCaller, req: &ExecRequest) -> Result<ExecutionResult, OrbitError> {
        self.scoped(|| github_quota::run_gh_request(&orbit_common::Env::capture(), c, req))
    }
    fn admit_gh(&self, c: GithubCaller, req: &ExecRequest) -> Result<(), OrbitError> {
        self.scoped(|| github_quota::admit_gh_request(&orbit_common::Env::capture(), c, req))
    }
    fn admit_gh_budget(
        &self,
        c: GithubCaller,
        req: &ExecRequest,
    ) -> Result<ExecRequest, OrbitError> {
        self.scoped(|| {
            github_quota::admit_gh_request_within_budget(&orbit_common::Env::capture(), c, req)
        })
    }
    fn spawn_refresh(
        &self,
        at: u64,
        remaining: u64,
        reset: u64,
    ) -> (Arc<Barrier>, std::thread::JoinHandle<()>) {
        let (started, finish) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
        let (dir, policy) = (self.path().to_path_buf(), self.policy);
        let (sw, fw) = (Arc::clone(&started), Arc::clone(&finish));
        let worker = std::thread::spawn(move || {
            store(&dir).refresh_if_due(&policy, ACCOUNT, at, &|| {
                sw.wait();
                fw.wait();
                known(remaining, reset)
            });
        });
        started.wait();
        (finish, worker)
    }
}

/// Exclusive account-lock holder on a background thread, released explicitly or on drop.
struct ContendedRecordLock {
    release_tx: Option<mpsc::Sender<()>>,
    holder: Option<std::thread::JoinHandle<()>>,
}

impl ContendedRecordLock {
    fn spawn(
        path: PathBuf,
        wait_before: Option<mpsc::Receiver<()>>,
        fixed: Option<Duration>,
    ) -> (Self, mpsc::Receiver<()>) {
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let eager = wait_before.is_none();
        let holder = std::thread::spawn(move || {
            if let Some(rx) = wait_before {
                recv_ready(&rx, "target child was never admitted");
            }
            let _l = hold_exclusive_file_lock_options(
                &path,
                "contending quota holder",
                FileLockOptions::default(),
            )
            .unwrap();
            held_tx.send(()).unwrap();
            if let Some(d) = fixed {
                std::thread::sleep(d);
            } else {
                let _ = release_rx.recv_timeout(Duration::from_secs(60));
            }
        });
        if eager {
            recv_ready(&held_rx, "the holder never took the account lock");
        }
        (
            Self {
                release_tx: Some(release_tx),
                holder: Some(holder),
            },
            held_rx,
        )
    }
    fn hold(path: PathBuf) -> Self {
        Self::spawn(path, None, None).0
    }
    fn stop(&mut self) -> std::thread::Result<()> {
        if let Some(tx) = self.release_tx.take() {
            let _ = tx.send(());
        }
        self.holder
            .take()
            .map_or(Ok(()), std::thread::JoinHandle::join)
    }
    fn release(mut self) {
        self.stop().unwrap();
    }
}

impl Drop for ContendedRecordLock {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn known(core_remaining: u64, reset: u64) -> RateLimitAnswer {
    let mut observed = BTreeMap::new();
    for (resource, remaining) in [(RESOURCE_CORE, core_remaining), (RESOURCE_GRAPHQL, 4000)] {
        observed.insert(
            resource.to_string(),
            ObservedQuota {
                limit: 5000,
                remaining,
                reset_epoch_seconds: reset,
            },
        );
    }
    RateLimitAnswer::Known(observed)
}

fn unknown_not_due() -> RateLimitAnswer {
    RateLimitAnswer::Unknown("not due".into())
}

fn recv_ready(rx: &mpsc::Receiver<()>, what: &str) {
    rx.recv_timeout(Duration::from_secs(10)).expect(what);
}

fn store(dir: &Path) -> GithubQuotaStore {
    GithubQuotaStore::new(dir.to_path_buf())
}

fn hold_lock(path: &Path, label: &str) -> orbit_common::fs::io::HeldFileLock {
    hold_exclusive_file_lock_options(path, label, FileLockOptions::default()).unwrap()
}

fn finish_refresh((finish, worker): (Arc<Barrier>, std::thread::JoinHandle<()>)) {
    finish.wait();
    worker.join().unwrap();
}

fn held_gh(timeout_ms: u64, now: u64, reset: u64) -> (QuotaFixture, FakeGh, ExecRequest, String) {
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (req, acct) = gh.view_req(Some(timeout_ms));
    f.seed(&acct, now, 4000, reset);
    f.record_hold(&acct, now, reset);
    (f, gh, req, acct)
}

fn refused_throttle(error: &OrbitError) -> ProviderReadThrottle {
    error
        .private_vcs_failure()
        .and_then(|f| f.read_throttle.clone())
        .unwrap_or_else(|| panic!("expected throttle: {error}"))
}

fn assert_core_throttle(error: &OrbitError, ctx: &str) {
    assert_eq!(
        refused_throttle(error).resource.as_deref(),
        Some(RESOURCE_CORE),
        "{ctx}"
    );
}

fn assert_timeout<T: Debug>(res: Result<T, OrbitError>, expected_ms: Option<u64>, ctx: &str) {
    let ok = match (&res, expected_ms) {
        (Err(OrbitError::ProcessTimeout { timeout_ms, .. }), Some(ms)) => *timeout_ms == ms,
        (Err(OrbitError::ProcessTimeout { .. }), None) => true,
        _ => false,
    };
    assert!(ok, "{ctx}: {res:?}");
}

fn assert_timed_out_no_gh(result: &ExecutionResult, gh: &FakeGh, ctx: &str) {
    assert!(result.timed_out && !result.success, "{ctx}: {result:?}");
    assert!(gh.calls().is_empty(), "{ctx}: {:?}", gh.calls());
}

fn run_with_simulated_clipped_probe<T>(expected_ms: u64, body: impl FnOnce() -> T) -> (usize, T) {
    let probes = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let observed = std::rc::Rc::clone(&probes);
    let output = github_quota::with_simulated_clipped_probe(
        move |probe| {
            assert_eq!(probe.args, ["api", "rate_limit"]);
            assert_eq!(probe.timeout_ms, Some(expected_ms));
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
    let f = QuotaFixture::new();
    let (now, reset) = (now(), now() + 1800);
    let acct = github_quota::inherited_account_key(&orbit_common::Env::capture());
    // 1,000 of 5,000 left is exactly the 20% agent reserve.
    f.seed(&acct, now, 1000, reset);

    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let refusals = f.scoped(|| {
        [
            ("github.run.view", json!({"run": "123"})),
            ("github.run.list", json!({})),
            ("github.run.logs", json!({"run": "123"})),
        ]
        .map(|(tool, input)| {
            let err = registry
                .execute(
                    tool,
                    &ToolContext {
                        env: orbit_common::Env::capture(),
                        ..ToolContext::default()
                    },
                    input,
                )
                .expect_err(tool);
            (tool, refused_throttle(&err), err.to_string())
        })
    });
    for (tool, throttle, msg) in refusals {
        assert_eq!(throttle.resource.as_deref(), Some(RESOURCE_CORE), "{tool}");
        assert_eq!(throttle.reset_epoch_seconds, Some(reset), "{tool}");
        assert!(msg.contains("do not retry before"), "{tool}: {msg}");
    }
    let core = f.core(&acct, now);
    assert!(core.refused.agent >= 3, "{core:?}");
    assert_eq!(
        core.calls.agent, 0,
        "a refused read must run nothing: {core:?}"
    );
    assert!(core.agent_reads_refused && !core.background_reads_refused);

    let gh = FakeGh::new("{}", "ok");
    let (delivery, delivery_acct) = gh.view_req(None);
    f.seed(&delivery_acct, now, 1000, reset);
    assert!(f.run_gh(Delivery, &delivery).unwrap().success);
    assert_eq!(gh.calls(), vec!["run view 123".to_string()]);
}

/// Criterion 2: one authenticated primary rate-limit answer for `core` holds
/// every other process's `core` reads on the host until the recorded reset,
/// and leaves `graphql` alone.
#[test]
fn primary_rate_limit_answer_holds_core_reads_host_wide_but_not_graphql() {
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("{}", "throttle"));
    let (req, acct) = gh.view_req(None);
    let (now, reset) = (now(), now() + 1800);
    f.seed(&acct, now, 4000, reset);
    assert!(!f.run_gh(Delivery, &req).unwrap().success);

    // Another process: its own handle on the same host directory.
    let other = std::thread::spawn({
        let (dir, policy, acct) = (f.path().to_path_buf(), f.policy, acct);
        move || {
            let s = store(&dir);
            let admit = |c, res, at| s.admit(&policy, &acct, c, res, at, &unknown_not_due);
            (
                admit(Delivery, RESOURCE_CORE, now + 1),
                admit(Agent, RESOURCE_GRAPHQL, now + 1),
                admit(Delivery, RESOURCE_CORE, reset),
            )
        }
    })
    .join()
    .unwrap();
    let held = other.0.expect_err("core must be held");
    assert_eq!(
        (
            held.reason,
            held.resource.as_str(),
            held.resume_epoch_seconds
        ),
        (QuotaRefusalReason::Exhausted, RESOURCE_CORE, reset)
    );
    assert!(other.1.is_ok(), "graphql is a separate bucket");
    assert!(other.2.is_ok(), "the hold ends at the reset");
    assert_eq!(gh.calls(), vec!["run view 123".to_string()]);
}

/// Criterion 3: however many readers race, the rate-limit endpoint is read at
/// most once per interval.
#[test]
fn concurrent_readers_refresh_at_most_once_per_interval() {
    let (f, refreshes, start) = (QuotaFixture::new(), Arc::new(AtomicUsize::new(0)), now());
    let race = |at: u64| {
        let barrier = Arc::new(Barrier::new(12));
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let (dir, policy) = (f.path().to_path_buf(), f.policy);
                let (refreshes, barrier) = (Arc::clone(&refreshes), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    store(&dir)
                        .admit(&policy, ACCOUNT, Background, RESOURCE_CORE, at, &|| {
                            refreshes.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            known(4000, start + 3600)
                        })
                        .unwrap();
                })
            })
            .collect();
        workers.into_iter().for_each(|w| w.join().unwrap());
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
    let (f, refreshes, start) = (QuotaFixture::new(), AtomicUsize::new(0), now());
    let refresh = || {
        refreshes.fetch_add(1, Ordering::SeqCst);
        known(4000, start + 3600)
    };
    let s = f.store();
    s.refresh_if_due(&f.policy, ACCOUNT, start + 1, &refresh);
    s.refresh_if_due(&f.policy, ACCOUNT, start, &refresh);
    assert_eq!(refreshes.load(Ordering::SeqCst), 1, "older sample waits");
    s.refresh_if_due(&f.policy, ACCOUNT, start + 60, &refresh);
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "the claim stayed at start + 1"
    );
    s.refresh_if_due(&f.policy, ACCOUNT, start + 61, &refresh);
    assert_eq!(refreshes.load(Ordering::SeqCst), 2, "interval elapsed");
}

/// Criteria 3 and 9: concurrent writers never tear the record, and it counts
/// every call by caller class.
#[test]
fn concurrent_writers_leave_a_whole_record_counting_every_class() {
    let (f, now) = (QuotaFixture::new(), now());
    f.seed(ACCOUNT, now, 4000, now + 3600);
    let classes = [Delivery, Background, Agent];
    let workers: Vec<_> = (0..9)
        .map(|i| {
            let (dir, policy, caller) = (f.path().to_path_buf(), f.policy, classes[i % 3]);
            std::thread::spawn(move || {
                for _ in 0..30 {
                    store(&dir)
                        .admit(
                            &policy,
                            ACCOUNT,
                            caller,
                            RESOURCE_CORE,
                            now,
                            &unknown_not_due,
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    workers.into_iter().for_each(|w| w.join().unwrap());

    let text = std::fs::read_to_string(f.record_path(ACCOUNT)).unwrap();
    serde_json::from_str::<serde_json::Value>(&text).expect("a whole record");
    let core = f.core(ACCOUNT, now);
    assert_eq!(
        (
            core.calls.delivery,
            core.calls.background,
            core.calls.agent,
            core.remaining
        ),
        (90, 90, 90, Some(4000 - 270))
    );
}

/// Criterion 4: a reserve of 0 disables that class's refusal.
#[test]
fn zero_reserve_disables_the_class_refusal() {
    let off = GithubQuotaPolicy {
        agent_reserve_percent: 0,
        background_reserve_percent: 0,
        ..GithubQuotaPolicy::default()
    };
    let (f, now) = (
        QuotaFixture {
            records: TempDir::new().unwrap(),
            policy: off,
        },
        now(),
    );
    f.seed(ACCOUNT, now, 5, now + 600);
    let admit = |policy: &GithubQuotaPolicy, caller| {
        f.store().admit(
            policy,
            ACCOUNT,
            caller,
            RESOURCE_CORE,
            now,
            &unknown_not_due,
        )
    };
    assert!(admit(&off, Agent).is_ok());
    assert!(admit(&off, Background).is_ok());
    let defaults = GithubQuotaPolicy::default();
    assert_eq!(
        admit(&defaults, Agent).unwrap_err().reason,
        QuotaRefusalReason::Reserve
    );
    assert!(admit(&defaults, Background).is_err());
    assert!(admit(&defaults, Delivery).is_ok());
}

/// Criterion 5: a refused, unauthenticated or unreadable rate-limit answer
/// leaves reads allowed and reports the quota unknown, never zero.
#[test]
fn unusable_rate_limit_answers_report_unknown_and_admit_reads() {
    let unauth = json!({"resources": {
        "core": {"limit": 60, "remaining": 0, "reset": now() + 600},
        "graphql": {"limit": 0, "remaining": 0, "reset": now() + 600},
    }})
    .to_string();
    for answer in ["refuse", unauth.as_str(), "not json"] {
        let (f, gh) = (QuotaFixture::new(), FakeGh::new(answer, "ok"));
        let (req, acct) = gh.view_req(None);
        assert!(f.run_gh(Agent, &req).unwrap().success, "{answer}");
        assert_eq!(
            gh.calls(),
            vec!["api rate_limit", "run view 123"],
            "{answer}"
        );
        let status = f.status(&acct, now());
        assert!(status.refresh_error.is_some(), "{answer}");
        for r in &status.resources {
            assert_eq!(
                (r.state, r.remaining, r.agent_reads_refused),
                ("unknown", None, false),
                "{answer}"
            );
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
        let (f, start, reset) = (QuotaFixture::new(), now(), now() + 1800);
        let refresh_at = if seeded {
            f.seed(ACCOUNT, start, 1001, reset);
            start + f.policy.refresh_interval.as_secs()
        } else {
            start
        };

        let in_flight = f.spawn_refresh(refresh_at, 1001, reset);
        let delivery = f.admit(ACCOUNT, Delivery, refresh_at, &|| {
            unreachable!("refresh claim is already held")
        });
        finish_refresh(in_flight);
        delivery.expect("delivery read must be admitted while near-reserve");

        let core = f.core(ACCOUNT, refresh_at);
        assert_eq!(
            (
                core.remaining,
                core.calls.delivery,
                core.agent_reads_refused
            ),
            (Some(1000), 1, true),
            "seeded={seeded}"
        );

        let refusal = f
            .admit(ACCOUNT, Agent, refresh_at, &unknown_not_due)
            .expect_err("refused at reserve");
        assert_eq!(
            (
                refusal.reason,
                refusal.remaining,
                refusal.limit,
                refusal.reserve
            ),
            (QuotaRefusalReason::Reserve, Some(1000), Some(5000), 1000)
        );
    }
}

/// Regression [ORB-25934]: a slow `api rate_limit` probe must be bounded by the
/// caller's `request.timeout_ms` instead of hard-coding 15 s, and an expired
/// budget must return timed-out without spawning the original `gh` request.
#[test]
fn slow_rate_limit_probe_bounds_to_request_timeout_and_skips_expired_request() {
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("hang", "ok"));
    let (req, _) = gh.view_req(Some(60));
    let started = Instant::now();
    let (probes, res) = run_with_simulated_clipped_probe(60, || f.run_gh(Delivery, &req));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "probe exceeded 60 ms: {:?}",
        started.elapsed()
    );
    assert_eq!(probes, 1, "clipped rate-limit probe must be attempted");
    assert_timed_out_no_gh(&res.unwrap(), &gh, "expired budget after clipped probe");

    let gh_zero = FakeGh::new("hang", "ok");
    let mut zero = gh_zero.request(&["run", "view", "456"]);
    zero.timeout_ms = Some(0);
    assert_timed_out_no_gh(&f.run_gh(Delivery, &zero).unwrap(), &gh_zero, "zero budget");
}

/// Regressions [ORB-26104] and [ORB-26122]: a read (`run_gh_request` or
/// `admit_gh_request`) whose budget a slow probe spent before the child could
/// start is not charged to a known record, while a runnable read still counts
/// once.
#[test]
fn expired_admission_before_spawn_is_not_charged_to_a_known_record() {
    let cases = [
        (
            "request_timed_out_before_spawn_is_not_charged_to_a_known_record",
            true,
        ),
        (
            "admission_past_the_budget_is_not_charged_to_a_known_record",
            false,
        ),
    ];
    for (case, via_run) in cases {
        let call = |f: &QuotaFixture, req: &ExecRequest| {
            if via_run {
                f.run_gh(Delivery, req).map(|r| r.timed_out)
            } else {
                f.admit_gh(Delivery, req).map(|()| false)
            }
        };
        let (f, gh) = (QuotaFixture::new(), FakeGh::new("hang", "ok"));
        let (req, acct) = gh.view_req(Some(60));
        let seeded_at = now() - 10_000;
        f.seed(&acct, seeded_at, 4000, seeded_at + 20_000);
        assert_eq!(f.core(&acct, now()).remaining, Some(4000), "{case}");

        let (probes, res) = run_with_simulated_clipped_probe(60, || call(&f, &req));
        assert_eq!(res.unwrap(), via_run, "{case}");
        assert_eq!(probes, 1, "{case}");
        assert!(gh.calls().is_empty(), "{case}: {:?}", gh.calls());
        let after = f.core(&acct, now());
        let c = &after.calls;
        assert_eq!(
            (after.remaining, c.delivery, c.background, c.agent),
            (Some(4000), 0, 0, 0),
            "{case}"
        );

        let (runnable_f, ran) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
        let (runnable, runnable_acct) = ran.view_req(Some(5_000));
        runnable_f.seed(&runnable_acct, now(), 4000, now() + 1800);
        call(&runnable_f, &runnable).unwrap();
        let counted = runnable_f.core(&runnable_acct, now());
        assert_eq!(
            (counted.calls.delivery, counted.remaining),
            (1, Some(3999)),
            "{case}"
        );
    }
}

/// Regression [ORB-26133]: a healthy known record whose read's deadline passes
/// while the record's refusal is being evaluated returns the pre-spawn timeout,
/// spawns nothing and leaves the call counts and remaining quota untouched. The
/// same healthy read with budget left charges exactly once.
#[test]
fn deadline_crossed_during_refusal_evaluation_is_not_charged_to_a_healthy_record() {
    let (ran_f, ran_gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (ran, ran_acct) = ran_gh.view_req(Some(60_000));
    ran_f.seed(&ran_acct, now(), 4000, now() + 1800);
    ran_f.run_gh(Delivery, &ran).unwrap();
    let counted = ran_f.core(&ran_acct, now());
    assert_eq!((counted.calls.delivery, counted.remaining), (1, Some(3999)));

    // Armed last: the skewed clock stays on this thread afterwards.
    let (exp_f, exp_gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (expired, acct) = exp_gh.view_req(Some(60_000));
    exp_f.seed(&acct, now(), 4000, now() + 1800);
    github_quota::advance_deadline_clock_at_next_refusal(Duration::from_secs(120));
    assert_timed_out_no_gh(
        &exp_f.run_gh(Delivery, &expired).unwrap(),
        &exp_gh,
        "crossed during refusal evaluation",
    );
    let after = exp_f.core(&acct, now());
    assert_eq!((after.remaining, after.calls.delivery), (Some(4000), 0));
}

/// Regression [ORB-26103]: a caller that spawns the child itself through
/// `admit_gh_request_within_budget` gets only the budget left after admission,
/// and no request to spawn once admission spent all of it. A record lock held
/// 300 ms of a 5 s budget leaves under 4.8 s; an unresponsive probe under a
/// 60 ms budget leaves nothing, and the unguarded zero budget never starts.
#[test]
fn admission_returns_only_the_budget_left_and_never_a_request_past_it() {
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (req, acct) = gh.view_req(Some(5_000));
    let holder =
        ContendedRecordLock::spawn(f.record_path(&acct), None, Some(Duration::from_millis(300)))
            .0
            .holder
            .take()
            .unwrap();
    let admitted = f
        .admit_gh_budget(Delivery, &req)
        .expect("lock frees inside budget");
    holder.join().unwrap();
    let left = admitted.timeout_ms.expect("finite budget stays finite");
    assert!(
        (1..=4_800).contains(&left),
        "300 ms contention must come off 5 s budget, left {left} ms"
    );

    let (slow_f, gh) = (QuotaFixture::new(), FakeGh::new("hang", "ok"));
    let (slow, _) = gh.view_req(Some(60));
    let (probes, admitted) =
        run_with_simulated_clipped_probe(60, || slow_f.admit_gh_budget(Delivery, &slow));
    assert_timeout(admitted, Some(60), "budget spent by probe");
    assert_eq!(probes, 1);
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

    let mut zero = gh.request(&["run", "view", "456"]);
    zero.timeout_ms = Some(0);
    let _ = std::fs::remove_file(gh.dir.path().join("calls"));
    assert_timeout(
        github_quota::admit_gh_request_within_budget(
            &orbit_common::Env::capture(),
            Delivery,
            &zero,
        ),
        None,
        "zero budget",
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
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (req, acct) = gh.view_req(Some(40));
    let lock = ContendedRecordLock::hold(f.record_path(&acct));
    let started = Instant::now();
    let res = f.run_gh(Delivery, &req);
    let elapsed = started.elapsed();
    lock.release();
    assert!(
        elapsed < Duration::from_secs(5),
        "lock wait must stop at 40 ms, took {elapsed:?}"
    );
    assert_timed_out_no_gh(&res.unwrap(), &gh, "contended record lock");
}

/// Regression [ORB-26105]: a finite request whose admission spends its budget
/// reports the timeout, not the typed throttle, when the quota record is
/// readable and holds a live hold. A peer holds the record lock past the
/// request's deadline, so admission reads the held record without the lock. A
/// refusal decided with budget left keeps its typed throttle, and the
/// child-spawning admission with the same spent budget returns its timeout.
#[test]
fn expired_admission_over_a_readable_hold_returns_the_timeout() {
    let now = now();
    let (f, gh, within, acct) = held_gh(5_000, now, now + 1800);
    assert_core_throttle(&f.run_gh(Delivery, &within).unwrap_err(), "within budget");
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

    // Budget spent on a peer-held record lock: readable hold stays, yet both entry points time out.
    let lock = ContendedRecordLock::hold(f.record_path(&acct));
    let (mut expired, mut expired_spawn) = (
        gh.request(&["run", "view", "456"]),
        gh.request(&["run", "view", "789"]),
    );
    (expired.timeout_ms, expired_spawn.timeout_ms) = (Some(40), Some(40));
    let (run, admitted) = f.scoped(|| {
        (
            github_quota::run_gh_request(&orbit_common::Env::capture(), Delivery, &expired),
            github_quota::admit_gh_request_within_budget(
                &orbit_common::Env::capture(),
                Delivery,
                &expired_spawn,
            ),
        )
    });
    lock.release();
    assert_timed_out_no_gh(&run.expect("timeout"), &gh, "expired run_gh");
    assert_timeout(admitted, Some(40), "admit_gh_within_budget over hold");
}

/// Regression [ORB-26129]: a finite read whose budget runs out while it probes
/// a writable record with a live core hold returns its timeout, spawns no
/// protected call and leaves the core refusal count unchanged. The same hold
/// refusing a read with budget left still returns the typed throttle and
/// counts that refusal once.
#[test]
fn expired_refusal_over_a_writable_hold_leaves_the_refusal_count() {
    let (now, reset) = (now(), now() + 1800);
    let (within_f, within_gh, within, within_acct) = held_gh(5_000, now, reset);
    assert_core_throttle(&within_f.run_gh(Delivery, &within).unwrap_err(), "within");
    assert!(within_gh.calls().is_empty(), "{:?}", within_gh.calls());
    assert_eq!(within_f.core(&within_acct, now).refused.delivery, 1);

    // Last claimed ten minutes ago, so the refresh is due and its probe runs inside 60 ms.
    let (exp_f, exp_gh) = (QuotaFixture::new(), FakeGh::new("hang", "ok"));
    let mut expired = exp_gh.request(&["run", "view", "456"]);
    expired.timeout_ms = Some(60);
    let exp_acct = github_quota::request_account_key(&orbit_common::Env::capture(), &expired);
    exp_f.seed(&exp_acct, now - 600, 4000, reset);
    exp_f.record_hold(&exp_acct, now, reset);
    let (probes, run) = run_with_simulated_clipped_probe(60, || exp_f.run_gh(Delivery, &expired));
    assert_timed_out_no_gh(&run.expect("timeout"), &exp_gh, "writable hold");
    assert_eq!(probes, 1);
    assert_eq!(exp_f.core(&exp_acct, now).refused.delivery, 0, "uncounted");
}

/// Regressions [ORB-26134] and [ORB-26140]: a refusal decided while budget
/// remains is counted once and returned as the typed throttle; once the budget
/// expires after the decision, it is left uncounted and surfaces as the typed
/// timeout without spawning `gh`.
#[test]
fn refusal_decided_when_budget_expires_is_not_counted_and_returns_timeout() {
    #[derive(Clone, Copy)]
    enum Entry {
        RunGh,
        AdmitWithinBudget,
        Admit,
    }

    let (now, reset) = (now(), now() + 1800);
    let cases = [
        (
            "refusal_decided_before_the_budget_ran_out_is_not_counted_when_it_becomes_a_timeout/run_gh_request",
            Delivery,
            Entry::RunGh,
        ),
        (
            "refusal_decided_before_the_budget_ran_out_is_not_counted_when_it_becomes_a_timeout/admit_gh_request_within_budget",
            Delivery,
            Entry::AdmitWithinBudget,
        ),
        (
            "admit_gh_request_outcome_agrees_with_whether_the_refusal_was_counted",
            Background,
            Entry::Admit,
        ),
    ];

    // Budget left first (unskewed), then armed last with post-decision clock skew.
    for arm_skew in [false, true] {
        for (case, caller, entry) in cases {
            let (f, gh, req, acct) = held_gh(60_000, now, reset);
            if arm_skew {
                github_quota::advance_deadline_clock_after_next_refusal_decision(
                    Duration::from_secs(120),
                );
            }
            let res = match entry {
                Entry::RunGh => f.run_gh(caller, &req).map(Some),
                Entry::AdmitWithinBudget => f.admit_gh_budget(caller, &req).map(|_| None),
                Entry::Admit => f.admit_gh(caller, &req).map(|()| None),
            };
            match (arm_skew, res) {
                (false, Err(err)) => assert_core_throttle(&err, case),
                (false, Ok(_)) => panic!("{case}: live hold must refuse within budget"),
                (true, Ok(Some(run))) => assert_timed_out_no_gh(&run, &gh, case),
                (true, other) => {
                    assert_timeout(other, None, case);
                    assert!(gh.calls().is_empty(), "{case}: {:?}", gh.calls());
                }
            }
            let r = f.core(&acct, now).refused;
            let got = match caller {
                Delivery => r.delivery,
                Background => r.background,
                Agent => r.agent,
            };
            assert_eq!(got, u64::from(!arm_skew), "{case}");
        }
    }
}

/// Regression [ORB-26101]: the accounting a caller runs after its own child
/// returns belongs to the original request deadline, not a fresh budget. A
/// primary throttle reported with 50 ms of a 5 s budget left must give up on a
/// peer-held record lock at that instant, not wait another 5 s; with the
/// record available the same near-expiry answer still records the hold.
#[test]
fn post_child_throttle_accounting_waits_only_for_the_remaining_budget() {
    let (f, gh) = (QuotaFixture::new(), FakeGh::new("{}", "ok"));
    let (req, acct) = gh.view_req(Some(5_050));
    let (now, reset) = (now(), now() + 1800);
    f.seed(&acct, now, 4000, reset);
    let throttled = ExecutionResult {
        success: false,
        timed_out: false,
        stdout: String::new(),
        stderr: "gh: API rate limit exceeded for user ID 42. (HTTP 403)".to_string(),
        exit_code: Some(1),
        duration_ms: 5_000,
        output: None,
    };
    // The request started 5 s ago: 50 ms of its budget remain.
    let started_5s_ago = Instant::now()
        .checked_sub(Duration::from_secs(5))
        .expect("clock far from origin");
    let deadline = github_quota::gh_request_deadline(&req, started_5s_ago);

    let lock = ContendedRecordLock::hold(f.record_path(&acct));
    let started = Instant::now();
    f.scoped(|| {
        github_quota::observe_gh_result_by_deadline(
            &orbit_common::Env::capture(),
            &req,
            &throttled,
            deadline,
        )
    });
    let elapsed = started.elapsed();
    lock.release();
    assert!(
        elapsed < Duration::from_secs(2),
        "wait must stop at original deadline, took {elapsed:?}"
    );

    // An available record takes the hold even with the budget nearly spent.
    let near = Instant::now() + Duration::from_millis(500);
    f.scoped(|| {
        github_quota::observe_gh_result_by_deadline(
            &orbit_common::Env::capture(),
            &req,
            &throttled,
            Some(near),
        )
    });
    let held = f
        .admit(&acct, Delivery, now + 1, &unknown_not_due)
        .expect_err("must hold core");
    assert_eq!(
        (held.reason, held.resume_epoch_seconds),
        (QuotaRefusalReason::Exhausted, reset)
    );
}

/// Regression [ORB-25964]: a probe cut short by one caller's remaining deadline
/// says nothing about the account, so it must neither claim the refresh
/// interval nor replace a known record with an unknown one. A second caller
/// still meets the agent reserve and the interval stays open for a real read.
#[test]
fn deadline_clipped_probe_leaves_known_quota_and_reserve_refusal_in_place() {
    let (f, gh, now) = (QuotaFixture::new(), FakeGh::new("hang", "ok"), now());
    let (req, acct) = gh.view_req(None);
    // 1,000 of 5,000 left is exactly the 20% agent reserve, due for refresh.
    f.seed(&acct, now - 120, 1000, now + 1800);

    let mut short = req.clone();
    short.timeout_ms = Some(60);
    let (probes, res) = run_with_simulated_clipped_probe(60, || f.run_gh(Delivery, &short));
    assert!(res.unwrap().timed_out);
    assert_eq!(probes, 1, "first clipped probe must be attempted");
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());

    let status = f.status(&acct, now);
    let core = &status.resources[0];
    assert_eq!(
        (
            core.state,
            status.refresh_error.as_deref(),
            core.agent_reads_refused
        ),
        ("known", None, true),
        "{status:?}"
    );

    // The interval was not claimed: the second caller reads the endpoint itself and hits reserve.
    let mut agent = req;
    agent.timeout_ms = Some(200);
    let (probes, admitted) = run_with_simulated_clipped_probe(200, || f.admit_gh(Agent, &agent));
    assert_timeout(admitted, None, "agent reserve after clipped probe");
    assert_eq!(
        probes, 1,
        "clipped probe must leave interval due for next caller"
    );
    assert!(gh.calls().is_empty(), "{:?}", gh.calls());
}

/// Regression [ORB-26220]: a clipped probe inside `with_simulated_clipped_probe`
/// must not leave this thread's admission clock advanced once it returns. A
/// 60 ms read that follows on the same thread still runs its `gh api
/// rate_limit` probe instead of timing out before spawn.
///
/// Regression [ORB-26468]: the 60 ms reads run inside one enclosing scope, not
/// in scopes of their own. A scope installs a fresh fixed clock, which would
/// replace a clock the clipped probe left advanced and hide the leak. The
/// shared read takes its deadline from the real clock, so a leaked advance
/// puts that deadline in the past: its probe is skipped, and the enclosing
/// counter sees it.
#[test]
fn clipped_probe_does_not_leak_its_deadline_into_a_later_short_read() {
    const LONG_MS: u64 = 5_000;
    let (f, gh, now) = (QuotaFixture::new(), FakeGh::new("hang", "ok"), now());
    let ((long, acct), (short, _)) = (gh.view_req(Some(LONG_MS)), gh.view_req(Some(60)));
    f.seed(&acct, now - 120, 4000, now + 1800);

    let enclosing = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let counted = std::rc::Rc::clone(&enclosing);
    github_quota::with_simulated_clipped_probe(
        move |_| counted.set(counted.get() + 1),
        || {
            let (probes, res) =
                run_with_simulated_clipped_probe(LONG_MS, || f.run_gh(Delivery, &long));
            assert!(res.unwrap().timed_out);
            assert_eq!(probes, 1);

            // The long probe's scope returned, so the enclosing real-clock deadline must run `short`'s probe.
            let reset = f.scoped(|| {
                github_quota::shared_exhausted_reset(
                    &orbit_common::Env::capture(),
                    &short,
                    &[RESOURCE_CORE],
                )
            });
            assert_eq!(reset, github_quota::SharedReset::Guarded(None), "{reset:?}");
            assert_eq!(
                enclosing.get(),
                1,
                "clipped probe must restore enclosing clock for shared read"
            );

            assert!(f.run_gh(Delivery, &short).unwrap().timed_out);
            assert_eq!(
                enclosing.get(),
                2,
                "60 ms read after clipped probe must still run its probe"
            );
        },
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
    let (reset, gh) = (now() + 1800, FakeGh::new("{}", "empty"));
    let (probe, acct) = gh.view_req(None);

    let read = |remaining: u64, input: serde_json::Value| {
        let f = QuotaFixture::new();
        f.seed(&acct, now(), remaining, reset);
        let mut requests = RunLogRequests::from_input(&input).unwrap();
        for req in [&mut requests.run_log, &mut requests.run_view] {
            (req.program, req.environment_mode) =
                (probe.program.clone(), probe.environment_mode.clone());
        }
        let _ = std::fs::remove_file(gh.dir.path().join("calls"));
        let res = f.scoped(|| {
            read_run_log_guarded(
                &orbit_common::Env::capture(),
                &requests,
                LogReadBounds::new(4096),
                None,
                &|req| {
                    github_quota::admit_gh_request(&orbit_common::Env::capture(), Agent, req)?;
                    Ok(req.clone())
                },
            )
        });
        (res, gh.calls())
    };

    // 1,001 of 5,000 left: one call above the 1,000 reserve.
    let (unscoped, calls) = read(1001, json!({"run": "123", "repo": "acme/orbit"}));
    let throttle = unscoped
        .expect("unscoped")
        .throttle_refusal()
        .expect("typed");
    assert_core_throttle(&throttle, "unscoped fallback");
    assert_eq!(refused_throttle(&throttle).reset_epoch_seconds, Some(reset));
    assert_eq!(calls.len(), 1, "refused fallback must not run: {calls:?}");

    let scoped_in = json!({"run": "123", "repo": "acme/orbit", "step": "Build"});
    let (scoped, calls) = read(1001, scoped_in.clone());
    let err = scoped.err().expect("scoped read has no selection");
    assert_core_throttle(&err, "scoped fallback");
    assert!(err.to_string().contains("do not retry before"), "{err}");
    assert_eq!(calls.len(), 1, "refused fallback must not run: {calls:?}");

    // With quota to spare the fallback runs and its ordinary failure is kept.
    let (scoped, calls) = read(4000, scoped_in);
    let err = scoped.err().expect("selection is unavailable");
    assert!(err.private_vcs_failure().is_none(), "{err}");
    assert!(err.to_string().contains("log step not found"), "{err}");
    assert_eq!(calls.len(), 2, "{calls:?}");
}

/// Regression [ORB-26095]: an unreadable or partially malformed existing quota
/// record must be preserved byte-for-byte across guard calls while admission
/// stays fail-open and status surfaces the read/parse failure as unknown; a
/// genuinely missing account file still initializes and counts normally.
#[test]
fn partially_malformed_record_survives_admit_and_reports_unknown_in_status() {
    let (f, now) = (QuotaFixture::new(), now());
    let malformed: &[u8] =
        br#"{"resources":{"core":{"calls":{"delivery":42,"agent":7},"remaining":"corrupt"}}}"#;
    let binary: &[u8] = b"{\"calls\":42}\xff\xfe";

    for (acct, bytes, terms) in [
        (ACCOUNT, malformed, &["unreadable", "malformed"][..]),
        ("github.com-binary", binary, &["unreadable"][..]),
    ] {
        let path = f.record_path(acct);
        std::fs::write(&path, bytes).unwrap();
        let refreshes = AtomicUsize::new(0);
        f.admit(acct, Agent, now, &|| {
            refreshes.fetch_add(1, Ordering::SeqCst);
            known(4000, now + 1800)
        })
        .expect("admission stays fail-open");
        assert_eq!(refreshes.load(Ordering::SeqCst), 0, "{acct}: no probe");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{acct}: unchanged");

        let status = f.status(acct, now);
        let err = status.refresh_error.as_deref().expect("refresh_error");
        for term in terms {
            assert!(err.contains(term), "{acct}: {err}");
        }
        for r in &status.resources {
            assert_eq!(
                (r.state, r.remaining, r.agent_reads_refused),
                ("unknown", None, false)
            );
        }
    }

    let missing = "github.com-missing";
    assert_eq!(f.status(missing, now).refresh_error, None);
    f.admit(missing, Agent, now, &|| known(4000, now + 1800))
        .unwrap();
    assert_eq!(f.status(missing, now).refresh_error, None);
    let c = f.core(missing, now);
    assert_eq!(
        (c.state, c.calls.agent, c.remaining),
        ("known", 1, Some(3999))
    );
}

fn run_contended_clipped_probe(dir: &Path, policy: GithubQuotaPolicy, acct: &str, now: u64) {
    let record_path = dir.join(format!("{acct}.json"));
    let ((probe_started_tx, probe_started_rx), (lock_held_tx, lock_held_rx)) =
        (mpsc::channel(), mpsc::channel());
    let (dir_buf, acct_owned, started) = (dir.to_path_buf(), acct.to_string(), Instant::now());
    let refresher = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_millis(80);
        store(&dir_buf).bounded_by(Some(deadline)).refresh_if_due(
            &policy,
            &acct_owned,
            now,
            &|| {
                probe_started_tx.send(()).unwrap();
                recv_ready(&lock_held_rx, "holder never acquired lock");
                std::thread::sleep(Duration::from_millis(100));
                RateLimitAnswer::Clipped("rate-limit read cut short".into())
            },
        );
    });

    recv_ready(&probe_started_rx, "probe never started");
    let lock = hold_lock(&record_path, "contending finalization lock");
    lock_held_tx.send(()).unwrap();
    refresher.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "exceeded deadline"
    );
    drop(lock);
}

fn assert_contended_clipped_recovery(f: &QuotaFixture, acct: &str, now: u64, reset: u64) {
    f.seed(acct, now - 120, 1000, reset);
    run_contended_clipped_probe(f.path(), f.policy, acct, now);

    let before = f.status(acct, now);
    let c = before
        .resources
        .iter()
        .find(|r| r.resource == RESOURCE_CORE)
        .unwrap();
    let got = (
        c.state,
        c.remaining,
        before.refresh_error.as_deref(),
        c.agent_reads_refused,
    );
    assert_eq!(got, ("known", Some(1000), None, true));

    // Immediately after lock release (at the same `now`), the next caller refreshes.
    let refreshes = AtomicUsize::new(0);
    f.admit(acct, Agent, now, &|| {
        refreshes.fetch_add(1, Ordering::SeqCst);
        known(3500, reset)
    })
    .expect("fresh observation above reserve must admit agent read");
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "must refresh immediately"
    );
    assert_eq!(f.core(acct, now).remaining, Some(3499));
}

fn seed_contended_clipped_marker(bytes: &[u8], now: u64, reset: u64) -> (QuotaFixture, PathBuf) {
    let f = QuotaFixture::new();
    f.seed(ACCOUNT, now - 120, 3500, reset);
    run_contended_clipped_probe(f.path(), f.policy, ACCOUNT, now);
    let marker = f.marker(ACCOUNT, 2);
    std::fs::write(&marker, bytes).unwrap();
    (f, marker)
}

/// Regression [ORB-26102]: when a caller's probe is clipped while a peer holds
/// the record lock at finalization, the caller must stay bounded by its
/// deadline, preserve the prior valid observation, and still let the next
/// caller refresh immediately once the lock is released.
#[test]
fn clipped_probe_with_contended_finalization_lock_releases_claim_and_preserves_record() {
    let (f, now) = (QuotaFixture::new(), now());
    assert_contended_clipped_recovery(&f, ACCOUNT, now, now + 1800);
}

/// Regressions [ORB-26115] and [ORB-26135]: malformed, missing-`previous_claim`,
/// and positional clipped-release markers at the active generation are
/// diagnostic evidence only. None releases the fresh active claim: bytes stay
/// on disk, the healthy account admits and counts its read, and no same-instant
/// refresh probes.
#[test]
fn preserved_clipped_release_markers_keep_fresh_claim_and_healthy_admission() {
    let cases: [(&str, &[u8]); 3] = [
        (
            "malformed_clipped_release_is_preserved_while_healthy_admission_proceeds",
            b"{\"generation\":",
        ),
        (
            "clipped_release_missing_previous_claim_keeps_fresh_claim_and_marker",
            b"{\"generation\":2}",
        ),
        (
            "positional_clipped_release_keeps_fresh_claim_and_marker",
            b"[2,null]",
        ),
    ];
    for (case, marker_bytes) in cases {
        let (now, reset) = (now(), now() + 1800);
        let (f, marker) = seed_contended_clipped_marker(marker_bytes, now, reset);
        let probes = AtomicUsize::new(0);
        f.admit(ACCOUNT, Agent, now, &|| {
            probes.fetch_add(1, Ordering::SeqCst);
            known(3500, reset)
        })
        .unwrap_or_else(|err| panic!("{case}: healthy account should admit: {err:?}"));
        assert_eq!(probes.load(Ordering::SeqCst), 0, "{case}: keep claim");
        assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes, "{case}");
        let core = f.core(ACCOUNT, now);
        assert_eq!(
            (core.remaining, core.calls.agent),
            (Some(3499), 1),
            "{case}"
        );
    }
}

/// Regression [ORB-26137]: ambiguous clipped-release bytes at the active
/// generation (malformed, or a payload naming another generation) are
/// diagnostic evidence. The due claim that advances the generation, and the
/// ordinary update after it, must leave those bytes on disk while accounting
/// and refresh proceed normally.
#[test]
fn ambiguous_clipped_release_survives_claim_advance_and_later_updates() {
    for bytes in AMBIGUOUS_RELEASE_PAYLOADS {
        let (now, reset) = (now(), now() + 1800);
        let (f, marker) = seed_contended_clipped_marker(bytes, now, reset);

        let later = now + f.policy.refresh_interval.as_secs();
        let probes = AtomicUsize::new(0);
        f.store().refresh_if_due(&f.policy, ACCOUNT, later, &|| {
            probes.fetch_add(1, Ordering::SeqCst);
            known(3400, reset)
        });
        assert_eq!(probes.load(Ordering::SeqCst), 1, "due claim refreshes");
        assert_eq!(std::fs::read(&marker).unwrap(), bytes, "survives advance");

        f.admit(ACCOUNT, Agent, later, &|| unreachable!("gen-3 fresh"))
            .expect("admit");
        assert_eq!(std::fs::read(&marker).unwrap(), bytes, "survives update");
        let core = f.core(ACCOUNT, later);
        assert_eq!((core.remaining, core.calls.agent), (Some(3399), 1));
    }
}

/// Regression [ORB-26137]: a clipped-release marker at the previous generation
/// is removed only once its payload proves it is a well-formed release for that
/// generation. An ordinary update after the record advanced must leave
/// ambiguous bytes at that path in place.
#[test]
fn ambiguous_previous_generation_marker_survives_ordinary_update() {
    for bytes in AMBIGUOUS_RELEASE_PAYLOADS {
        let (f, now, reset) = (QuotaFixture::new(), now(), now() + 1800);
        for at in [now - 120, now - 60, now] {
            f.seed(ACCOUNT, at, 3500, reset);
        }
        let marker = f.marker(ACCOUNT, 2);
        std::fs::write(&marker, bytes).unwrap();
        f.admit(ACCOUNT, Agent, now, &|| unreachable!("gen-3 fresh"))
            .expect("admit");
        assert_eq!(std::fs::read(&marker).unwrap(), bytes, "prev-gen survives");
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

    let (f, now, reset) = (QuotaFixture::new(), now(), now() + 1800);
    for index in 0..8 {
        std::fs::write(f.path().join(format!("unrelated-{index}.json")), "{}").unwrap();
    }
    let _restore = RestorePermissions(f.path());
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
    assert!(std::fs::read_dir(f.path()).is_err(), "requires 0o300 dir");

    assert_contended_clipped_recovery(&f, ACCOUNT, now, reset);
    assert!(!f.marker(ACCOUNT, 2).exists(), "reconciled marker removed");

    // Stale gen-3 marker written while gen 4 is in flight leaves gen 4 and debits intact.
    let t1 = now + 60;
    let in_flight = f.spawn_refresh(t1, 3500, reset + 1800);
    let stale = f.marker(ACCOUNT, 3);
    std::fs::write(&stale, r#"{"generation":3,"previous_claim":null}"#).unwrap();

    f.admit(ACCOUNT, Delivery, t1, &|| unreachable!("gen 4 in flight"))
        .unwrap();
    assert!(
        !stale.exists(),
        "must remove superseded marker without listing"
    );
    finish_refresh(in_flight);

    // Stale gen-3 marker staged after gen 4 completed must also be removed.
    std::fs::write(&stale, r#"{"generation":3,"previous_claim":null}"#).unwrap();
    let refreshes = AtomicUsize::new(0);
    f.store().refresh_if_due(&f.policy, ACCOUNT, t1, &|| {
        refreshes.fetch_add(1, Ordering::SeqCst);
        known(999, reset + 1800)
    });
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert!(!stale.exists(), "must remove stale marker without listing");
    assert_eq!(f.core(ACCOUNT, t1).remaining, Some(3499));
}

/// Regression [ORB-26102]: a delayed clipped-claim release from an older
/// refresh generation must never roll back or clear an active or completed
/// newer refresh generation.
#[test]
fn stale_clipped_release_does_not_clobber_newer_refresh_generation() {
    for complete_newer in [true, false] {
        let (f, start) = (QuotaFixture::new(), now());
        let (t1, t2, reset) = (start + 60, start + 120, start + 1920);
        f.seed(ACCOUNT, start, 1000, reset);

        let ((a_started_tx, a_started_rx), (a_proceed_tx, a_proceed_rx)) =
            (mpsc::channel(), mpsc::channel());
        let (dir_a, policy) = (f.path().to_path_buf(), f.policy);
        let caller_a = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(80);
            store(&dir_a)
                .bounded_by(Some(deadline))
                .refresh_if_due(&policy, ACCOUNT, t1, &|| {
                    a_started_tx.send(()).unwrap();
                    recv_ready(&a_proceed_rx, "caller A never signaled");
                    std::thread::sleep(Duration::from_millis(100));
                    RateLimitAnswer::Clipped("stale clipped probe".into())
                });
        });

        recv_ready(&a_started_rx, "caller A never claimed generation");
        let caller_b = if complete_newer {
            f.seed(ACCOUNT, t2, 3500, reset);
            None
        } else {
            Some(f.spawn_refresh(t2, 3500, reset))
        };

        let lock = hold_lock(&f.record_path(ACCOUNT), "contending lock");
        a_proceed_tx.send(()).unwrap();
        caller_a.join().unwrap();
        drop(lock);

        if let Some(in_flight) = caller_b {
            f.admit(ACCOUNT, Delivery, t2, &|| unreachable!("gen 3 in flight"))
                .unwrap();
            finish_refresh(in_flight);
        }

        let refreshes = AtomicUsize::new(0);
        f.store().refresh_if_due(&f.policy, ACCOUNT, t2, &|| {
            refreshes.fetch_add(1, Ordering::SeqCst);
            known(999, reset)
        });
        let expected = if complete_newer { 3500 } else { 3499 };
        assert_eq!(
            refreshes.load(Ordering::SeqCst),
            0,
            "reopened ({complete_newer})"
        );
        assert_eq!(
            f.core(ACCOUNT, t2).remaining,
            Some(expected),
            "{complete_newer}"
        );
        assert!(
            !f.marker(ACCOUNT, 2).exists(),
            "stale marker ({complete_newer})"
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

    for contended in [true, false] {
        for reader in [Reader::Stream, Reader::Metadata, Reader::Fallback] {
            let (now, reset) = (now(), now() + 1800);
            let answer = match (contended, reader) {
                (true, Reader::Fallback) => "empty-then-slow-throttle",
                (true, _) => "slow-throttle",
                (false, Reader::Fallback) => "empty-then-throttle",
                (false, _) => "throttle",
            };
            let (f, cache_dir) = (QuotaFixture::new(), TempDir::new().unwrap());
            let gh = FakeGh::new("{}", answer);
            let (probe, acct) = gh.view_req(None);
            f.seed(&acct, now, 4000, reset);
            let input = if reader == Reader::Metadata {
                json!({"run": "123", "job": "9", "repo": "acme/orbit"})
            } else {
                json!({"run": "123", "repo": "acme/orbit"})
            };
            let mut requests = RunLogRequests::from_input(&input).unwrap();
            if reader == Reader::Metadata {
                requests = requests.with_test_log_cache(cache_dir.path());
            }
            for req in [&mut requests.run_log, &mut requests.run_view] {
                req.program = probe.program.clone();
                req.environment_mode = probe.environment_mode.clone();
                req.timeout_ms = Some(1_600);
            }

            let target_call = usize::from(reader == Reader::Fallback);
            let (call_index, target_started) = (AtomicUsize::new(0), std::sync::Mutex::new(None));
            let (admitted_tx, admitted_rx) = mpsc::channel();
            let lock_state = contended
                .then(|| ContendedRecordLock::spawn(f.record_path(&acct), Some(admitted_rx), None));

            let res = f.scoped(|| {
                read_run_log_guarded(
                    &orbit_common::Env::capture(),
                    &requests,
                    LogReadBounds::new(4096),
                    None,
                    &|req| {
                        let index = call_index.fetch_add(1, Ordering::SeqCst);
                        if contended && reader == Reader::Metadata && index > target_call {
                            return Err(OrbitError::Execution("stop after metadata read".into()));
                        }
                        let admitted = github_quota::admit_gh_request_within_budget(
                            &orbit_common::Env::capture(),
                            Delivery,
                            req,
                        )?;
                        if let Some((_, ref lock_held_rx)) = lock_state
                            && index == target_call
                        {
                            admitted_tx.send(()).unwrap();
                            recv_ready(lock_held_rx, "holder never acquired lock");
                            *target_started.lock().unwrap() = Some(Instant::now());
                        }
                        Ok(admitted)
                    },
                )
            });

            let elapsed = target_started.lock().unwrap().map(|t| t.elapsed());
            if let Some((lock, _)) = lock_state {
                lock.release();
            }

            if contended {
                assert!(
                    gh.dir.path().join("throttled").exists(),
                    "{reader:?}: not throttled"
                );
                assert!(
                    elapsed.expect("ran") < Duration::from_millis(2_200),
                    "{reader:?}: {elapsed:?}"
                );
            } else {
                if reader == Reader::Metadata {
                    let expected =
                        vec!["api --method GET repos/acme/orbit/actions/jobs/9".to_string()];
                    assert_eq!(gh.calls(), expected, "metadata throttle must hold fallback");
                }
                let held = f
                    .admit(&acct, Delivery, now + 1, &unknown_not_due)
                    .expect_err("held");
                assert_eq!(
                    (held.reason, held.resume_epoch_seconds),
                    (QuotaRefusalReason::Exhausted, reset),
                    "{reader:?}"
                );
            }
            match (reader, res) {
                (Reader::Stream, Err(err)) => {
                    assert!(refused_throttle(&err).principal.is_some(), "{err}")
                }
                (Reader::Metadata, Err(err)) if contended => assert!(
                    err.to_string().contains("stop after metadata read"),
                    "{err}"
                ),
                (Reader::Metadata, Err(err)) => {
                    assert_core_throttle(&err, "throttled metadata hold")
                }
                (Reader::Fallback, Ok(read)) => {
                    let t = read.throttle_refusal().expect("typed throttle");
                    if contended {
                        assert!(refused_throttle(&t).principal.is_some(), "{t}");
                    }
                }
                (r, Err(err)) => panic!("unexpected error for {r:?}: {err}"),
                (r, Ok(_)) => panic!("unexpected ok for {r:?}"),
            }
        }
    }
}

/// A `RunLogRequests` whose every `gh` call runs the fake `gh`, each budgeted at
/// `budget_ms`.
fn budgeted_run_requests(gh: &FakeGh, input: serde_json::Value, budget_ms: u64) -> RunLogRequests {
    let probe = gh.request(&["run", "view", "123"]);
    let mut requests = RunLogRequests::from_input(&input).unwrap();
    for req in [&mut requests.run_log, &mut requests.run_view] {
        (req.program, req.environment_mode) =
            (probe.program.clone(), probe.environment_mode.clone());
        req.timeout_ms = Some(budget_ms);
    }
    requests
}

/// Regression [ORB-29630]: a budgeted GitHub read whose `gh` ignores SIGTERM ends
/// at its budget, not after the termination grace period. The run-view fallback
/// (`fallback_jobs`) is the read under test.
#[test]
fn budgeted_run_view_fallback_ends_at_its_budget_when_gh_ignores_sigterm() {
    let gh = FakeGh::new("{}", "empty-then-ignore-term");
    let requests = budgeted_run_requests(&gh, json!({"run": "123", "repo": "acme/orbit"}), 1_000);
    let started = Instant::now();
    let read = read_run_log_guarded(
        &orbit_common::Env::capture(),
        &requests,
        LogReadBounds::new(4096),
        None,
        &|req| Ok(req.clone()),
    )
    .expect("an unrecovered fallback is reported in the read");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1_500),
        "1000 ms budget; the read took {elapsed:?}"
    );
    let reason = read.fallback_error.expect("the run view read timed out");
    assert!(
        reason.contains("gh run view") && reason.contains("timed out"),
        "{reason}"
    );
}

/// Regression [ORB-29630]: the same bound holds for the metadata read that decides
/// whether a run-scoped log may be cached (`read_api_json`).
#[test]
fn budgeted_run_metadata_read_ends_at_its_budget_when_gh_ignores_sigterm() {
    let gh = FakeGh::new("{}", "log-then-ignore-term");
    let cache = TempDir::new().unwrap();
    let requests = budgeted_run_requests(&gh, json!({"run": "123", "repo": "acme/orbit"}), 1_000)
        .with_test_log_cache(cache.path());
    let started = Instant::now();
    let read = read_run_log_guarded(
        &orbit_common::Env::capture(),
        &requests,
        LogReadBounds::new(4096),
        None,
        &|req| Ok(req.clone()),
    )
    .expect("the run-scoped log is read once its metadata read is abandoned");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(1_500),
        "1000 ms budget; the read took {elapsed:?}"
    );
    assert!(
        gh.calls()
            .iter()
            .any(|call| call.starts_with("api --method GET repos/acme/orbit/actions/runs/123")),
        "{:?}",
        gh.calls()
    );
    assert!(read.log.text.contains("log line"), "{:?}", read.log.text);
}
