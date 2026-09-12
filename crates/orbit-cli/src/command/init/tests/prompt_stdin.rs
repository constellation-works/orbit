#![cfg(unix)]

use std::io::{ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::super::prompt_stdin::{STDIN_PROMPT_TIMEOUT, wait_for_fd};

#[test]
fn wait_for_fd_times_out_on_a_silent_open_socket() {
    let (reader, _writer) = UnixStream::pair().expect("unix socket pair");
    let start = Instant::now();
    let error = wait_for_fd(reader.as_raw_fd(), Duration::from_millis(200))
        .expect_err("silent socket times out");

    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert_eq!(error.to_string(), STDIN_PROMPT_TIMEOUT);
    assert!(
        error.to_string().contains("--task-prefix")
            && error.to_string().contains("--host-name")
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
    wait_for_fd(reader.as_raw_fd(), Duration::from_millis(200)).expect("data is ready");
}
