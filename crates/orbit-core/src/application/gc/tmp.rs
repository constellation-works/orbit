//! Checkout scratch collection. Run observation deliberately does not reconcile.

use std::path::PathBuf;

use orbit_common::fs::path::orbit_scratch_dir;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::JobRunState;
use serde::Serialize;

use crate::{OrbitError, OrbitRuntime};

#[derive(Debug, Serialize)]
pub struct TmpGcReport {
    #[serde(serialize_with = "display_path")]
    pub path: PathBuf,
    pub action: &'static str,
    pub bytes_reclaimable: u64,
    pub bytes_reclaimed: u64,
}

#[derive(Debug, Serialize)]
pub struct TmpGcResult {
    #[serde(serialize_with = "display_path")]
    pub path: PathBuf,
    pub dry_run: bool,
    /// Number of top-level entries removed, including whole directory trees.
    pub entries_removed: usize,
    pub bytes_reclaimable: u64,
    pub bytes_reclaimed: u64,
    pub reports: Vec<TmpGcReport>,
}

// Reports use the same lossy display as human output, while traversal retains
// the original byte names. A non-UTF-8 entry must not fail JSON after deletion.
fn display_path<S: serde::Serializer>(
    path: &std::path::Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

impl OrbitRuntime {
    /// Preview or empty this workspace checkout's `.orbit/tmp`, retaining the
    /// directory. Pending, running, and retrying runs refuse deletion even with
    /// stale owners.
    pub fn gc_tmp(&self, delete: bool) -> Result<TmpGcResult, OrbitError> {
        if delete {
            self.refuse_active_tmp_gc()?;
        }
        let checkout = self.paths().repo_root.canonicalize()?;
        let mut result = TmpGcResult {
            path: orbit_scratch_dir(&checkout),
            dry_run: !delete,
            entries_removed: 0,
            bytes_reclaimable: 0,
            bytes_reclaimed: 0,
            reports: Vec::new(),
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        filesystem::collect(&checkout, &mut result, || self.refuse_active_tmp_gc())?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(OrbitError::InvalidInput(
            "tmp collection requires Linux or macOS directory-relative filesystem operations"
                .into(),
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Ok(result)
    }

    fn refuse_active_tmp_gc(&self) -> Result<(), OrbitError> {
        let jobs = self.stores().jobs();
        let mut runs = jobs.list_job_runs_filtered(&JobRunQuery {
            active_only: true,
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        // `active_only` is pending/running only. A retrying run sleeps between
        // attempts and then executes again, so its scratch is still in use.
        runs.extend(jobs.list_job_runs_filtered(&JobRunQuery {
            state: Some(JobRunState::Retrying),
            include_steps: false,
            ..JobRunQuery::default()
        })?);
        let mut run_ids: Vec<_> = runs.into_iter().map(|run| run.run_id).collect();
        run_ids.sort();
        if run_ids.is_empty() {
            Ok(())
        } else {
            Err(OrbitError::TmpGcActiveRuns { run_ids })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod filesystem {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::{OrbitError, TmpGcReport, TmpGcResult};

    /// What one no-follow walk of an entry saw.
    #[derive(Clone, Copy, Default)]
    pub(in crate::application::gc) struct Tally {
        pub bytes: u64,
        /// Newest `(seconds, nanoseconds)` mtime of the entry or anything
        /// beneath it, read from the link itself for a symlink.
        pub newest: (i64, i64),
    }

    pub(super) fn collect(
        checkout: &Path,
        result: &mut TmpGcResult,
        check_runs: impl FnOnce() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        collect_inner(checkout, result, check_runs, || {})
    }

    #[cfg(test)]
    pub(in crate::application::gc) fn collect_with_after_listing(
        checkout: &Path,
        result: &mut TmpGcResult,
        check_runs: impl FnOnce() -> Result<(), OrbitError>,
        after_listing: impl FnMut(),
    ) -> Result<(), OrbitError> {
        collect_inner(checkout, result, check_runs, after_listing)
    }

    fn collect_inner(
        checkout: &Path,
        result: &mut TmpGcResult,
        check_runs: impl FnOnce() -> Result<(), OrbitError>,
        mut after_listing: impl FnMut(),
    ) -> Result<(), OrbitError> {
        // Pin each directory separately. Neither `.orbit`, `tmp`, nor any
        // descendant can redirect a traversal through a symlink after a check.
        let root_name = CString::new(checkout.as_os_str().as_bytes())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let root = open_directory(libc::AT_FDCWD, &root_name)?;
        let orbit = match open_directory(root.as_raw_fd(), c".orbit") {
            Ok(orbit) => orbit,
            // No `.orbit` means no scratch to measure or remove, in either
            // mode. Confirm must not create it as a side effect of GC.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let tmp = match open_directory(orbit.as_raw_fd(), c"tmp") {
            Ok(tmp) => tmp,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if result.dry_run {
                    return Ok(());
                }
                // SAFETY: orbit is an owned directory descriptor; name is a
                // single NUL-terminated component. No path traversal occurs.
                if unsafe { libc::mkdirat(orbit.as_raw_fd(), c"tmp".as_ptr(), 0o700) } < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                open_directory(orbit.as_raw_fd(), c"tmp")?
            }
            Err(error) => return Err(error.into()),
        };
        let mut names = names(&tmp)?;
        names.sort();
        after_listing();
        // Finish the entire no-follow measurement before removing anything.
        for name in &names {
            let tally = if result.dry_run {
                let Some(tally) = measure(&tmp, name, &mut after_listing)? else {
                    continue;
                };
                tally
            } else {
                walk(&tmp, name, false)?
            };
            let bytes = tally.bytes;
            result.bytes_reclaimable = result.bytes_reclaimable.saturating_add(bytes);
            result.reports.push(TmpGcReport {
                path: result
                    .path
                    .join(std::ffi::OsStr::from_bytes(name.to_bytes())),
                action: "would_remove",
                bytes_reclaimable: bytes,
                bytes_reclaimed: 0,
            });
        }
        if !result.dry_run {
            // Measurement can take time; observe admission again immediately
            // before deletion. Never reconcile or cancel a run to make room.
            check_runs()?;
            for (name, report) in names.iter().zip(&mut result.reports) {
                report.bytes_reclaimed = walk(&tmp, name, true)?.bytes;
                report.action = "removed";
                result.entries_removed += 1;
                result.bytes_reclaimed = result
                    .bytes_reclaimed
                    .saturating_add(report.bytes_reclaimed);
            }
        }
        Ok(())
    }

    pub(in crate::application::gc) fn open_directory(
        parent: libc::c_int,
        name: &CStr,
    ) -> io::Result<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: name is NUL-terminated, and parent is AT_FDCWD or a live fd.
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: openat returned a new descriptor owned by this caller.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    struct DirectoryStream(*mut libc::DIR);

    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: fdopendir created this stream and it is closed only here.
            unsafe { libc::closedir(self.0) };
        }
    }

    pub(in crate::application::gc) fn names(directory: &OwnedFd) -> io::Result<Vec<CString>> {
        // Opening `.` gives the stream its own offset, so subsequent walks
        // enumerate independently of earlier preview/measurement walks.
        let fd = open_directory(directory.as_raw_fd(), c".")?;
        // SAFETY: fd is owned and valid; on success fdopendir takes ownership.
        let stream = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _ = fd.into_raw_fd();
        let stream = DirectoryStream(stream);
        let mut names = Vec::new();
        loop {
            // SAFETY: errno is thread-local and stream is live. Copy the name
            // before readdir invalidates its returned pointer on the next call.
            unsafe {
                *errno() = 0;
                let entry = libc::readdir(stream.0);
                if entry.is_null() {
                    let error = *errno();
                    return if error == 0 {
                        Ok(names)
                    } else {
                        Err(io::Error::from_raw_os_error(error))
                    };
                }
                let name = CStr::from_ptr((*entry).d_name.as_ptr());
                if name != c"." && name != c".." {
                    names.push(name.to_owned());
                }
            }
        }
    }

    unsafe fn errno() -> *mut libc::c_int {
        #[cfg(target_os = "linux")]
        // SAFETY: libc returns this thread's errno storage.
        unsafe {
            libc::__errno_location()
        }
        #[cfg(target_os = "macos")]
        // SAFETY: libc returns this thread's errno storage.
        unsafe {
            libc::__error()
        }
    }

    pub(in crate::application::gc) fn walk(
        parent: &OwnedFd,
        name: &CStr,
        delete: bool,
    ) -> io::Result<Tally> {
        let Some(tally) = walk_inner(parent, name, delete, false, &mut || {})? else {
            return Err(io::Error::other(
                "strict scratch walk unexpectedly skipped a missing entry",
            ));
        };
        Ok(tally)
    }

    /// Measure one preview entry, ignoring it if it disappears at any point
    /// in its no-follow traversal. Strict walks used by deletion and age-based
    /// collection keep reporting the I/O error.
    fn measure(
        parent: &OwnedFd,
        name: &CStr,
        after_listing: &mut impl FnMut(),
    ) -> io::Result<Option<Tally>> {
        walk_inner(parent, name, false, true, after_listing)
    }

    fn walk_inner(
        parent: &OwnedFd,
        name: &CStr,
        delete: bool,
        skip_vanished: bool,
        after_listing: &mut impl FnMut(),
    ) -> io::Result<Option<Tally>> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live, name is NUL-terminated, and fstatat writes
        // the stat only on success. AT_SYMLINK_NOFOLLOW measures the link itself.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if skip_vanished && error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error);
        }
        // SAFETY: fstatat succeeded and initialized metadata.
        let metadata = unsafe { metadata.assume_init() };
        let is_directory = metadata.st_mode & libc::S_IFMT == libc::S_IFDIR;
        // The stat time fields are `i64` on 64-bit targets and narrower on
        // 32-bit ones; widen them without a cast either way.
        #[allow(clippy::useless_conversion)]
        let newest = (
            i64::from(metadata.st_mtime),
            i64::from(metadata.st_mtime_nsec),
        );
        let mut tally = Tally { bytes: 0, newest };
        if is_directory {
            let directory = match open_directory(parent.as_raw_fd(), name) {
                Ok(directory) => directory,
                Err(error) if skip_vanished && error.kind() == io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let children = match names(&directory) {
                Ok(children) => children,
                Err(error) if skip_vanished && error.kind() == io::ErrorKind::NotFound => {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            after_listing();
            for child_name in children {
                let Some(child) = walk_inner(
                    &directory,
                    &child_name,
                    delete,
                    skip_vanished,
                    after_listing,
                )?
                else {
                    continue;
                };
                tally.bytes = tally.bytes.saturating_add(child.bytes);
                tally.newest = tally.newest.max(child.newest);
            }
        } else if matches!(
            metadata.st_mode & libc::S_IFMT,
            libc::S_IFREG | libc::S_IFLNK
        ) {
            tally.bytes = u64::try_from(metadata.st_size).unwrap_or(0);
        }
        if delete {
            let flags = if is_directory { libc::AT_REMOVEDIR } else { 0 };
            // SAFETY: unlinkat addresses a single entry of the pinned parent.
            // It unlinks a symlink itself, never its target. A type swap fails
            // rather than switching between file and directory removal.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Some(tally))
    }
}
