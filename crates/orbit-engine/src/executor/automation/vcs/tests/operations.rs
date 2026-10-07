#![cfg(unix)]

use std::fs;
use std::path::Path;

use serde_json::json;

use super::super::run_private_operation;

const PUSH_HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PUSH_PREVIOUS_HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// Fault injection at the private operation boundary forces lost push replies
// and server refusals without involving GitHub or the shipment pipeline.
#[test]
fn pushes_recover_from_transient_remote_failures() {
    let fixture = PushFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    for operation in ["push", "push.candidate_ref"] {
        for message in [
            "! [remote rejected] branch -> branch (failed)",
            "fatal: unable to access origin: The requested URL returned error: 500",
            "fatal: HTTP 503 Service Unavailable",
            "error: RPC failed; curl 56 receive error",
            "fatal: early EOF",
            "fatal: connection reset by peer",
            "fatal: TLS handshake timeout",
        ] {
            fixture.reset(message, 1, false);
            // A matching SHA on a different ref does not confirm this push.
            fixture.write("remote", &format!("{PUSH_HEAD}\trefs/heads/another\n"));
            let output = run_private_operation(operation, &fixture.input(operation, false))
                .expect("transient push failure must recover");
            assert_eq!(output["stdout"], "pushed\n", "{operation}: {message}");
            assert_eq!(fixture.calls(), ["push", "ls-remote", "push"]);
            assert_eq!(fixture.push_count(), 2, "{operation}: {message}");
        }
    }
}

#[test]
fn pushes_do_not_retry_permanent_refusals_even_with_transport_diagnostics() {
    let fixture = PushFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    for operation in ["push", "push.candidate_ref"] {
        for message in [
            "fatal: Authentication failed for origin",
            "git@github.com: Permission denied (publickey).",
            "remote: Write access to repository not granted.",
            "fatal: unable to access origin: The requested URL returned error: 403",
            "fatal: could not read Username for origin: terminal prompts disabled",
            "! [rejected] branch -> branch (non-fast-forward)",
            "! [rejected] branch -> branch (fetch first)",
            "! [remote rejected] branch -> branch (pre-receive hook declined)",
            "remote: error: GH006: Protected branch update failed",
            "remote: error: GH013: Repository rule violations found",
            "! [rejected] branch -> branch (stale info)",
            "! [remote rejected] branch -> branch (unexpected reason)",
        ] {
            let mixed = format!("{message}\nerror: RPC failed; HTTP 503\n");
            // An unknown rejection alone is permanent; the other cases must
            // dominate even an additional transient transport diagnostic.
            let failure = if message.ends_with("(unexpected reason)") {
                message
            } else {
                &mixed
            };
            fixture.reset(failure, 9, false);
            let error = run_private_operation(operation, &fixture.input(operation, true))
                .expect_err("permanent refusals must fail immediately");
            assert!(error.to_string().contains(message), "{operation}: {error}");
            assert_eq!(fixture.calls(), ["push"], "{operation}: {message}");
            assert_eq!(fixture.push_count(), 1);
        }
    }
}

#[test]
fn pushes_confirm_lost_replies_without_reusing_a_consumed_lease() {
    let fixture = PushFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    for (operation, lease) in [
        ("push", false),
        ("push", true),
        ("push.candidate_ref", false),
    ] {
        fixture.reset("error: RPC failed; connection reset by peer", 1, true);
        let output = run_private_operation(operation, &fixture.input(operation, lease))
            .expect("the exact remote head proves the failed reply hid a successful push");
        assert!(output["stdout"].as_str().unwrap().starts_with(PUSH_HEAD));
        assert_eq!(fixture.calls(), ["push", "ls-remote"]);
        assert_eq!(fixture.push_count(), 1, "{operation}: lease={lease}");
    }
}

#[test]
fn pushes_bound_retries_and_stop_when_remote_confirmation_fails() {
    let fixture = PushFixture::new();
    let _path = orbit_common::test_env::scoped([("PATH", Some(fixture.path.as_str()))]);
    for operation in ["push", "push.candidate_ref"] {
        fixture.reset("! [remote rejected] branch -> branch (failed)", 9, false);
        let error = run_private_operation(operation, &fixture.input(operation, true))
            .expect_err("repeated transient failures must exhaust the push budget");
        assert!(error.to_string().contains("(failed)"));
        assert_eq!(fixture.push_count(), 3);
        assert_eq!(
            fixture.calls(),
            [
                "push",
                "ls-remote",
                "push",
                "ls-remote",
                "push",
                "ls-remote"
            ]
        );

        fixture.reset("fatal: early EOF", 1, false);
        fixture.write("lookup-failure", "fatal: unable to read remote ref");
        let error = run_private_operation(operation, &fixture.input(operation, false))
            .expect_err("a failed remote read must not trigger another mutation");
        assert!(error.to_string().contains("unable to read remote ref"));
        assert_eq!(fixture.calls(), ["push", "ls-remote"]);
        assert_eq!(fixture.push_count(), 1);
    }
}

struct PushFixture {
    root: tempfile::TempDir,
    path: String,
}

impl PushFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary push fixture");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).expect("create git stub directory");
        write_executable(
            &bin.join("git"),
            &format!(
                r#"#!/bin/sh
while [ "$1" = -c ]; do shift 2; done
command=$1
shift
case "$command" in
  rev-parse)
    [ "$1" = --verify ] && [ "$2" = refs/heads/branch ] || exit 2
    printf '%s\n' '{PUSH_HEAD}'
    ;;
  push)
    printf '%s\n' push >> calls
    [ "$1" = --no-verify ] || exit 2
    shift
    if [ -s lease ]; then
      [ "$1" = '--force-with-lease=refs/heads/branch:{PUSH_PREVIOUS_HEAD}' ] || exit 2
      shift
    fi
    [ "$1" = -- ] && [ "$2" = origin ] || exit 2
    case "$3" in
      branch) target=refs/heads/branch ;;
      '+{PUSH_HEAD}:refs/orbit/candidates/run') target=refs/orbit/candidates/run ;;
      *) exit 2 ;;
    esac
    count=$(cat count)
    count=$((count + 1))
    printf '%s\n' "$count" > count
    if [ "$(cat landed)" = true ]; then
      printf '%s\t%s\n' '{PUSH_HEAD}' "$target" > remote
    fi
    if [ "$count" -le "$(cat failures)" ]; then cat failure >&2; exit 1; fi
    printf '%s\n' pushed
    ;;
  ls-remote)
    printf '%s\n' ls-remote >> calls
    [ "$1" = --refs ] && [ "$2" = -- ] && [ "$3" = origin ] || exit 2
    case "$4" in refs/heads/branch|refs/orbit/candidates/run) ;; *) exit 2 ;; esac
    if [ -s lookup-failure ]; then cat lookup-failure >&2; exit 1; fi
    cat remote
    ;;
  *) exit 2 ;;
esac
"#
            ),
        );
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths)
            .expect("compose git stub PATH")
            .into_string()
            .expect("git stub PATH is UTF-8");
        Self { root, path }
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.root.path().join(name), contents).expect("write push fixture state");
    }

    fn reset(&self, failure: &str, failures: usize, landed: bool) {
        self.write("count", "0");
        self.write("calls", "");
        self.write("failure", failure);
        self.write("failures", &failures.to_string());
        self.write("landed", if landed { "true" } else { "false" });
        self.write("remote", "");
        self.write("lookup-failure", "");
        self.write("lease", "");
    }

    fn input(&self, operation: &str, lease: bool) -> serde_json::Value {
        if operation == "push.candidate_ref" {
            json!({
                "repo_root": self.root.path(),
                "head_sha": PUSH_HEAD,
                "target_ref": "refs/orbit/candidates/run",
            })
        } else {
            self.write("lease", if lease { "true" } else { "" });
            json!({
                "repo_root": self.root.path(),
                "branch": "branch",
                "force_with_lease": lease,
                "expected_remote_sha": PUSH_PREVIOUS_HEAD,
            })
        }
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.path().join("calls"))
            .expect("read git calls")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn push_count(&self) -> usize {
        read_count(&self.root.path().join("count"))
    }
}

#[test]
fn pr_status_retries_the_landed_run_graphql_failure_and_stops_on_permanent_errors() {
    let fixture = tempfile::tempdir().expect("temporary VCS fixture");
    let bin = fixture.path().join("bin");
    fs::create_dir(&bin).expect("create executable directory");
    let count_file = fixture.path().join("count");
    let failure_file = fixture.path().join("failure");
    let gh = bin.join("gh");
    write_executable(
        &gh,
        &format!(
            "#!/bin/sh\n\
             count=0\n\
             if [ -f '{}' ]; then count=$(cat '{}'); fi\n\
             count=$((count + 1))\n\
             printf '%s\\n' \"$count\" > '{}'\n\
             if [ -f '{}' ]; then cat '{}' >&2; exit 1; fi\n\
             if [ \"$count\" -eq 1 ]; then\n\
               printf '%s\\n' 'GraphQL: Something went wrong while executing your query on 2026-10-06T19:28:45Z. Please include D10C when reporting this issue.' >&2\n\
               exit 1\n\
             fi\n\
             printf '%s\\n' '{{\"number\":123,\"state\":\"MERGED\"}}'\n",
            shell_quote(&count_file),
            shell_quote(&count_file),
            shell_quote(&count_file),
            shell_quote(&failure_file),
            shell_quote(&failure_file),
        ),
    );
    let original_path = std::env::var("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&original_path));
    let path = std::env::join_paths(paths).expect("compose test PATH");
    let path = path.to_str().expect("test PATH is UTF-8");
    let _path = orbit_common::test_env::scoped([("PATH", Some(path))]);
    let input = json!({"pr": "123", "workspace_path": fixture.path()});

    let recovered = run_private_operation("pr.status", &input)
        .expect("PR status should recover after the transient GraphQL error");
    assert_eq!(
        recovered,
        json!({"pull_request":{"number":123,"state":"MERGED"}})
    );
    assert_eq!(
        read_count(&count_file),
        2,
        "the transient failure retries once"
    );

    for (message, label) in [
        ("HTTP 401: Bad credentials", "auth"),
        ("unknown head: refs/heads/missing", "unknown head"),
        (
            "invalid selector: expected a PR number or URL",
            "invalid selector",
        ),
    ] {
        fs::write(&count_file, "0").expect("reset call count");
        fs::write(&failure_file, message).expect("write permanent failure");
        let error = run_private_operation("pr.status", &input)
            .expect_err("permanent lookup failures must not be retried");
        assert!(error.to_string().contains(message), "{label}: {error}");
        assert_eq!(
            read_count(&count_file),
            1,
            "{label} fails on the first attempt"
        );
    }
}

fn read_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .expect("read gh call count")
        .trim()
        .parse()
        .expect("gh call count is numeric")
}

fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write gh stub");
    let permissions = fs::Permissions::from_mode(0o755);
    fs::set_permissions(path, permissions).expect("make gh stub executable");
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
