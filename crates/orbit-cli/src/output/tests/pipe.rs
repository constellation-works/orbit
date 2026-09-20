use std::io;

use super::super::pipe::{is_broken_pipe, is_closed_stdout_panic};

#[test]
fn a_broken_pipe_io_error_is_recognized() {
    assert!(is_broken_pipe(&io::Error::from(io::ErrorKind::BrokenPipe)));
    assert!(!is_broken_pipe(&io::Error::from(io::ErrorKind::WriteZero)));
}

#[test]
fn the_panic_std_raises_for_a_closed_stdout_is_recognized() {
    // Exactly what `std::io::_print` formats, with the error `head`
    // closing the pipe produces.
    let raised = format!(
        "failed printing to stdout: {}",
        io::Error::from_raw_os_error(32)
    );

    assert!(is_closed_stdout_panic(&raised), "{raised}");
}

#[test]
fn an_unwritable_stdout_still_panics() {
    let disk_full = format!(
        "failed printing to stdout: {}",
        io::Error::from_raw_os_error(28)
    );

    assert!(
        !is_closed_stdout_panic(&disk_full),
        "ENOSPC is a real failure and must not exit 0: {disk_full}"
    );
    assert!(!is_closed_stdout_panic("index out of bounds"));
}
