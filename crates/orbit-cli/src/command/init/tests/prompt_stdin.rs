#![cfg(unix)]

use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::super::prompt_stdin::{
    MAX_NON_TTY_LINE_BYTES, STDIN_PROMPT_TIMEOUT, take_complete_line, wait_for_fd,
};

#[test]
fn wait_for_fd_times_out_on_a_silent_open_socket() {
    let (reader, _writer) = UnixStream::pair().expect("unix socket pair");
    let start = Instant::now();
    let error = wait_for_fd(reader.as_raw_fd(), start + Duration::from_millis(200))
        .expect_err("silent socket times out");

    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert_eq!(error.to_string(), STDIN_PROMPT_TIMEOUT);
    assert!(
        error.to_string().contains("--task-prefix")
            && error.to_string().contains("--machine-name")
            && error.to_string().contains("--non-interactive"),
        "{error}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "timeout took {:?}",
        start.elapsed()
    );
}

#[test]
fn wait_for_fd_returns_when_the_peer_writes() {
    let (reader, mut writer) = UnixStream::pair().expect("unix socket pair");
    writer.write_all(b"host\n").expect("write answer");
    wait_for_fd(
        reader.as_raw_fd(),
        Instant::now() + Duration::from_millis(200),
    )
    .expect("data is ready");
}

#[test]
fn wait_for_fd_uses_the_supplied_deadline() {
    let (reader, _writer) = UnixStream::pair().expect("unix socket pair");
    let deadline = Instant::now() + Duration::from_millis(150);
    std::thread::sleep(Duration::from_millis(100));
    let start = Instant::now();
    let error = wait_for_fd(reader.as_raw_fd(), deadline).expect_err("remaining time expires");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_millis(125));
}

#[test]
fn line_limit_applies_to_bytes_before_newline_and_keeps_following_answer() {
    let mut buf = vec![b'a'; MAX_NON_TTY_LINE_BYTES];
    buf.extend_from_slice(b"\nnext\n");
    let line = take_complete_line(&mut buf, 0, Some(MAX_NON_TTY_LINE_BYTES))
        .expect("line within limit")
        .expect("first line");
    assert_eq!(line.len(), MAX_NON_TTY_LINE_BYTES);
    assert_eq!(
        take_complete_line(&mut buf, 0, Some(MAX_NON_TTY_LINE_BYTES)).expect("second line"),
        Some("next".to_string())
    );

    let mut too_long = vec![b'a'; MAX_NON_TTY_LINE_BYTES + 1];
    too_long.push(b'\n');
    let error = take_complete_line(&mut too_long, 0, Some(MAX_NON_TTY_LINE_BYTES))
        .expect_err("oversized line must fail");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(
        take_complete_line(&mut too_long, 0, None)
            .expect("TTY line has no size limit")
            .is_some()
    );
}
