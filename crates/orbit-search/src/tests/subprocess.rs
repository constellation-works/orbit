//! Unit tests for `subprocess` retry hygiene (ORB-10006), RPC-stream
//! resynchronization (ORB-11705) and RPC/Drop deadlines — sibling layout.

use std::time::Duration;

use crate::subprocess::{
    DEFAULT_RPC_TIMEOUT, retry_backoff_bound_ms, rpc_read_deadline, spawn_error_is_permanent,
};

#[test]
fn spawn_error_classification_table() {
    use std::io::{Error, ErrorKind};
    // Deterministic spawn failures are permanent; resource exhaustion and
    // anything unrecognized stays transient (conservative toward retrying).
    let table = [
        (ErrorKind::NotFound, true),
        (ErrorKind::PermissionDenied, true),
        (ErrorKind::WouldBlock, false),  // EAGAIN
        (ErrorKind::OutOfMemory, false), // ENOMEM
        (ErrorKind::Interrupted, false),
        (ErrorKind::Other, false),
    ];
    for (kind, expect_permanent) in table {
        assert_eq!(
            spawn_error_is_permanent(&Error::new(kind, "boom")),
            expect_permanent,
            "kind {kind:?} misclassified"
        );
    }
}

#[test]
fn retry_backoff_bound_doubles_and_saturates_at_cap() {
    let first = retry_backoff_bound_ms(1);
    let second = retry_backoff_bound_ms(2);
    assert_eq!(second, first * 2, "bound must double per attempt");
    let mut previous = 0;
    for attempt in 1..12 {
        let bound = retry_backoff_bound_ms(attempt);
        assert!(bound >= previous, "bound shrank at attempt {attempt}");
        previous = bound;
    }
    assert_eq!(previous, retry_backoff_bound_ms(30), "bound must saturate");
}

#[test]
fn rpc_read_deadline_scales_with_payload() {
    let small = rpc_read_deadline(Duration::from_secs(1), "x");
    let large = rpc_read_deadline(Duration::from_secs(1), &"y".repeat(4096));
    assert_eq!(small, Duration::from_secs(1));
    assert!(large > small, "4 KiB payload must add budget: {large:?}");
    assert_eq!(
        rpc_read_deadline(DEFAULT_RPC_TIMEOUT, ""),
        DEFAULT_RPC_TIMEOUT
    );
}

#[cfg(unix)]
mod fake_companion {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use orbit_common::OrbitError;

    use crate::embedder::Embedder;
    use crate::subprocess::{
        CompanionStderr, RPC_MAX_ATTEMPTS, SubprocessEmbedder, retry_backoff_bound_ms,
    };

    /// Shared JSON-Lines dispatcher for the fake companion. `$EMBED_BODY`
    /// is spliced per test to control embed behavior.
    fn write_companion_script(dir: &Path, embed_body: &str) -> PathBuf {
        let path = dir.join("fake-companion.sh");
        let script = format!(
            r#"#!/bin/sh
while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"info"'*)
      printf '{{"id":%s,"result":{{"model_id":"fake","dim":2,"max_input_tokens":16,"version":null}}}}\n' "$id" ;;
    *'"method":"embed"'*)
      {embed_body} ;;
    *'"method":"exit"'*)
      printf '{{"id":%s,"result":{{"ok":true}}}}\n' "$id"; exit 0 ;;
    *)
      printf '{{"id":%s,"error":{{"code":"bad_request","message":"unknown method"}}}}\n' "$id" ;;
  esac
done
"#
        );
        std::fs::write(&path, script).expect("write fake companion");
        chmod_executable(&path);
        path
    }

    #[test]
    fn transport_failure_respawns_companion_and_recovers() {
        // First-generation companion dies on the first embed request (EOF
        // mid-RPC — a transient transport failure). The embedder must
        // respawn it and replay the request; the second generation answers.
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("crashed-once");
        let embed_body = format!(
            r#"if [ ! -f "{marker}" ]; then touch "{marker}"; exit 1; fi
      printf '{{"id":%s,"result":{{"vectors":[[0.5,0.25]]}}}}\n' "$id""#,
            marker = marker.display()
        );
        let script = write_companion_script(temp.path(), &embed_body);

        let embedder =
            SubprocessEmbedder::with_path_and_model(script, "fake").expect("construct embedder");
        let vectors = embedder
            .embed(&["hello"])
            .expect("embed must succeed after respawn");
        assert_eq!(vectors, vec![vec![0.5, 0.25]]);
        assert!(marker.exists(), "first generation should have crashed");
    }

    #[test]
    fn companion_reported_error_is_permanent_and_not_retried() {
        // A companion-reported RPC error is deterministic — the embedder
        // must surface it immediately without burning respawn attempts.
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("embed-requests.log");
        let embed_body = format!(
            r#"printf 'x' >> "{log}"
      printf '{{"id":%s,"error":{{"code":"input_too_large","message":"nope"}}}}\n' "$id""#,
            log = log.display()
        );
        let script = write_companion_script(temp.path(), &embed_body);

        let embedder =
            SubprocessEmbedder::with_path_and_model(script, "fake").expect("construct embedder");
        let pid = embedder.child_id().expect("companion pid");
        let err = embedder
            .embed(&["hello"])
            .expect_err("companion error must surface");
        assert!(
            err.to_string().contains("input_too_large"),
            "error should carry the companion code: {err}"
        );
        assert!(
            err.to_string().contains("nope"),
            "error should carry the companion message: {err}"
        );
        let attempts = std::fs::read(&log).expect("embed log").len();
        assert_eq!(attempts, 1, "permanent RPC error must not be retried");
        assert_eq!(
            embedder.child_id().expect("companion pid"),
            pid,
            "an answered request must not discard the companion"
        );
    }

    #[test]
    fn stray_response_line_recovers_through_respawn() {
        // The first-generation companion prefixes its first embed answer with
        // one extra stdout line, exactly once across restarts. The stray line
        // desynchronizes the response stream — and leaves the real answer
        // queued behind it — so the embedder must discard that companion,
        // respawn, and answer from a clean stream. Later calls must see no
        // trace of the stale queue.
        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("stray-emitted");
        let embed_body = format!(
            r#"if [ ! -f "{marker}" ]; then touch "{marker}"; printf '{{"id":424242,"result":{{"ok":true}}}}\n'; fi
      printf '{{"id":%s,"result":{{"vectors":[[0.5,0.25]]}}}}\n' "$id""#,
            marker = marker.display()
        );
        let script = write_companion_script(temp.path(), &embed_body);

        let embedder =
            SubprocessEmbedder::with_path_and_model(script, "fake").expect("construct embedder");
        let first_pid = embedder.child_id().expect("companion pid");

        assert_eq!(
            embedder
                .embed(&["hello"])
                .expect("embed must recover from the stray line"),
            vec![vec![0.5, 0.25]]
        );
        assert!(marker.exists(), "first generation must emit the stray line");
        assert_ne!(
            embedder.child_id().expect("companion pid"),
            first_pid,
            "the desynchronized companion must be replaced, not reused"
        );

        assert_eq!(
            embedder
                .embed(&["world"])
                .expect("stream must stay clean after recovery"),
            vec![vec![0.5, 0.25]]
        );
    }

    #[test]
    fn unparseable_response_exhausts_the_retry_budget_without_orphans() {
        let err = assert_protocol_violation_exhausts_budget("printf 'this is not json\\n'");
        assert!(
            err.contains("unparseable") && err.contains("this is not json"),
            "error should quote the offending line: {err}"
        );
    }

    #[test]
    fn mismatched_response_id_exhausts_the_retry_budget_without_orphans() {
        let err = assert_protocol_violation_exhausts_budget(
            r#"printf '{"id":%s,"result":{"vectors":[[0.5,0.25]]}}\n' "$((id+1000))""#,
        );
        assert!(
            err.contains("response id"),
            "error should name the foreign response id: {err}"
        );
    }

    /// Drive a companion that violates the protocol on every embed and assert
    /// the shared contract: the retry budget is spent exactly once per
    /// attempt, the failure surfaces as a protocol violation naming the
    /// budget, and no child or reader is left behind. Returns the rendered
    /// error so each caller can check its own diagnosis.
    fn assert_protocol_violation_exhausts_budget(bad_response: &str) -> String {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("embed-requests.log");
        let embed_body = format!(
            r#"printf 'x' >> "{log}"
      {bad_response}"#,
            log = log.display()
        );
        let script = write_companion_script(temp.path(), &embed_body);
        let embedder = SubprocessEmbedder::with_path_model_stderr_and_timeouts(
            script.clone(),
            "fake",
            CompanionStderr::Suppress,
            Duration::from_millis(500),
            Duration::from_millis(200),
        )
        .expect("construct embedder");
        let first_pid = embedder.child_id().expect("companion pid");

        let err = embedder
            .embed(&["hello"])
            .expect_err("a persistent protocol violation must fail");
        assert!(
            matches!(err, OrbitError::AgentProtocolViolation(_)),
            "persistent violations must stay diagnosable as protocol violations: {err:?}"
        );
        let rendered = err.to_string();
        assert!(
            rendered.contains(&format!("after {RPC_MAX_ATTEMPTS} attempts")),
            "error should name the exhausted budget: {rendered}"
        );

        let attempts = std::fs::read(&log).expect("embed log").len();
        assert_eq!(
            attempts, RPC_MAX_ATTEMPTS as usize,
            "each attempt must respawn and retry exactly once"
        );
        // Every generation is reaped on its own failing attempt; a hung
        // reader thread would deadlock the join inside that reap.
        assert_no_companion_processes(&script, first_pid);
        rendered
    }

    #[test]
    fn missing_companion_binary_fails_without_retry_delay() {
        let started = std::time::Instant::now();
        let err = match SubprocessEmbedder::with_path_and_model(
            PathBuf::from("/nonexistent/orbit-fake-companion"),
            "fake",
        ) {
            Ok(_) => panic!("missing binary must fail"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("/nonexistent/orbit-fake-companion"),
            "error should name the binary: {err}"
        );
        // Permanent classification skips the backoff sleeps entirely.
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "ENOENT should fail fast, not retry"
        );
    }

    #[test]
    fn hung_embed_times_out_and_reaps_the_child() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = write_companion_script(temp.path(), "sleep 3600");
        let rpc_timeout = Duration::from_millis(300);
        let drop_timeout = Duration::from_millis(200);
        let embedder = SubprocessEmbedder::with_path_model_stderr_and_timeouts(
            script.clone(),
            "fake",
            CompanionStderr::Suppress,
            rpc_timeout,
            drop_timeout,
        )
        .expect("construct embedder");
        let pid = embedder.child_id().expect("companion pid");

        let started = Instant::now();
        let err = embedder
            .embed(&["hello"])
            .expect_err("wedged companion must surface an error");
        let elapsed = started.elapsed();
        let budget = rpc_timeout * RPC_MAX_ATTEMPTS
            + Duration::from_millis(retry_backoff_bound_ms(1) + retry_backoff_bound_ms(2))
            + Duration::from_secs(2);
        assert!(
            elapsed < budget,
            "embed hung for {elapsed:?} (budget {budget:?}): {err}"
        );
        assert!(
            err.to_string().contains("timed out"),
            "error should name the deadline: {err}"
        );
        assert_no_companion_processes(&script, pid);
    }

    #[test]
    fn drop_reaps_a_child_that_ignores_exit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = write_ignore_exit_companion(temp.path());
        let rpc_timeout = Duration::from_millis(300);
        let drop_timeout = Duration::from_millis(200);
        let embedder = SubprocessEmbedder::with_path_model_stderr_and_timeouts(
            script.clone(),
            "fake",
            CompanionStderr::Suppress,
            rpc_timeout,
            drop_timeout,
        )
        .expect("construct embedder");
        let pid = embedder.child_id().expect("companion pid");

        let started = Instant::now();
        drop(embedder);
        let elapsed = started.elapsed();
        let budget = drop_timeout + Duration::from_secs(2);
        assert!(
            elapsed < budget,
            "Drop hung for {elapsed:?} (budget {budget:?})"
        );
        assert_no_companion_processes(&script, pid);
    }

    fn write_ignore_exit_companion(dir: &Path) -> PathBuf {
        let path = dir.join("fake-companion.sh");
        let script = r#"#!/bin/sh
while read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"info"'*)
      printf '{"id":%s,"result":{"model_id":"fake","dim":2,"max_input_tokens":16,"version":null}}\n' "$id" ;;
    *'"method":"exit"'*)
      sleep 3600 ;;
    *)
      printf '{"id":%s,"error":{"code":"bad_request","message":"unknown method"}}\n' "$id" ;;
  esac
done
"#;
        std::fs::write(&path, script).expect("write ignore-exit companion");
        chmod_executable(&path);
        path
    }

    fn chmod_executable(path: &Path) {
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod fake companion");
    }

    fn assert_no_companion_processes(script: &Path, pgid: u32) {
        let pgid_arg = pgid.to_string();
        let group = pgrep_args(&["-g", &pgid_arg]);
        assert!(
            group.is_empty(),
            "process group {pgid} still has pids {group:?}"
        );
        let matching = pgrep_full_command(script);
        assert!(
            matching.is_empty(),
            "pgrep -f still sees companion {}: {matching:?}",
            script.display()
        );
    }

    fn pgrep_full_command(script: &Path) -> Vec<u32> {
        let pattern = script.to_str().expect("utf-8 companion path");
        pgrep_args(&["-f", pattern])
            .into_iter()
            .filter(|&pid| {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map(|comm| comm.trim() != "pgrep")
                    .unwrap_or(true)
            })
            .collect()
    }

    fn pgrep_args(args: &[&str]) -> Vec<u32> {
        let output = Command::new("pgrep").args(args).output().expect("pgrep");
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .filter_map(|pid| pid.parse().ok())
            .collect()
    }
}
