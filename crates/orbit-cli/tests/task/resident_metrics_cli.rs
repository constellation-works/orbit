//! `orbit resident metrics` plain text on a workspace whose nullable measures are null, as on a
//! fresh workspace with no answered escalations and no reversal denominator.

use crate::isolated_cli_fixture::Fixture;

/// The token that follows `label` in `output`, ending at the next delimiter. Empty when the label
/// is absent or nothing follows it, so a missing line fails the caller rather than passing.
fn token_after<'a>(output: &'a str, label: &str) -> &'a str {
    output
        .split_once(label)
        .and_then(|(_, rest)| rest.split([',', ')', ' ']).next())
        .unwrap_or_default()
}

#[test]
fn resident_metrics_text_prints_a_value_for_each_null_measure() {
    if crate::isolated_test::run_in_child(
        "resident_metrics_cli::resident_metrics_text_prints_a_value_for_each_null_measure",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let stdout = fixture
        .command(&["resident", "metrics", "--since", "7d"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(stdout).unwrap();

    // Median answer latency, recommended share and the reversal rate are null here. Each must
    // print a readable value, not an empty slot followed by its unit or punctuation.
    let latency = token_after(&text, "median answer latency ");
    assert!(
        !latency.is_empty() && latency != "s",
        "median answer latency printed no value before its unit (ORB-26516): {text}"
    );
    let share = token_after(&text, "recommended share ");
    assert!(
        !share.is_empty(),
        "recommended share printed no value before its comma (ORB-26516): {text}"
    );
    let rate = token_after(&text, "(rate ");
    assert!(
        !rate.is_empty(),
        "reversal rate printed no value before its closing paren (ORB-26516): {text}"
    );
}
