use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

use crate::observability::logging::RedactingFields;
use crate::test_env::scoped;

#[test]
fn tracing_debug_field_redacts_secret_with_backslash_and_quote() {
    // [ORB-14120] A tracing `?` field is Debug-escaped before redaction. The
    // trailing BEL makes that body differ from the JSON-string body, so
    // collecting only the JSON form would leave the password in the event.
    let secret = "pa\\ss\"word99\u{7}";
    let _env = scoped([("DB_PASSWORD", Some(secret))]);
    let logs = capture_redacted_info(|| {
        tracing::info!(credential = ?secret, "emitted");
    });
    let debug_body = debug_string_body(secret);
    let json_body = json_string_body(secret);

    assert_ne!(
        debug_body, json_body,
        "fixture must make the Debug and JSON encodings differ"
    );
    assert!(
        logs.contains("[REDACTED_ENV]"),
        "debug-escaped secret was not redacted: {logs}"
    );
    assert!(
        !logs.contains(&debug_body),
        "debug-escaped secret survived tracing ? field: {logs}"
    );
    assert!(
        !logs.contains(secret),
        "raw secret survived tracing ? field: {logs}"
    );
}

fn capture_redacted_info(emit: impl FnOnce()) -> String {
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
        .with_max_level(tracing::Level::TRACE)
        .fmt_fields(RedactingFields::text())
        .with_writer(Capture(Arc::clone(&buffer)))
        .with_ansi(false)
        .without_time()
        .finish();
    tracing::subscriber::with_default(subscriber, emit);
    String::from_utf8(buffer.lock().expect("capture").clone()).expect("utf8 logs")
}

fn json_string_body(value: &str) -> String {
    let encoded = serde_json::to_string(value).expect("json string");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("json quotes")
        .to_string()
}

fn debug_string_body(value: &str) -> String {
    let encoded = format!("{value:?}");
    encoded
        .strip_prefix('"')
        .and_then(|body| body.strip_suffix('"'))
        .expect("debug quotes")
        .to_string()
}
