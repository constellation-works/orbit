#![cfg(unix)]

use std::fs;
use std::path::Path;

use serde_json::json;

use super::super::run_private_operation;

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
