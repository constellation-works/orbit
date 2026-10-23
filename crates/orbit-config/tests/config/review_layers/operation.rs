//! Review preferences and the translation of deprecated review keys
//! [ORB-13992].

use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use super::review_layers_write_config;
use orbit_config::ConfigRoots;

use orbit_config::OperationLayerSource;
use orbit_config::ResolvedConfig;
use orbit_config::admit_settable_config_key;

/// Load `workspace` over an empty global layer, returning the resolved
/// config beside every `tracing` event the load wrote.
fn load_workspace(workspace_body: &str) -> (ResolvedConfig, String) {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    review_layers_write_config(workspace.path(), workspace_body);
    let (config, logs) =
        capture_logs(|| ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())));
    (config.expect("workspace config loads"), logs)
}

pub(super) fn capture_logs<T>(f: impl FnOnce() -> T) -> (T, String) {
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

pub(super) fn warned_deprecated(logs: &str, key: &str) -> bool {
    logs.lines()
        .any(|line| line.contains(key) && line.contains("config key is deprecated"))
}

fn warned_removed(logs: &str, key: &str) -> bool {
    logs.lines()
        .any(|line| line.contains(key) && line.contains("removed and ignored"))
}

#[test]
fn review_table_sets_timing_and_minutes_with_their_layer() {
    let (config, logs) = load_workspace("[review]\ntiming = \"before_pr\"\nminutes = 45\n");

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
fn legacy_before_pr_policy_turns_before_pr_on_with_a_deprecation_warning() {
    let (config, logs) = load_workspace("[operation]\nreview_policy = \"before-pr\"\n");

    assert!(config.operation.review_before_pr.value);
    assert_eq!(
        config.operation.review_before_pr.source,
        OperationLayerSource::Workspace
    );
    // The snapshot `orbit config get` reads sees the translation too.
    assert_eq!(
        config.snapshot.review_timing(),
        orbit_config::ReviewTimingSetting::BeforePr
    );
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
    review_layers_write_config(
        global.path(),
        "[operation]\nreview_policy = \"after-landing\"\n",
    );
    review_layers_write_config(workspace.path(), "[operation]\nreview_policy = \"none\"\n");

    let config =
        ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())).expect("loads");

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
    assert_eq!(review.review_crew.value, vec!["grok".to_string()]);
    assert_eq!(review.legacy_after_landing, None);
    assert_eq!(config.snapshot.review_minutes, 120);
    for key in [
        "operation.review_policy",
        "operation.review_minutes",
        "operation.review_crew",
    ] {
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

/// A config written before the scoreboard was retired keeps loading until the
/// key's sunset: `[scoring]` is warned and ignored, and edits are refused.
#[test]
fn retired_scoring_table_loads_with_a_warning_and_refuses_edits() {
    let (_config, logs) = load_workspace("[scoring]\nenabled = false\n");
    assert!(
        warned_removed(&logs, "scoring.enabled"),
        "scoring.enabled ignored with a warning: {logs}"
    );
    let error = admit_settable_config_key("scoring.enabled").expect_err("retired key refused");
    assert!(
        error.to_string().contains("scoreboard was retired"),
        "the refusal says why: {error}"
    );
}

#[test]
fn retired_review_keys_are_not_settable() {
    for key in [
        "operation.review_policy",
        "operation.review_minutes",
        "operation.review_reviewer_starts",
        "operation.review_repair_cycles",
        "operation.review_crew",
        "review.before_pr",
        "review.before_landing",
    ] {
        let error = admit_settable_config_key(key).expect_err("retired key refused");
        assert!(
            error.to_string().contains("review.")
                || error.to_string().contains("[review]")
                || error.to_string().contains("workflow.purpose_crews.review"),
            "{key} names its replacement: {error}"
        );
    }
    admit_settable_config_key("review.timing").expect("live key");
    admit_settable_config_key("workflow.purpose_crews.review").expect("live key");
    admit_settable_config_key("review.minutes").expect("live key");
}

#[test]
fn invalid_legacy_values_still_fail_closed() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    for body in [
        "[operation]\nreview_policy = \"before_pr\"\n",
        "[operation]\nreview_minutes = 0\n",
    ] {
        review_layers_write_config(workspace.path(), body);
        assert!(
            ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())).is_err(),
            "refused: {body}"
        );
    }
}

const CODEQL_RULE: &str = "[[review.host_evidence]]\n\
    kind = \"codeql\"\n\
    name = \"Rust CodeQL (Linux)\"\n\
    paths = [\"**/*.rs\"]\n\
    os = \"linux\"\n\
    command = \"scripts/codeql-rust-local.sh --ram 16384 codeql/rust-queries:codeql-suites/rust-security-extended.qls\"\n\
    artifact = \"evidence/codeql-rust-linux.json\"\n";

#[test]
fn host_evidence_rules_load_and_a_workspace_list_replaces_the_global_one() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    review_layers_write_config(
        global.path(),
        "[[review.host_evidence]]\nkind = \"host_sandbox_test\"\nname = \"Seatbelt\"\n\
         paths = [\"crates/orbit-exec/**\"]\nos = \"macos\"\n\
         command = \"cargo test -p orbit-exec --test seatbelt\"\n\
         artifact = \"evidence/seatbelt-macos.json\"\n",
    );
    review_layers_write_config(workspace.path(), CODEQL_RULE);

    let config = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("the rules load");
    let rules = &config.operation.review_host_evidence;
    assert_eq!(rules.source, OperationLayerSource::Workspace);
    assert_eq!(rules.value.len(), 1, "{rules:?}");
    assert_eq!(rules.value[0].name, "Rust CodeQL (Linux)");
    assert_eq!(
        rules.value[0].kind,
        orbit_types::workflow::ReviewEvidenceKind::CodeQl
    );
    assert_eq!(
        rules.value[0].os,
        orbit_types::workflow::EvidenceHostOs::Linux
    );

    assert_eq!(rules.value[0].hosted_check, None);

    review_layers_write_config(
        workspace.path(),
        &format!("{CODEQL_RULE}hosted_check = \" CodeQL / Analyze (rust) \"\n"),
    );
    let declared = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("a linux codeql rule may name its hosted check");
    assert_eq!(
        declared.operation.review_host_evidence.value[0]
            .hosted_check
            .as_deref(),
        Some("CodeQL / Analyze (rust)")
    );

    let (unset, _) = load_workspace("[review]\nbefore_pr = true\n");
    assert!(unset.operation.review_host_evidence.value.is_empty());
}

#[test]
fn host_evidence_rules_orbit_could_never_fulfil_fail_the_load() {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    let rule = |kind: &str, paths: &str, command: &str, artifact: &str| {
        format!(
            "[[review.host_evidence]]\nkind = \"{kind}\"\nname = \"Check\"\npaths = {paths}\n\
             os = \"linux\"\ncommand = \"{command}\"\nartifact = \"{artifact}\"\n"
        )
    };
    for body in [
        rule("hosted_ci", "[\"**/*.rs\"]", "ci", "evidence/ci.json"),
        rule(
            "codeql",
            "[]",
            "scripts/codeql-rust-local.sh",
            "evidence/a.json",
        ),
        rule("codeql", "[\"**/*.rs\"]", " ", "evidence/a.json"),
        rule(
            "codeql",
            "[\"**/*.rs\"]",
            "scripts/codeql-rust-local.sh",
            "evidence/a.log",
        ),
        rule(
            "codeql",
            "[\"**/*.rs\"]",
            "scripts/codeql-rust-local.sh",
            "../a.json",
        ),
        rule(
            "host_sandbox_test",
            "[\"**/*.rs\"]",
            "cargo test -p x --test y; rm -r x",
            "evidence/a.json",
        ),
        format!(
            "{}{}",
            rule(
                "codeql",
                "[\"**/*.rs\"]",
                "scripts/codeql-rust-local.sh",
                "evidence/a.json"
            ),
            rule(
                "codeql",
                "[\"**/*.rs\"]",
                "scripts/codeql-rust-local.sh",
                "evidence/a.json"
            ),
        ),
        format!("{CODEQL_RULE}timeout = 5\n"),
        format!("{CODEQL_RULE}hosted_check = \" \"\n"),
        format!(
            "{}hosted_check = \"CI\"\n",
            rule(
                "host_sandbox_test",
                "[\"**/*.rs\"]",
                "cargo test -p x --test y",
                "evidence/a.json"
            )
        ),
    ] {
        review_layers_write_config(workspace.path(), &body);
        assert!(
            ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path())).is_err(),
            "refused: {body}"
        );
    }
}
