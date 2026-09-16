//! [ORB-00415] Rotation + retention tests for the global JSONL tracing feed.

use std::io::Write;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use crate::fs::io::append_private_file;
use crate::observability::log_rotation::{
    LogRotationConfig, maybe_roll, prune_archives, rotate_and_prune,
    rotate_if_active_exceeds_budget,
};

fn archive_names(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("orbit.jsonl."))
        .collect()
}

fn backdate(path: &std::path::Path, ago: Duration) {
    let when = SystemTime::now().checked_sub(ago).expect("valid time");
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open for mtime")
        .set_modified(when)
        .expect("set mtime");
}

#[test]
fn oversized_active_file_rolls_to_dated_archive_preserving_old_content() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    let old_content = "OLD-LINE\n".repeat(20); // ~180 bytes
    std::fs::write(&active, &old_content).expect("write active");

    let config = LogRotationConfig {
        retention_days: 7,
        max_total_bytes: 10_000_000,
        max_file_bytes: 50,
    };
    maybe_roll(&active, &config).expect("roll");

    assert!(!active.exists(), "active file should have been rolled away");
    let archives = archive_names(dir.path());
    assert_eq!(
        archives.len(),
        1,
        "expected one dated archive, got {archives:?}"
    );
    let archived = std::fs::read_to_string(dir.path().join(&archives[0])).expect("read archive");
    assert_eq!(
        archived, old_content,
        "old content must be preserved in the archive"
    );

    // The subscriber reopening the fixed active path creates a fresh file at
    // the boundary.
    std::fs::write(&active, "NEW-LINE\n").expect("reopen active");
    assert!(active.exists());
    assert_eq!(
        std::fs::read_to_string(&active).expect("read active"),
        "NEW-LINE\n"
    );
}

#[test]
fn within_budget_active_file_is_not_rolled() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    std::fs::write(&active, "small\n").expect("write active");

    let config = LogRotationConfig {
        retention_days: 7,
        max_total_bytes: 10_000_000,
        max_file_bytes: 1_000,
    };
    maybe_roll(&active, &config).expect("roll");

    assert!(active.exists(), "within-budget file must not be rolled");
    assert!(
        archive_names(dir.path()).is_empty(),
        "no archive should be created"
    );
}

#[test]
fn retention_deletes_archives_older_than_age_budget() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    let old = dir.path().join("orbit.jsonl.OLD");
    let recent = dir.path().join("orbit.jsonl.RECENT");
    std::fs::write(&old, "old").expect("write old");
    std::fs::write(&recent, "recent").expect("write recent");
    backdate(&old, Duration::from_secs(10 * 86_400)); // 10 days old

    let config = LogRotationConfig {
        retention_days: 7,
        max_total_bytes: 10_000_000,
        max_file_bytes: 10_000_000,
    };
    prune_archives(&active, &config).expect("prune");

    assert!(
        !old.exists(),
        "archive older than the age budget must be deleted"
    );
    assert!(recent.exists(), "recent archive must be kept");
}

#[test]
fn retention_deletes_oldest_archives_beyond_total_size_budget() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    let a = dir.path().join("orbit.jsonl.A");
    let b = dir.path().join("orbit.jsonl.B");
    let c = dir.path().join("orbit.jsonl.C");
    for (path, secs_ago) in [(&a, 30u64), (&b, 20), (&c, 10)] {
        std::fs::write(path, "x".repeat(100)).expect("write archive");
        backdate(path, Duration::from_secs(secs_ago));
    }

    // 300 bytes total, budget 150: delete oldest (A) -> 200, then B -> 100.
    let config = LogRotationConfig {
        retention_days: 3_650,
        max_total_bytes: 150,
        max_file_bytes: 10_000_000,
    };
    prune_archives(&active, &config).expect("prune");

    assert!(!a.exists(), "oldest archive should be deleted first");
    assert!(!b.exists(), "second-oldest deleted to fit the budget");
    assert!(c.exists(), "newest archive kept within budget");
}

#[test]
fn over_budget_active_file_rolls_and_prunes_old_archives() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    std::fs::write(&active, "x".repeat(100)).expect("write active");
    let old = dir.path().join("orbit.jsonl.OLD");
    std::fs::write(&old, "old").expect("write old archive");
    backdate(&old, Duration::from_secs(10 * 86_400));

    let config = LogRotationConfig {
        retention_days: 7,
        max_total_bytes: 10_000_000,
        max_file_bytes: 50,
    };
    rotate_if_active_exceeds_budget(&active, &config);

    assert!(!active.exists(), "oversized active file should roll");
    assert!(
        !old.exists(),
        "full rotate_and_prune must run when the active file is over budget"
    );
    assert_eq!(archive_names(dir.path()).len(), 1);
}

#[test]
fn within_budget_does_not_prune_archives() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    std::fs::write(&active, "small\n").expect("write active");
    let old = dir.path().join("orbit.jsonl.OLD");
    std::fs::write(&old, "old").expect("write old archive");
    backdate(&old, Duration::from_secs(10 * 86_400));

    let config = LogRotationConfig {
        retention_days: 7,
        max_total_bytes: 10_000_000,
        max_file_bytes: 1_000,
    };
    rotate_if_active_exceeds_budget(&active, &config);

    assert!(active.exists(), "within-budget file must not be rolled");
    assert!(
        old.exists(),
        "must not walk or prune archives when the active file is within budget"
    );
}

#[test]
fn missing_active_file_does_not_walk_archives() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    let old = dir.path().join("orbit.jsonl.OLD");
    std::fs::write(&old, "old").expect("write old archive");
    backdate(&old, Duration::from_secs(10 * 86_400));

    rotate_if_active_exceeds_budget(&active, &LogRotationConfig::default());

    assert!(
        old.exists(),
        "a missing active file must skip the archive walk"
    );
}

#[test]
fn rotate_and_prune_is_a_noop_when_nothing_to_do() {
    let dir = TempDir::new().expect("tempdir");
    let active = dir.path().join("orbit.jsonl");
    std::fs::write(&active, "line\n").expect("write");
    // Generous budgets -> no roll, no prune, no panic even with no archives.
    rotate_and_prune(&active, &LogRotationConfig::default());
    assert!(active.exists());
    assert!(archive_names(dir.path()).is_empty());
}

#[test]
fn missing_archive_directory_is_a_noop() {
    let dir = TempDir::new().expect("tempdir");
    let missing_dir = dir.path().join("missing");
    let active = missing_dir.join("orbit.jsonl");

    prune_archives(&active, &LogRotationConfig::default())
        .expect("a missing archive directory is an empty archive set");

    assert!(
        !missing_dir.exists(),
        "pruning must not create the missing directory"
    );
}

#[test]
fn genuine_archive_directory_errors_include_path_context() {
    let dir = TempDir::new().expect("tempdir");
    let not_a_directory = dir.path().join("not-a-directory");
    std::fs::write(&not_a_directory, "file").expect("write blocking file");
    let active = not_a_directory.join("orbit.jsonl");

    let error = prune_archives(&active, &LogRotationConfig::default())
        .expect_err("a non-directory archive parent must remain visible");

    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error
            .to_string()
            .contains(&not_a_directory.display().to_string()),
        "error should identify the archive directory: {error}"
    );
}

#[test]
fn concurrent_writers_do_not_corrupt_jsonl_lines() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    const PER_WRITER: usize = 300;

    let handles: Vec<_> = (0..2)
        .map(|writer_id| {
            let path = path.clone();
            std::thread::spawn(move || {
                for seq in 0..PER_WRITER {
                    // Re-open per line (O_APPEND) to mirror separate processes
                    // appending concurrently; short lines stay under PIPE_BUF.
                    let mut file = append_private_file(&path).expect("append open");
                    let mut line = serde_json::to_string(
                        &serde_json::json!({"writer": writer_id, "seq": seq}),
                    )
                    .expect("serialize");
                    // One write() per line, newline included — mirrors the fmt
                    // layer, which write_all()s the whole formatted event.
                    // (`writeln!` issues a second syscall for the `\n`, which
                    // can interleave between concurrent appenders.)
                    line.push('\n');
                    file.write_all(line.as_bytes()).expect("write line");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("join writer");
    }

    let content = std::fs::read_to_string(&path).expect("read log");
    let mut count = 0;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|error| panic!("corrupt interleaved line `{line}`: {error}"));
        count += 1;
    }
    assert_eq!(
        count,
        2 * PER_WRITER,
        "every line from both writers must parse"
    );
}

#[test]
fn from_parts_validates_and_converts_units() {
    assert_eq!(
        LogRotationConfig::from_parts(None, None, None).expect("defaults"),
        LogRotationConfig::default()
    );

    let config = LogRotationConfig::from_parts(Some(3), Some(200), Some(50)).expect("valid");
    assert_eq!(config.retention_days, 3);
    assert_eq!(config.max_total_bytes, 200 * 1024 * 1024);
    assert_eq!(config.max_file_bytes, 50 * 1024 * 1024);

    assert!(LogRotationConfig::from_parts(Some(0), None, None).is_err());
    assert!(LogRotationConfig::from_parts(None, Some(0), None).is_err());
    assert!(LogRotationConfig::from_parts(None, None, Some(0)).is_err());
    assert!(
        LogRotationConfig::from_parts(None, Some(10), Some(50)).is_err(),
        "a per-file budget larger than the total budget must be rejected"
    );
}
