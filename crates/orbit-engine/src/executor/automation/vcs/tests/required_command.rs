//! Missing-tool classification is combinatorial pure logic over shell,
//! `make`, cargo and guardrail diagnostics; a wrong answer either repairs code
//! that has no defect or hides a real failure behind "install a tool"
//! [ORB-13987]. Capture keeps a head and a tail of each stream so a stdout
//! longer than the cap cannot discard the stderr failure before that
//! classification [ORB-14086].

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::super::required_command::{RequiredCommandRun, missing_tool, run_required_command};
use crate::RuntimeHost;

const DIAGNOSTIC: &str = "make: rg: No such file or directory";
/// Larger than the 256 KiB capture cap, so a prefix-only capture drops the tail.
const OVERSIZED_BYTES: usize = 400 * 1024;

const EMPTY_PATH: &str = "/nonexistent-orbit-path";

fn tool(exit_code: i32, output: &str) -> Option<Option<String>> {
    missing_tool(Some(exit_code), output, EMPTY_PATH).map(|missing| missing.tool)
}

#[test]
fn shell_and_launcher_diagnostics_name_the_missing_tool() {
    for (exit_code, output, expected) in [
        (127, "/bin/sh: 1: rg: not found", "rg"),
        (127, "bash: line 1: cargo: command not found", "cargo"),
        (
            2,
            "scripts/ci.sh: line 21: jq: command not found\nmake: *** [ci] Error 2",
            "jq",
        ),
        (127, "zsh: command not found: node", "node"),
        (
            2,
            "make: taplo: No such file or directory\nmake: *** [fmt] Error 127",
            "taplo",
        ),
        (127, "env: 'python3': No such file or directory", "python3"),
        (101, "error: no such command: `nextest`", "cargo-nextest"),
        (
            2,
            "ci-guardrails: ripgrep (rg) is required; install it before running\nmake: *** [ci-fast] Error 1",
            "rg",
        ),
        (
            1,
            "Error: jq is required. Install with: brew install jq",
            "jq",
        ),
    ] {
        assert_eq!(
            tool(exit_code, output),
            Some(Some(expected.to_string())),
            "{output}"
        );
    }
}

#[test]
fn an_unnamed_exit_127_is_still_an_environment_failure() {
    assert_eq!(tool(127, "something went wrong"), Some(None));
    assert_eq!(
        tool(2, "make: *** [Makefile:3: ci] Error 127"),
        Some(None),
        "make reports a recipe whose command was not found"
    );
}

#[test]
fn ordinary_check_failures_are_candidate_failures() {
    for (exit_code, output) in [
        (
            1,
            "test tests::parses ... FAILED\nassertion failed: config.toml: not found",
        ),
        (1, "error: field `name` is required"),
        (2, "src/feature.txt: not formatted"),
        (
            101,
            "thread 'main' panicked at src/lib.rs:3:5:\nworkspace_path is required",
        ),
        (1, "cat: missing.txt: No such file or directory"),
    ] {
        assert_eq!(tool(exit_code, output), None, "{output}");
    }
}

#[test]
fn a_guardrail_for_a_tool_on_path_is_not_a_missing_tool() {
    let output = "check: sh (sh) is required; install it before running";
    assert!(missing_tool(Some(1), output, "/bin:/usr/bin").is_none());
    assert!(missing_tool(Some(1), output, EMPTY_PATH).is_some());
}

struct CaptureHost;

impl RuntimeHost for CaptureHost {}

/// A stdout past 256 KiB used to fill the capture and delete stderr, so exit 2
/// was classified `candidate` and repair read the middle of stdout
/// [ORB-14086]. The recorded text keeps each stream's head and tail instead.
#[test]
fn oversized_stdout_keeps_the_stderr_tail_and_names_the_missing_tool() {
    let dir = tempfile::tempdir().expect("temp dir");
    let under_cap = run_in(
        dir.path(),
        "printf '%s\\n' hello; printf '%s\\n' 'make: rg: No such file or directory' >&2; exit 2",
    );
    assert_environment(&under_cap, "under the cap");
    assert_eq!(
        captured_body(&under_cap.output),
        format!("hello\n{DIAGNOSTIC}")
    );

    let stdout = oversized_stream("STDOUT_HEAD", "STDOUT_MIDDLE", "STDOUT_TAIL");
    let small_stderr = format!("{DIAGNOSTIC}\n");
    write_streams(&dir, &stdout, &small_stderr);
    let small = run_in(
        dir.path(),
        "/bin/cat stdout.txt; /bin/cat stderr.txt >&2; exit 2",
    );
    assert_environment(&small, "stdout over the cap");
    assert_stream_bounds(
        &small,
        "STDOUT_HEAD",
        "STDOUT_MIDDLE",
        "STDOUT_TAIL",
        None,
        None,
    );
    assert!(
        captured_body(&small.output).ends_with(&format!("STDOUT_TAIL\n{DIAGNOSTIC}")),
        "a short stderr is kept whole after the stdout tail: {}",
        captured_body(&small.output)
    );

    let stderr = oversized_stream("STDERR_HEAD", "STDERR_MIDDLE", &format!("\n{DIAGNOSTIC}"));
    write_streams(&dir, &stdout, &stderr);
    let both = run_in(
        dir.path(),
        "/bin/cat stdout.txt; /bin/cat stderr.txt >&2; exit 2",
    );
    assert_environment(&both, "both streams over the cap");
    assert_stream_bounds(
        &both,
        "STDOUT_HEAD",
        "STDOUT_MIDDLE",
        "STDOUT_TAIL",
        Some("STDERR_HEAD"),
        Some("STDERR_MIDDLE"),
    );
}

fn assert_environment(run: &RequiredCommandRun, label: &str) {
    assert_eq!(run.exit_code, 2, "{label}: {}", run.output);
    assert!(!run.passed, "{label}");
    assert!(!run.timed_out, "{label}");
    assert_eq!(
        run.failure_kind(),
        json!("environment"),
        "{label}: {}",
        run.output
    );
    assert_eq!(
        run.missing_tool_name(),
        Some("rg"),
        "{label}: {}",
        run.output
    );
    let body = captured_body(&run.output);
    assert!(
        body.ends_with(DIAGNOSTIC),
        "{label}: stderr tail missing from recorded output: {body}"
    );
    assert!(
        body.len() <= 256 * 1024 + 512,
        "{label}: recorded body is {} bytes",
        body.len()
    );
    let diag_at = body.rfind(DIAGNOSTIC).expect("diagnostic");
    assert!(
        run.output.len() - diag_at <= 32 * 1024,
        "{label}: candidate_resume reads the last 32 KiB, but the diagnostic is {} bytes from the end",
        run.output.len() - diag_at
    );
}

fn assert_stream_bounds(
    run: &RequiredCommandRun,
    head: &str,
    middle: &str,
    tail: &str,
    stderr_head: Option<&str>,
    stderr_middle: Option<&str>,
) {
    let body = captured_body(&run.output);
    let head_at = body
        .find(head)
        .unwrap_or_else(|| panic!("missing {head}: {body}"));
    let tail_at = body
        .find(tail)
        .unwrap_or_else(|| panic!("missing {tail}: {body}"));
    let diag_at = body.rfind(DIAGNOSTIC).expect("diagnostic");
    assert!(head_at < tail_at && tail_at < diag_at, "{body}");
    assert!(
        !body.contains(middle),
        "omitted middle of stdout survived: {body}"
    );
    if let Some(stderr_head) = stderr_head {
        let stderr_at = body
            .find(stderr_head)
            .unwrap_or_else(|| panic!("missing {stderr_head}: {body}"));
        assert!(tail_at < stderr_at && stderr_at < diag_at, "{body}");
    }
    if let Some(stderr_middle) = stderr_middle {
        assert!(
            !body.contains(stderr_middle),
            "omitted middle of stderr survived: {body}"
        );
    }
}

fn captured_body(output: &str) -> &str {
    output
        .rsplit_once("\nRequired validation PATH=")
        .map(|(body, _path)| body)
        .unwrap_or(output)
}

fn run_in(dir: &Path, command: &str) -> RequiredCommandRun {
    run_required_command(&CaptureHost, dir, command, None).expect(command)
}

fn write_streams(dir: &TempDir, stdout: &str, stderr: &str) {
    std::fs::write(dir.path().join("stdout.txt"), stdout).expect("stdout");
    std::fs::write(dir.path().join("stderr.txt"), stderr).expect("stderr");
}

/// `total` bytes with `head` at the start, `middle` at the midpoint, and
/// `tail` at the end. The midpoint sits in the span a half-and-half capture
/// omits once `total` exceeds the stream's share of the 256 KiB cap.
fn oversized_stream(head: &str, middle: &str, tail: &str) -> String {
    assert!(head.len() + middle.len() + tail.len() + 2 < OVERSIZED_BYTES);
    let middle_at = OVERSIZED_BYTES / 2;
    assert!(middle_at >= head.len());
    assert!(middle_at + middle.len() <= OVERSIZED_BYTES - tail.len());
    let mut out = String::with_capacity(OVERSIZED_BYTES);
    out.push_str(head);
    let before_middle = middle_at - out.len();
    push_filler(&mut out, before_middle);
    out.push_str(middle);
    let before_tail = OVERSIZED_BYTES - tail.len() - out.len();
    push_filler(&mut out, before_tail);
    out.push_str(tail);
    assert_eq!(out.len(), OVERSIZED_BYTES);
    out
}

fn push_filler(out: &mut String, nbytes: usize) {
    let pairs = nbytes / 2;
    out.reserve(nbytes);
    out.extend(std::iter::repeat_n('é', pairs));
    if nbytes % 2 == 1 {
        out.push('X');
    }
}
