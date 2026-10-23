//! CI collection preserves malformed provider identities as retryable evidence.

use serde_json::{Value, json};

const HEAD: &str = "1111111111111111111111111111111111111111";

/// Exercise the public action boundary, including GitHub's databaseId projection.
/// Substitute executables and their mutable fixture state stay in an isolated child.
#[cfg(unix)]
#[test]
fn malformed_run_identity_is_retryable_at_the_action_boundary() {
    use std::os::unix::fs::PermissionsExt;

    const MARKER: &str = "ORBIT_TEST_RUN_IDENTITY_CHILD";
    let test = concat!(
        module_path!(),
        "::malformed_run_identity_is_retryable_at_the_action_boundary"
    );
    let test = test.split_once("::").expect("crate-qualified test").1;
    if std::env::var(MARKER).as_deref() != Ok(test) {
        let root = tempfile::tempdir().expect("fixture root");
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("fixture bin");
        for (name, script) in [
            (
                "gh",
                r#"#!/bin/sh
set -eu
case "$1 ${2:-}" in
  "auth status") exit 0 ;;
  "repo view") printf '%s\n' '{"name":"orbit","nameWithOwner":"acme/orbit","defaultBranchRef":{"name":"topic"}}' ;;
  "pr list") printf '[]\n' ;;
  "run list") cat runs.json ;;
  "run view") printf '%s\n' "$3" >> views; printf '{"databaseId":42,"jobs":[]}\n' ;;
  "api --method") printf '{"jobs":[]}\n' ;;
  *) exit 2 ;;
esac
"#,
            ),
            (
                "git",
                r#"#!/bin/sh
while [ "${1:-}" = "-c" ]; do shift 2; done
case "$*" in
  "ls-remote --heads origin") printf '1111111111111111111111111111111111111111\trefs/heads/topic\n' ;;
  *) exit 2 ;;
esac
"#,
            ),
        ] {
            let path = bin.join(name);
            std::fs::write(&path, script).expect("fixture executable");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("fixture permissions");
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"));
        orbit_common::test_env::clear_inherited_authority(|key| {
            child.env_remove(key);
        });
        child
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(MARKER, test)
            .env("PATH", std::env::join_paths(paths).expect("fixture PATH"))
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .current_dir(root.path());
        let output = orbit_common::test_env::run_child_test(&mut child, test, root.path());
        orbit_common::test_env::assert_child_test_passed(
            test,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    }

    struct Host;
    impl orbit_engine::test_support::NullRuntimeHost for Host {}

    for (identity, budget) in [
        (Value::Null, 0),
        (Value::Null, 6),
        (json!("42"), 0),
        (json!("42"), 6),
        (json!(42), 6),
    ] {
        let listing = json!([{
            "databaseId": identity,
            "workflowName": "CI",
            "status": "completed",
            "conclusion": "failure",
            "event": "push",
            "headBranch": "topic",
            "headSha": HEAD,
            "createdAt": "2026-08-30T05:00:00Z",
        }]);
        std::fs::write("runs.json", listing.to_string()).expect("run listing");
        let mut input = json!({"integration_branch": "topic", "max_checkout_log_reads": 1});
        input["max_investigated_runs"] = json!(budget);
        input["workspace_path"] = json!(std::env::current_dir().expect("fixture cwd"));
        let output = orbit_engine::execute_deterministic_action(
            &Host,
            "collect_ci_evidence",
            &json!({}),
            &input,
            false,
            &std::collections::HashMap::new(),
            None,
        )
        .expect("collect action");
        let evidence = &output["ci_evidence"];
        assert_eq!(
            evidence["outcome_hint"],
            json!("retryable_error"),
            "{identity}"
        );
        let errors = evidence["retryable_errors"].as_array().expect("errors");
        assert_eq!(errors.len(), 1, "{evidence}");
        assert_eq!(errors[0]["retryable"], json!(true));
        assert_eq!(errors[0]["stage"], json!("registration"));
        assert_eq!(
            errors[0]["operation"],
            json!(if identity.is_u64() {
                "run_view"
            } else {
                "run_identity"
            })
        );
        if !identity.is_u64() {
            assert!(
                !std::path::Path::new("views").exists(),
                "malformed IDs must not be sent to gh run view"
            );
        }
    }
    assert_eq!(
        std::fs::read_to_string("views").expect("numeric view"),
        "42\n"
    );
}
