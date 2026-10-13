//! The macOS process tree names the calling process and a live child, and
//! drops the child once it has been reaped.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sleep");
    let child_pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let found = loop {
        let current = ancestry::process_tree(self_pid);
        if current.is_none() {
            let _ = child.kill();
            let _ = child.wait();
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
            let _ = child.kill();
            let _ = child.wait();
            panic!("sleep {child_pid} never appeared in the process tree: {current:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        found.iter().any(|process| process.key.pid == child_pid),
        "direct child is part of the tree"
    );

    child.kill().expect("kill sleep");
    child.wait().expect("reap sleep");
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
    use crate::process::ancestry::ProcessStartKey;
    use crate::process::ancestry::darwin_tree::{ReadError, read_process_with, snapshot_with};
    use std::cell::Cell;

    // A stable read is charged; reuse or reparenting during the usage read
    // is an exit of the original process, not CPU under its former key.
    for after in [(20, 1), (21, 1), (20, 9)] {
        let mut reads = 0;
        let measured = read_process_with(
            2,
            |_| {
                reads += 1;
                Ok(if reads == 1 { (20, 1) } else { after })
            },
            |_| Ok((900_000_000, 4096)),
        );
        if after == (20, 1) {
            let (process, parent) = measured.unwrap();
            assert_eq!(
                process.key,
                ProcessStartKey {
                    pid: 2,
                    starttime: 20
                }
            );
            assert_eq!(process.cpu_time_ns, 900_000_000);
            assert_eq!(parent, 1);
        } else {
            assert!(matches!(measured, Err(ReadError::Gone)), "{after:?}");
        }
    }

    // Make each race happen at its kernel boundary. An unrelated replacement
    // has a different parent; a parent can also be reused during its listing
    // or before its queued child is read. Denied reads fail the whole sample.
    for scenario in [
        "stable",
        "child_parent",
        "parent_listing",
        "parent_reuse",
        "denied",
    ] {
        let listed_child = Cell::new(false);
        let root_reads = Cell::new(0);
        let tree = snapshot_with(
            1,
            |pid| {
                let parent = if pid == 1 {
                    0
                } else if scenario == "child_parent" {
                    9
                } else {
                    1
                };
                Ok((
                    ProcessTreeProcess {
                        key: ProcessStartKey {
                            pid,
                            starttime: u64::from(pid) * 10,
                        },
                        cpu_time_ns: u64::from(pid) * 1000,
                        phys_footprint: u64::from(pid) * 4096,
                    },
                    parent,
                ))
            },
            |pid| {
                if pid == 1 {
                    listed_child.set(true);
                    Ok(vec![2])
                } else {
                    Ok(Vec::new())
                }
            },
            |pid| {
                if pid == 1 {
                    root_reads.set(root_reads.get() + 1);
                    if scenario == "parent_listing" && listed_child.get()
                        || scenario == "parent_reuse" && root_reads.get() > 1
                    {
                        return Ok((11, 0));
                    }
                    if scenario == "denied" && root_reads.get() > 1 {
                        return Err(ReadError::Unreadable);
                    }
                }
                Ok((u64::from(pid) * 10, if pid == 1 { 0 } else { 1 }))
            },
        );
        match scenario {
            "parent_listing" | "denied" => assert!(tree.is_none(), "{scenario}: {tree:?}"),
            "stable" => assert_eq!(tree.unwrap().len(), 2, "{scenario}"),
            _ => {
                let tree = tree.unwrap();
                assert_eq!(tree.len(), 1, "{scenario}: {tree:?}");
                assert_eq!(
                    tree[0].key.pid, 1,
                    "only the original root belongs to this tree"
                );
            }
        }
    }
}
