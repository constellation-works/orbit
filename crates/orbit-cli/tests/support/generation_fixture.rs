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

/// Copy `source` to `destination` as a distinct, [`assess`]ed candidate.
pub(crate) fn distinct_copy(source: &Path, destination: &Path) {
    std::fs::copy(source, destination).expect("candidate copy");
    // Darwin names an image by its Mach-O UUID and rejects bytes appended after
    // the signed image, so the candidate gets a new UUID and is re-signed.
    #[cfg(target_os = "macos")]
    {
        renew_macho_uuid(destination);
        let signed = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(destination)
            .output()
            .expect("ad-hoc sign candidate");
        assert!(signed.status.success(), "codesign: {signed:?}");
    }
    #[cfg(not(target_os = "macos"))]
    std::fs::OpenOptions::new()
        .append(true)
        .open(destination)
        .expect("open candidate")
        .write_all(b"\nupgrade-regression-candidate\n")
        .expect("distinct executable");
    assess(destination);
}

/// Install `source`'s bytes at `installed` the way an installer does: write
/// beside it, then rename over it, so the running inode is left untouched.
/// The staged copy is [`assess`]ed before the rename, as `orbit update`
/// probes its staged release.
pub(crate) fn install_over(source: &Path, installed: &Path) {
    let staged = installed.with_extension("staged");
    std::fs::copy(source, &staged).expect("stage replacement");
    assess(&staged);
    std::fs::rename(&staged, installed).expect("replace installation");
}

/// Run a freshly written executable's handover contract before installing it,
/// so a later timed probe exercises an already completed command path.
///
/// macOS assesses each new executable file on its first exec and holds the
/// process until that finishes: seconds for the debug `orbit` image, queued
/// behind every other new image on the host. A long-lived process probes a
/// replaced installation within ten seconds and keeps running the old image
/// when the probe times out, so on a loaded host an unassessed replacement
/// never took over. The assessment stays with the file across a rename. This
/// run is bounded only as a hang guard.
pub(crate) fn assess(executable: &Path) {
    let mut command = assert_cmd::Command::new(executable);
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["update", "--contract", "--json"])
        .timeout(orbit_common::test_env::FIXTURE_STEP_DEADLINE);
    let output = launch(|| command.output()).expect("run the new executable");
    assert!(
        output.status.success(),
        "{} update --contract --json: {output:?}",
        executable.display()
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("handover contract");
    assert_eq!(
        report["admission_contract"],
        orbit_common::fs::generation::GENERATION_CONTRACT,
        "the warmed executable must speak the handover admission contract"
    );
}

/// Flip a byte of the image's `LC_UUID`, leaving every other byte intact.
#[cfg(target_os = "macos")]
fn renew_macho_uuid(path: &Path) {
    use std::io::{Read, Seek, SeekFrom};
    const LC_UUID: u32 = 0x1b;
    const HEADER: u64 = 32;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open candidate");
    let mut header = [0u8; HEADER as usize];
    file.read_exact(&mut header).expect("Mach-O header");
    let size = u32::from_le_bytes(header[20..24].try_into().expect("sizeofcmds")) as usize;
    let mut commands = vec![0u8; size];
    file.read_exact(&mut commands).expect("load commands");
    let mut offset = 0;
    while offset + 8 <= commands.len() {
        let kind = u32::from_le_bytes(commands[offset..offset + 4].try_into().expect("cmd"));
        let length = u32::from_le_bytes(
            commands[offset + 4..offset + 8]
                .try_into()
                .expect("cmdsize"),
        ) as usize;
        assert!(length >= 8, "malformed load command");
        if kind == LC_UUID {
            let uuid = HEADER + offset as u64 + 8;
            file.seek(SeekFrom::Start(uuid)).expect("seek uuid");
            file.write_all(&[commands[offset + 8] ^ 0xff])
                .expect("rewrite uuid");
            return;
        }
        offset += length;
    }
    panic!("candidate has no LC_UUID");
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
