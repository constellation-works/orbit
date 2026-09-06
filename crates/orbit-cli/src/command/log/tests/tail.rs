use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use serde_json::json;
use tempfile::tempdir;

use crate::command::log::format::{LevelFilter, format_message};
use crate::command::log::tail::{
    FollowTestControl, MatchingLineWindow, TailArgs, build_filters, run_tail,
    run_tail_with_test_control,
};

fn fixture_lines() -> Vec<String> {
    vec![
        json!({
            "timestamp": "2026-04-27T01:00:01.123456789Z",
            "level": "INFO",
            "target": "orbit.job.step_started",
            "fields": {
                "job_run_id": "run-1",
                "task_id": "T123",
                "step_id": "build",
                "message": "step started"
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-04-27T01:00:02.000000000Z",
            "level": "INFO",
            "target": "orbit.job.step_finished",
            "fields": {
                "job_run_id": "run-1",
                "task_id": "T123",
                "step_id": "build",
                "outcome": "success",
                "success": true,
                "message": "step finished"
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-04-27T01:00:03.000000000Z",
            "level": "WARN",
            "target": "orbit.policy.deny",
            "fields": {
                "tool": "fs.write",
                "path": "/etc/passwd",
                "profile": "writer",
                "matched_rule": "/etc/**",
                "message": "policy deny"
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-04-27T01:00:04.000000000Z",
            "level": "WARN",
            "target": "orbit.friction.reported",
            "fields": {
                "task_id": "ORB-1011",
                "agent": "codex",
                "model": TEST_CODEX_MODEL,
                "summary": "tool docs missing",
                "message": "friction reported"
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-04-27T01:00:05.000000000Z",
            "level": "INFO",
            "target": "orbit_engine::activity_job::cli_runner",
            "fields": {
                "provider": "codex",
                "stream": "stdout",
                "job_run_id": "jrun-1",
                "task_id": "T123",
                "line": "hello world",
                "message": "subprocess line"
            }
        })
        .to_string(),
    ]
}

fn write_fixture(path: &Path, lines: &[String]) {
    let mut content = String::new();
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(path, content).expect("write fixture");
}

fn capture(path: &Path, args: TailArgs) -> String {
    let filters = build_filters(&args).expect("build filters");
    let mut buf: Vec<u8> = Vec::new();
    run_tail(path, &args, &filters, false, &mut buf).expect("tail run");
    String::from_utf8(buf).expect("utf8")
}

fn make_args(path: PathBuf) -> TailArgs {
    TailArgs {
        lines: 50,
        follow: false,
        target: None,
        level: None,
        since: None,
        json: false,
        path: Some(path),
    }
}

#[test]
fn default_tail_prints_last_n_formatted_columns_and_exits() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let output = capture(&path, make_args(path.clone()));
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(lines[0].contains("01:00:01"));
    assert!(lines[0].contains("job"));
    assert!(lines[0].contains("INF"));
    assert!(lines[0].contains("step build started"));
    assert!(lines[2].contains("DENY"));
    assert!(lines[2].contains("policy"));
    assert!(lines[2].contains("path=/etc/passwd"));
    assert!(lines[3].contains("FRC"));
    assert!(lines[3].contains("friction reported on ORB-1011"));
    assert!(lines[4].contains("codex"));
    assert!(lines[4].contains("[stdout] hello world"));
}

#[test]
fn target_prefix_filter_matches_only_dotted_prefix() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    args.target = Some("orbit.policy".to_string());
    let output = capture(&path, args);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("DENY"));
}

#[test]
fn level_filter_drops_below_threshold() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    args.level = Some(LevelFilter::Warn);
    let output = capture(&path, args);
    let lines: Vec<&str> = output.lines().collect();
    // INFO step_started + step_finished + cli_runner are dropped; WARN
    // policy.deny + friction.reported remain.
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("DENY"));
    assert!(lines[1].contains("FRC"));
}

#[test]
fn since_filter_drops_older_events() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    // Make `since` newer than the fixture's timestamps so only events
    // strictly after that cutoff would survive — but the fixture sits at
    // 2026-04-27T01:00:0X which is in the past relative to now-anchored
    // durations. Use a tiny window pinned to the future to assert the
    // filter actually drops.
    args.since = Some("0s".to_string());
    let output = capture(&path, args);
    // All fixture events have timestamps before "now-0s"; they should all
    // be dropped.
    assert_eq!(output.lines().count(), 0);
}

#[test]
fn n_flag_limits_history() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    args.lines = 2;
    let output = capture(&path, args);
    assert_eq!(output.lines().count(), 2);
    // Should be the last two: friction.reported + cli_runner.
    let lines: Vec<&str> = output.lines().collect();
    assert!(lines[0].contains("FRC"));
    assert!(lines[1].contains("[stdout] hello world"));
}

#[test]
fn n_zero_prints_no_initial_history_rows() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    args.lines = 0;
    assert!(capture(&path, args).is_empty());
}

#[test]
fn initial_tail_streams_large_interleaved_log_to_last_matching_rows_in_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    let mut file = File::create(&path).expect("create fixture");
    let mut expected = Vec::new();

    for index in 0..4_096 {
        match index % 5 {
            0 => {
                let line = json!({
                    "timestamp": "2026-04-27T01:00:01.000000000Z",
                    "level": "WARN",
                    "target": "orbit.policy.deny",
                    "fields": {"message": format!("matching-{index}")}
                })
                .to_string();
                if index >= 4_065 {
                    expected.push(line.clone());
                }
                writeln!(file, "{line}").expect("write matching row");
            }
            3 => writeln!(file, "{{malformed-{index}").expect("write malformed row"),
            _ => writeln!(
                file,
                "{}",
                json!({
                    "timestamp": "2026-04-27T01:00:01.000000000Z",
                    "level": "INFO",
                    "target": "orbit.unrelated.event",
                    "fields": {"message": format!("nonmatching-{index}")}
                })
            )
            .expect("write nonmatching row"),
        }
    }

    let mut args = make_args(path.clone());
    args.lines = 7;
    args.target = Some("orbit.policy".to_string());
    args.json = true;

    assert_eq!(capture(&path, args).lines().collect::<Vec<_>>(), expected);
}

#[test]
fn matching_line_window_never_retains_more_rows_than_requested() {
    let mut window = MatchingLineWindow::new(3);
    for index in 0..4_096 {
        window.push(format!("matching-{index}"));
        assert!(
            window.len() <= 3,
            "the bounded window exceeded its requested capacity at row {index}"
        );
    }

    let mut zero_window = MatchingLineWindow::new(0);
    zero_window.push("matching".to_string());
    assert_eq!(zero_window.len(), 0);
}

#[test]
fn json_flag_emits_raw_lines_unchanged() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut args = make_args(path.clone());
    args.json = true;
    let output = capture(&path, args);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 5);
    for (i, line) in lines.iter().enumerate() {
        assert_eq!(*line, fixture_lines()[i]);
    }
}

#[test]
fn non_tty_output_contains_no_ansi_escapes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let output = capture(&path, make_args(path.clone()));
    assert!(
        !output.as_bytes().contains(&0x1b),
        "non-tty output leaked ANSI escape: {output}"
    );
}

#[test]
fn follow_mode_emits_appended_line_within_window() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    // The worker starts late on purpose: appending is nevertheless safe
    // because readiness is the post-offset transition, not elapsed time.
    let mut follower = spawn_follower(path.clone(), false, Duration::from_millis(100));
    follower.wait_until_ready();

    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append fixture");
    let appended = json!({
        "timestamp": "2026-04-27T01:00:06.000000000Z",
        "level": "INFO",
        "target": "orbit.job.step_started",
        "fields": {
            "job_run_id": "run-2",
            "step_id": "post-fixture",
            "message": "step started"
        }
    })
    .to_string();
    writeln!(file, "{appended}").expect("write appended");
    file.flush().ok();

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(line) = follower.recv_timeout(Duration::from_millis(50))
            && line.contains("post-fixture")
        {
            found = true;
            break;
        }
    }

    assert!(found, "follow mode did not surface appended line");
    follower.finish();
}

#[test]
fn follow_mode_with_json_flag_emits_appended_line_as_raw_jsonl() {
    // Regression for review thread P2: follow mode must honor `--json` for
    // appended lines, not just for the initial window.
    let dir = tempdir().unwrap();
    let path = dir.path().join("orbit.jsonl");
    write_fixture(&path, &fixture_lines());

    let mut follower = spawn_follower(path.clone(), true, Duration::ZERO);
    follower.wait_until_ready();

    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append fixture");
    let appended_raw = json!({
        "timestamp": "2026-04-27T01:00:07.000000000Z",
        "level": "INFO",
        "target": "orbit.job.step_started",
        "fields": {
            "job_run_id": "run-3",
            "step_id": "json-followed",
            "message": "step started"
        }
    })
    .to_string();
    writeln!(file, "{appended_raw}").expect("write appended");
    file.flush().ok();

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut got_raw = false;
    while Instant::now() < deadline {
        if let Ok(chunk) = follower.recv_timeout(Duration::from_millis(50)) {
            // Followed JSON output is the raw JSONL line — i.e. the same
            // string we appended, optionally followed by a newline. The
            // formatted four-column view would render `step json-followed
            // started [run=run-3]` instead, so asserting the literal raw
            // body is sufficient.
            if chunk.trim_end().ends_with(&appended_raw) {
                got_raw = true;
                break;
            }
        }
    }

    assert!(
        got_raw,
        "follow mode with --json did not surface appended line as raw JSONL",
    );
    follower.finish();
}

fn spawn_follower(path: PathBuf, json: bool, startup_delay: Duration) -> FollowWorker {
    let (output_tx, output_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        thread::sleep(startup_delay);
        let mut buf = TeeWriter::new(output_tx);
        let args = TailArgs {
            lines: 0,
            follow: true,
            target: None,
            level: None,
            since: None,
            json,
            path: Some(path.clone()),
        };
        let filters = build_filters(&args).expect("filters");
        run_tail_with_test_control(
            &path,
            &args,
            &filters,
            false,
            &mut buf,
            FollowTestControl::new(ready_tx, stop_rx),
        )
    });
    FollowWorker {
        output_rx,
        ready_rx,
        stop_tx,
        handle: Some(handle),
    }
}

struct FollowWorker {
    output_rx: mpsc::Receiver<String>,
    ready_rx: mpsc::Receiver<()>,
    stop_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<io::Result<()>>>,
}

impl FollowWorker {
    fn wait_until_ready(&self) {
        self.ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("follower established its initial offset");
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<String, mpsc::RecvTimeoutError> {
        self.output_rx.recv_timeout(timeout)
    }

    fn finish(&mut self) {
        let _ = self.stop_tx.send(());
        self.join();
    }

    fn join(&mut self) {
        self.handle
            .take()
            .expect("follower handle is present")
            .join()
            .expect("follower thread did not panic")
            .expect("follower exited cleanly");
    }
}

impl Drop for FollowWorker {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn format_message_renders_each_high_value_target() {
    let policy = format_message(
        "orbit.policy.deny",
        &json!({
            "tool": "fs.write",
            "path": "/etc/passwd",
            "profile": "writer",
            "matched_rule": "/etc/**"
        }),
    );
    assert_eq!(
        policy,
        "tool=fs.write path=/etc/passwd profile=writer rule=/etc/**"
    );

    let friction = format_message(
        "orbit.friction.reported",
        &json!({
            "task_id": "ORB-1011",
            "agent": "codex",
            "model": TEST_CODEX_MODEL,
            "summary": "missing"
        }),
    );
    assert!(friction.starts_with("friction reported on ORB-1011"));
    assert!(friction.contains("by codex/gpt-5.5"));
    assert!(friction.ends_with(": missing"));

    let started = format_message(
        "orbit.job.step_started",
        &json!({"job_run_id": "r", "step_id": "s"}),
    );
    assert_eq!(started, "step s started [run=r]");

    let finished_ok = format_message(
        "orbit.job.step_finished",
        &json!({"step_id": "s", "outcome": "success", "success": true}),
    );
    assert_eq!(finished_ok, "step s finished ok (success)");

    let runner = format_message(
        "orbit_engine::activity_job::cli_runner",
        &json!({
            "provider": "codex",
            "stream": "stderr",
            "line": "boom"
        }),
    );
    assert_eq!(runner, "[stderr] boom");
}

struct TeeWriter {
    tx: mpsc::Sender<String>,
}

impl TeeWriter {
    fn new(tx: mpsc::Sender<String>) -> Self {
        Self { tx }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(text) = std::str::from_utf8(buf) {
            let _ = self.tx.send(text.to_string());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
