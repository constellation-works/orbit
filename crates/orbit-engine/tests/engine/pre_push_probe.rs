//! The pre-push probe recovers from a timed-out read without retrying refusals.
//! Fake Git stalls by exec'ing sleep; the supervisor's deadline kills it. Each
//! body runs in a child so PATH changes cannot affect concurrent fixtures.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use orbit_common::OrbitError;
use orbit_engine::execute_deterministic_action;
use serde_json::{Value, json};

use super::git_fixture;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Host;
impl orbit_engine::test_support::NullRuntimeHost for Host {}

#[test]
fn a_timed_out_pre_push_probe_recovers_and_allows_publication() {
    git_fixture::isolated(
        module_path!(),
        "a_timed_out_pre_push_probe_recovers_and_allows_publication",
        || {
            let fixture = Fixture::new();
            for remote in ["", &format!("{HEAD}\trefs/heads/candidate\n")] {
                fixture.reset(1, remote, "", "");
                let output = fixture.push().expect("a timed-out read is safe to retry");
                assert_eq!(output["remote_probe_attempts"], 2, "{output}");
                assert!(output["remote_probe_waited_ms"].as_u64().unwrap() > 0);
                assert!(
                    output["remote_probe_elapsed_ms"].as_u64().unwrap()
                        >= output["remote_probe_waited_ms"].as_u64().unwrap()
                );
                if remote.is_empty() {
                    assert_eq!(output["decision"], "performed_create");
                    assert_eq!(fixture.calls(), ["probe", "probe", "push"]);
                    assert_eq!(
                        fs::read_to_string(fixture.root.path().join("published"))
                            .unwrap()
                            .trim(),
                        HEAD
                    );
                } else {
                    assert_eq!(output["decision"], "reused_current");
                    assert_eq!(fixture.calls(), ["probe", "probe"]);
                }
            }
        },
    );
}

#[test]
fn exhausted_pre_push_timeouts_report_the_stage_budget_and_contention_evidence() {
    git_fixture::isolated(
        module_path!(),
        "exhausted_pre_push_timeouts_report_the_stage_budget_and_contention_evidence",
        || {
            let fixture = Fixture::new();
            for diagnostic in ["", "fatal: Unable to create 'index.lock': File exists."] {
                fixture.reset(100, "", diagnostic, "");
                let error = fixture
                    .push()
                    .expect_err("the read retry budget must be finite");
                assert!(
                    error.is_timeout(),
                    "timeout classification is retained: {error}"
                );
                let message = error.to_string();
                assert!(message.contains("pre-push ls-remote probe"), "{message}");
                assert!(message.contains("3 attempts"), "{message}");
                let elapsed = message
                    .split("elapsed=")
                    .nth(1)
                    .unwrap()
                    .split("ms")
                    .next()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                assert!(
                    elapsed >= 30,
                    "elapsed covers attempts and waits: {message}"
                );
                assert!(message.contains("per-attempt timeout=30000ms"), "{message}");
                let expected_lock = if diagnostic.is_empty() {
                    "not observed"
                } else {
                    "observed"
                };
                assert!(
                    message.contains(&format!(
                        "local lock contention={expected_lock} in Git stderr"
                    )),
                    "{message}"
                );
                assert!(
                    message.contains("IO contention="),
                    "the diagnostic reports observed or unknown IO evidence: {message}"
                );
                assert_eq!(fixture.calls(), ["probe", "probe", "probe"]);
            }
        },
    );
}

#[test]
fn completed_probe_and_push_refusals_are_not_retried() {
    git_fixture::isolated(
        module_path!(),
        "completed_probe_and_push_refusals_are_not_retried",
        || {
            let fixture = Fixture::new();
            for refusal in [
                "fatal: Authentication failed",
                "fatal: connection timed out",
            ] {
                fixture.reset(0, "", refusal, "");
                let error = fixture
                    .push()
                    .expect_err("a completed probe refusal is not a supervisor timeout");
                assert!(!error.is_timeout(), "{error}");
                assert!(error.to_string().contains(refusal), "{error}");
                assert_eq!(fixture.calls(), ["probe"]);
            }
            for refusal in [
                "fatal: Authentication failed",
                "! [rejected] candidate -> candidate (non-fast-forward)",
            ] {
                fixture.reset(1, "", "", refusal);
                let error = fixture.push().expect_err(
                    "a permanent push refusal stays single-attempt after a recovered probe",
                );
                assert!(error.to_string().contains(refusal), "{error}");
                assert_eq!(fixture.calls(), ["probe", "probe", "push"]);
            }
        },
    );
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let git = bin.join("git");
        fs::write(&git, FAKE_GIT.replace("__HEAD__", HEAD)).unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        // The body owns its isolated child process.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        }
        Self { root }
    }

    fn reset(&self, timeouts: usize, remote: &str, probe_failure: &str, push_failure: &str) {
        for (name, contents) in [
            ("calls", "".to_string()),
            ("count", "0".to_string()),
            ("timeouts", timeouts.to_string()),
            ("remote", remote.to_string()),
            ("probe-failure", probe_failure.to_string()),
            ("push-failure", push_failure.to_string()),
        ] {
            fs::write(self.root.path().join(name), contents).unwrap();
        }
    }

    fn push(&self) -> Result<Value, OrbitError> {
        use orbit_common::test_env::{
            SupervisorSeam, with_supervision_clock, with_supervisor_seam,
        };
        use std::{
            cell::Cell,
            rc::Rc,
            time::{Duration, Instant},
        };
        let origin = Instant::now();
        let now = Rc::new(Cell::new(origin));
        let observed = Rc::clone(&now);
        let clock = Rc::clone(&now);
        let ready = self.root.path().join("probe-ready");
        let seam = SupervisorSeam {
            wait: Some(Box::new(move |child| {
                if ready.exists() {
                    fs::remove_file(&ready).expect("consume probe readiness");
                    now.set(now.get() + Duration::from_secs(31));
                    return Ok(None);
                }
                child.try_wait()
            })),
            ..SupervisorSeam::default()
        };
        let result = with_supervision_clock(
            move || clock.get(),
            || {
                with_supervisor_seam(seam, || {
                    orbit_engine::test_support::with_remote_probe_backoff(10, || {
                        execute_deterministic_action(
                            &Host,
                            "git_push",
                            &json!({}),
                            &json!({
                                "workspace_path": self.root.path(),
                                "branch": "candidate",
                            }),
                            false,
                            &HashMap::new(),
                            None,
                        )
                    })
                })
            },
        );
        let injected_attempts = fs::read_to_string(self.root.path().join("timeouts"))
            .unwrap()
            .parse::<u64>()
            .unwrap()
            .min(3);
        assert!(
            observed.get().duration_since(origin) >= Duration::from_secs(31 * injected_attempts),
            "each stalled ls-remote must reach its supervised deadline: {result:?}"
        );
        result
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.path().join("calls"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

const FAKE_GIT: &str = r#"#!/bin/sh
set -eu
while [ "$1" = -c ]; do shift 2; done
case "$1" in
  remote) printf '%s\n' 'https://fixture.invalid/orbit.git' ;;
  rev-parse) printf '%s\n' '__HEAD__' ;;
  ls-remote)
    [ "$2" = --heads ] && [ "$3" = origin ] && [ "$4" = refs/heads/candidate ] || exit 2
    printf '%s\n' probe >> calls
    count=$(($(cat count) + 1))
    printf '%s\n' "$count" > count
    if [ "$count" -le "$(cat timeouts)" ]; then
      cat probe-failure >&2
      : > probe-ready
      exec sleep 600
    fi
    if [ -s probe-failure ]; then cat probe-failure >&2; exit 1; fi
    cat remote ;;
  push)
    printf '%s\n' push >> calls
    if [ -s push-failure ]; then cat push-failure >&2; exit 1; fi
    printf '%s\n' '__HEAD__' > published ;;
  *) exit 2 ;;
esac
"#;
