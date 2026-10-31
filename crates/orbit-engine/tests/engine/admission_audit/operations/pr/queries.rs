use std::fs;

use serde_json::json;

use super::test_support::{read_count, shell_quote, with_named_stub_path, write_executable};
use orbit_engine::run_private_vcs_operation as run_private_operation;

// The root's name carries a space and an apostrophe, both valid in a TMPDIR, so
// each path the stub interpolates must survive shell word splitting and quoting.
#[test]
fn pr_status_retries_the_landed_run_graphql_failure_and_stops_on_permanent_errors() {
    with_named_stub_path(
        "pr stub spaced's",
        concat!(
            module_path!(),
            "::pr_status_retries_the_landed_run_graphql_failure_and_stops_on_permanent_errors"
        ),
        |fixture| {
            let bin = fixture.join("bin");
            let count_file = fixture.join("count");
            let failure_file = fixture.join("failure");
            let gh = bin.join("gh");
            write_executable(
                &gh,
                &format!(
                    "#!/bin/sh\n\
                 count=0\n\
                 if [ -f {} ]; then count=$(cat {}); fi\n\
                 count=$((count + 1))\n\
                 printf '%s\\n' \"$count\" > {}\n\
                 if [ -f {} ]; then cat {} >&2; exit 1; fi\n\
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
            let input = json!({"pr": "123", "workspace_path": fixture});

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
        },
    );
}
