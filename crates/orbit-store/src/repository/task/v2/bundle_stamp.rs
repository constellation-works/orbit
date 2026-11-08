//! Whole-bundle freshness probes, without reading bundle contents.

use std::ffi::CStr;
use std::fs;
use std::path::Path;

use super::envelope_cache;

/// Renames and artifact removals move directory stamps; appends and
/// truncations move the logs. Stamp the envelope itself too, replacing the
/// decision scan's separate envelope freshness pass, including in-place edits.
const BUNDLE_STAMP_ENTRIES: [(&str, &CStr); 6] = [
    ("", c"."),
    ("task.yaml", c"task.yaml"),
    ("artifacts", c"artifacts"),
    ("artifacts/files", c"artifacts/files"),
    ("events.jsonl", c"events.jsonl"),
    ("comments.jsonl", c"comments.jsonl"),
];

/// A proof of the entries above, including absence. Unprovable entries
/// leave the caller to read the bundle. Linux anchors probes to one open
/// directory, avoiding repeated traversal of a workspace's long prefix.
pub(super) fn task_bundle_freshness_stamp(bundle_dir: &Path) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_PATH needs only traversal permission, just like metadata(). A
        // platform or sandbox that refuses it keeps the ordinary probes.
        if let Ok(directory) = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
            .open(bundle_dir)
        {
            return hash_bundle_entry_stamps(|_, entry, output| {
                linux_relative_entry_stamp(&directory, entry, output)
            });
        }
    }
    hash_bundle_entry_stamps(|entry, _, output| {
        match fs::metadata(bundle_dir.join(entry)) {
            Ok(metadata) => output.push_str(&envelope_cache::persisted_metadata_stamp(&metadata)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => output.push_str("absent"),
            Err(_) => return None,
        }
        Some(())
    })
}

fn hash_bundle_entry_stamps(
    mut probe: impl FnMut(&str, &CStr, &mut String) -> Option<()>,
) -> Option<String> {
    let mut lines = String::with_capacity(512);
    for (name, entry) in BUNDLE_STAMP_ENTRIES {
        lines.push_str(name);
        lines.push('=');
        probe(name, entry, &mut lines)?;
        lines.push('\n');
    }
    Some(blake3::hash(lines.as_bytes()).to_hex().to_string())
}

#[cfg(target_os = "linux")]
fn linux_relative_entry_stamp(
    directory: &fs::File,
    entry: &CStr,
    output: &mut String,
) -> Option<()> {
    use std::fmt::Write as _;
    use std::os::fd::AsRawFd;

    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the directory remains open, entry is NUL-terminated, and stat
    // points to writable storage. Flags 0 follow symlinks, as metadata does.
    let result =
        unsafe { libc::fstatat(directory.as_raw_fd(), entry.as_ptr(), stat.as_mut_ptr(), 0) };
    if result != 0 {
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
            return None;
        }
        output.push_str("absent");
        return Some(());
    }
    // SAFETY: a successful fstatat initialized the entire stat structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_size < 0 || stat.st_mtime < 0 || !(0..1_000_000_000).contains(&stat.st_mtime_nsec) {
        return None;
    }
    // The same len:seconds.nanoseconds:device:inode representation as
    // envelope_cache::persisted_metadata_stamp on Linux.
    write!(
        output,
        "{}:{}.{:09}:{}:{}",
        stat.st_size, stat.st_mtime, stat.st_mtime_nsec, stat.st_dev, stat.st_ino
    )
    .ok()
}
