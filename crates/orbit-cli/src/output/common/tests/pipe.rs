use std::io;

use super::super::pipe::is_closed_stdout_panic;

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
