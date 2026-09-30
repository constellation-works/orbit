// Shared test helpers and utilities. Visible to child test submodules (subscriber, redaction)
// because they are descendants. These use private items of the parent logging module for
// test setup (e.g. jsonl_layer_at_path) which is allowed for submodules.

use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use tracing::Dispatch;
use tracing_subscriber::{
    Registry,
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
};

use super::super::logging::{
    RedactingFields, env_filter, jsonl_layer_at_path, stderr_ansi_enabled,
};

// Every environment mutation and every read that depends on it (log path
// resolution, rotation config the background writer loads from `HOME`) goes
// through `crate::test_env`'s process-wide guard. A module-local lock would
// not exclude sibling modules in this binary that also replace `HOME`, such
// as the home-directory redaction tests.

fn with_test_subscriber_at_path<W>(
    default_filter: &str,
    log_path: &Path,
    stderr_writer: W,
    f: impl FnOnce(),
) where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = env_filter(default_filter);
    let stderr_layer = fmt::layer()
        .with_writer(stderr_writer)
        .fmt_fields(RedactingFields::default());
    let (file_layer, guard) = jsonl_layer_at_path(log_path);
    let subscriber = Registry::default()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer);
    let dispatch = Dispatch::new(subscriber);
    tracing::dispatcher::with_default(&dispatch, f);
    drop(guard);
}

fn read_jsonl_values(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("read jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid json line"))
        .collect()
}

#[derive(Clone, Default)]
struct BufferMakeWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl BufferMakeWriter {
    fn buffer(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.buffer)
    }
}

impl<'writer> MakeWriter<'writer> for BufferMakeWriter {
    type Writer = BufferWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        BufferWriter {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

struct BufferWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer
            .lock()
            .expect("buffer lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

mod redaction {
    use std::{fmt, io};

    use tempfile::tempdir;

    use super::super::super::logging::*;
    use super::{BufferMakeWriter, read_jsonl_values, with_test_subscriber_at_path};
    use crate::test_env::{scoped, unset};

    #[derive(Debug)]
    struct SecretDisplayError;

    impl fmt::Display for SecretDisplayError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("request failed with Authorization: Bearer error-secret")
        }
    }

    impl std::error::Error for SecretDisplayError {}

    #[test]
    fn jsonl_redacting_fields_preserves_typed_values_and_redacts_strings() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(count = 42, ok = true, secret = "Authorization: Bearer abc");
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let fields = &values[0]["fields"];
        assert_eq!(fields["count"], 42);
        assert_eq!(fields["ok"], true);
        assert!(
            !fields["secret"]
                .as_str()
                .unwrap_or_default()
                .contains("abc")
        );
    }

    #[test]
    fn jsonl_redacting_fields_preserves_sensitive_field_names() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(password = "plain-public-value");
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["fields"]["password"], "plain-public-value");
    }

    #[test]
    fn jsonl_redacting_fields_redacts_unstructured_message() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!("Bearer abc123 leaked");
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let message = values[0]["fields"]["message"]
            .as_str()
            .expect("message is string");
        assert!(message.contains("[REDACTED_AUTH]"));
        assert!(!message.contains("abc123"));
    }

    #[test]
    fn jsonl_redacting_fields_redacts_debug_values() {
        struct Payload {
            header: &'static str,
        }

        impl std::fmt::Debug for Payload {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("Payload")
                    .field("header", &self.header)
                    .finish()
            }
        }

        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            let payload = Payload {
                header: "Authorization: Bearer abc456",
            };
            tracing::info!(payload = ?payload);
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let payload = values[0]["fields"]["payload"]
            .as_str()
            .expect("payload is string");
        assert!(payload.contains("[REDACTED_AUTH]"));
        assert!(!payload.contains("abc456"));
    }

    #[test]
    fn redacting_fields_redacts_bare_error_values() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");
        let stderr = BufferMakeWriter::default();
        let stderr_buffer = stderr.buffer();

        with_test_subscriber_at_path("info", &log_path, stderr, || {
            let error = SecretDisplayError;
            tracing::error!(
                error = &error as &(dyn std::error::Error + 'static),
                "operation failed"
            );
        });

        let stderr_text = String::from_utf8(stderr_buffer.lock().expect("stderr lock").clone())
            .expect("stderr utf8");
        assert!(stderr_text.contains("[REDACTED_AUTH]"));
        assert!(!stderr_text.contains("error-secret"));

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let error = values[0]["fields"]["error"]
            .as_str()
            .expect("error is string");
        assert!(error.contains("[REDACTED_AUTH]"));
        assert!(!error.contains("error-secret"));
    }

    #[test]
    fn jsonl_redacting_fields_redacts_byte_values() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            let payload = b"Authorization: Bearer byte-secret".as_slice();
            tracing::info!(payload = payload);
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let payload = values[0]["fields"]["payload"]
            .as_str()
            .expect("payload is string");
        assert!(payload.contains("[REDACTED_AUTH]"));
        assert!(!payload.contains("byte-secret"));
    }

    #[test]
    fn default_pattern_redactor_is_initialized_once() {
        let first = crate::security::redaction::default_pattern_redactor();
        let second = crate::security::redaction::default_pattern_redactor();

        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn redact_event_text_still_scrubs_sensitive_text() {
        let _env = scoped([("ORBIT_TEST_TOKEN", Some("super-secret-value"))]);

        let redacted = redact_event_text("token is super-secret-value");

        assert!(!redacted.contains("super-secret-value"));
        assert!(redacted.contains("[REDACTED_ENV]"));
    }
}

mod subscriber {
    use std::{fs, io};

    use regex::Regex;
    use serde_json::Value;
    use tempfile::tempdir;
    use tracing::Dispatch;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    use super::super::super::logging::JSONL_QUEUE_LINES;
    use super::{
        BufferMakeWriter, jsonl_layer_at_path, path_str, read_jsonl_values,
        with_test_subscriber_at_path,
    };
    use crate::test_env::{scoped, unset};

    #[test]
    fn jsonl_layer_honors_rust_log_filter() {
        let _env = scoped([("RUST_LOG", Some("orbit_common=debug"))]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("trace", &log_path, io::sink, || {
            tracing::debug!(target: "orbit_common::filter_probe", accepted = true);
            tracing::trace!(target: "orbit_common::filter_probe", rejected = true);
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["level"], "DEBUG");
        assert_eq!(values[0]["fields"]["accepted"], true);
        assert!(values[0]["fields"].get("rejected").is_none());
    }

    #[test]
    fn jsonl_event_contains_required_shape_and_fields() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(provider = "codex", stream = "stdout", line = "hi");
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let event = &values[0];
        let timestamp = event["timestamp"].as_str().expect("timestamp string");
        let timestamp_re =
            Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}").expect("valid regex");
        assert!(
            timestamp_re.is_match(timestamp),
            "timestamp should be ISO-like, got {timestamp}"
        );
        assert_eq!(event["level"], "INFO");
        assert!(event.get("target").is_some());
        assert_eq!(event["fields"]["provider"], "codex");
        assert_eq!(event["fields"]["stream"], "stdout");
        assert_eq!(event["fields"]["line"], "hi");
    }

    #[test]
    fn jsonl_event_preserves_cli_runner_structured_fields() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(
                provider = "codex",
                stream = "stderr",
                job_run_id = "jrun-123",
                task_id = "T20260426-2343",
                line = "hello"
            );
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        let fields = &values[0]["fields"];
        assert_eq!(fields["provider"], "codex");
        assert_eq!(fields["stream"], "stderr");
        assert_eq!(fields["job_run_id"], "jrun-123");
        assert_eq!(fields["task_id"], "T20260426-2343");
        assert_eq!(fields["line"], "hello");
    }

    #[test]
    fn jsonl_file_appends_to_existing_content() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");
        fs::write(&log_path, "sentinel\n").expect("write sentinel");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(line = "after-sentinel");
        });

        let content = fs::read_to_string(&log_path).expect("read log");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "sentinel");
        let appended: Value = serde_json::from_str(lines[1]).expect("appended line is json");
        assert_eq!(appended["fields"]["line"], "after-sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn jsonl_file_and_created_state_dirs_are_private() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let orbit_dir = dir.path().join(".orbit");
        let state_dir = orbit_dir.join("state");
        let log_dir = state_dir.join("logs");
        let log_path = log_dir.join("orbit.jsonl");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(line = "private-log");
        });

        assert_eq!(mode(&log_path), 0o600);
        assert_eq!(mode(&orbit_dir), 0o700);
        assert_eq!(mode(&state_dir), 0o700);
        assert_eq!(mode(&log_dir), 0o700);
    }

    #[test]
    fn jsonl_layer_keeps_a_burst_that_fits_its_queue_even_before_the_writer_starts() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");
        // Deterministic whatever the writer thread's scheduling: the whole
        // burst fits the queue, so nothing may be dropped.
        let burst = JSONL_QUEUE_LINES / 2;

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            for sequence in 0..burst {
                tracing::info!(sequence, "burst");
            }
        });

        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), burst);
        let last = values.last().expect("last event");
        assert_eq!(last["fields"]["sequence"], burst - 1);
    }

    #[test]
    fn jsonl_layer_does_not_create_or_open_the_file_until_the_first_event() {
        // Mutates nothing; holds the shared guard so no sibling swaps `HOME` mid-test.
        let _env = unset([]);
        let dir = tempdir().expect("tempdir");
        let log_dir = dir.path().join("logs");
        let log_path = log_dir.join("orbit.jsonl");

        let (file_layer, guard) = jsonl_layer_at_path(&log_path);
        let subscriber = Registry::default().with(file_layer);
        let dispatch = Dispatch::new(subscriber);
        tracing::dispatcher::with_default(&dispatch, || {});
        drop(guard);

        assert!(
            !log_dir.exists(),
            "lazy JSONL layer must not create the log directory at construction"
        );
        assert!(
            !log_path.exists(),
            "lazy JSONL layer must not open the active file at construction"
        );
    }

    #[test]
    fn first_jsonl_event_rolls_an_oversized_active_file() {
        let home = tempdir().expect("home");
        // The guard outlives `with_test_subscriber_at_path`, which joins the
        // background writer before returning, so the writer's rotation-config
        // read always sees this `HOME`.
        let _env = scoped([
            ("RUST_LOG", None),
            ("HOME", Some(path_str(home.path()))),
            ("USERPROFILE", Some(path_str(home.path()))),
        ]);
        fs::create_dir_all(home.path().join(".orbit")).expect("orbit dir");
        fs::write(
            home.path().join(".orbit/config.toml"),
            "[runtime]\nlog_retention_days = 7\nlog_max_total_mb = 10\nlog_max_file_mb = 1\n",
        )
        .expect("write config");

        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("orbit.jsonl");
        fs::write(&log_path, vec![b'x'; 2 * 1024 * 1024]).expect("oversized active file");

        with_test_subscriber_at_path("info", &log_path, io::sink, || {
            tracing::info!(line = "after-roll");
        });

        let archives: Vec<_> = fs::read_dir(dir.path())
            .expect("read log dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("orbit.jsonl."))
            .collect();
        assert_eq!(
            archives.len(),
            1,
            "oversized active file should roll to one archive, got {archives:?}"
        );
        let values = read_jsonl_values(&log_path);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["fields"]["line"], "after-roll");
    }

    #[test]
    fn file_layer_failure_falls_back_to_stderr_layer() {
        let _env = unset(["RUST_LOG"]);
        let dir = tempdir().expect("tempdir");
        let blocked_parent = dir.path().join("not-a-directory");
        fs::write(&blocked_parent, "file, not dir").expect("write blocking file");
        let log_path = blocked_parent.join("orbit.jsonl");
        let stderr = BufferMakeWriter::default();
        let stderr_buffer = stderr.buffer();

        with_test_subscriber_at_path("info", &log_path, stderr, || {
            tracing::info!(line = "stderr-still-works");
        });

        let stderr_text = String::from_utf8(stderr_buffer.lock().expect("stderr lock").clone())
            .expect("stderr utf8");
        assert!(stderr_text.contains("stderr-still-works"));
        assert!(
            !log_path.exists(),
            "a blocked parent must not produce a JSONL file"
        );
    }

    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }
}

mod stderr_style {
    use std::ffi::OsStr;

    use tracing::Dispatch;
    use tracing_subscriber::{Registry, fmt, layer::SubscriberExt};

    use super::{BufferMakeWriter, RedactingFields, stderr_ansi_enabled};

    fn logged_stderr(ansi: bool) -> String {
        let stderr = BufferMakeWriter::default();
        let buffer = stderr.buffer();
        let layer = fmt::layer()
            .with_writer(stderr)
            .with_ansi(ansi)
            .fmt_fields(RedactingFields::default());
        let dispatch = Dispatch::new(Registry::default().with(layer));
        tracing::dispatcher::with_default(&dispatch, || tracing::warn!(line = "styled?"));
        String::from_utf8(buffer.lock().expect("stderr lock").clone()).expect("stderr utf8")
    }

    #[test]
    fn only_a_terminal_without_no_color_gets_ansi_styling() {
        assert!(stderr_ansi_enabled(true, None));
        assert!(stderr_ansi_enabled(true, Some(OsStr::new(""))));
        assert!(!stderr_ansi_enabled(true, Some(OsStr::new("1"))));
        assert!(!stderr_ansi_enabled(false, None));
        assert!(!stderr_ansi_enabled(false, Some(OsStr::new(""))));
    }

    #[test]
    fn captured_stderr_carries_no_escape_sequences() {
        // The control: the layer does emit escapes when asked, so the assertion
        // below can fail.
        assert!(logged_stderr(true).contains('\u{1b}'));

        let captured = logged_stderr(stderr_ansi_enabled(false, None));
        assert!(captured.contains("styled?"));
        assert!(
            !captured.contains('\u{1b}'),
            "a non-terminal stderr (an MCP client's log file) must be plain text: {captured:?}"
        );
    }
}

mod path {
    use std::{path::PathBuf, sync::mpsc, thread, time::Duration};

    use tempfile::tempdir;

    use super::path_str;
    use crate::observability::logging::global_jsonl_log_path;
    use crate::security::redaction::redact_home_dir;
    use crate::test_env::scoped;

    #[test]
    fn managed_child_logs_use_the_registry_root_with_provider_home() {
        let provider_home = tempdir().expect("provider home");
        let registry_root = tempdir().expect("registry root");
        let _env = scoped([
            ("HOME", Some(path_str(provider_home.path()))),
            ("ORBIT_MANAGED_RUN_CONTEXT", Some("1")),
            ("ORBIT_RUN_ID", Some("jrun-logging-path")),
            ("ORBIT_REGISTRY_ROOT", Some(path_str(registry_root.path()))),
        ]);

        let expected = PathBuf::from(registry_root.path()).join("state/logs/orbit.jsonl");
        assert_eq!(global_jsonl_log_path().expect("resolve log path"), expected);
    }

    #[test]
    fn registry_root_is_ignored_without_managed_run_context() {
        let home = tempdir().expect("home");
        let registry_root = tempdir().expect("registry root");
        let _env = scoped([
            ("HOME", Some(path_str(home.path()))),
            ("ORBIT_MANAGED_RUN_CONTEXT", None),
            ("ORBIT_RUN_ID", None),
            ("ORBIT_REGISTRY_ROOT", Some(path_str(registry_root.path()))),
        ]);

        let expected = home.path().join(".orbit/state/logs/orbit.jsonl");
        assert_eq!(global_jsonl_log_path().expect("resolve log path"), expected);
    }

    /// Controlled overlap with the home-redaction fixture shape: a sibling
    /// thread that replaces `HOME` must wait for this test's scope, so path
    /// resolution only ever sees this test's temporary `HOME`, and the
    /// sibling sees only its own once it runs.
    #[test]
    fn log_path_resolution_excludes_a_concurrent_home_redaction_fixture() {
        let baseline_home = current_home();
        let home = tempdir().expect("home");
        let env = scoped([
            ("HOME", Some(path_str(home.path()))),
            ("ORBIT_MANAGED_RUN_CONTEXT", None),
            ("ORBIT_RUN_ID", None),
            ("ORBIT_REGISTRY_ROOT", None),
        ]);

        let (entered_tx, entered_rx) = mpsc::channel();
        let sibling = thread::spawn(move || {
            let _home = scoped([("HOME", Some("/Users/a"))]);
            entered_tx.send(()).expect("signal sibling entry");
            (
                redact_home_dir("/Users/ab/x"),
                redact_home_dir("/Users/a/x"),
            )
        });

        assert!(
            entered_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a sibling HOME fixture must not enter while this scope holds the environment"
        );
        let expected = home.path().join(".orbit/state/logs/orbit.jsonl");
        assert_eq!(global_jsonl_log_path().expect("resolve log path"), expected);

        drop(env);
        let (sibling_boundary, sibling_home) = sibling.join().expect("sibling fixture");
        assert_eq!(sibling_boundary, "/Users/ab/x");
        assert_eq!(sibling_home, "~/x");
        assert_eq!(
            current_home(),
            baseline_home,
            "both scopes must restore the HOME that preceded them"
        );
    }

    fn current_home() -> Option<String> {
        let _env = scoped(std::iter::empty());
        std::env::var("HOME").ok()
    }

    #[test]
    fn managed_registry_root_must_be_absolute() {
        let _env = scoped([
            ("ORBIT_MANAGED_RUN_CONTEXT", Some("true")),
            ("ORBIT_RUN_ID", Some("jrun-logging-path")),
            ("ORBIT_REGISTRY_ROOT", Some("relative-registry-root")),
        ]);

        let error = global_jsonl_log_path().expect_err("relative registry root must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
