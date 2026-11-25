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

            let recovered =
                run_private_operation(&orbit_common::Env::capture(), "pr.status", &input)
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
                let error =
                    run_private_operation(&orbit_common::Env::capture(), "pr.status", &input)
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

#[test]
fn required_checks_retries_unprefixed_graphql_failure_and_keeps_deadline_failure_typed() {
    with_named_stub_path(
        "required checks unprefixed outage",
        concat!(
            module_path!(),
            "::required_checks_retries_unprefixed_graphql_failure_and_keeps_deadline_failure_typed"
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
                     [ \"$1\" = --warm-up ] && exit 0\n\
                     [ \"$*\" = 'api rate_limit' ] && {{ printf '{{}}\\n'; exit 0; }}\n\
                     count=0\n\
                     if [ -f {} ]; then count=$(cat {}); fi\n\
                     count=$((count + 1))\n\
                     printf '%s\\n' \"$count\" > {}\n\
                     if [ -f {} ]; then cat {} >&2; exit 1; fi\n\
                     if [ \"$count\" -eq 1 ]; then\n\
                       printf '%s\\n' 'gh: Something went wrong while executing your query on 2026-11-25T04:40:48Z. Please include RQ123 when reporting this issue.' >&2\n\
                       exit 1\n\
                     fi\n\
                     printf '%s\\n' '{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"headRefOid\":\"abc123\"}}}}}}}}'\n",
                    shell_quote(&count_file),
                    shell_quote(&count_file),
                    shell_quote(&count_file),
                    shell_quote(&failure_file),
                    shell_quote(&failure_file),
                ),
            );
            let input = json!({"pr": "123", "workspace_path": fixture});

            let recovered =
                run_private_operation(&orbit_common::Env::capture(), "pr.required_checks", &input)
                    .expect(
                        "required-check observation recovers after the unprefixed provider error",
                    );
            assert_eq!(recovered, json!({"pull_request":{"headRefOid":"abc123"}}));
            assert_eq!(
                read_count(&count_file),
                2,
                "the bounded retry recovers once"
            );

            fs::write(&count_file, "0").expect("reset call count");
            fs::write(
                &failure_file,
                "gh: Something went wrong while executing your query on 2026-11-25T04:40:48Z. Please include RQ123 when reporting this issue.\n",
            )
            .expect("write the unprefixed provider diagnostic");
            let now_ms = u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock is after the epoch")
                    .as_millis(),
            )
            .expect("current milliseconds fit in u64");
            let deadline_ms = now_ms + 30_000;
            let error = run_private_operation(
                &orbit_common::Env::capture(),
                "pr.required_checks",
                &json!({"pr": "123", "workspace_path": fixture, "deadline_ms": deadline_ms}),
            )
            .expect_err("deadline-bound observation leaves retry policy to its caller");
            let failure = error
                .private_vcs_failure()
                .expect("provider observation failure remains typed");
            assert_eq!(
                failure.class,
                orbit_common::PrivateVcsFailureClass::TransientObservation
            );
            assert_eq!(
                read_count(&count_file),
                1,
                "deadline-bound reads preserve the single-read policy"
            );

            fs::write(&count_file, "0").expect("reset call count");
            let exhausted =
                run_private_operation(&orbit_common::Env::capture(), "pr.required_checks", &input)
                    .expect_err("an exhausted observation cannot provide check evidence");
            assert_eq!(
                exhausted
                    .private_vcs_failure()
                    .expect("exhausted provider error remains typed")
                    .class,
                orbit_common::PrivateVcsFailureClass::TransientObservation
            );
            assert_eq!(
                read_count(&count_file),
                3,
                "the unprefixed provider diagnostic uses the existing bounded attempts"
            );

            fs::write(&count_file, "0").expect("reset call count");
            fs::write(&failure_file, "HTTP 401: Bad credentials\n")
                .expect("write a permanent authentication failure");
            let permanent =
                run_private_operation(&orbit_common::Env::capture(), "pr.required_checks", &input)
                    .expect_err("permanent authentication failures do not provide check evidence");
            assert_eq!(
                permanent
                    .private_vcs_failure()
                    .expect("permanent provider error remains typed")
                    .class,
                orbit_common::PrivateVcsFailureClass::Other
            );
            assert_eq!(read_count(&count_file), 1, "auth failure is not retried");

            fs::write(&count_file, "0").expect("reset call count");
            let invalid = run_private_operation(
                &orbit_common::Env::capture(),
                "pr.required_checks",
                &json!({"pr": "not-a-pr", "workspace_path": fixture}),
            )
            .expect_err("an invalid PR selector is refused before querying GitHub");
            assert!(
                invalid
                    .to_string()
                    .contains("invalid private automation VCS PR selector")
            );
            assert_eq!(read_count(&count_file), 0, "invalid selector ran no read");
        },
    );
}
