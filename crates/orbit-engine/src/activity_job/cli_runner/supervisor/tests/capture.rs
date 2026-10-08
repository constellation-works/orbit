use std::time::Duration;

use tempfile::tempdir;

use super::super::{SpawnWithTimeoutRequest, spawn_with_timeout};
use super::test_support::{spawn_test_request, stdin_trace};

/// The supervisor's private capture boundary must be exercised before the
/// blob store can redact it; public runtime fixtures cannot control pipe cuts.
#[test]
fn output_capture_redacts_boundary_tokens_before_blob_storage() {
    use orbit_common::security::redaction::{argv_redactor, redact_all};
    use orbit_common::storage::blob_store::BlobStore;

    use super::super::super::stdout_preview::bounded_redacted_text;

    const LIMIT: usize = 1024 * 1024;
    const SECRET: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    const MARKER: &str = "[REDACTED_SECRET]";
    let args = Vec::new();
    let dir = tempdir().expect("blob fixture");
    let blobs = BlobStore::new(dir.path());
    let protocol = b"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}\n";

    // Oversized captures keep half the limit as a diagnostic prefix. The
    // second case also plants a token across the total capture threshold.
    for cut in [LIMIT / 2, LIMIT] {
        let mut raw = vec![b'.'; cut - 20];
        *raw.last_mut().expect("padding") = b'\n';
        raw.extend_from_slice(SECRET.as_bytes());
        raw.push(b'\n');
        raw.resize(LIMIT + 8192, b'.');
        raw.push(b'\n');
        raw.extend_from_slice(protocol);
        let (stdout, stderr, code, _, timed_out) = spawn_with_timeout(SpawnWithTimeoutRequest {
            stdin_bytes: &raw,
            output_capture_limit: Some(LIMIT),
            ..spawn_test_request(
                "/bin/cat",
                &args,
                None,
                Duration::from_secs(10),
                stdin_trace(),
            )
        })
        .expect("capture boundary output");
        assert_eq!(code, Some(0));
        assert!(!timed_out);
        assert!(stderr.bytes().is_empty());
        assert!(stdout.truncated());
        assert_eq!(stdout.observed_bytes(), raw.len());
        assert_eq!(stdout.capture_limit_bytes(), LIMIT);
        assert!(stdout.bytes().len() <= LIMIT + 128);
        assert!(
            stdout.protocol_bytes().ends_with(protocol),
            "preserve raw final protocol frame"
        );

        let hash = blobs.write(stdout.bytes()).expect("store capture");
        let stored = blobs.read(&hash).expect("read capture");
        assert!(String::from_utf8_lossy(&stored).contains(MARKER));
        let fragment = &SECRET[..MARKER.len() + 1];
        let diagnostic = String::from_utf8_lossy(stdout.bytes());
        for (surface, bytes) in [
            (
                "diagnostic prefix",
                stdout.bytes()[..stdout.bytes().len() - stdout.protocol_bytes().len()].to_vec(),
            ),
            ("blob", stored.clone()),
            (
                "blob prefix",
                blobs.read_prefix(&hash, cut).expect("blob preview"),
            ),
            (
                "head preview",
                bounded_redacted_text(&diagnostic, argv_redactor(), false, cut)
                    .text
                    .into_bytes(),
            ),
            (
                "tail preview",
                bounded_redacted_text(&diagnostic, argv_redactor(), true, cut)
                    .text
                    .into_bytes(),
            ),
        ] {
            assert!(
                !bytes
                    .windows(fragment.len())
                    .any(|window| window == fragment.as_bytes()),
                "ORB-14575: {surface} must not retain a token prefix longer than its marker (cut={cut})"
            );
        }
    }

    // At or below the cap even invalid UTF-8 and secret-shaped bytes retain
    // the original protocol/capture representation. Blob redaction remains
    // exactly the existing whole-input redaction, including lossy decoding.
    for len in [0, 64, LIMIT] {
        let mut raw = vec![b'.'; len];
        if len > SECRET.len() {
            raw[..SECRET.len()].copy_from_slice(SECRET.as_bytes());
            raw[SECRET.len()] = 0xff;
        }
        let (stdout, _, code, _, timed_out) = spawn_with_timeout(SpawnWithTimeoutRequest {
            stdin_bytes: &raw,
            output_capture_limit: Some(LIMIT),
            ..spawn_test_request(
                "/bin/cat",
                &args,
                None,
                Duration::from_secs(10),
                stdin_trace(),
            )
        })
        .expect("capture under limit");
        assert_eq!(code, Some(0));
        assert!(!timed_out);
        assert!(!stdout.truncated());
        assert_eq!(stdout.bytes(), raw);
        assert_eq!(stdout.protocol_bytes(), raw);
        let hash = blobs
            .write(stdout.bytes())
            .expect("store under-limit output");
        assert_eq!(
            blobs.read(&hash).expect("read under-limit output"),
            redact_all(&String::from_utf8_lossy(&raw)).into_bytes()
        );
    }
}
