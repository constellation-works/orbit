use orbit_types::workflow::activity_job::{
    worktree_failure_display_message, worktree_failure_message,
};
use serde_json::{Value, json};

const DRIFT: &str = "Run failed because the primary checkout changed during execution.";

const INSPECT: &str = "Inspect and preserve its changes before retrying.";

const CONTENT_CONFLICT: &str = concat!(
    "Run failed because the provider moved its worktree branch or HEAD. ",
    "Inspect and preserve the candidate before retrying."
);

fn drift_diagnostic(
    before: (&str, Option<&str>),
    after: (&str, Option<&str>),
    paths: Value,
) -> Value {
    json!({
        "primary_before": {"head": before.0, "branch": before.1},
        "primary_after": {"head": after.0, "branch": after.1},
        "primary_changed_paths": paths,
    })
}

fn drift_message(diagnostic: &Value) -> String {
    worktree_failure_display_message("primary_checkout_drift", diagnostic)
}

fn fixed_drift_message() -> String {
    format!("{DRIFT} {INSPECT}")
}

#[test]
fn untracked_primary_file_with_unchanged_head_is_named_and_quoted() {
    let diagnostic = drift_diagnostic(("208fc65", None), ("208fc65", None), json!(["0"]));
    assert_eq!(
        drift_message(&diagnostic),
        format!("{DRIFT} HEAD did not move. Changed primary paths: \"0\". {INSPECT}")
    );
}

#[test]
fn head_move_says_head_moved() {
    let diagnostic = drift_diagnostic(
        ("aaa111", Some("main")),
        ("bbb222", Some("main")),
        json!(["src/lib.rs"]),
    );
    assert_eq!(
        drift_message(&diagnostic),
        format!("{DRIFT} HEAD moved. Changed primary paths: \"src/lib.rs\". {INSPECT}")
    );
}

#[test]
fn branch_switch_at_the_same_head_is_not_called_a_head_move() {
    let diagnostic = drift_diagnostic(
        ("aaa111", Some("main")),
        ("aaa111", Some("hotfix")),
        json!(["<branch-ref>"]),
    );
    assert_eq!(
        drift_message(&diagnostic),
        format!(
            "{DRIFT} HEAD did not move, but the branch changed. \
             Changed primary paths: \"<branch-ref>\". {INSPECT}"
        )
    );
}

#[test]
fn named_paths_are_bounded_in_count_and_length() {
    let named = ["p1", "p2", "p3", "p4", "p5"].map(|path| format!("\"{path}\""));
    let diagnostic = drift_diagnostic(
        ("aaa111", None),
        ("aaa111", None),
        json!(["p1", "p2", "p3", "p4", "p5", "p6", "p7"]),
    );
    assert_eq!(
        drift_message(&diagnostic),
        format!(
            "{DRIFT} HEAD did not move. Changed primary paths: {} and 2 more. {INSPECT}",
            named.join(", ")
        )
    );

    let cut = "a".repeat(120);
    let diagnostic = drift_diagnostic(("aaa111", None), ("aaa111", None), json!(["a".repeat(200)]));
    assert_eq!(
        drift_message(&diagnostic),
        format!("{DRIFT} HEAD did not move. Changed primary paths: \"{cut}...\". {INSPECT}")
    );
}

#[test]
fn long_paths_are_cut_at_a_character_boundary() {
    // The 120-byte mark falls inside the two-byte `é`, so the cut stops before it.
    let path = format!("{}é{}", "a".repeat(119), "b");
    let diagnostic = drift_diagnostic(("aaa111", None), ("aaa111", None), json!([path]));
    let cut = "a".repeat(119);
    assert_eq!(
        drift_message(&diagnostic),
        format!("{DRIFT} HEAD did not move. Changed primary paths: \"{cut}...\". {INSPECT}")
    );
}

#[test]
fn diagnostics_without_path_or_head_fields_keep_the_fixed_message() {
    let without_paths = json!({
        "primary_before": {"head": "aaa111", "branch": null},
        "primary_after": {"head": "bbb222", "branch": null},
    });
    assert_eq!(drift_message(&without_paths), fixed_drift_message());

    let without_summaries = json!({"primary_changed_paths": ["0"]});
    assert_eq!(drift_message(&without_summaries), fixed_drift_message());

    assert_eq!(drift_message(&Value::Null), fixed_drift_message());
}

#[test]
fn other_codes_keep_their_fixed_message() {
    let diagnostic = drift_diagnostic(("aaa111", None), ("bbb222", None), json!(["0"]));
    assert_eq!(
        worktree_failure_display_message("worktree_content_conflict", &diagnostic),
        CONTENT_CONFLICT
    );
}

#[test]
fn fixed_message_api_keeps_its_static_return_type_and_wording() {
    let diagnostic = drift_diagnostic(("aaa111", None), ("bbb222", None), json!(["0"]));
    let message: &'static str = worktree_failure_message("primary_checkout_drift", &diagnostic);
    assert_eq!(message, fixed_drift_message());
}
