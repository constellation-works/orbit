//! [ORB-11245] `orbit run job` / `orbit job run` have no `--crew` flag; crew
//! selection is `--input crew=<name>`. Clap's default "pass it after `--`"
//! tip is wrong for this shape (there is no positional slot for it to land
//! in), so [`crate::repair_crew_flag_suggestion`] repoints the tip without
//! changing what argv the parser accepts.

use clap::CommandFactory;

use crate::command::Cli;
use crate::{install_format_arg, repair_crew_flag_suggestion};

fn rejected(argv: &[&str]) -> clap::error::Error {
    install_format_arg(Cli::command())
        .try_get_matches_from(argv)
        .expect_err("argv should be rejected")
}

#[test]
fn run_job_crew_flag_points_at_the_input_contract_instead_of_trailing_dashes() {
    let err = repair_crew_flag_suggestion(rejected(&[
        "orbit",
        "run",
        "job",
        "task_pilot_pipeline",
        "--crew",
        "luna",
    ]));
    let rendered = err.render().to_string();

    assert!(
        rendered.contains("--input crew=<name>"),
        "expected the repaired tip, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("-- --crew"),
        "misleading trailing-arg tip should have been replaced:\n{rendered}"
    );
}

#[test]
fn job_run_alias_gets_the_same_repair() {
    let err = repair_crew_flag_suggestion(rejected(&[
        "orbit",
        "job",
        "run",
        "task_pilot_pipeline",
        "--crew",
        "luna",
    ]));
    let rendered = err.render().to_string();

    assert!(
        rendered.contains("--input crew=<name>"),
        "expected the repaired tip, got:\n{rendered}"
    );
}

#[test]
fn crew_flag_rejection_still_fails_the_command() {
    let err = repair_crew_flag_suggestion(rejected(&[
        "orbit",
        "run",
        "job",
        "task_pilot_pipeline",
        "--crew",
        "luna",
    ]));

    // The repair only rewrites the tip text; `--crew` must still be refused
    // rather than silently parsed as an activity/input argument.
    assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn unrelated_unknown_argument_keeps_the_default_trailing_arg_tip() {
    // `run ship` has no `--input` escape hatch for `--crew`, so the repair
    // must not touch it — only the `run job` / `job run` shape qualifies.
    let err = repair_crew_flag_suggestion(rejected(&["orbit", "run", "ship", "--crew", "luna"]));
    let rendered = err.render().to_string();

    assert!(
        rendered.contains("-- --crew"),
        "unrelated command's default tip should be untouched:\n{rendered}"
    );
}
