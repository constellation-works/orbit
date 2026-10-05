//! Review preferences and the translation of deprecated review keys
//! [ORB-13992].

use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use super::{roots, write_config};
use crate::ResolvedConfig;
use crate::operation::OperationLayerSource;
use crate::registry::admit_settable_config_key;

/// Load `workspace` over an empty global layer, returning the resolved
/// config beside every `tracing` event the load wrote.
fn load_workspace(workspace_body: &str) -> (ResolvedConfig, String) {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(workspace.path(), workspace_body);
    let (config, logs) =
        capture_logs(|| ResolvedConfig::load(&roots(global.path(), workspace.path())));
    (config.expect("workspace config loads"), logs)
}

fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
    use std::io::{self, Write};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&buffer)))
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs = String::from_utf8(buffer.lock().expect("capture").clone()).expect("utf8 logs");
    (result, logs)
}

fn warned_deprecated(logs: &str, key: &str) -> bool {
    logs.lines()
        .any(|line| line.contains(key) && line.contains("deprecated and translated"))
}

fn warned_removed(logs: &str, key: &str) -> bool {
    logs.lines()
        .any(|line| line.contains(key) && line.contains("removed and ignored"))
}

#[test]
fn review_table_sets_before_pr_and_minutes_with_their_layer() {
    let (config, logs) = load_workspace("[review]\nbefore_pr = true\nminutes = 45\n");

    let review = &config.operation;
    assert!(review.review_before_pr.value);
    assert_eq!(
        review.review_before_pr.source,
        OperationLayerSource::Workspace
    );
    assert_eq!(review.review_minutes.value, 45);
    assert_eq!(review.review_budget().minutes, 45);
    assert_eq!(review.legacy_after_landing, None);
    assert!(
        !logs.contains("deprecated"),
        "no legacy key, no warning: {logs}"
    );
}

#[test]
fn omitted_review_table_is_off_with_the_default_minutes() {
    let (config, _) = load_workspace("[scoring]\nenabled = false\n");

    assert!(!config.operation.review_before_pr.value);
    assert_eq!(
        config.operation.review_before_pr.source,
        OperationLayerSource::BuiltIn
    );
    assert_eq!(config.operation.review_minutes.value, 30);
}

#[test]
fn legacy_before_pr_policy_turns_before_pr_on_with_a_deprecation_warning() {
    let (config, logs) = load_workspace("[operation]\nreview_policy = \"before-pr\"\n");

    assert!(config.operation.review_before_pr.value);
    assert_eq!(
        config.operation.review_before_pr.source,
        OperationLayerSource::Workspace
    );
    // The snapshot `orbit config get` reads sees the translation too.
    assert!(config.snapshot.review_before_pr);
    assert_eq!(config.operation.legacy_after_landing, None);
    assert!(
        warned_deprecated(&logs, "operation.review_policy"),
        "legacy policy is warned as deprecated: {logs}"
    );
}

#[test]
fn legacy_after_landing_policy_asks_for_the_auto_task_not_before_pr() {
    let (config, logs) = load_workspace("[operation]\nreview_policy = \"after-landing\"\n");

    assert!(!config.operation.review_before_pr.value);
    assert_eq!(
        config.operation.legacy_after_landing,
        Some(OperationLayerSource::Workspace)
    );
    assert!(
        warned_deprecated(&logs, "operation.review_policy"),
        "{logs}"
    );
}

#[test]
fn legacy_none_policy_turns_neither_on() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    write_config(
        global.path(),
        "[operation]\nreview_policy = \"after-landing\"\n",
    );
    write_config(workspace.path(), "[operation]\nreview_policy = \"none\"\n");

    let config = ResolvedConfig::load(&roots(global.path(), workspace.path())).expect("loads");

    assert!(!config.operation.review_before_pr.value);
    assert_eq!(
        config.operation.legacy_after_landing, None,
        "the workspace's legacy `none` overrides the global after-landing, as the enum did"
    );
}

#[test]
fn review_table_wins_over_its_legacy_spelling_in_the_same_file() {
    let (config, _) = load_workspace(
        "[review]\nbefore_pr = false\nminutes = 20\n[operation]\nreview_policy = \"before-pr\"\nreview_minutes = 90\n",
    );

    assert!(!config.operation.review_before_pr.value);
    assert_eq!(config.operation.review_minutes.value, 20);
}

/// The ws_orbit config on the box when the enum was retired (on-call
/// 2026-10-04): it must keep loading, keep its 120 minutes, and name every
/// legacy key in a warning.
#[test]
fn box_ws_orbit_review_config_migrates_with_warnings() {
    let (config, logs) = load_workspace(
        r#"[operation]
review_crew = "grok"
review_policy = "none"
review_reviewer_starts = 3
review_repair_cycles = 3
review_minutes = 120
"#,
    );

    let review = &config.operation;
    assert!(!review.review_before_pr.value);
    assert_eq!(review.review_minutes.value, 120);
    assert_eq!(
        review.review_minutes.source,
        OperationLayerSource::Workspace
    );
    assert_eq!(review.review_crew.value.as_deref(), Some("grok"));
    assert_eq!(review.legacy_after_landing, None);
    assert_eq!(config.snapshot.review_minutes, 120);
    for key in ["operation.review_policy", "operation.review_minutes"] {
        assert!(warned_deprecated(&logs, key), "{key} warned: {logs}");
    }
    for key in [
        "operation.review_reviewer_starts",
        "operation.review_repair_cycles",
    ] {
        assert!(
            warned_removed(&logs, key),
            "{key} ignored with a warning: {logs}"
        );
    }
}

#[test]
fn retired_review_keys_are_not_settable() {
    for key in [
        "operation.review_policy",
        "operation.review_minutes",
        "operation.review_reviewer_starts",
        "operation.review_repair_cycles",
    ] {
        let error = admit_settable_config_key(key).expect_err("retired key refused");
        assert!(
            error.to_string().contains("review.") || error.to_string().contains("[review]"),
            "{key} names its replacement: {error}"
        );
    }
    admit_settable_config_key("review.before_pr").expect("live key");
    admit_settable_config_key("review.minutes").expect("live key");
}

#[test]
fn invalid_legacy_values_still_fail_closed() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    for body in [
        "[operation]\nreview_policy = \"before_pr\"\n",
        "[operation]\nreview_minutes = 0\n",
        "[review]\nbefore_pr = true\nreviewers = 2\n",
    ] {
        write_config(workspace.path(), body);
        assert!(
            ResolvedConfig::load(&roots(global.path(), workspace.path())).is_err(),
            "refused: {body}"
        );
    }
}
