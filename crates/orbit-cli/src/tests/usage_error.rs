//! Reading the output mode's argv rungs before clap has accepted the argv.

use std::ffi::OsString;

use crate::output::sink::FormatArg;
use crate::usage_error::pre_parse_format;

fn scan(argv: &[&str]) -> (Option<FormatArg>, bool) {
    let args: Vec<OsString> = argv.iter().map(OsString::from).collect();
    pre_parse_format(&args)
}

#[test]
fn format_is_read_in_both_spellings_and_the_last_mode_wins() {
    assert_eq!(scan(&["orbit", "task", "show"]), (None, false));
    assert_eq!(
        scan(&["orbit", "--format", "json", "task", "show"]),
        (Some(FormatArg::Json), false)
    );
    assert_eq!(
        scan(&["orbit", "task", "show", "--format=ndjson"]),
        (Some(FormatArg::Ndjson), false)
    );
    assert_eq!(
        scan(&["orbit", "--format", "json", "task", "--format", "table"]),
        (Some(FormatArg::Table), false)
    );
}

#[test]
fn a_value_that_is_not_a_mode_is_not_a_request() {
    // `audit export` owns a `--format` of its own; `csv` names its file format.
    assert_eq!(
        scan(&["orbit", "audit", "export", "--format", "csv"]),
        (None, false)
    );
    // The invalid value clap is about to reject leaves the mode to the
    // environment rung rather than to a guess.
    assert_eq!(
        scan(&["orbit", "task", "list", "--format", "xml"]),
        (None, false)
    );
    assert_eq!(scan(&["orbit", "task", "list", "--format"]), (None, false));
}

#[test]
fn legacy_json_flags_are_seen_and_nothing_after_a_bare_separator_counts() {
    assert_eq!(scan(&["orbit", "task", "list", "--json"]), (None, true));
    assert_eq!(scan(&["orbit", "task", "list", "--ops"]), (None, true));
    assert_eq!(
        scan(&[
            "orbit", "tool", "run", "x", "--", "--json", "--format", "json"
        ]),
        (None, false)
    );
}
