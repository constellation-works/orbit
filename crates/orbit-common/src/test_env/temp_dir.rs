#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicUsize, Ordering};

/// The system temporary directory with every symlink resolved.
///
/// macOS points `TMPDIR` under `/var`, a symlink to `/private/var`, so a
/// `tempfile::tempdir()` root is spelled through a link. Orbit resolves
/// symlinks in the roots it is given (discovery lists a canonical directory and
/// reports canonical paths), so a fixture that compares those paths with the
/// spelled root, or hands a path to code that must see one physical spelling,
/// passes on Linux and fails on macOS. Create such a fixture's root with
/// `tempfile::tempdir_in(canonical_temp_dir())` instead of overriding `TMPDIR`
/// for the whole suite: fixtures that do not care keep the ordinary root, so a
/// symlinked ancestor keeps being exercised where it should be.
pub fn canonical_temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// A parent for the root of a fixture that seeds thousands of durable writes.
///
/// Every task write fsyncs its files and their directory. On a tmpfs temp
/// directory that costs nothing. A managed executor points `TMPDIR` at
/// its scratch directory inside the checkout, though, and there a disk under a
/// busy build host took over a second per fsync: a 4,000-task seed that runs
/// in 10 s on tmpfs reached 185 tasks in 270 s (ORB-14818). On Linux this is
/// the system temp directory when it is a tmpfs, else the first writable tmpfs
/// of `/dev/shm` and `/tmp`, else the system temp directory. Create the root
/// with `tempfile::tempdir_in(bulk_write_temp_dir())`.
pub fn bulk_write_temp_dir() -> std::path::PathBuf {
    let default = std::env::temp_dir();
    #[cfg(target_os = "linux")]
    {
        let tmpfs = |dir: &std::path::Path| {
            use std::os::unix::ffi::OsStrExt;
            let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
                return false;
            };
            let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
            // Safety: `statfs` reads the NUL-terminated path and fills only
            // the struct it is handed.
            if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
                return false;
            }
            // Safety: a zero return means the kernel filled `stat`.
            unsafe { stat.assume_init() }.f_type == libc::TMPFS_MAGIC
        };
        // A sandbox may mount a directory it does not let this process write.
        let writable = |dir: &std::path::Path| {
            static PROBES: AtomicUsize = AtomicUsize::new(0);
            let probe = dir.join(format!(
                ".orbit-bulk-write-probe-{}-{}",
                std::process::id(),
                PROBES.fetch_add(1, Ordering::Relaxed)
            ));
            let created = crate::fs::io::create_private_dir(&probe).is_ok();
            created && std::fs::remove_dir(&probe).is_ok()
        };
        if !tmpfs(&default) {
            let candidates = ["/dev/shm", "/tmp"].map(std::path::PathBuf::from);
            if let Some(dir) = candidates
                .into_iter()
                .find(|dir| tmpfs(dir) && writable(dir))
            {
                return dir;
            }
        }
    }
    default
}
