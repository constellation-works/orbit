//! Directories held open by no-follow descriptors.
//!
//! A path is re-resolved on every use, so a component replaced after a check
//! redirects whatever the path is used for next. A descriptor stays bound to
//! the directory it opened. Every operation here addresses one entry of a held
//! directory and never follows a symlink, so replacing an ancestor or the entry
//! itself after it was checked cannot redirect a permission change, traversal
//! or unlink outside the held tree.

use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

/// A path descriptor needs only search access to the parent, not read access
/// to the directory, so it can hold one whose access is still to be restored.
const HELD_FLAGS: libc::c_int =
    libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
/// Listing reopens a held directory's own `.`, which is never a symlink.
const LIST_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
const OWNER_RWX: libc::mode_t = 0o700;

/// The device and inode an entry had when it was observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}

/// What a no-follow stat of one entry saw.
pub(super) struct Entry {
    pub(super) identity: Identity,
    /// A real directory, not a symlink to one.
    pub(super) is_directory: bool,
}

pub(super) struct HeldDirectory {
    fd: OwnedFd,
    identity: Identity,
    mode: libc::mode_t,
}

/// One path component as the C string the `*at` calls take.
pub(super) fn entry_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

impl HeldDirectory {
    /// Open an absolute path from `/`, one component at a time, without
    /// following a symlink anywhere along it.
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let mut held = Self::from_fd(open_at(libc::AT_FDCWD, c"/", HELD_FLAGS)?)?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => held = held.open_child(&entry_name(name)?)?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("'{}' is not an absolute canonical path", path.display()),
                    ));
                }
            }
        }
        Ok(held)
    }

    pub(super) fn identity(&self) -> Identity {
        self.identity
    }

    /// Open a child directory. A symlink is refused (`ELOOP`), never followed.
    pub(super) fn open_child(&self, name: &CStr) -> io::Result<Self> {
        Self::from_fd(open_at(self.fd.as_raw_fd(), name, HELD_FLAGS)?)
    }

    /// Open a child directory only while it is still the observed one.
    pub(super) fn open_entry(&self, name: &CStr, expected: Identity) -> io::Result<Self> {
        let child = self.open_child(name)?;
        if child.identity != expected {
            return Err(replaced(name));
        }
        Ok(child)
    }

    /// Stat one entry without following it.
    pub(super) fn entry(&self, name: &CStr) -> io::Result<Entry> {
        let metadata = stat_at(self.fd.as_raw_fd(), name)?;
        Ok(Entry {
            identity: identity(&metadata),
            is_directory: metadata.st_mode & libc::S_IFMT == libc::S_IFDIR,
        })
    }

    /// The names of this directory's entries, without `.` and `..`.
    pub(super) fn names(&self) -> io::Result<Vec<CString>> {
        // Opening `.` gives the stream its own offset and descriptor, which
        // the stream then owns and closes.
        let fd = open_at(self.fd.as_raw_fd(), c".", LIST_FLAGS)?;
        // SAFETY: fd is a live directory descriptor; on success the stream
        // takes ownership of it.
        let stream = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _ = fd.into_raw_fd();
        let stream = DirectoryStream(stream);
        let mut names = Vec::new();
        loop {
            // SAFETY: errno is this thread's and the stream is live. The name
            // is copied before the next readdir invalidates the entry.
            unsafe {
                *libc::__errno_location() = 0;
                let entry = libc::readdir(stream.0);
                if entry.is_null() {
                    let error = *libc::__errno_location();
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

    /// The summed size of every entry beneath this directory.
    pub(super) fn contents_bytes(&self) -> io::Result<u64> {
        let mut total = 0u64;
        for name in self.names()? {
            let metadata = stat_at(self.fd.as_raw_fd(), &name)?;
            total = total.saturating_add(u64::try_from(metadata.st_size).unwrap_or(0));
            if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
                let child = self.open_entry(&name, identity(&metadata))?;
                total = total.saturating_add(child.contents_bytes()?);
            }
        }
        Ok(total)
    }

    /// Remove the child directory `name` and everything beneath it, while it
    /// is still the `expected` directory.
    ///
    /// Owner access is restored to each directory through its own descriptor
    /// before its entries are unlinked, so a read-only directory a test left
    /// behind cannot stop the removal midway. Symlinks inside are unlinked,
    /// never followed. After the opening identity check, only the final
    /// unlink of `name` addresses it by name again: that removes an empty
    /// directory, so a substitute that took the name in that instant is
    /// refused unless it is empty.
    pub(super) fn remove_directory(&self, name: &CStr, expected: Identity) -> io::Result<()> {
        let directory = self.open_entry(name, expected)?;
        if directory.mode & OWNER_RWX != OWNER_RWX {
            // A path descriptor cannot be passed to fchmod. Its /proc link
            // names exactly the held inode, never a path that can move.
            fs::set_permissions(
                format!("/proc/self/fd/{}", directory.fd.as_raw_fd()),
                fs::Permissions::from_mode((directory.mode | OWNER_RWX) & 0o7777),
            )?;
        }
        for child in directory.names()? {
            let entry = directory.entry(&child)?;
            if entry.is_directory {
                directory.remove_directory(&child, entry.identity)?;
            } else {
                // A type swap to a directory fails rather than switching to
                // directory removal; a symlink is unlinked, not followed.
                unlink_at(directory.fd.as_raw_fd(), &child, 0)?;
            }
        }
        if self.entry(name)?.identity != expected {
            return Err(replaced(name));
        }
        unlink_at(self.fd.as_raw_fd(), name, libc::AT_REMOVEDIR)
    }

    fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        let metadata = stat_fd(fd.as_raw_fd())?;
        Ok(Self {
            fd,
            identity: identity(&metadata),
            mode: metadata.st_mode,
        })
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: fdopendir created this stream and it is closed only here.
        unsafe { libc::closedir(self.0) };
    }
}

fn replaced(name: &CStr) -> io::Error {
    io::Error::other(format!(
        "'{}' was replaced after it was checked",
        name.to_string_lossy()
    ))
}

// `dev_t` and `ino_t` are `u64` on 64-bit targets and narrower on some 32-bit
// ones; widen them without a cast either way.
#[allow(clippy::useless_conversion)]
fn identity(metadata: &libc::stat) -> Identity {
    Identity {
        device: u64::from(metadata.st_dev),
        inode: u64::from(metadata.st_ino),
    }
}

fn open_at(parent: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: name is NUL-terminated and parent is AT_FDCWD or a live fd.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: openat returned a new descriptor owned by this caller.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn stat_at(parent: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: parent is live, name is NUL-terminated, and fstatat writes the
    // stat only on success. AT_SYMLINK_NOFOLLOW stats a link itself.
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat succeeded and initialized metadata.
    Ok(unsafe { metadata.assume_init() })
}

fn stat_fd(fd: RawFd) -> io::Result<libc::stat> {
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is live and fstat writes the stat only on success.
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded and initialized metadata.
    Ok(unsafe { metadata.assume_init() })
}

fn unlink_at(parent: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: unlinkat addresses a single entry of a live parent descriptor
    // and removes a symlink itself, never its target.
    if unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
