//! Worker log tail size and mutation behavior.

use std::fs::{self, File, OpenOptions};
use std::io::Write;

use super::super::log::{read_pipeline_worker_log_tail, read_pipeline_worker_log_tail_at_size};

const LOG_TAIL_BYTES: usize = 16 * 1024;

fn log_file(contents: &[u8]) -> (tempfile::TempDir, File) {
    let directory = tempfile::tempdir().expect("temporary log directory");
    let path = directory.path().join("worker.log");
    fs::write(&path, contents).expect("write worker log");
    let file = File::open(path).expect("open worker log");
    (directory, file)
}

#[test]
fn empty_short_and_exact_limit_logs_keep_their_expected_content() {
    let (_directory, mut file) = log_file(b"");
    assert_eq!(read_pipeline_worker_log_tail(&mut file), None);

    let (_directory, mut file) = log_file(b"short worker output\n");
    assert_eq!(
        read_pipeline_worker_log_tail(&mut file).as_deref(),
        Some("short worker output")
    );

    let exact = vec![b'x'; LOG_TAIL_BYTES];
    let (_directory, mut file) = log_file(&exact);
    assert_eq!(
        read_pipeline_worker_log_tail(&mut file).as_deref(),
        Some(std::str::from_utf8(&exact).expect("ASCII log content"))
    );
}

#[test]
fn long_logs_report_the_snapshot_window_and_return_only_its_tail() {
    let mut contents = vec![b'a'; 128];
    contents.extend(vec![b'b'; LOG_TAIL_BYTES]);
    let (_directory, mut file) = log_file(&contents);

    let output = read_pipeline_worker_log_tail(&mut file).expect("nonempty log tail");
    let (diagnostic, tail) = output.split_once('\n').expect("truncation diagnostic");
    assert!(diagnostic.contains("truncated to final 16384 bytes"));
    assert_eq!(tail.len(), LOG_TAIL_BYTES);
    assert_eq!(tail, "b".repeat(LOG_TAIL_BYTES));
}

#[test]
fn appends_after_the_size_snapshot_are_excluded_from_the_bounded_read() {
    let original = vec![b'x'; LOG_TAIL_BYTES + 128];
    let (directory, mut reader) = log_file(&original);
    let snapshot_size = reader.metadata().expect("log metadata").len();

    let path = directory.path().join("worker.log");
    let mut writer = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open log for append");
    for _ in 0..4096 {
        writer
            .write_all(b"APPENDED-DATA\n")
            .expect("append after size snapshot");
    }

    let output = read_pipeline_worker_log_tail_at_size(&mut reader, snapshot_size)
        .expect("nonempty snapshotted log tail");
    assert!(!output.contains("APPENDED-DATA"));
    let (_diagnostic, tail) = output.split_once('\n').expect("truncation diagnostic");
    assert_eq!(tail.len(), LOG_TAIL_BYTES);
    assert_eq!(tail, "x".repeat(LOG_TAIL_BYTES));
}

#[test]
fn shrink_after_the_size_snapshot_reports_only_bytes_still_available() {
    let original = vec![b'x'; LOG_TAIL_BYTES + 1024];
    let (directory, mut reader) = log_file(&original);
    let snapshot_size = reader.metadata().expect("log metadata").len();
    let path = directory.path().join("worker.log");

    OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open log for truncation")
        .set_len(2048)
        .expect("shrink after size snapshot");

    let output = read_pipeline_worker_log_tail_at_size(&mut reader, snapshot_size)
        .expect("available portion of the snapshotted window");
    assert!(output.contains("file changed after size snapshot; read 1024 of 16384 selected bytes"));
    let tail = output.rsplit_once('\n').expect("log body").1;
    assert_eq!(tail.len(), 1024);
    assert_eq!(tail, "x".repeat(1024));
}
