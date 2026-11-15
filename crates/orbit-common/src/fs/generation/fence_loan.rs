//! Lending an updater's exclusive generation fence to one convergence child.
//!
//! `orbit update` pins the candidate generation in every authority it
//! admitted against and holds those pins until convergence ends, so no older
//! executable enters between it and its candidate's children. A pending
//! layout migration, run by the `migrate --confirm` child, must hold
//! admission exclusively on every other authority that has used the
//! workspace. The updater's pin is a shared hold that registers no
//! participant, which the child's fence cannot tell from an unknown client.
//!
//! The updater therefore takes that fence itself
//! ([`GenerationUpdate::fence_pinned`]) and lends it: both lock descriptors
//! are inherited by the child, and [`LENT_FENCE_ENV`] names them.
//! [`GenerationUpdate::acquire_fence`] adopts a lent fence only after the
//! kernel proves it: each descriptor must be the authority's lock file (same
//! device and inode) and must take its exclusive lock without waiting, which
//! succeeds only on the description that already holds it or when nobody
//! holds it at all. Anything else is ignored and the child takes admission
//! itself, which refuses while the updater holds it. The child never unlocks
//! a borrowed description; once it exits, the updater confirms it still holds
//! both locks ([`GenerationUpdate::ensure_still_exclusive`]) and pins the
//! candidate again before releasing admission, so the authority is never left
//! unprotected between the pin, the migration and the rest of convergence.

use std::path::Path;
use std::process::Command;

use super::update::GenerationUpdate;
use crate::OrbitError;

/// Environment variable naming the generation fences a parent lent to this
/// process: one line per authority, `<admission fd>:<generation fd>:<root>`.
pub const LENT_FENCE_ENV: &str = "ORBIT_LENT_GENERATION_FENCE";

impl GenerationUpdate {
    /// Take exclusive admission on `root` for a fence, as [`Self::acquire`]
    /// does, unless this process inherited exactly that fence from the
    /// process that started it (see the module docs). A pending switch still
    /// refuses an adopted fence.
    pub fn acquire_fence(root: &Path) -> Result<Self, OrbitError> {
        match lent::adopt(root) {
            Some(update) => {
                update.refuse_pending_switch()?;
                Ok(update)
            }
            None => Self::acquire(root),
        }
    }

    /// Let the child `command` starts inherit `fences`, for
    /// [`Self::acquire_fence`] to adopt. Descriptor inheritance is a Unix
    /// contract; elsewhere nothing is lent and returns `false`, so the caller
    /// keeps its pins instead of fencing.
    pub fn lend_fences(command: &mut Command, fences: &[&GenerationUpdate]) -> bool {
        lent::lend(command, fences)
    }

    /// Whether fences can be lent to a child on this platform.
    pub fn can_lend_fences() -> bool {
        cfg!(unix)
    }
}

#[cfg(unix)]
mod lent {
    use std::fmt::Write as _;
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    use fs2::FileExt;

    use super::super::paths::{
        ADMISSION_LOCK, GENERATION_LOCK, validated_generation_record_path,
        validated_generation_root,
    };
    use super::super::records::Record;
    use super::super::update::GenerationUpdate;
    use super::LENT_FENCE_ENV;

    pub(super) fn lend(command: &mut Command, fences: &[&GenerationUpdate]) -> bool {
        let mut lent = String::new();
        let mut fds = Vec::new();
        for fence in fences {
            let Some(root) = fence.root.to_str().filter(|root| !root.contains('\n')) else {
                // Not nameable in the variable: the child fences it itself
                // and refuses against the held admission.
                continue;
            };
            let admission = fence.admission.file.as_raw_fd();
            let generation = fence.generation.file.as_raw_fd();
            let _ = writeln!(lent, "{admission}:{generation}:{root}");
            fds.extend([admission, generation]);
        }
        if fds.is_empty() {
            return false;
        }
        command.env(LENT_FENCE_ENV, lent);
        // SAFETY: the closure runs in the forked child before exec and only
        // clears close-on-exec, async-signal-safely, on descriptors the
        // parent keeps open until the child has exited.
        unsafe {
            command.pre_exec(move || {
                for fd in &fds {
                    let flags = libc::fcntl(*fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        true
    }

    pub(super) fn adopt(root: &Path) -> Option<GenerationUpdate> {
        let lent = std::env::var(LENT_FENCE_ENV).ok()?;
        let validated = validated_generation_root(root).ok()?;
        lent.lines().find_map(|line| {
            let mut parts = line.splitn(3, ':');
            let admission = parts.next()?.parse::<RawFd>().ok()?;
            let generation = parts.next()?.parse::<RawFd>().ok()?;
            if Path::new(parts.next()?) != validated {
                return None;
            }
            let admission = borrow(admission, &validated, ADMISSION_LOCK)?;
            let generation = borrow(generation, &validated, GENERATION_LOCK)?;
            Some(GenerationUpdate {
                root: validated.clone(),
                admission,
                generation,
            })
        })
    }

    /// The lent descriptor `fd` of `root`'s lock `name`, proven to hold it
    /// exclusively.
    fn borrow(fd: RawFd, root: &Path, name: &str) -> Option<Record> {
        if fd <= libc::STDERR_FILENO {
            return None;
        }
        // SAFETY: `F_DUPFD_CLOEXEC` only reads `fd`, failing with EBADF when
        // it is not open; on success it returns a new descriptor.
        let dup = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            return None;
        }
        // SAFETY: `dup` was just created and nothing else owns it.
        let file = unsafe { File::from_raw_fd(dup) };
        // Keep the lent descriptor from reaching this process's own children.
        // SAFETY: `fd` is open (it was just duplicated); this sets one flag.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
        let path = validated_generation_record_path(root, name).ok()?;
        let expected = std::fs::metadata(&path).ok()?;
        let actual = file.metadata().ok()?;
        if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
            return None;
        }
        // A description that already holds the lock exclusively takes it
        // again at once; any other succeeds only when nobody holds it.
        let record = Record {
            writable: {
                // SAFETY: `dup` is open and owned by `file`; this reads flags.
                let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
                flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDWR
            },
            file,
            borrowed: true,
        };
        FileExt::try_lock_exclusive(&record.file).ok()?;
        Some(record)
    }
}

#[cfg(not(unix))]
mod lent {
    use std::path::Path;
    use std::process::Command;

    use super::super::update::GenerationUpdate;

    pub(super) fn lend(_command: &mut Command, _fences: &[&GenerationUpdate]) -> bool {
        false
    }

    pub(super) fn adopt(_root: &Path) -> Option<GenerationUpdate> {
        None
    }
}
