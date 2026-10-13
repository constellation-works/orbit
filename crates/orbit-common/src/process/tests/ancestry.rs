//! The macOS process tree names the calling process and a live child, and
//! drops the child once it has been reaped.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::process::OwnedChild;
use crate::process::ancestry::{self, ProcessTreeProcess};

#[test]
fn a_child_is_in_the_process_tree_until_it_exits() {
    const TEST: &str = "process::tests::ancestry::a_child_is_in_the_process_tree_until_it_exits";
    let Some(tree) = ancestry::process_tree(std::process::id()) else {
        match crate::test_env::ps_stat(std::process::id()) {
            crate::test_env::PsRun::Denied(reason) => {
                crate::test_env::report_ps_denied(TEST, &reason);
                return;
            }
            crate::test_env::PsRun::Ran(output) => panic!(
                "process tree is unreadable but ps ran (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    };
    let self_pid = std::process::id();
    let mine = tree
        .iter()
        .find(|process| process.key.pid == self_pid)
        .expect("the tree includes its root");
    assert!(
        mine.phys_footprint > 0,
        "ri_phys_footprint of this process is non-zero"
    );
    // A second read cannot match the first byte for byte: the walk allocates
    // between them. It still has to be the footprint, which stays at or under
    // the lifetime maximum, and nearer that counter than resident size when
    // the two differ.
    let direct = direct_rusage(self_pid);
    assert!(
        mine.phys_footprint <= direct.lifetime_max,
        "tree footprint {} exceeds ri_lifetime_max_phys_footprint {}",
        mine.phys_footprint,
        direct.lifetime_max
    );
    if direct.phys_footprint.abs_diff(direct.resident_size) > 1024 * 1024 {
        let nearer_footprint = mine.phys_footprint.abs_diff(direct.phys_footprint)
            <= mine.phys_footprint.abs_diff(direct.resident_size);
        assert!(
            nearer_footprint,
            "tree reports resident size rather than ri_phys_footprint: tree {} footprint {} resident {}",
            mine.phys_footprint, direct.phys_footprint, direct.resident_size
        );
    }

    let child = Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let mut child = OwnedChild::from_spawned(child);
    let child_pid = child.pid();
    let deadline = Instant::now() + Duration::from_secs(5);
    let found = loop {
        let current = ancestry::process_tree(self_pid);
        if current.is_none() {
            child.kill_and_reap();
            match crate::test_env::ps_stat(self_pid) {
                crate::test_env::PsRun::Denied(reason) => {
                    crate::test_env::report_ps_denied(TEST, &reason);
                    return;
                }
                crate::test_env::PsRun::Ran(_) => {
                    panic!("tree became unreadable after spawning a child, and ps was allowed")
                }
            }
        }
        if current
            .as_ref()
            .is_some_and(|processes| contains_pid(processes, child_pid))
        {
            break current.expect("just checked");
        }
        if Instant::now() > deadline {
            child.kill_and_reap();
            panic!("sleep {child_pid} never appeared in the process tree: {current:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        found.iter().any(|process| process.key.pid == child_pid),
        "direct child is part of the tree"
    );

    child.kill_and_reap();
    let after = ancestry::process_tree(self_pid).expect("tree still readable after reap");
    assert!(
        !contains_pid(&after, child_pid),
        "a reaped child is not still in the tree"
    );
}

fn contains_pid(processes: &[ProcessTreeProcess], pid: u32) -> bool {
    processes.iter().any(|process| process.key.pid == pid)
}

struct DirectRusage {
    phys_footprint: u64,
    resident_size: u64,
    lifetime_max: u64,
}

fn direct_rusage(pid: u32) -> DirectRusage {
    use std::mem::MaybeUninit;

    let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // Safety: `info` is a writable `rusage_info_v4`. The flavor selects that
    // layout; a non-zero return leaves the value unread.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage of this process");
    // Safety: a zero return wrote the structure.
    let info = unsafe { info.assume_init() };
    DirectRusage {
        phys_footprint: info.ri_phys_footprint,
        resident_size: info.ri_resident_size,
        lifetime_max: info.ri_lifetime_max_phys_footprint,
    }
}

/// Kernel-call interleavings must never attribute a replacement process's
/// usage or descendants to the tree the walk started from.
#[test]
fn tree_walk_omits_reused_pids_and_reparented_children() {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use crate::process::ancestry::ProcessStartKey;
    use crate::process::ancestry::darwin_tree::{ReadError, read_process_with, snapshot_with};

    // Usage is charged to one identity only if the start key and parent read
    // around it agree. Reuse or reparenting reads as an exit; a refused read
    // fails closed.
    for (name, second_start, rusage_denied, expected) in [
        ("stable", Ok((20, 1)), false, Ok(1)),
        (
            "pid reused during usage read",
            Ok((21, 1)),
            false,
            Err("gone"),
        ),
        (
            "reparented during usage read",
            Ok((20, 9)),
            false,
            Err("gone"),
        ),
        ("identity reread denied", Err(()), false, Err("unreadable")),
        ("usage read denied", Ok((20, 1)), true, Err("unreadable")),
    ] {
        let reads = Cell::new(0);
        let measured = read_process_with(
            2,
            |_| {
                reads.set(reads.get() + 1);
                match (reads.get(), second_start) {
                    (1, _) => Ok((20, 1)),
                    (_, Ok(start)) => Ok(start),
                    (_, Err(())) => Err(ReadError::Unreadable),
                }
            },
            |_| {
                if rusage_denied {
                    Err(ReadError::Unreadable)
                } else {
                    Ok((900_000_000, 4096))
                }
            },
        );
        match (measured, expected) {
            (Ok((process, parent)), Ok(expected_parent)) => {
                assert_eq!(
                    process.key,
                    ProcessStartKey {
                        pid: 2,
                        starttime: 20
                    },
                    "{name}"
                );
                assert_eq!(process.cpu_time_ns, 900_000_000, "{name}");
                assert_eq!(parent, expected_parent, "{name}");
            }
            (Err(ReadError::Gone), Err("gone"))
            | (Err(ReadError::Unreadable), Err("unreadable")) => {}
            (measured, _) => panic!("{name}: {measured:?}"),
        }
    }

    // Tree R(1) -> A(2) -> B(3) -> C(4), each start key `pid * 10`.
    // Every fault lands at one kernel-call boundary of the walk.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Fault {
        None,
        RootGone,
        ChildGone,
        ChildReparented,
        ChildListingGone,
        RootReplacedDuringListing,
        ParentReplacedBeforeChildRead,
        ParentReparentedBeforeChildRead,
        ChildReplacedDuringListing,
        ParentReadDenied,
    }
    for (fault, expected) in [
        (Fault::None, Some(vec![1, 2, 3, 4])),
        (Fault::RootGone, None),
        (Fault::ChildGone, Some(vec![1])),
        (Fault::ChildReparented, Some(vec![1])),
        (Fault::ChildListingGone, Some(vec![1, 2])),
        (Fault::RootReplacedDuringListing, None),
        (Fault::ParentReplacedBeforeChildRead, Some(vec![1])),
        (Fault::ParentReparentedBeforeChildRead, Some(vec![1, 2, 3])),
        (Fault::ChildReplacedDuringListing, Some(vec![1, 2])),
        (Fault::ParentReadDenied, None),
    ] {
        let listed = RefCell::new(Vec::new());
        let start_reads = RefCell::new(HashMap::<u32, u32>::new());
        let parent_reparented = Cell::new(false);
        let original = |pid: u32| (u64::from(pid) * 10, pid.saturating_sub(1));
        let tree = snapshot_with(
            1,
            |pid| {
                // B has listed C and passed its post-listing identity check.
                // A now exits, reparenting B outside R; C still names B.
                if fault == Fault::ParentReparentedBeforeChildRead && pid == 4 {
                    parent_reparented.set(true);
                }
                if fault == Fault::RootGone && pid == 1 || fault == Fault::ChildGone && pid == 2 {
                    return Err(ReadError::Gone);
                }
                let (starttime, mut parent) = original(pid);
                if fault == Fault::ChildReparented && pid == 2 {
                    parent = 9;
                }
                Ok((
                    ProcessTreeProcess {
                        key: ProcessStartKey { pid, starttime },
                        cpu_time_ns: u64::from(pid) * 1_000,
                        phys_footprint: u64::from(pid) * 4_096,
                    },
                    parent,
                ))
            },
            |pid| {
                listed.borrow_mut().push(pid);
                match pid {
                    2 if fault == Fault::ChildListingGone => Err(ReadError::Gone),
                    1..=3 => Ok(vec![pid + 1]),
                    _ => Ok(Vec::new()),
                }
            },
            |pid| {
                let reads = {
                    let mut counts = start_reads.borrow_mut();
                    let count = counts.entry(pid).or_default();
                    *count += 1;
                    *count
                };
                let after_listing = listed.borrow().contains(&pid);
                let (starttime, parent) = original(pid);
                let replaced = Ok((starttime + 1, parent));
                match (fault, pid) {
                    (Fault::RootReplacedDuringListing, 1) if after_listing => replaced,
                    (Fault::ChildReplacedDuringListing, 2) if after_listing => replaced,
                    // The root's first reread follows its own listing; the
                    // second is the ownership check for its queued child.
                    (Fault::ParentReplacedBeforeChildRead, 1) if reads > 1 => replaced,
                    (Fault::ParentReadDenied, 1) if reads > 1 => Err(ReadError::Unreadable),
                    (Fault::ParentReparentedBeforeChildRead, 3) if parent_reparented.get() => {
                        Ok((starttime, 9))
                    }
                    _ => Ok((starttime, parent)),
                }
            },
        );
        let pids = tree.map(|tree| {
            tree.iter()
                .map(|process| process.key.pid)
                .collect::<Vec<_>>()
        });
        assert_eq!(pids, expected, "{fault:?}");
    }
}
