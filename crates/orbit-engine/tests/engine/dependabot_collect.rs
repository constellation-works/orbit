//! Security-alert collection for an explicitly named repository.
//!
//! The substitute `gh` rejects `repo view --repo` the way upstream `gh` does
//! (`unknown flag: --repo`) and answers the alert queries only after a
//! positional `repo view <owner/name>`. `PATH` is process-global, so the body
//! re-runs in an isolated copy of this binary.

#![cfg(unix)]
#![allow(missing_docs)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::Duration;

use orbit_common::{OrbitError, process::run_bounded_capped, test_env};
use orbit_engine::{RuntimeHost, execute_deterministic_action};
use serde_json::json;

const CHILD_ENV: &str = "ORBIT_DEPENDABOT_COLLECT_CHILD";
const LOG_ENV: &str = "ORBIT_FAKE_GH_LOG";
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;

const FAKE_GH: &str = r#"#!/bin/sh
set -eu
log=${ORBIT_FAKE_GH_LOG:?}
printf '%s\n' "$*" >> "$log"
case "$1" in
  auth)
    if [ "${2:-}" != status ]; then
      echo "fake gh: unsupported call: $*" >&2
      exit 2
    fi
    exit 0
    ;;
  repo)
    if [ "${2:-}" != view ]; then
      echo "fake gh: unsupported call: $*" >&2
      exit 2
    fi
    for arg in "$@"; do
      if [ "$arg" = "--repo" ]; then
        echo "unknown flag: --repo" >&2
        exit 1
      fi
    done
    shift 2
    if [ "${1:-}" != "acme/orbit" ]; then
      echo "fake gh: repo view expected positional acme/orbit, got: $*" >&2
      exit 1
    fi
    printf '%s\n' '{"name":"orbit","nameWithOwner":"acme/orbit","defaultBranchRef":{"name":"agent-main"}}'
    ;;
  api)
    printf '%s\n' '[]'
    ;;
  pr)
    printf '%s\n' '[]'
    ;;
  *)
    echo "fake gh: unsupported call: $*" >&2
    exit 2
    ;;
esac
"#;

struct RootHost {
    root: String,
}

impl RuntimeHost for RootHost {
    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.root.clone())
    }
}

#[test]
fn explicit_repository_reaches_alert_queries() {
    const TEST: &str = "explicit_repository_reaches_alert_queries";
    if std::env::var(CHILD_ENV).as_deref() != Ok(TEST) {
        spawn_isolated(TEST);
        return;
    }

    let root = tempfile::tempdir().expect("repo root");
    let host = RootHost {
        root: root.path().to_string_lossy().into_owned(),
    };
    let outcome = execute_deterministic_action(
        &host,
        "collect_dependabot_alerts",
        &json!({}),
        &json!({"repo": "acme/orbit"}),
        false,
        &HashMap::new(),
        None,
    )
    .expect("explicit repo collection reaches the alert queries");

    let snapshot = &outcome["dependabot_snapshot"];
    assert_eq!(outcome["phase"], "collect_dependabot_alerts");
    assert_eq!(snapshot["repository"]["full_name"], "acme/orbit");
    assert_eq!(snapshot["repository"]["name"], "orbit");
    assert_eq!(snapshot["repository"]["default_branch"], "agent-main");
    assert_eq!(snapshot["collection_status"], "fully_collected");
    assert_eq!(snapshot["collected"], true);
    assert_eq!(
        snapshot["code_scanning"]["collection_status"],
        "fully_collected"
    );
    assert_eq!(
        snapshot["secret_scanning"]["collection_status"],
        "fully_collected"
    );

    let calls =
        fs::read_to_string(std::env::var(LOG_ENV).expect("fake gh log")).expect("read gh calls");
    let lines: Vec<&str> = calls.lines().collect();
    assert!(
        lines.iter().any(|line| {
            *line == "repo view acme/orbit --json name,nameWithOwner,defaultBranchRef"
        }),
        "repo view must take the repository positionally: {lines:?}"
    );
    for endpoint in [
        "repos/acme/orbit/dependabot/alerts",
        "repos/acme/orbit/code-scanning/alerts",
        "repos/acme/orbit/secret-scanning/alerts",
    ] {
        assert!(
            lines.iter().any(|line| line.contains(endpoint)),
            "collection must query {endpoint} after repo view: {lines:?}"
        );
    }
}

fn spawn_isolated(test: &str) {
    let sandbox = tempfile::tempdir_in(test_env::canonical_temp_dir()).expect("sandbox");
    let bin = sandbox.path().join("bin");
    fs::create_dir_all(&bin).expect("bin dir");
    let gh = bin.join("gh");
    fs::write(&gh, FAKE_GH).expect("write fake gh");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
    let log = sandbox.path().join("calls");
    let mut path = vec![bin];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") || name.starts_with("GH_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .current_dir(sandbox.path())
        .env_remove("GITHUB_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN")
        .env(CHILD_ENV, test)
        .env(LOG_ENV, &log)
        .env("PATH", std::env::join_paths(path).expect("PATH"))
        .env("HOME", sandbox.path())
        .env("TMPDIR", sandbox.path());
    let output = run_bounded_capped(&mut command, CHILD_DEADLINE, 1024 * 1024)
        .unwrap_or_else(|error| panic!("{test} did not finish in its isolated child: {error}"));
    test_env::assert_child_test_passed(&qualified, output.status, output.stdout, output.stderr);
}
