//! The descriptor contract between the two `pre_exec` hooks a confined spawn
//! registers.
//!
//! `attach_inherited_fds` overwrites every target number in the forked child.
//! Anything the parent opened for its own use in that child — today the
//! Landlock ruleset — is read *after* that remap, so it has to live above
//! every target. These pin the numbers rather than the spawn, so no ordering
//! of the rest of the suite can decide whether the invariant holds.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};

use super::{InheritedFd, relocate_clear_of_targets};

/// An `OwnedFd` on a file holding `contents`, so a relocation can be checked
/// to have carried the open file with it rather than opened a new one.
fn credential(contents: &str) -> (tempfile::TempDir, OwnedFd) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("record");
    let mut file = std::fs::File::create(&path).expect("create");
    file.write_all(contents.as_bytes()).expect("write");
    drop(file);
    let fd = OwnedFd::from(std::fs::File::open(&path).expect("open"));
    (dir, fd)
}

fn cloexec(fd: &OwnedFd) -> bool {
    // SAFETY: `fd` is an open descriptor owned by the caller and `F_GETFD`
    // only reads its flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    assert!(flags >= 0, "F_GETFD: {}", std::io::Error::last_os_error());
    flags & libc::FD_CLOEXEC != 0
}

#[test]
fn a_spawn_that_inherits_nothing_leaves_the_descriptor_alone() {
    let (_dir, fd) = credential("kept");
    let before = fd.as_raw_fd();

    let after = relocate_clear_of_targets(fd, &[]).expect("no targets is a no-op");

    assert_eq!(
        after.as_raw_fd(),
        before,
        "with nothing to collide with there is nothing to move"
    );
}

#[test]
fn a_descriptor_already_above_every_target_stays_where_it_is() {
    let (_dir, fd) = credential("kept");
    let before = fd.as_raw_fd();
    let targets = [InheritedFd {
        source: before,
        target: before - 1,
    }];

    let after = relocate_clear_of_targets(fd, &targets).expect("clear of the target");

    assert_eq!(
        after.as_raw_fd(),
        before,
        "relocation is for overlap, not for every spawn"
    );
}

/// The regression this file exists for: a ruleset that lands on the number the
/// child remaps is replaced by the credential, and `landlock_restrict_self`
/// answers `EBADFD`. The relocation has to clear *every* target, not just the
/// one the descriptor happened to be sitting on.
#[test]
fn a_descriptor_on_a_remapped_number_is_lifted_above_every_target() {
    let (_dir, fd) = credential("credential-bytes\n");
    let collision = fd.as_raw_fd();
    let highest = collision + 5;
    let targets = [
        InheritedFd {
            source: collision,
            target: collision,
        },
        InheritedFd {
            source: collision,
            target: highest,
        },
    ];

    let after = relocate_clear_of_targets(fd, &targets).expect("relocate off the target");

    assert!(
        after.as_raw_fd() > highest,
        "moved to {} which the child still overwrites (targets {collision} and {highest})",
        after.as_raw_fd()
    );
    assert!(
        cloexec(&after),
        "a relocated ruleset that lost close-on-exec leaks into every child"
    );

    // Same open file, not a re-open: the relocation must not depend on the
    // descriptor still being nameable by path.
    let mut moved = std::fs::File::from(after);
    let mut read = String::new();
    moved.read_to_string(&mut read).expect("read the moved fd");
    assert_eq!(read, "credential-bytes\n");
}
