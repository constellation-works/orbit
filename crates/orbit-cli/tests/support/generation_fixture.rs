//! Observe the image a live Orbit participant registered, on Linux and macOS.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

use orbit_common::fs::generation::ParticipantRecord;

pub(crate) fn running_digest(root: &Path, pid: u32) -> Option<String> {
    let entries = std::fs::read_dir(root.join(".generation-participants")).ok()?;
    for entry in entries.flatten() {
        let Ok(file) = File::open(entry.path()) else {
            continue;
        };
        // SAFETY: flock only probes this fixture's open descriptor. A record
        // without its writer's exclusive lock is stale, including after exec.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            continue;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
            continue;
        }
        let Ok(record) = serde_json::from_reader::<_, ParticipantRecord>(&file) else {
            continue;
        };
        if record.pid == pid {
            return Some(record.digest);
        }
    }
    None
}

pub(crate) fn distinct_copy(source: &Path, destination: &Path) {
    std::fs::copy(source, destination).expect("candidate copy");
    std::fs::OpenOptions::new()
        .append(true)
        .open(destination)
        .expect("open candidate")
        .write_all(b"\nupgrade-regression-candidate\n")
        .expect("distinct executable");
    // Darwin validates Mach-O signatures at exec, including on copied files.
    #[cfg(target_os = "macos")]
    {
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(destination)
            .output()
            .expect("ad-hoc sign candidate");
        assert!(signed.status.success(), "codesign: {signed:?}");
    }
}

pub(crate) fn launch<T>(operation: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    #[cfg(target_os = "linux")]
    {
        orbit_common::test_process::retry_executable_busy(operation)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut operation = operation;
        operation()
    }
}
