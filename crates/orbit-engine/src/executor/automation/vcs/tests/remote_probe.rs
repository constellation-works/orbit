//! Fault injection at the branch lookup used by PR preparation. The private
//! Git budget scales supervisor deadlines without changing production limits.
//! The stub PATH belongs to an isolated child, never to parallel Git fixtures.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use super::super::freshness::remote_branch_sha;
use super::super::git::{GitTimeoutBudget, GitTimeoutBudgetGuard};

#[test]
fn branch_lookup_retries_only_supervisor_timeouts_with_a_bounded_budget() {
    let test = concat!(
        module_path!(),
        "::branch_lookup_retries_only_supervisor_timeouts_with_a_bounded_budget"
    )
    .split_once("::")
    .unwrap()
    .1;
    const CHILD: &str = "ORBIT_REMOTE_PROBE_TEST_CHILD";
    if std::env::var(CHILD).as_deref() != Ok(test) {
        let root = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir()).unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("git"), STUB_GIT).unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut child = Command::new(std::env::current_exe().unwrap());
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", test, "--nocapture"])
            .current_dir(root.path())
            .env(CHILD, test)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("HOME", root.path())
            .env("USERPROFILE", root.path());
        let output = orbit_common::test_env::run_child_test(&mut child, test, root.path());
        orbit_common::test_env::assert_child_test_passed(
            test,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    }

    let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
        default_ms: 1_000,
        ..GitTimeoutBudget::DEFAULT
    });
    let root = std::env::current_dir().unwrap();
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (timeouts, response, refusal, calls) in [
        (1, format!("{sha}\trefs/heads/candidate\n"), "", 2),
        (1, String::new(), "", 2),
        (100, String::new(), "", 3),
        (0, String::new(), "fatal: Authentication failed", 1),
        (0, String::new(), "fatal: connection timed out", 1),
        (1, String::new(), "fatal: Authentication failed", 2),
    ] {
        for (name, value) in [
            ("count", "0".to_string()),
            ("timeouts", timeouts.to_string()),
            ("response", response.clone()),
            ("refusal", refusal.to_string()),
        ] {
            fs::write(root.join(name), value).unwrap();
        }
        let result = remote_branch_sha(&root, "candidate");
        if timeouts == 100 {
            let error = result.expect_err("remote reads must exhaust a bounded retry budget");
            assert!(
                error.is_timeout(),
                "timeout classification survives: {error}"
            );
            let message = error.to_string();
            assert!(
                message.contains("branch freshness ls-remote probe"),
                "{message}"
            );
            assert!(message.contains("3 attempts"), "{message}");
            assert!(message.contains("per-attempt timeout=1000ms"), "{message}");
            assert!(message.contains("backoff=750ms"), "{message}");
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
                elapsed >= 3_750,
                "elapsed includes attempts and backoff: {message}"
            );
        } else if !refusal.is_empty() {
            let error = result.expect_err("a completed Git refusal must stop the read retry");
            assert!(
                !error.is_timeout(),
                "a diagnostic is not a deadline: {error}"
            );
            assert!(error.to_string().contains(refusal), "{error}");
        } else {
            let expected = (!response.is_empty()).then(|| sha.to_string());
            assert_eq!(
                result.unwrap(),
                expected,
                "a stalled PR preparation read must recover"
            );
        }
        assert_eq!(
            fs::read_to_string(root.join("count")).unwrap().trim(),
            calls.to_string(),
            "only supervisor deadlines may consume additional attempts"
        );
    }
}

const STUB_GIT: &str = r#"#!/bin/sh
set -eu
while [ "$1" = -c ]; do shift 2; done
[ "$1" = ls-remote ] && [ "$2" = --heads ] && [ "$3" = origin ] && [ "$4" = refs/heads/candidate ] || exit 2
count=$(($(cat count) + 1))
printf '%s\n' "$count" > count
if [ "$count" -le "$(cat timeouts)" ]; then exec sleep 600; fi
if [ -s refusal ]; then cat refusal >&2; exit 1; fi
cat response
"#;
