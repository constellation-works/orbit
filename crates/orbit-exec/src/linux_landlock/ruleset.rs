//! The Landlock syscall layer: build a ruleset, then restrict the child to it.
//!
//! `libc` has no wrappers for the three Landlock syscalls, so they are issued
//! through `syscall(2)` with the ABI's own structures. Everything here is
//! Linux-only; the platform decision itself lives in the parent module.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Child;

use orbit_common::OrbitError;

use super::{LandlockGrant, LandlockPathGrant, NETWORK_LANDLOCK_ABI, RulesetScope};
use crate::runner::ExecRequest;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;
const ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
/// Moving or linking a file into a different directory. Landlock refuses a
/// reparent that would give the file more access at its destination, which is
/// what keeps a denied file from being relocated into a readable directory.
const ACCESS_FS_REFER: u64 = 1 << 13;
/// ABI 3: `truncate(2)` and `O_TRUNC` on an already-granted file.
const ACCESS_FS_TRUNCATE: u64 = 1 << 14;

const HANDLED_FS_ACCESS: u64 =
    ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_REFER;

/// Every write-side right the plugin boundary takes over from the kernel's
/// default allow. `TRUNCATE` is masked off below ABI 3, where the kernel does
/// not know it.
const HANDLED_FS_WRITE_ACCESS: u64 = ACCESS_FS_WRITE_FILE
    | ACCESS_FS_REMOVE_DIR
    | ACCESS_FS_REMOVE_FILE
    | ACCESS_FS_MAKE_CHAR
    | ACCESS_FS_MAKE_DIR
    | ACCESS_FS_MAKE_REG
    | ACCESS_FS_MAKE_SOCK
    | ACCESS_FS_MAKE_FIFO
    | ACCESS_FS_MAKE_BLOCK
    | ACCESS_FS_MAKE_SYM
    | ACCESS_FS_TRUNCATE;

/// ABI 4: TCP bind and connect. Handling both with no port rule refuses every
/// TCP endpoint, which is how `network: none` is held at the kernel.
const ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;

/// First ABI that knows `TRUNCATE`.
const TRUNCATE_ABI: i64 = 3;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

/// `struct landlock_ruleset_attr` as of ABI 4. A kernel that predates the
/// network field accepts the longer struct as long as the extra bytes are
/// zero, so one layout serves every ABI this module runs on.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
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
    scope: RulesetScope,
    inherited_fds: &[crate::process::InheritedFd],
) -> Result<Child, OrbitError> {
    let ruleset = Ruleset::create(scope, abi_version())?;
    for grant in grants {
        ruleset.add_path(grant)?;
    }
    spawn_with_ruleset(req, ruleset, inherited_fds)
}

/// Register both `pre_exec` hooks and start the child.
///
/// Split from [`spawn_restricted`] so the descriptor contract between the two
/// hooks can be exercised against a ruleset whose number is known to collide.
fn spawn_with_ruleset(
    req: &ExecRequest,
    ruleset: Ruleset,
    inherited_fds: &[crate::process::InheritedFd],
) -> Result<Child, OrbitError> {
    // The child remaps every inherited target with `dup2` before this ruleset
    // is applied, so a ruleset sitting on one of those numbers is replaced by
    // the credential and `landlock_restrict_self` refuses it with `EBADFD`.
    // The number is not arbitrary: minting a callback credential frees the
    // target, and the ruleset opened afterwards drops straight into the gap.
    // Moving the ruleset above every target settles it before either hook
    // exists, rather than depending on which descriptors happen to be free.
    let ruleset = ruleset.clear_of_targets(inherited_fds)?;

    let mut command = crate::process::command(req);
    // Before the ruleset, so a credential descriptor is in place whatever the
    // restriction does. `dup2` is not a filesystem access, so the order is a
    // readability choice rather than a requirement.
    crate::process::attach_inherited_fds(&mut command, inherited_fds);
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
    /// The filesystem rights this ruleset takes over; a rule may only grant a
    /// subset of them.
    handled_fs: u64,
}

impl Ruleset {
    fn create(scope: RulesetScope, abi: i64) -> Result<Self, OrbitError> {
        let mut handled_fs = HANDLED_FS_ACCESS;
        if scope.confine_writes {
            handled_fs |= HANDLED_FS_WRITE_ACCESS;
            if abi < TRUNCATE_ABI {
                handled_fs &= !ACCESS_FS_TRUNCATE;
            }
        }
        let handled_access_net = if scope.deny_tcp {
            if abi < NETWORK_LANDLOCK_ABI {
                return Err(OrbitError::PolicyDenied(format!(
                    "refusing TCP at the process boundary requires Landlock ABI \
                     {NETWORK_LANDLOCK_ABI} or later; this kernel supports ABI {abi}"
                )));
            }
            ACCESS_NET_BIND_TCP | ACCESS_NET_CONNECT_TCP
        } else {
            0
        };
        let attr = RulesetAttr {
            handled_access_fs: handled_fs,
            handled_access_net,
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
            handled_fs,
        };
        set_cloexec(ruleset.as_raw_fd())?;
        Ok(ruleset)
    }

    fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// Lift the ruleset above every descriptor number the child overwrites on
    /// its way to `exec`. A no-op when the spawn inherits nothing.
    fn clear_of_targets(
        self,
        inherited_fds: &[crate::process::InheritedFd],
    ) -> Result<Self, OrbitError> {
        let Self { fd, handled_fs } = self;
        let fd = crate::process::relocate_clear_of_targets(fd, inherited_fds).map_err(|error| {
            OrbitError::Io(format!(
                "move the landlock ruleset clear of the child's inherited descriptors: {error}"
            ))
        })?;
        Ok(Self { fd, handled_fs })
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
            allowed_access: access_bits(grant.grant) & self.handled_fs,
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
/// with `READ_DIR`. The write bits are masked to the handled set by the
/// caller, so a read-only ruleset never asks the kernel for a right it did
/// not take over.
fn access_bits(grant: LandlockGrant) -> u64 {
    match grant {
        LandlockGrant::ReadTree => {
            ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR | ACCESS_FS_REFER
        }
        LandlockGrant::ListOnly => ACCESS_FS_READ_DIR | ACCESS_FS_REFER,
        LandlockGrant::ReadFile => ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE,
        LandlockGrant::WriteTree => {
            ACCESS_FS_EXECUTE
                | ACCESS_FS_READ_FILE
                | ACCESS_FS_READ_DIR
                | ACCESS_FS_REFER
                | HANDLED_FS_WRITE_ACCESS
        }
        LandlockGrant::WriteFile => ACCESS_FS_READ_FILE | ACCESS_FS_WRITE_FILE | ACCESS_FS_TRUNCATE,
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

#[cfg(test)]
#[path = "tests/ruleset.rs"]
mod tests;
