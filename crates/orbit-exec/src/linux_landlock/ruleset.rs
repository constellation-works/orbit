//! The Landlock syscall layer: build a ruleset, then restrict the child to it.
//!
//! `libc` has no wrappers for the three Landlock syscalls, so they are issued
//! through `syscall(2)` with the ABI's own structures. Everything here is
//! Linux-only; the platform decision itself lives in the parent module.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Child;

use orbit_common::OrbitError;

use super::{LandlockGrant, LandlockPathGrant};
use crate::runner::ExecRequest;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
/// Moving or linking a file into a different directory. Landlock refuses a
/// reparent that would give the file more access at its destination, which is
/// what keeps a denied file from being relocated into a readable directory.
const ACCESS_FS_REFER: u64 = 1 << 13;

const HANDLED_FS_ACCESS: u64 =
    ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_REFER;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Ask the kernel which Landlock ABI it implements. A negative result means
/// Landlock is absent or disabled.
pub(super) fn abi_version() -> i64 {
    // SAFETY: the version probe is defined as a null attribute pointer and a
    // zero size; it reads nothing from user space.
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    }
}

pub(super) fn spawn_restricted(
    req: &ExecRequest,
    grants: &[LandlockPathGrant],
) -> Result<Child, OrbitError> {
    let ruleset = Ruleset::create()?;
    for grant in grants {
        ruleset.add_path(grant)?;
    }

    let mut command = crate::process::command(req);
    restrict_child(&mut command, ruleset.as_raw_fd());
    command.spawn().map_err(|error| {
        OrbitError::Execution(format!("failed to spawn `{}`: {error}", req.program))
    })
}

/// Apply the ruleset in the child, between `fork` and `exec`, so the confined
/// process and every descendant it later spawns inherit it.
fn restrict_child(command: &mut std::process::Command, ruleset_fd: i32) {
    // SAFETY: the closure runs in the forked child before exec and issues only
    // async-signal-safe syscalls on the inherited ruleset descriptor.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::syscall(
                libc::SYS_landlock_restrict_self,
                ruleset_fd as libc::c_long,
                0u32,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

struct Ruleset {
    fd: OwnedFd,
}

impl Ruleset {
    fn create() -> Result<Self, OrbitError> {
        let attr = RulesetAttr {
            handled_access_fs: HANDLED_FS_ACCESS,
        };
        // SAFETY: `attr` is a well-formed `landlock_ruleset_attr` and its size
        // is passed alongside it.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::addr_of!(attr),
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(OrbitError::PolicyDenied(format!(
                "landlock_create_ruleset failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `fd` is a freshly created ruleset descriptor this scope owns.
        let ruleset = Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        };
        set_cloexec(ruleset.as_raw_fd())?;
        Ok(ruleset)
    }

    fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    fn add_path(&self, grant: &LandlockPathGrant) -> Result<(), OrbitError> {
        use std::os::unix::ffi::OsStrExt;

        let path = std::ffi::CString::new(grant.path.as_os_str().as_bytes()).map_err(|_| {
            OrbitError::InvalidInput(format!(
                "landlock path `{}` contains an interior NUL",
                grant.path.display()
            ))
        })?;
        // SAFETY: `path` is a NUL-terminated absolute path. `O_PATH` opens a
        // descriptor that names the inode without granting access to it.
        let parent_fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if parent_fd < 0 {
            return Err(OrbitError::Io(format!(
                "open landlock grant `{}`: {}",
                grant.path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let rule = PathBeneathAttr {
            allowed_access: access_bits(grant.grant),
            parent_fd,
        };
        // SAFETY: `rule` holds the live `O_PATH` descriptor opened above and an
        // access mask that is a subset of the handled set.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.as_raw_fd() as libc::c_long,
                LANDLOCK_RULE_PATH_BENEATH as libc::c_long,
                std::ptr::addr_of!(rule),
                0u32,
            )
        };
        // SAFETY: `parent_fd` is the descriptor opened in this function and is
        // no longer referenced by the kernel after `landlock_add_rule` returns.
        unsafe { libc::close(parent_fd) };
        if added != 0 {
            return Err(OrbitError::PolicyDenied(format!(
                "landlock_add_rule failed for `{}`: {}",
                grant.path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

/// `REFER` is only meaningful on a directory, so a file grant omits it along
/// with `READ_DIR`.
fn access_bits(grant: LandlockGrant) -> u64 {
    match grant {
        LandlockGrant::ReadTree => {
            ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_REFER
        }
        LandlockGrant::ListOnly => ACCESS_FS_READ_DIR | ACCESS_FS_REFER,
        LandlockGrant::ReadFile => ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE,
    }
}

fn set_cloexec(fd: i32) -> Result<(), OrbitError> {
    // SAFETY: `fd` is an open descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    // SAFETY: as above; only the close-on-exec flag is added.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(OrbitError::Io(format!(
            "set close-on-exec on landlock ruleset: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}
