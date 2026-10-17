//! Bound JSONL records and follow the public CLI through log rotations.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_common::test_env;
use serde_json::{Value, json};

fn event(message: &str, level: &str, target: &str) -> String {
    json!({
        "timestamp": "2026-10-07T07:00:00Z",
        "level": level,
        "target": target,
        "fields": {"message": message},
    })
    .to_string()
}

fn append(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// How long a follower may take to emit an appended record. The wait ends as
/// soon as the record arrives; the ceiling only bounds a missing one. A fixed
/// five seconds was exceeded by the CLI's own startup on a saturated host.
const RECORD_WAIT: Duration = Duration::from_secs(60);

/// `orbit log tail --path <path>` in an isolated home, with no streaming flags.
fn tail_command(fixture: &crate::git_repo::WorkCheckout, path: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(&fixture.work)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env_remove("ORBIT_FORMAT")
        .env_remove("ORBIT_LOG_PATH")
        .env("NO_COLOR", "1")
        .args(["log", "tail"])
        .arg("--path")
        .arg(path);
    command
}

struct Follower {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    stderr: File,
}

impl Follower {
    fn start(fixture: &crate::git_repo::WorkCheckout, path: &Path, json: bool) -> Self {
        let mut command = tail_command(fixture, path);
        let stderr = tempfile::tempfile().unwrap();
        command
            .args(["-f", "-n", "1", "--level", "warn", "--target", "orbit.test"])
            .stdout(Stdio::piped())
            .stderr(stderr.try_clone().unwrap());
        if json {
            command.arg("--json");
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
            stderr,
        }
    }

    fn expect_record(&self, expected: &str, json: bool) {
        let line = self.lines.recv_timeout(RECORD_WAIT).unwrap_or_else(|err| {
            let mut stderr = &self.stderr;
            let mut errors = String::new();
            use std::io::{Read, Seek, SeekFrom};
            stderr.seek(SeekFrom::Start(0)).unwrap();
            stderr.read_to_string(&mut errors).unwrap();
            panic!(
                "missing {expected} after {RECORD_WAIT:?} ({}): {err}; stderr: {errors}",
                test_env::host_load()
            );
        });
        if json {
            let value: Value = serde_json::from_str(&line).expect("JSONL output");
            assert_eq!(value["fields"]["message"], expected);
        } else {
            assert!(line.contains(expected), "formatted record: {line}");
            assert!(!line.starts_with('{'), "expected the formatted view");
            assert!(!line.contains('\u{1b}'), "NO_COLOR must survive reopen");
        }
    }

    fn expect_no_record(&self) {
        assert!(
            matches!(
                self.lines.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "partial and filtered records must not be emitted"
        );
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn follow_rotation_drains_archive_then_preserves_filters_and_partial_records() {
    for json in [true, false] {
        let fixture = crate::git_repo::WorkCheckout::new();
        let path = fixture.work.join("orbit.jsonl");
        fs::write(&path, format!("{}\n", event("ready", "WARN", "orbit.test"))).unwrap();
        let follower = Follower::start(&fixture, &path, json);
        follower.expect_record("ready", json);

        let old = event("archive-rest", "ERROR", "orbit.test.child");
        let split = old.len() - 4;
        append(&path, &old.as_bytes()[..split]);
        follower.expect_no_record();
        let mut archive_writer = OpenOptions::new().append(true).open(&path).unwrap();
        fs::rename(&path, fixture.work.join("archive.jsonl")).unwrap();
        // The path stays absent for multiple polls. Complete the pending
        // record through the descriptor the writer kept across the rename.
        follower.expect_no_record();
        archive_writer.write_all(&old.as_bytes()[split..]).unwrap();
        writeln!(archive_writer).unwrap();
        follower.expect_record("archive-rest", json);
        archive_writer.write_all(b"{\"torn_archive\":").unwrap();
        follower.expect_no_record();

        fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                event("filtered-level", "INFO", "orbit.test"),
                event("filtered-target", "ERROR", "orbit.other"),
                event("replacement-start", "WARN", "orbit.test.child"),
            ),
        )
        .unwrap();
        follower.expect_record("replacement-start", json);

        let partial = event("completed-🌍", "WARN", "orbit.test");
        let split = partial.find('🌍').unwrap() + 1;
        append(&path, &partial.as_bytes()[..split]);
        follower.expect_no_record();
        append(&path, &partial.as_bytes()[split..]);
        follower.expect_no_record();
        append(&path, b"\n");
        follower.expect_record("completed-🌍", json);
        follower.expect_no_record();
    }
}

#[test]
fn follow_truncation_resets_offset_and_discards_the_previous_partial_record() {
    for json in [true, false] {
        let fixture = crate::git_repo::WorkCheckout::new();
        let path = fixture.work.join("orbit.jsonl");
        let ready = "r".repeat(1024);
        fs::write(&path, format!("{}\n", event(&ready, "WARN", "orbit.test"))).unwrap();
        let follower = Follower::start(&fixture, &path, json);
        follower.expect_record(&ready, json);
        append(&path, b"{\"old_partial\":");
        follower.expect_no_record();

        let replacement = event("after-truncate", "ERROR", "orbit.test");
        let split = replacement.len() / 2;
        fs::write(&path, &replacement.as_bytes()[..split]).unwrap();
        follower.expect_no_record();
        append(&path, &replacement.as_bytes()[split..]);
        append(&path, b"\n");
        follower.expect_record("after-truncate", json);
        follower.expect_no_record();
    }
}

fn one_shot_tail(fixture: &crate::git_repo::WorkCheckout, path: &Path) -> Output {
    tail_command(fixture, path)
        .arg("--json")
        .output()
        .expect("log tail output")
}

// A few MiB of fixture data exercise the production bound without a stress
// workload or an operator-configurable override of the memory safeguard.
const RECORD_LIMIT: usize = 1024 * 1024;

fn padded_event(message: &str, bytes: usize) -> String {
    let record = event(message, "WARN", "orbit.test");
    format!("{}{record}", " ".repeat(bytes - record.len()))
}

#[test]
fn history_skips_oversized_records_and_bounds_delimiter_free_zero_history() {
    let fixture = crate::git_repo::WorkCheckout::new();
    // Exercise both the single-file and merged-feed entrypoints.
    for name in ["custom.jsonl", "orbit.jsonl"] {
        let path = fixture.work.join(name);
        let content = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}",
            event("before", "WARN", "orbit.test"),
            padded_event("at-limit", RECORD_LIMIT),
            padded_event("oversized-valid", RECORD_LIMIT + 1),
            "x".repeat(RECORD_LIMIT + 1),
            event("filtered", "INFO", "orbit.test"),
            event("after", "ERROR", "orbit.test.child"),
            // An oversized valid record without a delimiter must also be
            // discarded by the one-shot reader, including with no history.
            padded_event("oversized-eof", RECORD_LIMIT + 1),
        );
        fs::write(&path, content).unwrap();
        for lines in ["50", "0"] {
            let mut command = assert_cmd::Command::from_std(tail_command(&fixture, &path));
            command.timeout(test_env::FIXTURE_STEP_DEADLINE).args([
                "--json",
                "-n",
                lines,
                "--level",
                "warn",
                "--target",
                "orbit.test",
            ]);
            let started = std::time::Instant::now();
            let output = command.output().unwrap();
            test_env::assert_step_finished("log tail", started.elapsed(), &output.status);
            assert!(output.status.success(), "stderr: {:?}", output.stderr);
            let records: Vec<Value> = String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            let messages: Vec<&str> = records
                .iter()
                .map(|record| record["fields"]["message"].as_str().unwrap())
                .collect();
            let expected = if lines == "0" {
                vec![]
            } else {
                vec!["before", "at-limit", "after"]
            };
            assert_eq!(messages, expected);
        }
    }
}

#[test]
fn follow_discards_oversized_initial_and_growing_partials_through_the_delimiter() {
    for (name, prefix_len) in [
        ("custom.jsonl", RECORD_LIMIT / 2),
        ("orbit.jsonl", RECORD_LIMIT + 1),
    ] {
        let fixture = crate::git_repo::WorkCheckout::new();
        let path = fixture.work.join(name);
        fs::write(
            &path,
            format!(
                "{}\n{}",
                event("ready", "WARN", "orbit.test"),
                " ".repeat(prefix_len)
            ),
        )
        .unwrap();
        let follower = Follower::start(&fixture, &path, true);
        // Initial history is emitted only after reading to EOF, so readiness
        // proves this partial was read before any of the following appends.
        follower.expect_record("ready", true);
        append(&path, " ".repeat(RECORD_LIMIT + 1 - prefix_len).as_bytes());
        append(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n",
                event("discard-this-suffix", "WARN", "orbit.test"),
                event("filtered-level", "INFO", "orbit.test"),
                event("filtered-target", "WARN", "orbit.other"),
                event("after-oversized", "ERROR", "orbit.test.child"),
            )
            .as_bytes(),
        );
        // If discard state were lost at EOF, the JSON suffix would be
        // emitted instead. Positive delivery also proves delimiter recovery.
        follower.expect_record("after-oversized", true);
        append(
            &path,
            format!("{}\n", padded_event("oversized-live", RECORD_LIMIT + 1)).as_bytes(),
        );
        let partial = event("completed-🌍", "WARN", "orbit.test");
        let split = partial.find('🌍').unwrap() + 1;
        append(&path, &partial.as_bytes()[..split]);
        follower.expect_no_record();
        append(&path, &partial.as_bytes()[split..]);
        append(&path, b"\n");
        follower.expect_record("completed-🌍", true);
    }
}

#[test]
fn follow_rotation_and_truncation_reset_oversized_partial_discard_state() {
    for rotate in [true, false] {
        let fixture = crate::git_repo::WorkCheckout::new();
        let path = fixture.work.join("orbit.jsonl");
        fs::write(
            &path,
            format!(
                "{}\n{}",
                event("ready", "WARN", "orbit.test"),
                " ".repeat(RECORD_LIMIT + 1)
            ),
        )
        .unwrap();
        let follower = Follower::start(&fixture, &path, true);
        follower.expect_record("ready", true);
        if rotate {
            fs::rename(&path, fixture.work.join("archive.jsonl")).unwrap();
        }
        fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                event("replacement", "ERROR", "orbit.test"),
                event("filtered", "INFO", "orbit.test"),
                event("replacement-sentinel", "WARN", "orbit.test"),
            ),
        )
        .unwrap();
        // The first replacement record must not inherit the old discard flag.
        follower.expect_record("replacement", true);
        follower.expect_record("replacement-sentinel", true);
    }
}

#[test]
fn one_shot_tail_with_neither_split_feed_fails_naming_the_operational_path() {
    let fixture = crate::git_repo::WorkCheckout::new();
    let path = fixture.work.join("orbit.jsonl");
    let output = one_shot_tail(&fixture, &path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a missing log must not look like an empty one; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains(&format!("orbit log file not found: {}", path.display())),
        "stderr: {stderr}"
    );
}

#[test]
fn one_shot_tail_reads_the_agent_feed_when_the_operational_feed_is_absent() {
    let fixture = crate::git_repo::WorkCheckout::new();
    let path = fixture.work.join("orbit.jsonl");
    fs::write(
        fixture.work.join("orbit-agent.jsonl"),
        format!("{}\n", event("agent-only", "WARN", "orbit.test")),
    )
    .unwrap();
    let output = one_shot_tail(&fixture, &path);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("agent-only"), "stdout: {stdout}");
}

#[test]
fn tail_merges_agent_history_and_follows_both_feeds() {
    let fixture = crate::git_repo::WorkCheckout::new();
    let path = fixture.work.join("orbit.jsonl");
    let agent = fixture.work.join("orbit-agent.jsonl");
    fs::write(
        &path,
        format!("{}\n", event("operational", "WARN", "orbit.test")),
    )
    .unwrap();
    fs::write(
        &agent,
        format!("{}\n", event("agent-history", "WARN", "orbit.test")),
    )
    .unwrap();
    let follower = Follower::start(&fixture, &path, true);
    follower.expect_record("agent-history", true);
    append(
        &path,
        format!("{}\n", event("operational-live", "WARN", "orbit.test")).as_bytes(),
    );
    append(
        &agent,
        format!("{}\n", event("agent-live", "WARN", "orbit.test")).as_bytes(),
    );
    follower.expect_record("operational-live", true);
    follower.expect_record("agent-live", true);
    fs::rename(&agent, fixture.work.join("orbit-agent.jsonl.old")).unwrap();
    fs::write(
        &agent,
        format!("{}\n", event("agent-rotated", "WARN", "orbit.test")),
    )
    .unwrap();
    follower.expect_record("agent-rotated", true);
    append(
        &path,
        format!("{}\n", event("operational-sentinel", "WARN", "orbit.test")).as_bytes(),
    );
    follower.expect_record("operational-sentinel", true);
}

#[test]
fn one_shot_tail_renders_supervisor_agent_output_with_provider_source() {
    let fixture = crate::git_repo::WorkCheckout::new();
    let path = fixture.work.join("orbit.jsonl");
    // The target and fields the supervisor emits per agent output line
    // (`orbit-engine`'s `cli_runner/supervisor/output.rs`).
    let record = json!({
        "timestamp": "2026-10-07T07:00:00Z",
        "level": "INFO",
        "target": "orbit_engine::activity_job::cli_runner::supervisor",
        "fields": {
            "agent_output": true,
            "provider": "claude",
            "stream": "stdout",
            "job_run_id": "jrun-1",
            "line": "hello from the agent",
        },
    });
    fs::write(&path, format!("{record}\n")).unwrap();
    let mut command = tail_command(&fixture, &path);
    let output = command.output().expect("log tail output");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(
        &words[1..],
        ["claude", "INF", "[stdout]", "hello", "from", "the", "agent"],
        "agent output must render as `provider  INF  [stream] line`: {stdout}"
    );
}

#[test]
fn one_shot_tail_labels_a_double_colon_target_by_its_last_segment() {
    let fixture = crate::git_repo::WorkCheckout::new();
    let path = fixture.work.join("orbit.jsonl");
    let record = event(
        "qualified",
        "INFO",
        "orbit_engine::activity_job::step_runner",
    );
    fs::write(&path, format!("{record}\n")).unwrap();
    let output = tail_command(&fixture, &path)
        .output()
        .expect("log tail output");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(
        &words[1..],
        ["step_runner", "INF", "qualified"],
        "a `::` target must show the same source label as the dashboard: {stdout}"
    );
}
