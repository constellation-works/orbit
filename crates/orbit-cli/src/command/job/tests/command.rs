use clap::{Parser, error::ErrorKind};

use crate::command::Cli;

fn assert_cli_rejects(args: &[&str], kind: ErrorKind, expected: &str) {
    let error = match Cli::try_parse_from(args.iter().copied()) {
        Ok(_) => panic!("form should be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), kind, "{error}");
    let message = error.to_string();
    assert!(message.contains(expected), "{message}");
}

#[test]
fn rejects_removed_job_run_inspection_aliases() {
    for (args, retired_subcommand) in [
        (
            &["orbit", "job", "history", "task_auto_pipeline"][..],
            "history",
        ),
        (&["orbit", "job", "run-state", "jrun-1"][..], "run-state"),
    ] {
        assert_cli_rejects(
            args,
            ErrorKind::InvalidSubcommand,
            &format!("unrecognized subcommand '{retired_subcommand}'"),
        );
    }
}

#[test]
fn parses_job_replay_subcommand() {
    assert!(Cli::try_parse_from(["orbit", "job", "replay", "jrun-1", "--json"]).is_ok());
}

#[test]
fn parses_job_resume_subcommand() {
    assert!(Cli::try_parse_from(["orbit", "job", "resume", "jrun-1", "--json"]).is_ok());
}

#[test]
fn write_v2_step_has_no_debug_output_for_fan_in_and_retry() {
    use crate::command::job::support::{
        format_fan_in, format_join_mode, format_retry, write_v2_step,
    };
    use orbit_types::workflow::{
        BackoffStrategy, FanInSpec, FanOutBlock, JobV2Step, JobV2StepBody, JoinMode, ParallelBlock,
        RetrySpec,
    };

    let fan_in = FanInSpec {
        join: JoinMode::Any,
        collect: Some("pilot_results".to_string()),
    };
    assert_eq!(format_fan_in(&fan_in), "join=any collect=pilot_results");

    let retry = RetrySpec {
        max_attempts: 3,
        initial_backoff_ms: 1000,
        backoff_cap_ms: 30000,
        backoff_strategy: BackoffStrategy::Exponential,
    };
    assert_eq!(
        format_retry(&retry),
        "max_attempts=3 initial_backoff_ms=1000 backoff_cap_ms=30000 strategy=exponential"
    );

    assert_eq!(format_join_mode(&JoinMode::All), "all");
    assert_eq!(format_join_mode(&JoinMode::Quorum { n: 2 }), "quorum(2)");

    let step = JobV2Step {
        id: "fan_out_step".to_string(),
        when: None,
        retry: Some(retry),
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::FanOut {
            fan_out: FanOutBlock {
                items: "items".to_string(),
                max_workers: 4,
                worker: Box::new(JobV2Step {
                    id: "worker_step".to_string(),
                    when: None,
                    retry: None,
                    recovery_activity: None,
                    resolved_recovery_activity: None,
                    body: JobV2StepBody::Parallel {
                        parallel: ParallelBlock {
                            join: JoinMode::All,
                            branches: vec![],
                        },
                    },
                }),
            },
            fan_in,
        },
    };

    let mut out = String::new();
    write_v2_step(&step, 0, &mut out);
    assert!(
        !out.contains("{:?}"),
        "output should not contain {{:?}} placeholder: {out}"
    );
    assert!(
        !out.contains("FanInSpec {"),
        "output should not contain debug FanInSpec: {out}"
    );
    assert!(
        !out.contains("RetrySpec {"),
        "output should not contain debug RetrySpec: {out}"
    );
    assert!(
        out.contains("join=any collect=pilot_results"),
        "output should contain formatted fan-in: {out}"
    );
    assert!(
        out.contains("all\n"),
        "output should contain formatted join: {out}"
    );
}
